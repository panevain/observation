//! Autosave to `localStorage`, and JSON scene import/export.

use crate::platform::{KeyValueStore, Platform};
use crate::state::{
    AddressJump, Editor, LoadArea, Models, Panels, Scene, Toasts, TrackModels, TrackScene,
};
use crate::theme::UiFont;
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_client_model::persist::{
    ADDRESS_JUMP_KEY, Autosaver, EXPORT_NAME, FONT_KEY, RestoreOutcome, SCENE_ACCEPT, SCENE_MIME,
};
use observation_core::scene::SceneFile;

#[derive(Resource)]
pub struct Persist {
    /// The text of a picked file, or why it could not be read.
    import_tx: Sender<Result<String, String>>,
    import_rx: Receiver<Result<String, String>>,
}

impl Default for Persist {
    fn default() -> Self {
        let (import_tx, import_rx) = unbounded();
        Self {
            import_tx,
            import_rx,
        }
    }
}

/// The scene's autosave and how its last write went, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Autosave(Autosaver);

/// Requests from the UI.
#[derive(Debug, Clone, Copy, Message)]
pub enum PersistAction {
    Export,
    Import,
}

pub struct PersistPlugin;

impl Plugin for PersistPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Persist>()
            .init_resource::<Autosave>()
            .add_message::<PersistAction>()
            .add_systems(Startup, restore)
            .add_systems(
                Update,
                (
                    // After the frame's edits: an Export includes them.
                    handle_actions
                        .after(crate::history::record)
                        .after(crate::input::PointerSet),
                    receive_imports,
                    autosave,
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

/// Loads the autosaved scene, or the demo street on first run.
fn restore(
    mut scene: ResMut<Scene>,
    mut models: ResMut<Models>,
    mut load: MessageWriter<LoadArea>,
    mut panels: ResMut<Panels>,
    mut toasts: ResMut<Toasts>,
    platform: Res<Platform>,
) {
    let area = RestoreOutcome::read(&*platform.local).apply(
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
    mut saver: ResMut<Autosave>,
    platform: Res<Platform>,
) {
    if !saver.due(time.delta_secs()) {
        return;
    }
    let Some(file) = scene.to_file(&models) else {
        return;
    };
    saver.save(file.to_json(), real.elapsed_secs_f64(), &*platform.local);
}

fn handle_actions(
    mut actions: MessageReader<PersistAction>,
    p: Res<Persist>,
    scene: Res<Scene>,
    models: Res<Models>,
    mut toasts: ResMut<Toasts>,
    platform: Res<Platform>,
) {
    for a in actions.read() {
        match a {
            PersistAction::Export => match scene.to_file(&models) {
                Some(file) => {
                    if let Err(e) =
                        platform
                            .files
                            .download(EXPORT_NAME, SCENE_MIME, &file.to_json())
                    {
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
        }
    }
}

pub fn receive_imports(
    p: Res<Persist>,
    mut scene: ResMut<Scene>,
    mut models: ResMut<Models>,
    mut editor: ResMut<Editor>,
    mut toasts: ResMut<Toasts>,
    mut load: MessageWriter<LoadArea>,
) {
    for picked in p.import_rx.try_iter() {
        let text = match picked {
            Ok(text) => text,
            Err(e) => {
                toasts.error_sticky(format!("Import failed: could not read the file: {e}"));
                continue;
            }
        };
        match SceneFile::from_json(&text) {
            Ok(file) => {
                let area = file.area;
                let n = file.cameras.len();
                scene.load_file(file, &mut models);
                editor.selected = None;
                load.write(LoadArea::new(area, true));
                toasts.info(format!("Imported scene with {n} camera(s)"));
            }
            Err(e) => toasts.error_sticky(format!("Import failed: {e}")),
        }
    }
}

#[cfg(test)]
mod golden;
