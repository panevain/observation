//! Undo and redo for the whole document: cameras, analysis settings, the
//! region, custom models and the loaded area.
//!
//! Edits are recorded as snapshots once they settle: a drag, a held key or a
//! focused text field counts as one step when it ends, not one per frame.
//! Undoing across an area change reloads that area and keeps the restored
//! cameras.

use crate::state::{LoadArea, Loading, Models, Scene, Snapshot, ToastAction, UiState};
use bevy::input::touch::Touches;
use bevy::prelude::*;
use bevy_egui::input::EguiWantsInput;

/// Steps kept in each direction.
const MAX_STEPS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOp {
    Undo,
    Redo,
}

#[derive(Default, Resource)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The document as of the last recorded step; `None` until the first area
    /// has loaded, so the start-up restore is not an undoable step.
    last: Option<Snapshot>,
    last_revision: u64,
}

impl History {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// What undoing would revert, for tooltips.
    pub fn undo_label(&self) -> Option<String> {
        Some(describe(self.undo.last()?, self.last.as_ref()?))
    }

    /// What redoing would reapply, for tooltips.
    pub fn redo_label(&self) -> Option<String> {
        Some(describe(self.last.as_ref()?, self.redo.last()?))
    }
}

pub struct HistoryPlugin;

impl Plugin for HistoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<History>()
            .add_systems(Update, (shortcuts, record).chain());
    }
}

/// Ctrl/Cmd+Z undoes; Ctrl/Cmd+Shift+Z and Ctrl/Cmd+Y redo. Text fields keep
/// their own undo while focused.
fn shortcuts(keys: Res<ButtonInput<KeyCode>>, wants: Res<EguiWantsInput>, mut ui: ResMut<UiState>) {
    if wants.wants_any_keyboard_input() {
        return;
    }
    // Held now, or let go this very frame (a quick chord on a slow frame).
    let held = |k: [KeyCode; 2]| keys.any_pressed(k) || keys.any_just_released(k);
    let command = held([KeyCode::ControlLeft, KeyCode::ControlRight])
        || held([KeyCode::SuperLeft, KeyCode::SuperRight]);
    if !command {
        return;
    }
    let shift = held([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if keys.just_pressed(KeyCode::KeyZ) {
        ui.history_request = Some(if shift {
            HistoryOp::Redo
        } else {
            HistoryOp::Undo
        });
    } else if keys.just_pressed(KeyCode::KeyY) {
        ui.history_request = Some(HistoryOp::Redo);
    }
}

#[allow(clippy::too_many_arguments)]
fn record(
    mut history: ResMut<History>,
    mut scene: ResMut<Scene>,
    mut models: ResMut<Models>,
    mut ui: ResMut<UiState>,
    loading: Res<Loading>,
    mouse: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
    wants: Res<EguiWantsInput>,
    mut load: MessageWriter<LoadArea>,
) {
    let h = &mut *history;
    if let Some(op) = ui.history_request.take() {
        let (from, to) = match op {
            HistoryOp::Undo => (&mut h.undo, &mut h.redo),
            HistoryOp::Redo => (&mut h.redo, &mut h.undo),
        };
        if let (Some(target), Some(current)) = (from.pop(), h.last.take()) {
            let verb = match op {
                HistoryOp::Undo => "Undid",
                HistoryOp::Redo => "Redid",
            };
            let what = match op {
                HistoryOp::Undo => describe(&target, &current),
                HistoryOp::Redo => describe(&current, &target),
            };
            // Any offered Undo now points at a different step.
            ui.toasts
                .retain(|t| !matches!(t.action, Some(ToastAction::Undo)));
            ui.info(format!("{verb}: {what}"));
            if target.area != current.area
                && let Some(area) = target.area
            {
                load.write(LoadArea {
                    area,
                    keep_cameras: true,
                });
            }
            to.push(current);
            scene.restore(&target, &mut models);
            if ui.selected.is_some_and(|id| scene.camera(id).is_none()) {
                ui.selected = None;
            }
            h.last = Some(target);
            h.last_revision = scene.revision;
        }
        return;
    }

    if scene.revision == h.last_revision {
        return;
    }
    // Wait for the edit to settle: nothing held, no text field focused, no
    // area in flight.
    let busy = mouse.get_pressed().next().is_some()
        || touches.iter().next().is_some()
        || wants.wants_any_keyboard_input()
        || loading.area.is_some();
    if busy || scene.area.is_none() {
        return;
    }
    let now = scene.snapshot(&models);
    h.last_revision = scene.revision;
    let Some(last) = h.last.take() else {
        h.last = Some(now);
        return;
    };
    if last == now {
        h.last = Some(last);
        return;
    }
    if let Some(text) = destructive(&last, &now) {
        ui.info_with(text, ToastAction::Undo);
    }
    h.undo.push(last);
    if h.undo.len() > MAX_STEPS {
        h.undo.remove(0);
    }
    h.redo.clear();
    h.last = Some(now);
}

/// A step that removes work worth offering an Undo for, described.
fn destructive(before: &Snapshot, after: &Snapshot) -> Option<String> {
    let lost = before
        .cameras
        .iter()
        .filter(|c| !after.cameras.iter().any(|a| a.id == c.id))
        .count();
    if before.area != after.area && lost > 0 {
        return Some(format!("New area loaded; {lost} camera(s) removed"));
    }
    match lost {
        0 => None,
        1 => before
            .cameras
            .iter()
            .find(|c| !after.cameras.iter().any(|a| a.id == c.id))
            .map(|c| format!("Deleted {}", c.label)),
        n => Some(format!("{n} cameras removed")),
    }
}

/// A short name for the change from `before` to `after`.
fn describe(before: &Snapshot, after: &Snapshot) -> String {
    if before.area != after.area {
        return "area change".to_owned();
    }
    let added: Vec<_> = after
        .cameras
        .iter()
        .filter(|c| !before.cameras.iter().any(|b| b.id == c.id))
        .collect();
    let removed: Vec<_> = before
        .cameras
        .iter()
        .filter(|c| !after.cameras.iter().any(|a| a.id == c.id))
        .collect();
    match (added.as_slice(), removed.as_slice()) {
        ([c], []) => return format!("add {}", c.label),
        ([], [c]) => return format!("delete {}", c.label),
        ([], []) => {}
        _ => return "camera changes".to_owned(),
    }
    let changed: Vec<_> = after
        .cameras
        .iter()
        .filter(|c| before.cameras.iter().any(|b| b.id == c.id && b != *c))
        .collect();
    match changed.as_slice() {
        [c] => format!("edit {}", c.label),
        [] if before.region != after.region => "region change".to_owned(),
        [] if before.params != after.params => "analysis settings".to_owned(),
        [] if before.custom_models != after.custom_models => "custom model".to_owned(),
        [] => "change".to_owned(),
        _ => "camera changes".to_owned(),
    }
}
