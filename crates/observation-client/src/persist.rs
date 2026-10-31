//! Autosave to `localStorage`, and JSON scene import/export.

use crate::state::{LoadArea, Models, SaveState, Scene, UiState};
use crate::theme::UiFont;
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_core::scene::SceneFile;

const STORAGE_KEY: &str = "observation.scene.v1";
/// The chosen UI font (a [`UiFont::key`]).
const FONT_KEY: &str = "observation.font";
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

fn receive_imports(
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
                ui.forget_missing_place_model(&models);
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

#[cfg(target_arch = "wasm32")]
mod web {
    use crate::net::js_cause;
    use crate::state::SaveFailure;
    use crossbeam_channel::Sender;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    pub fn storage_get(key: &str) -> Option<String> {
        storage()?.get_item(key).ok().flatten()
    }

    /// Whether the value was stored, and if not why. Quota and privacy errors
    /// are non-fatal: the status bar says so and the user can still export.
    pub fn storage_set(key: &str, value: &str) -> Result<(), SaveFailure> {
        let window = web_sys::window().ok_or(SaveFailure::Unavailable)?;
        // Reading `localStorage` itself throws when site data is blocked.
        let storage = match window.local_storage() {
            Ok(Some(s)) => s,
            Ok(None) => return Err(SaveFailure::Unavailable),
            Err(_) => return Err(SaveFailure::Blocked),
        };
        storage.set_item(key, value).map_err(|e| {
            let name = js_sys::Reflect::get(&e, &"name".into())
                .ok()
                .and_then(|n| n.as_string());
            match name.as_deref() {
                Some("QuotaExceededError" | "NS_ERROR_DOM_QUOTA_REACHED") => SaveFailure::Full,
                _ => SaveFailure::Blocked,
            }
        })
    }

    pub fn download(name: &str, text: &str) -> Result<(), String> {
        let doc = web_sys::window()
            .and_then(|w| w.document())
            .ok_or("the browser gave the app no page to download from")?;
        let parts = js_sys::Array::of1(&JsValue::from_str(text));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type("application/json");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts)
            .map_err(|e| format!("could not prepare the file ({})", js_cause(&e)))?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)
            .map_err(|e| format!("could not prepare the download ({})", js_cause(&e)))?;
        let a = doc
            .create_element("a")
            .map(|e| e.unchecked_into::<web_sys::HtmlAnchorElement>())
            .map_err(|e| format!("could not start the download ({})", js_cause(&e)))?;
        a.set_href(&url);
        a.set_download(name);
        a.click();
        let _ = web_sys::Url::revoke_object_url(&url);
        Ok(())
    }

    pub fn pick_file(tx: Sender<Result<String, String>>) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            let _ = tx.send(Err(
                "the browser gave the app no page to ask from".to_owned()
            ));
            return;
        };
        let input = match doc
            .create_element("input")
            .map(|e| e.unchecked_into::<web_sys::HtmlInputElement>())
        {
            Ok(input) => input,
            Err(e) => {
                let _ = tx.send(Err(format!(
                    "could not open the file picker ({})",
                    js_cause(&e)
                )));
                return;
            }
        };
        input.set_type("file");
        input.set_accept(".json,application/json");
        let el = input.clone();
        let on_change = Closure::once_into_js(move || {
            let Some(file) = el.files().and_then(|f| f.get(0)) else {
                return;
            };
            wasm_bindgen_futures::spawn_local(async move {
                let read = match wasm_bindgen_futures::JsFuture::from(file.text()).await {
                    Ok(v) => v.as_string().ok_or_else(|| "it is not text".to_owned()),
                    Err(e) => Err(js_cause(&e)),
                };
                let _ = tx.send(read);
            });
        });
        input.set_onchange(Some(on_change.unchecked_ref()));
        input.click();
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod web {
    use crate::state::SaveFailure;
    use crossbeam_channel::Sender;

    pub fn storage_get(_: &str) -> Option<String> {
        None
    }
    pub fn storage_set(_: &str, _: &str) -> Result<(), SaveFailure> {
        Ok(())
    }
    pub fn download(name: &str, text: &str) -> Result<(), String> {
        std::fs::write(name, text).map_err(|e| format!("could not write {name} ({e})"))
    }
    pub fn pick_file(_: Sender<Result<String, String>>) {}
}

use web::{download, pick_file, storage_get, storage_set};
