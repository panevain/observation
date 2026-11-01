//! Undo and redo for the whole document: cameras, analysis settings, the
//! region, custom models and the loaded area.
//!
//! Edits are recorded as snapshots once they settle: a drag, a held key or a
//! focused text field counts as one step when it ends, not one per frame.
//! Undoing across an area change reloads that area and keeps the restored
//! cameras.

use crate::state::{LoadArea, Loading, Models, Scene, Snapshot, ToastAction, Tool, UiState};
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::touch::Touches;
use bevy::prelude::*;
use bevy_egui::input::EguiWantsInput;
use observation_core::camera::CameraModel;

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
///
/// Reads key events by their logical key, so the shortcuts follow the key
/// labels on any layout (Z and Y swap places on QWERTZ), and every press
/// counts: macOS browsers send no key-up for keys pressed with Cmd held, so
/// the Z key's pressed state cannot be trusted there.
fn shortcuts(
    mut events: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    wants: Res<EguiWantsInput>,
    mut ui: ResMut<UiState>,
) {
    if wants.wants_any_keyboard_input() {
        events.clear();
        return;
    }
    // Held now, or let go this very frame (a quick chord on a slow frame).
    let held = |k: [KeyCode; 2]| keys.any_pressed(k) || keys.any_just_released(k);
    let command = held([KeyCode::ControlLeft, KeyCode::ControlRight])
        || held([KeyCode::SuperLeft, KeyCode::SuperRight]);
    let shift = held([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    for e in events.read() {
        if !command || e.state != ButtonState::Pressed {
            continue;
        }
        let Key::Character(c) = &e.logical_key else {
            continue;
        };
        match c.to_lowercase().as_str() {
            "z" if shift => ui.history_request = Some(HistoryOp::Redo),
            "z" => ui.history_request = Some(HistoryOp::Undo),
            "y" => ui.history_request = Some(HistoryOp::Redo),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn record(
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
    // An edit in progress (a drag, a focused text field) or an area in
    // flight: neither record nor jump, or the half-done edit would be lost.
    let busy = mouse.get_pressed().next().is_some()
        || touches.iter().next().is_some()
        || wants.wants_any_keyboard_input()
        || loading.area.is_some()
        // Cameras from a flat-ground scene, still waiting for their area to
        // lift them onto the terrain: recording them would let an undo sink them.
        || scene.on_flat_ground;
    if busy {
        ui.history_request = None;
        // The first snapshot can be taken mid-load: the start-up restore must
        // be undoable to if the user replaces it before its area arrives.
        if h.last.is_none() && scene.area.is_some() && !scene.on_flat_ground {
            h.last = Some(scene.snapshot(&models));
            h.last_revision = scene.revision;
        }
        return;
    }

    // Record a settled edit before any jump, so it is part of the history.
    if scene.revision != h.last_revision && scene.area.is_some() {
        let now = scene.snapshot(&models);
        h.last_revision = scene.revision;
        match h.last.take() {
            None => h.last = Some(now),
            Some(last) if last == now => h.last = Some(last),
            Some(last) => {
                // An offered Undo belongs to the step before this one.
                ui.toasts
                    .retain(|t| !matches!(t.action, Some(ToastAction::Undo)));
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
        }
    }

    let Some(op) = ui.history_request.take() else {
        return;
    };
    let (from, to) = match op {
        HistoryOp::Undo => (&mut h.undo, &mut h.redo),
        HistoryOp::Redo => (&mut h.redo, &mut h.undo),
    };
    // Check both before taking either: an empty stack must not drop `last`.
    if from.is_empty() || h.last.is_none() {
        return;
    }
    let (Some(target), Some(current)) = (from.pop(), h.last.take()) else {
        return;
    };
    let (verb, what) = match op {
        HistoryOp::Undo => ("Undid", describe(&target, &current)),
        HistoryOp::Redo => ("Redid", describe(&current, &target)),
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
    // The notice described a restored or imported file; once undo or redo
    // moves away from that step it no longer describes the scene.
    ui.data_change = None;
    // UI state that pointed into the old document.
    if ui.selected.is_some_and(|id| scene.camera(id).is_none()) {
        ui.selected = None;
    }
    if ui.selected.is_none() && ui.tool.needs_selection() {
        ui.tool = Tool::Select;
    }
    h.last = Some(target);
    h.last_revision = scene.revision;
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
        0 => deleted_model(before, after).map(|m| format!("Deleted model {}", m.display_name())),
        1 => before
            .cameras
            .iter()
            .find(|c| !after.cameras.iter().any(|a| a.id == c.id))
            .map(|c| format!("Deleted {}", c.label)),
        n => Some(format!("{n} cameras removed")),
    }
}

/// The custom model a step deleted, if deleting it is all the step did (as
/// the inspector does; an import that drops a model changes more).
fn deleted_model<'a>(before: &'a Snapshot, after: &Snapshot) -> Option<&'a CameraModel> {
    // Exhaustive, so a new field cannot be left out of the comparison.
    let Snapshot {
        area,
        origin,
        cameras,
        params,
        region,
        custom_models: b,
    } = before;
    let a = &after.custom_models;
    let rest_equal = *area == after.area
        && *origin == after.origin
        && *cameras == after.cameras
        && *params == after.params
        && *region == after.region;
    if !rest_equal || b.len() != a.len() + 1 {
        return None;
    }
    // The first difference is the deleted model; the rest must follow in order.
    let i = b.iter().zip(a).take_while(|(x, y)| x == y).count();
    b.get(i).filter(|_| b.get(i + 1..) == a.get(i..))
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
