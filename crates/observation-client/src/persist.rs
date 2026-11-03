//! Autosave to browser storage, one scene at a time (see [`crate::slots`]),
//! and JSON scene import/export.

use crate::history::History;
use crate::slots::{self, SlotError, SlotIndex, Slots, Store, slot_key, stored_len};
use crate::state::{
    AddressJump, LoadArea, Loading, Models, SaveFailure, SaveState, Scene, ToastAction, UiState,
};
use crate::theme::UiFont;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_core::scene::SceneFile;

/// The chosen UI font (a [`UiFont::key`]).
const FONT_KEY: &str = "observation.font";
/// Whether an Address tool click scrolls the camera list (`on` or `off`).
const ADDRESS_JUMP_KEY: &str = "observation.address-jump";
/// Seconds between autosave checks.
const AUTOSAVE_INTERVAL: f32 = 1.5;

/// A file the user picked to import.
pub struct Picked {
    pub name: String,
    pub text: String,
}

#[derive(Resource)]
pub struct Persist {
    /// The JSON last written to the open scene's slot.
    last_saved: String,
    timer: f32,
    /// Nothing is written: the open slot holds a scene that could not be
    /// read (and no other could be opened), or the list of scenes is in a
    /// newer format. Opening or starting a scene ends it.
    paused: bool,
    /// A picked file, or why it could not be read.
    import_tx: Sender<Result<Picked, String>>,
    import_rx: Receiver<Result<Picked, String>>,
    /// Picked files waiting for an area load to finish.
    pending: Vec<Picked>,
}

impl Default for Persist {
    fn default() -> Self {
        let (import_tx, import_rx) = unbounded();
        Self {
            last_saved: String::new(),
            timer: 0.0,
            paused: false,
            import_tx,
            import_rx,
            pending: Vec::new(),
        }
    }
}

/// Requests from the UI.
#[derive(Debug, Clone, Message)]
pub enum PersistAction {
    Export,
    Import,
    /// Read the list of scenes and their sizes again (the Scenes dialog opened).
    RefreshScenes,
    /// Start an empty scene on the current area, and open it.
    NewScene,
    /// Copy the open scene, and open the copy.
    DuplicateScene,
    OpenScene(u32),
    RenameScene(u32, String),
    DeleteScene(u32),
}

pub struct PersistPlugin;

impl Plugin for PersistPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Persist>()
            .init_resource::<Slots>()
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

/// `localStorage` (natively, a map that lasts for the run).
struct Browser;

impl Store for Browser {
    fn get(&self, key: &str) -> Option<String> {
        storage_get(key)
    }
    fn set(&mut self, key: &str, value: &str) -> Result<(), SaveFailure> {
        storage_set(key, value)
    }
    fn remove(&mut self, key: &str) {
        storage_remove(key);
    }
    fn keys(&self) -> Vec<String> {
        storage_keys()
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

/// Loads the scene open last, or the demo street on first run.
fn restore(mut d: Doc) {
    d.slots.load(&Browser);
    if let Some(v) = d.slots.newer {
        d.p.paused = true;
        d.ui.error_sticky(format!(
            "{} Until then nothing is saved, and the demo street opens instead.",
            SlotError::Newer(v).reason()
        ));
        d.load.write(LoadArea::demo());
        return;
    }
    let stored = Slots::read(&Browser, d.slots.index.active);
    match stored.map(|s| SceneFile::from_json(&s)) {
        Some(Ok(file)) => {
            let area = file.area;
            d.scene.load_file(file, &mut d.models);
            d.load.write(LoadArea {
                area,
                keep_cameras: true,
            });
        }
        Some(Err(e)) => {
            // Left as it is, in case a newer app (or a fix by hand) can read
            // it: the demo goes into a scene of its own.
            let name = d.slots.index.active_name().to_owned();
            match d.slots.create(&mut Browser, None, None) {
                Ok(_) => d.ui.error_sticky(format!(
                    "“{name}” could not be restored: {e}. It is kept as it was (delete it under Scenes if it is of no use), and the demo street opens as a new scene"
                )),
                Err(why) => {
                    d.p.paused = true;
                    d.ui.error_sticky(format!(
                        "“{name}” could not be restored: {e}. The demo street opens instead, and is not saved, so the stored scene is kept ({})",
                        why.reason()
                    ));
                }
            }
            d.load.write(LoadArea::demo());
        }
        None => {
            // A first visit; an empty scene of a returning user just gets the
            // demo.
            d.ui.help_open = !SlotIndex::stored(&Browser);
            d.load.write(LoadArea::demo());
        }
    }
}

fn autosave(time: Res<Time>, mut d: Doc) {
    d.p.timer += time.delta_secs();
    if d.p.timer < AUTOSAVE_INTERVAL {
        return;
    }
    d.p.timer = 0.0;
    // A refusal is in the status bar; the next check tries again.
    let _ = d.save_open_scene();
}

/// The document and the scene list, for saving and switching scenes.
#[derive(SystemParam)]
pub struct Doc<'w> {
    p: ResMut<'w, Persist>,
    scene: ResMut<'w, Scene>,
    models: ResMut<'w, Models>,
    ui: ResMut<'w, UiState>,
    history: ResMut<'w, History>,
    slots: ResMut<'w, Slots>,
    loading: Res<'w, Loading>,
    load: MessageWriter<'w, LoadArea>,
    real: Res<'w, Time<Real>>,
}

impl Doc<'_> {
    /// Writes the open scene if it changed since it was last written, and
    /// records the outcome for the status bar.
    fn save_open_scene(&mut self) -> Result<(), SaveFailure> {
        if self.p.paused {
            return Ok(());
        }
        let Some(file) = self.scene.to_file(&self.models) else {
            return Ok(());
        };
        let json = file.to_json();
        if json == self.p.last_saved {
            return Ok(());
        }
        let active = self.slots.index.active;
        if let Err(why) = Browser.set(&slot_key(active), &json) {
            // `last_saved` is left alone, so the next check tries again.
            self.ui.save = SaveState::Failed(why);
            return Err(why);
        }
        self.ui.save = SaveState::Saved(self.real.elapsed_secs_f64());
        self.slots.sizes.insert(active, stored_len(&json));
        self.p.last_saved = json;
        if let Err(why) = self.slots.ensure_listed(&mut Browser) {
            bevy::log::warn!("could not list the open scene again: {}", why.reason());
        }
        Ok(())
    }

    /// Whether an area is loading, which holds back switching scenes (a
    /// reply for the old scene would land in the new one); says so if it is.
    fn busy(&mut self) -> bool {
        let busy = self.loading.area.is_some();
        if busy {
            self.ui
                .error("Wait for the area to finish loading, then try again");
        }
        busy
    }

    /// Saves the open scene before leaving it; if that fails, says so and
    /// returns false, and the scene stays open.
    fn leave(&mut self) -> bool {
        let Err(why) = self.save_open_scene() else {
            return true;
        };
        let room = if why == SaveFailure::Full {
            ", or delete a scene to make room"
        } else {
            ""
        };
        self.ui.error_sticky(format!(
            "{} The open scene's latest changes could not be saved, so it stays open. Export it to keep your work{room}",
            why.reason()
        ));
        false
    }

    /// Makes `file` the document (an empty one on the current area if
    /// `None`), as stored in the slot just opened.
    fn show(&mut self, file: Option<SceneFile>, stored: String) {
        match file {
            Some(file) => {
                let area = file.area;
                self.scene.load_file(file, &mut self.models);
                self.load.write(LoadArea {
                    area,
                    keep_cameras: true,
                });
            }
            None => self.scene.clear_document(&mut self.models),
        }
        self.p.last_saved = stored;
        self.p.paused = false;
        self.ui.save = SaveState::Saved(self.real.elapsed_secs_f64());
        self.ui.selected = None;
        if self.ui.tool.needs_selection() {
            self.ui.tool = crate::state::Tool::Select;
        }
        self.ui.region_draft.clear();
        self.ui.data_change = None;
        // The undo steps were the other scene's, and so was any area load to
        // retry.
        self.history.reset();
        self.ui
            .toasts
            .retain(|t| !matches!(t.action, Some(ToastAction::Undo | ToastAction::Retry(_))));
    }

    /// Scene `id` as stored, checked before anything changes so an unreadable
    /// scene is never written over: `None` (and an error shown) if it does not
    /// parse; a file of `None` if the slot holds nothing yet.
    fn read(&mut self, id: u32, name: &str) -> Option<(Option<SceneFile>, String)> {
        let stored = Slots::read(&Browser, id);
        match stored.as_deref().map(SceneFile::from_json) {
            Some(Ok(file)) => Some((Some(file), stored.unwrap_or_default())),
            Some(Err(e)) => {
                self.ui
                    .error_sticky(format!("“{name}” could not be opened: {e}"));
                None
            }
            None => Some((None, String::new())),
        }
    }

    /// Opens scene `id`, saving the open one first.
    fn open(&mut self, id: u32) {
        let Some(name) = self.slots.index.get(id).map(|s| s.name.clone()) else {
            return;
        };
        let Some((file, stored)) = self.read(id, &name) else {
            return;
        };
        if !self.leave() {
            return;
        }
        match self.slots.open(&mut Browser, id) {
            Ok(true) => {
                // An empty slot gets a blank scene, written at the next autosave.
                self.show(file, stored);
                self.ui.info(format!("Opened “{name}”"));
            }
            Ok(false) => self
                .ui
                .error(format!("“{name}” was deleted in another tab")),
            Err(why) => self
                .ui
                .error(format!("Could not open “{name}”: {}", why.reason())),
        }
    }

    fn new_scene(&mut self) {
        if !self.leave() {
            return;
        }
        let blank = self.scene.blank_file().map(|f| f.to_json());
        match self.slots.create(&mut Browser, None, blank.as_deref()) {
            Ok(_) => {
                self.show(None, blank.unwrap_or_default());
                let name = self.slots.index.active_name().to_owned();
                self.ui.info(format!("Started “{name}”"));
            }
            Err(why) => self
                .ui
                .error(format!("Could not start a new scene: {}", why.reason())),
        }
    }

    fn duplicate(&mut self) {
        if !self.leave() {
            return;
        }
        let Some(json) = self.scene.to_file(&self.models).map(|f| f.to_json()) else {
            self.ui
                .error("Nothing to copy yet; wait for an area to finish loading");
            return;
        };
        let name = slots::copy_name(self.slots.index.active_name());
        match self.slots.create(&mut Browser, Some(name), Some(&json)) {
            Ok(_) => {
                // Same document, so its undo steps still apply.
                self.p.last_saved = json;
                self.p.paused = false;
                let name = self.slots.index.active_name().to_owned();
                self.ui
                    .info(format!("Copied to “{name}”, which is now open"));
            }
            Err(why) => self
                .ui
                .error(format!("Could not copy the scene: {}", why.reason())),
        }
    }

    fn delete(&mut self, id: u32) {
        let Some(name) = self.slots.index.get(id).map(|s| s.name.clone()) else {
            return;
        };
        if id != self.slots.index.active {
            match self.slots.delete(&mut Browser, id) {
                Ok(()) => self.ui.info(format!("Deleted “{name}”")),
                Err(why) => self
                    .ui
                    .error(format!("Could not delete “{name}”: {}", why.reason())),
            }
            return;
        }
        let Some(next) = self.slots.neighbour(id) else {
            self.ui
                .error("This is the only scene; start a new one before deleting it");
            return;
        };
        let next_name = self
            .slots
            .index
            .get(next)
            .map_or_else(String::new, |s| s.name.clone());
        let Some((file, stored)) = self.read(next, &next_name) else {
            return;
        };
        // One write removes this scene and opens the next, so if it is
        // refused this one is still open with its unsaved changes.
        match self.slots.delete_open(&mut Browser, next) {
            Ok(true) => {
                self.show(file, stored);
                self.ui
                    .info(format!("Deleted “{name}”; “{next_name}” is open"));
            }
            Ok(false) => self
                .ui
                .error(format!("“{next_name}” was deleted in another tab")),
            Err(why) => self
                .ui
                .error(format!("Could not delete “{name}”: {}", why.reason())),
        }
    }

    fn rename(&mut self, id: u32, name: &str) {
        if let Err(why) = self.slots.rename(&mut Browser, id, name) {
            self.ui
                .error(format!("Could not rename the scene: {}", why.reason()));
        }
    }
}

pub fn handle_actions(mut actions: MessageReader<PersistAction>, mut d: Doc) {
    for a in actions.read() {
        match a {
            PersistAction::Export => match d.scene.to_file(&d.models) {
                Some(file) => {
                    let name = export_name(d.slots.index.active_name());
                    if let Err(e) = download(&name, &file.to_json()) {
                        d.ui.error(format!("Could not export the scene: {e}"));
                    }
                }
                None => {
                    d.ui.error("Nothing to export yet; wait for an area to finish loading")
                }
            },
            PersistAction::Import => pick_file(d.p.import_tx.clone()),
            PersistAction::RefreshScenes => d.slots.refresh(&Browser),
            PersistAction::RenameScene(id, name) => d.rename(*id, name),
            PersistAction::NewScene if !d.busy() => d.new_scene(),
            PersistAction::DuplicateScene if !d.busy() => d.duplicate(),
            PersistAction::OpenScene(id) if *id != d.slots.index.active && !d.busy() => {
                d.open(*id);
            }
            PersistAction::DeleteScene(id) if !d.busy() => d.delete(*id),
            PersistAction::NewScene
            | PersistAction::DuplicateScene
            | PersistAction::OpenScene(_)
            | PersistAction::DeleteScene(_) => {}
        }
    }
}

/// A download file name for a scene: its name with anything a file system
/// might object to replaced, then `.json`.
fn export_name(scene: &str) -> String {
    let stem: String = scene
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let stem = stem.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if stem.is_empty() {
        "observation-scene.json".to_owned()
    } else {
        format!("{stem}.json")
    }
}

/// An imported file becomes a scene of its own, so the open one is kept.
/// Files picked while an area loads wait for it (its reply would otherwise
/// land in the imported scene).
pub fn receive_imports(mut d: Doc) {
    let picked: Vec<_> = d.p.import_rx.try_iter().collect();
    for picked in picked {
        match picked {
            Ok(p) => d.p.pending.push(p),
            Err(e) => {
                d.ui.error_sticky(format!("Import failed: could not read the file: {e}"))
            }
        }
    }
    if d.p.pending.is_empty() || d.loading.area.is_some() {
        return;
    }
    for Picked { name, text } in std::mem::take(&mut d.p.pending) {
        let file = match SceneFile::from_json(&text) {
            Ok(file) => file,
            Err(e) => {
                d.ui.error_sticky(format!("Import failed: {e}"));
                continue;
            }
        };
        let n = file.cameras.len();
        let json = file.to_json();
        let scene_name = slots::name_from_file(&name);
        let saved = d.save_open_scene().map_err(SlotError::from).and_then(|()| {
            d.slots
                .create(&mut Browser, Some(scene_name.clone()), Some(&json))
        });
        match saved {
            Ok(_) => {
                d.show(Some(file), json);
                d.ui.info(format!(
                    "Imported “{scene_name}” with {n} camera(s) as a new scene"
                ));
            }
            Err(SlotError::Storage(_)) if !storage_available() => {
                // No storage at all, so no other scene to keep: the import
                // replaces the document, which is not saved (the status bar
                // says why), and can be undone.
                let area = file.area;
                d.scene.load_file(file, &mut d.models);
                d.ui.selected = None;
                d.load.write(LoadArea {
                    area,
                    keep_cameras: true,
                });
                d.ui.info(format!("Imported scene with {n} camera(s)"));
            }
            Err(why) => {
                let room = if why.is_full() {
                    " Delete a scene to make room, or export the open one first"
                } else {
                    ""
                };
                d.ui.error_sticky(format!("Import failed: {}{room}", why.reason()));
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::Picked;
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

    pub fn storage_remove(key: &str) {
        if let Some(s) = storage() {
            let _ = s.remove_item(key);
        }
    }

    pub fn storage_keys() -> Vec<String> {
        let Some(s) = storage() else {
            return Vec::new();
        };
        let n = s.length().unwrap_or(0);
        (0..n).filter_map(|i| s.key(i).ok().flatten()).collect()
    }

    /// Whether the page can use `localStorage` at all.
    pub fn storage_available() -> bool {
        storage().is_some()
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

    pub fn pick_file(tx: Sender<Result<Picked, String>>) {
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
            let name = file.name();
            wasm_bindgen_futures::spawn_local(async move {
                let read = match wasm_bindgen_futures::JsFuture::from(file.text()).await {
                    Ok(v) => v.as_string().ok_or_else(|| "it is not text".to_owned()),
                    Err(e) => Err(js_cause(&e)),
                };
                let _ = tx.send(read.map(|text| Picked { name, text }));
            });
        });
        input.set_onchange(Some(on_change.unchecked_ref()));
        input.click();
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod web {
    use super::Picked;
    use crate::state::SaveFailure;
    use crossbeam_channel::Sender;
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        /// Storage for the run, so switching scenes works natively too.
        static STORE: RefCell<HashMap<String, String>> = RefCell::default();
    }

    pub fn storage_get(key: &str) -> Option<String> {
        STORE.with_borrow(|s| s.get(key).cloned())
    }
    pub fn storage_set(key: &str, value: &str) -> Result<(), SaveFailure> {
        STORE.with_borrow_mut(|s| s.insert(key.to_owned(), value.to_owned()));
        Ok(())
    }
    pub fn storage_remove(key: &str) {
        STORE.with_borrow_mut(|s| s.remove(key));
    }
    pub fn storage_keys() -> Vec<String> {
        STORE.with_borrow(|s| s.keys().cloned().collect())
    }
    pub fn storage_available() -> bool {
        true
    }
    pub fn download(name: &str, text: &str) -> Result<(), String> {
        std::fs::write(name, text).map_err(|e| format!("could not write {name} ({e})"))
    }
    pub fn pick_file(_: Sender<Result<Picked, String>>) {}
}

use web::{
    download, pick_file, storage_available, storage_get, storage_keys, storage_remove, storage_set,
};
