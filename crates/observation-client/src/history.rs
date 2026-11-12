//! Undo and redo for the whole document: cameras, analysis settings, the
//! region, custom models and the loaded area.
//!
//! Edits are recorded as snapshots once they settle: a drag, a held key or a
//! focused text field counts as one step when it ends, not one per frame.
//! Undoing across an area change reloads that area and keeps the restored
//! cameras.

use crate::state::{
    DataChangeNotice, Editor, LoadArea, Loading, Models, Scene, Toasts, TrackModels, TrackScene,
};
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::touch::Touches;
use bevy::prelude::*;
use bevy_egui::input::EguiWantsInput;
use observation_client_model::history;

pub use observation_client_model::history::HistoryOp;

/// The undo history, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct History(history::History);

/// Undo or redo, asked for by a shortcut or the UI. Like any request, it is
/// dropped while an edit is in progress or an area is in flight.
#[derive(Debug, Clone, Copy, Message)]
pub struct HistoryCommand(pub HistoryOp);

pub struct HistoryPlugin;

impl Plugin for HistoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<History>()
            .add_message::<HistoryCommand>()
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
    mut commands: MessageWriter<HistoryCommand>,
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
        let op = match c.to_lowercase().as_str() {
            "z" if shift => HistoryOp::Redo,
            "z" => HistoryOp::Undo,
            "y" => HistoryOp::Redo,
            _ => continue,
        };
        commands.write(HistoryCommand(op));
    }
}

/// Records settled edits and runs the last undo or redo asked for.
#[allow(clippy::too_many_arguments)]
pub fn record(
    mut history: ResMut<History>,
    mut scene: ResMut<Scene>,
    mut models: ResMut<Models>,
    mut notice: ResMut<DataChangeNotice>,
    mut editor: ResMut<Editor>,
    mut toasts: ResMut<Toasts>,
    loading: Res<Loading>,
    mouse: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
    wants: Res<EguiWantsInput>,
    mut commands: MessageReader<HistoryCommand>,
    mut load: MessageWriter<LoadArea>,
) {
    // An edit in progress (a drag, a focused text field) or an area in flight.
    let editing = mouse.get_pressed().next().is_some()
        || touches.iter().next().is_some()
        || wants.wants_any_keyboard_input()
        || loading.area().is_some();
    if !history.observe(&scene, &models, editing, &mut toasts) {
        commands.clear();
        return;
    }
    // The last request wins.
    let Some(&HistoryCommand(op)) = commands.read().last() else {
        return;
    };
    let Some(outcome) = history.step(
        op,
        &mut TrackScene(&mut scene),
        &mut TrackModels(&mut models),
        &mut editor,
        &mut toasts,
    ) else {
        return;
    };
    if let Some(area) = outcome.reload {
        load.write(LoadArea::new(area, true));
    }
    // The notice described a restored or imported file; once undo or redo
    // moves away from that step it no longer describes the scene.
    **notice = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::message::Messages;
    use observation_core::camera::{CameraPlacement, Catalog};
    use observation_core::provenance::DataChange;
    use observation_core::scene::AreaRef;

    /// The history systems over the resources they use, with the demo area
    /// loaded and one camera added as an undoable step.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Models::new(Catalog::default()))
            .init_resource::<Scene>()
            .init_resource::<DataChangeNotice>()
            .init_resource::<Editor>()
            .init_resource::<Toasts>()
            .init_resource::<Loading>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<Touches>()
            .init_resource::<EguiWantsInput>()
            .add_message::<KeyboardInput>()
            .add_message::<LoadArea>()
            .add_plugins(HistoryPlugin);
        {
            let mut scene = app.world_mut().resource_mut::<Scene>();
            scene.area = Some(AreaRef::Demo);
            scene.touch();
        }
        app.update();
        app.world_mut()
            .resource_mut::<Scene>()
            .add_camera(CameraPlacement {
                id: 0,
                label: "Porch".to_owned(),
                model_id: "m".to_owned(),
                position: [0.0, 3.0, 0.0],
                yaw_deg: 0.0,
                pitch_deg: 0.0,
                roll_deg: 0.0,
                color: None,
                enabled: true,
                group: None,
            });
        app.update();
        assert!(app.world().resource::<History>().can_undo());
        app
    }

    fn command(app: &mut App, op: HistoryOp) {
        app.world_mut()
            .resource_mut::<Messages<HistoryCommand>>()
            .write(HistoryCommand(op));
    }

    fn undo(app: &mut App) {
        command(app, HistoryOp::Undo);
    }

    fn cameras(app: &App) -> usize {
        app.world().resource::<Scene>().cameras.len()
    }

    #[test]
    fn a_command_undoes_on_the_next_run_and_clears_the_data_notice() {
        let mut app = app();
        **app.world_mut().resource_mut::<DataChangeNotice>() = Some(DataChange {
            saved: Some(1),
            now: Some(2),
            saved_app: String::new(),
            now_app: String::new(),
        });
        // As the UI does: written after this frame's `record`, seen by the next.
        undo(&mut app);
        app.update();
        assert_eq!(cameras(&app), 0);
        assert_eq!(**app.world().resource::<DataChangeNotice>(), None);
        assert!(app.world().resource::<Messages<LoadArea>>().is_empty());
        // Read once: it does not undo again.
        app.update();
        assert!(!app.world().resource::<History>().can_undo());
        assert!(app.world().resource::<History>().can_redo());
    }

    #[test]
    fn a_command_is_dropped_while_busy_not_delayed() {
        let mut app = app();
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        undo(&mut app);
        app.update();
        assert_eq!(cameras(&app), 1);
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .release(MouseButton::Left);
        app.update();
        assert_eq!(cameras(&app), 1, "the request was dropped");
        assert!(app.world().resource::<History>().can_undo());
    }

    #[test]
    fn of_two_commands_in_one_frame_the_last_wins_and_both_are_used_up() {
        // Redo then Undo: Redo alone has nothing to do, so only the last
        // one can explain the undo.
        let mut app = app();
        command(&mut app, HistoryOp::Redo);
        undo(&mut app);
        app.update();
        assert_eq!(cameras(&app), 0);
        // Undo then Redo: the camera is back only if Redo was not dropped
        // for coming second, and the next frame finds nothing left to run.
        let mut app = app_with_redo();
        undo(&mut app);
        command(&mut app, HistoryOp::Redo);
        app.update();
        assert_eq!(cameras(&app), 1);
        app.update();
        assert_eq!(cameras(&app), 1);
        assert!(app.world().resource::<History>().can_undo());
        assert!(!app.world().resource::<History>().can_redo());
    }

    /// As [`app`], with the camera undone so a redo is offered.
    fn app_with_redo() -> App {
        let mut app = app();
        undo(&mut app);
        app.update();
        assert_eq!(cameras(&app), 0);
        app
    }
}
