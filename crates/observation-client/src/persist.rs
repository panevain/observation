//! Autosave to `localStorage`, and JSON scene import/export.

use crate::state::{AddressJump, LoadArea, Models, SaveState, Scene, UiState};
use crate::theme::UiFont;
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_core::scene::SceneFile;

const STORAGE_KEY: &str = "observation.scene.v1";
/// The chosen UI font (a [`UiFont::key`]).
const FONT_KEY: &str = "observation.font";
/// Whether an Address tool click scrolls the camera list (`on` or `off`).
const ADDRESS_JUMP_KEY: &str = "observation.address-jump";
/// Seconds between autosave checks.
const AUTOSAVE_INTERVAL: f32 = 1.5;

#[derive(Resource)]
pub struct Persist {
    last_saved: String,
    timer: f32,
    /// The text of a picked file, or why it could not be read.
    import_tx: Sender<Result<String, String>>,
    import_rx: Receiver<Result<String, String>>,
}

impl Default for Persist {
    fn default() -> Self {
        let (import_tx, import_rx) = unbounded();
        Self {
            last_saved: String::new(),
            timer: 0.0,
            import_tx,
            import_rx,
        }
    }
}

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
            .add_message::<PersistAction>()
            .add_systems(Startup, restore)
            .add_systems(
                Update,
                (
                    handle_actions,
                    receive_imports,
                    autosave,
                    save_font.run_if(resource_changed::<UiFont>),
                    save_address_jump.run_if(resource_changed::<AddressJump>),
                ),
            );
    }
}

/// The UI font saved in this browser, else the default.
pub fn load_font() -> UiFont {
    storage_get(FONT_KEY)
        .and_then(|k| UiFont::from_key(&k))
        .unwrap_or_default()
}

fn save_font(font: Res<UiFont>) {
    // Not worth a warning: the font just reverts on the next visit.
    if !font.is_added()
        && let Err(why) = storage_set(FONT_KEY, font.key())
    {
        bevy::log::warn!("could not store the font choice: {why:?}");
    }
}

/// The Address tool's jump setting saved in this browser, else on.
pub fn load_address_jump() -> AddressJump {
    match storage_get(ADDRESS_JUMP_KEY).as_deref() {
        Some("off") => AddressJump(false),
        _ => AddressJump::default(),
    }
}

fn save_address_jump(jump: Res<AddressJump>) {
    let value = if jump.0 { "on" } else { "off" };
    if !jump.is_added()
        && let Err(why) = storage_set(ADDRESS_JUMP_KEY, value)
    {
        bevy::log::warn!("could not store the address jump setting: {why:?}");
    }
}

/// Loads the autosaved scene, or the demo street on first run.
fn restore(
    mut scene: ResMut<Scene>,
    mut models: ResMut<Models>,
    mut load: MessageWriter<LoadArea>,
    mut ui: ResMut<UiState>,
) {
    match storage_get(STORAGE_KEY).map(|s| SceneFile::from_json(&s)) {
        Some(Ok(file)) => {
            let area = file.area;
            scene.load_file(file, &mut models);
            load.write(LoadArea {
                area,
                keep_cameras: true,
            });
        }
        Some(Err(e)) => {
            ui.error_sticky(format!(
                "Your saved scene could not be restored: {e}. Starting with the demo street instead"
            ));
            load.write(LoadArea::demo());
        }
        None => {
            ui.help_open = true;
            load.write(LoadArea::demo());
        }
    }
}

fn autosave(
    time: Res<Time>,
    real: Res<Time<Real>>,
    scene: Res<Scene>,
    models: Res<Models>,
    mut p: ResMut<Persist>,
    mut ui: ResMut<UiState>,
) {
    p.timer += time.delta_secs();
    if p.timer < AUTOSAVE_INTERVAL {
        return;
    }
    p.timer = 0.0;
    let Some(file) = scene.to_file(&models) else {
        return;
    };
    let json = file.to_json();
    if json != p.last_saved {
        match storage_set(STORAGE_KEY, &json) {
            Ok(()) => {
                ui.save = SaveState::Saved(real.elapsed_secs_f64());
                p.last_saved = json;
            }
            Err(why) => {
                // Leave `last_saved` alone so the next check tries again.
                ui.save = SaveState::Failed(why);
            }
        }
    }
}

fn handle_actions(
    mut actions: MessageReader<PersistAction>,
    p: Res<Persist>,
    scene: Res<Scene>,
    models: Res<Models>,
    mut ui: ResMut<UiState>,
) {
    for a in actions.read() {
        match a {
            PersistAction::Export => match scene.to_file(&models) {
                Some(file) => {
                    if let Err(e) = download("observation-scene.json", &file.to_json()) {
                        ui.error(format!("Could not export the scene: {e}"));
                    }
                }
                None => ui.error("Nothing to export yet; wait for an area to finish loading"),
            },
            PersistAction::Import => pick_file(p.import_tx.clone()),
        }
    }
}

pub fn receive_imports(
    p: Res<Persist>,
    mut scene: ResMut<Scene>,
    mut models: ResMut<Models>,
    mut ui: ResMut<UiState>,
    mut load: MessageWriter<LoadArea>,
) {
    for picked in p.import_rx.try_iter() {
        let text = match picked {
            Ok(text) => text,
            Err(e) => {
                ui.error_sticky(format!("Import failed: could not read the file: {e}"));
                continue;
            }
        };
        match SceneFile::from_json(&text) {
            Ok(file) => {
                let area = file.area;
                let n = file.cameras.len();
                scene.load_file(file, &mut models);
                ui.selected = None;
                load.write(LoadArea {
                    area,
                    keep_cameras: true,
                });
                ui.info(format!("Imported scene with {n} camera(s)"));
            }
            Err(e) => ui.error_sticky(format!("Import failed: {e}")),
        }
    }
}

use crate::platform::{download, pick_file, storage_get, storage_set};
