//! Autosave to `localStorage`, one scene at a time (see
//! [`observation_client_model::scenes`]), and JSON scene import/export.

use crate::history::History;
use crate::platform::{KeyValueStore, PickedFile, Platform};
use crate::state::{
    AddressJump, DataChangeNotice, Editor, LoadArea, Loading, Models, Panels, Scene, Toasts,
    TrackModels, TrackScene,
};
use crate::theme::UiFont;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_client_model::persist::{
    ADDRESS_JUMP_KEY, Autosaver, FONT_KEY, SCENE_ACCEPT, SCENE_MIME, export_name,
};
use observation_client_model::scenes::{self, SceneAction, SceneCtx};
use observation_client_model::slots::Slots;

#[derive(Resource)]
pub struct Persist {
    /// A picked file, or why it could not be read.
    import_tx: Sender<Result<PickedFile, String>>,
    import_rx: Receiver<Result<PickedFile, String>>,
    /// Picked files waiting for an area load to finish.
    pending: Vec<PickedFile>,
}

impl Default for Persist {
    fn default() -> Self {
        let (import_tx, import_rx) = unbounded();
        Self {
            import_tx,
            import_rx,
            pending: Vec::new(),
        }
    }
}

/// The open scene's autosave and how its last write went, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Autosave(Autosaver);

/// The scenes saved in this browser as this tab knows them, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct SceneSlots(Slots);

/// Requests from the UI.
#[derive(Debug, Clone, Message)]
pub enum PersistAction {
    Export,
    Import,
    /// A change from the Scenes dialog.
    Scene(SceneAction),
}

pub struct PersistPlugin;

impl Plugin for PersistPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Persist>()
            .init_resource::<Autosave>()
            .init_resource::<SceneSlots>()
            .add_message::<PersistAction>()
            .add_systems(Startup, restore)
            .add_systems(
                Update,
                (
                    // After the frame's edits: an Export includes them.
                    handle_actions
                        .after(crate::history::record)
                        .after(crate::input::PointerSet),
                    open_scenes,
                    // After a switch: the scene just opened is not written
                    // back, and the one left was written by the switch.
                    autosave.after(open_scenes),
                    save_font.run_if(resource_changed::<UiFont>),
                    save_address_jump.run_if(resource_changed::<AddressJump>),
                ),
            );
    }
}

/// The UI font saved in this browser, else the default.
pub fn load_font(store: &dyn KeyValueStore) -> UiFont {
    store
        .get(FONT_KEY)
        .and_then(|k| UiFont::from_key(&k))
        .unwrap_or_default()
}

fn save_font(font: Res<UiFont>, platform: Res<Platform>) {
    // Not worth a warning: the font just reverts on the next visit.
    if !font.is_added()
        && let Err(why) = platform.local.set(FONT_KEY, font.key())
    {
        bevy::log::warn!("could not store the font choice: {why:?}");
    }
}

/// The Address tool's jump setting saved in this browser, else on.
pub fn load_address_jump(store: &dyn KeyValueStore) -> AddressJump {
    match store.get(ADDRESS_JUMP_KEY).as_deref() {
        Some("off") => AddressJump(false),
        _ => AddressJump::default(),
    }
}

fn save_address_jump(jump: Res<AddressJump>, platform: Res<Platform>) {
    let value = if jump.0 { "on" } else { "off" };
    if !jump.is_added()
        && let Err(why) = platform.local.set(ADDRESS_JUMP_KEY, value)
    {
        bevy::log::warn!("could not store the address jump setting: {why:?}");
    }
}

/// Restores the scene open last, or the demo street on a first visit.
fn restore(
    mut scene: ResMut<Scene>,
    mut models: ResMut<Models>,
    mut slots: ResMut<SceneSlots>,
    mut saver: ResMut<Autosave>,
    mut load: MessageWriter<LoadArea>,
    mut panels: ResMut<Panels>,
    mut toasts: ResMut<Toasts>,
    platform: Res<Platform>,
) {
    let area = scenes::restore(
        &*platform.local,
        &mut slots,
        &mut saver,
        &mut TrackScene(&mut scene),
        &mut TrackModels(&mut models),
        &mut panels,
        &mut toasts,
    );
    load.write(area.into());
}

fn autosave(
    time: Res<Time>,
    real: Res<Time<Real>>,
    scene: Res<Scene>,
    models: Res<Models>,
    mut slots: ResMut<SceneSlots>,
    mut saver: ResMut<Autosave>,
    platform: Res<Platform>,
) {
    if !saver.due(time.delta_secs()) {
        return;
    }
    let Some(file) = scene.to_file(&models) else {
        return;
    };
    // A refusal is in the status bar; the next check tries again.
    let _ = scenes::autosave(
        &*platform.local,
        &mut slots,
        &mut saver,
        file.to_json(),
        real.elapsed_secs_f64(),
    );
}

fn handle_actions(
    mut actions: MessageReader<PersistAction>,
    p: Res<Persist>,
    scene: Res<Scene>,
    models: Res<Models>,
    slots: Res<SceneSlots>,
    mut toasts: ResMut<Toasts>,
    platform: Res<Platform>,
) {
    for a in actions.read() {
        match a {
            PersistAction::Export => match scene.to_file(&models) {
                Some(file) => {
                    let name = export_name(slots.index.active_name());
                    if let Err(e) = platform.files.download(&name, SCENE_MIME, &file.to_json()) {
                        toasts.error(format!("Could not export the scene: {e}"));
                    }
                }
                None => toasts.error("Nothing to export yet; wait for an area to finish loading"),
            },
            PersistAction::Import => {
                let tx = p.import_tx.clone();
                platform.files.pick_text(
                    SCENE_ACCEPT,
                    Box::new(move |picked| {
                        // The receiver lives as long as the app; a send error means shutdown.
                        let _ = tx.send(picked);
                    }),
                );
            }
            // `open_scenes` carries these out.
            PersistAction::Scene(_) => {}
        }
    }
}

/// What opening a scene changes, for [`open_scenes`].
#[derive(SystemParam)]
pub struct OpenScenes<'w> {
    p: ResMut<'w, Persist>,
    slots: ResMut<'w, SceneSlots>,
    saver: ResMut<'w, Autosave>,
    scene: ResMut<'w, Scene>,
    models: ResMut<'w, Models>,
    editor: ResMut<'w, Editor>,
    history: ResMut<'w, History>,
    notice: ResMut<'w, DataChangeNotice>,
    toasts: ResMut<'w, Toasts>,
    loading: Res<'w, Loading>,
    real: Res<'w, Time<Real>>,
    platform: Res<'w, Platform>,
}

/// Carries out the Scenes dialog's requests and imports picked files, each
/// as a scene of its own; files picked while an area loads wait for it.
pub fn open_scenes(
    mut actions: MessageReader<PersistAction>,
    mut d: OpenScenes,
    mut load: MessageWriter<LoadArea>,
) {
    for picked in d.p.import_rx.try_iter().collect::<Vec<_>>() {
        match picked {
            Ok(file) => d.p.pending.push(file),
            Err(e) => d
                .toasts
                .error_sticky(format!("Import failed: could not read the file: {e}")),
        }
    }
    let scene_actions: Vec<SceneAction> = actions
        .read()
        .filter_map(|a| match a {
            PersistAction::Scene(action) => Some(action.clone()),
            PersistAction::Export | PersistAction::Import => None,
        })
        .collect();
    if scene_actions.is_empty() && d.p.pending.is_empty() {
        return;
    }
    let mut ctx = SceneCtx {
        store: &*d.platform.local,
        slots: &mut d.slots,
        saver: &mut d.saver,
        doc: &mut TrackScene(&mut d.scene),
        models: &mut TrackModels(&mut d.models),
        editor: &mut d.editor,
        history: &mut d.history,
        notice: &mut d.notice,
        toasts: &mut d.toasts,
        loading: d.loading.area().is_some(),
        now: d.real.elapsed_secs_f64(),
    };
    for action in scene_actions {
        if let Some(area) = ctx.apply(action) {
            load.write(area.into());
            // The new area request holds back what follows in this frame.
            ctx.loading = true;
        }
    }
    if let Some(area) = ctx.import_pending(&mut d.p.pending) {
        load.write(area.into());
    }
}

#[cfg(test)]
mod golden;
