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
    import_tx: Sender<String>,
    import_rx: Receiver<String>,
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
    if !font.is_added() && !storage_set(FONT_KEY, font.key()) {
        bevy::log::warn!("could not store the font choice");
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
            ui.error(format!(
                "Saved scene could not be restored ({e}); starting fresh"
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
        if storage_set(STORAGE_KEY, &json) {
            ui.save = SaveState::Saved(real.elapsed_secs_f64());
            p.last_saved = json;
        } else {
            // Leave `last_saved` alone so the next check tries again.
            ui.save = SaveState::Failed;
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
                Some(file) => download("observation-scene.json", &file.to_json()),
                None => ui.error("Nothing to export yet"),
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
    for text in p.import_rx.try_iter() {
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
            Err(e) => ui.error(format!("Import failed: {e}")),
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use crossbeam_channel::Sender;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    pub fn storage_get(key: &str) -> Option<String> {
        storage()?.get_item(key).ok().flatten()
    }

    /// Whether the value was stored. Quota and privacy errors are non-fatal:
    /// the status bar says so and the user can still export.
    pub fn storage_set(key: &str, value: &str) -> bool {
        storage().is_some_and(|s| s.set_item(key, value).is_ok())
    }

    pub fn download(name: &str, text: &str) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let parts = js_sys::Array::of1(&JsValue::from_str(text));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type("application/json");
        let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts) else {
            return;
        };
        let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
            return;
        };
        if let Ok(a) = doc
            .create_element("a")
            .map(|e| e.unchecked_into::<web_sys::HtmlAnchorElement>())
        {
            a.set_href(&url);
            a.set_download(name);
            a.click();
        }
        let _ = web_sys::Url::revoke_object_url(&url);
    }

    pub fn pick_file(tx: Sender<String>) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Ok(input) = doc
            .create_element("input")
            .map(|e| e.unchecked_into::<web_sys::HtmlInputElement>())
        else {
            return;
        };
        input.set_type("file");
        input.set_accept(".json,application/json");
        let el = input.clone();
        let on_change = Closure::once_into_js(move || {
            let Some(file) = el.files().and_then(|f| f.get(0)) else {
                return;
            };
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(v) = wasm_bindgen_futures::JsFuture::from(file.text()).await
                    && let Some(s) = v.as_string()
                {
                    let _ = tx.send(s);
                }
            });
        });
        input.set_onchange(Some(on_change.unchecked_ref()));
        input.click();
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod web {
    use crossbeam_channel::Sender;

    pub fn storage_get(_: &str) -> Option<String> {
        None
    }
    pub fn storage_set(_: &str, _: &str) -> bool {
        true
    }
    pub fn download(name: &str, text: &str) {
        if let Err(e) = std::fs::write(name, text) {
            bevy::log::error!("export failed: {e}");
        }
    }
    pub fn pick_file(_: Sender<String>) {}
}

use web::{download, pick_file, storage_get, storage_set};
