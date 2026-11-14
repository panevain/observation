//! Opening, starting, copying, renaming, deleting and importing scenes, the
//! autosave of the open one, and restoring the one open last at start-up:
//! what the Scenes dialog, Import and the autosave do with
//! [`crate::slots`].
//!
//! Every switch writes the open scene first and leaves it open if that
//! write is refused; a scene that does not parse is never opened, so it is
//! never written over. Switching waits while an area loads, so the reply
//! cannot land in another scene.

use crate::commands::Tracked;
use crate::document::{Document, LoadArea, Models};
use crate::editor::{Editor, Panels};
use crate::history::History;
use crate::persist::{Autosaver, RestoreOutcome};
use crate::platform::{KeyValueStore, PickedFile, SaveFailure};
use crate::slots::{self, SlotError, SlotIndex, Slots, slot_key, stored_len};
use crate::toasts::Toasts;
use observation_core::provenance::DataChange;
use observation_core::scene::SceneFile;

/// A change to the scenes, asked for in the Scenes dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneAction {
    /// Read the list of scenes and their sizes again (the dialog opened).
    Refresh,
    /// Start an empty scene on the current area, and open it.
    New,
    /// Copy the open scene, and open the copy.
    Duplicate,
    Open(u32),
    Rename(u32, String),
    Delete(u32),
}

/// Reads the list of scenes and restores the one open last: it becomes the
/// document and keeps its cameras over its area. Returns the area to load.
///
/// A first visit opens the help over the demo street. A scene that cannot
/// be read is reported and kept as it is, and the demo street opens as a new
/// scene (if that scene cannot be made, nothing is saved, so the stored one
/// is still kept). A list in a newer format is left alone, and nothing is
/// saved.
pub fn restore(
    store: &dyn KeyValueStore,
    slots: &mut Slots,
    saver: &mut Autosaver,
    doc: &mut dyn Tracked<Document>,
    models: &mut dyn Tracked<Models>,
    panels: &mut Panels,
    toasts: &mut Toasts,
) -> LoadArea {
    slots.load(store);
    if let Some(v) = slots.newer {
        saver.pause();
        toasts.error_sticky(format!(
            "{} Until then nothing is saved, and the demo street opens instead.",
            SlotError::Newer(v).reason()
        ));
        return LoadArea::demo();
    }
    match RestoreOutcome::read(store, &slot_key(slots.index.active)) {
        RestoreOutcome::Saved(file) => {
            let area = file.area;
            doc.get_mut().load_file(*file, models.get_mut());
            if let Some(file) = doc.get().to_file(models.get()) {
                saver.restored(file.to_json());
            }
            LoadArea {
                area,
                keep_cameras: true,
            }
        }
        RestoreOutcome::Unreadable(e) => {
            // Left as it is, in case a newer app (or a fix by hand) can read
            // it: the demo goes into a scene of its own.
            let name = slots.index.active_name().to_owned();
            match slots.create(store, None, None) {
                Ok(_) => toasts.error_sticky(format!(
                    "“{name}” could not be restored: {e}. It is kept as it was (delete it under Scenes if it is of no use), and the demo street opens as a new scene"
                )),
                Err(why) => {
                    saver.pause();
                    toasts.error_sticky(format!(
                        "“{name}” could not be restored: {e}. The demo street opens instead, and is not saved, so the stored scene is kept ({})",
                        why.reason()
                    ));
                }
            }
            LoadArea::demo()
        }
        RestoreOutcome::Nothing => {
            // A first visit; an empty scene of a returning user just gets
            // the demo.
            panels.help_open = !SlotIndex::stored(store);
            LoadArea::demo()
        }
    }
}

/// Writes the open scene's `json` if it changed since it was last written,
/// recording the outcome for the status bar, and lists the open scene again
/// if another tab removed it from the list. The scene itself is stored even
/// when listing it again fails: that error is `Ok(Some(_))`, for the log, and
/// the next write tries again.
pub fn autosave(
    store: &dyn KeyValueStore,
    slots: &mut Slots,
    saver: &mut Autosaver,
    json: String,
    now: f64,
) -> Result<Option<SlotError>, SaveFailure> {
    let active = slots.index.active;
    let len = stored_len(&json);
    if saver.save(store, &slot_key(active), json, now)? {
        slots.sizes.insert(active, len);
        return Ok(slots.ensure_listed(store).err());
    }
    Ok(None)
}

/// What switching scenes may change.
pub struct SceneCtx<'a> {
    pub store: &'a dyn KeyValueStore,
    pub slots: &'a mut Slots,
    pub saver: &'a mut Autosaver,
    pub doc: &'a mut dyn Tracked<Document>,
    pub models: &'a mut dyn Tracked<Models>,
    pub editor: &'a mut Editor,
    pub history: &'a mut History,
    /// The Coverage card's "map data has changed" notice.
    pub notice: &'a mut Option<DataChange>,
    pub toasts: &'a mut Toasts,
    /// Whether an area request is in flight.
    pub loading: bool,
    /// Seconds since start-up.
    pub now: f64,
}

impl std::fmt::Debug for SceneCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneCtx")
            .field("slots", &self.slots)
            .field("saver", &self.saver)
            .field("doc", self.doc.get())
            .field("loading", &self.loading)
            .finish_non_exhaustive()
    }
}

impl SceneCtx<'_> {
    /// Carries out a request from the Scenes dialog; returns the area to
    /// load when a scene was opened.
    pub fn apply(&mut self, action: SceneAction) -> Option<LoadArea> {
        match action {
            SceneAction::Refresh => {
                self.slots.refresh(self.store);
                None
            }
            SceneAction::Rename(id, name) => {
                if let Err(why) = self.slots.rename(self.store, id, &name) {
                    self.toasts
                        .error(format!("Could not rename the scene: {}", why.reason()));
                }
                None
            }
            SceneAction::Open(id) if id == self.slots.index.active => None,
            // Deleting a scene that is not open switches nothing.
            SceneAction::Delete(id) if id != self.slots.index.active => self.delete(id),
            _ if self.busy() => None,
            SceneAction::New => {
                self.new_scene();
                None
            }
            SceneAction::Duplicate => {
                self.duplicate();
                None
            }
            SceneAction::Open(id) => self.open(id),
            SceneAction::Delete(id) => self.delete(id),
        }
    }

    /// Imports the picked files waiting in `pending`, unless an area is
    /// loading (its reply would land in the imported scene): each becomes a
    /// scene of its own, so the open one is kept. Stops at the first that
    /// loads an area, and returns that area; the files after it wait for it.
    pub fn import_pending(&mut self, pending: &mut Vec<PickedFile>) -> Option<LoadArea> {
        while !self.loading && !pending.is_empty() {
            let picked = pending.remove(0);
            if let Some(area) = self.import(picked) {
                self.loading = true;
                return Some(area);
            }
        }
        None
    }

    fn import(&mut self, PickedFile { name, text }: PickedFile) -> Option<LoadArea> {
        let file = match SceneFile::from_json(&text) {
            Ok(file) => file,
            Err(e) => {
                self.toasts.error_sticky(format!("Import failed: {e}"));
                return None;
            }
        };
        let n = file.cameras.len();
        let json = file.to_json();
        let scene_name = slots::name_from_file(&name);
        let saved = self
            .save_open_scene()
            .map_err(SlotError::from)
            .and_then(|()| {
                self.slots
                    .create(self.store, Some(scene_name.clone()), Some(&json))
            });
        match saved {
            Ok(_) => {
                let load = self.show(Some(file), json);
                self.toasts.info(format!(
                    "Imported “{scene_name}” with {n} camera(s) as a new scene"
                ));
                load
            }
            Err(SlotError::Storage(SaveFailure::Unavailable | SaveFailure::Blocked)) => {
                // No storage the page can use, so no other scene to keep: the import
                // replaces the document, which is not saved (the status bar
                // says why), and can be undone.
                let area = file.area;
                self.doc.get_mut().load_file(file, self.models.get_mut());
                self.editor.selected = None;
                self.toasts
                    .info(format!("Imported scene with {n} camera(s)"));
                Some(LoadArea {
                    area,
                    keep_cameras: true,
                })
            }
            Err(why) => {
                let room = if why.is_full() {
                    " Delete a scene to make room, or export the open one first"
                } else {
                    ""
                };
                self.toasts
                    .error_sticky(format!("Import failed: {}{room}", why.reason()));
                None
            }
        }
    }

    /// Writes the open scene if it changed since it was last written.
    fn save_open_scene(&mut self) -> Result<(), SaveFailure> {
        let Some(file) = self.doc.get().to_file(self.models.get()) else {
            return Ok(());
        };
        autosave(self.store, self.slots, self.saver, file.to_json(), self.now).map(|_| ())
    }

    /// Whether an area is loading, which holds back switching scenes (a
    /// reply for the old scene would land in the new one); says so if it is.
    fn busy(&mut self) -> bool {
        if self.loading {
            self.toasts
                .error("Wait for the area to finish loading, then try again");
        }
        self.loading
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
        self.toasts.error_sticky(format!(
            "{} The open scene's latest changes could not be saved, so it stays open. Export it to keep your work{room}",
            why.reason()
        ));
        false
    }

    /// Makes `file` the document (an empty one on the current area if
    /// `None`), as `stored` under the scene just opened. Returns the area to
    /// load.
    fn show(&mut self, file: Option<SceneFile>, stored: String) -> Option<LoadArea> {
        let load = match file {
            Some(file) => {
                let area = file.area;
                self.doc.get_mut().load_file(file, self.models.get_mut());
                Some(LoadArea {
                    area,
                    keep_cameras: true,
                })
            }
            None => {
                self.doc.get_mut().clear_document(self.models.get_mut());
                None
            }
        };
        self.saver.opened(stored, self.now);
        self.editor.selected = None;
        if self.editor.tool.needs_selection() {
            self.editor.tool = crate::editor::Tool::Select;
        }
        self.editor.region_draft.clear();
        self.editor.forget_missing_places(self.doc.get());
        *self.notice = None;
        // The undo steps were the other scene's, and so was any area load to
        // retry.
        self.history.reset();
        self.toasts.forget_undo();
        self.toasts.forget_retry();
        load
    }

    /// Scene `id` as stored, checked before anything changes so an
    /// unreadable scene is never written over: `None` (and an error shown)
    /// if it does not parse; a file of `None` if its key holds nothing yet.
    fn read(&mut self, id: u32, name: &str) -> Option<(Option<SceneFile>, String)> {
        let stored = Slots::read(self.store, id);
        match stored.as_deref().map(SceneFile::from_json) {
            Some(Ok(file)) => Some((Some(file), stored.unwrap_or_default())),
            Some(Err(e)) => {
                self.toasts
                    .error_sticky(format!("“{name}” could not be opened: {e}"));
                None
            }
            None => Some((None, String::new())),
        }
    }

    /// Opens scene `id`, saving the open one first.
    fn open(&mut self, id: u32) -> Option<LoadArea> {
        let name = self.slots.index.get(id)?.name.clone();
        let (file, stored) = self.read(id, &name)?;
        if !self.leave() {
            return None;
        }
        match self.slots.open(self.store, id) {
            Ok(true) => {
                // An empty scene gets a blank document, written at the next
                // autosave.
                let load = self.show(file, stored);
                self.toasts.info(format!("Opened “{name}”"));
                load
            }
            Ok(false) => {
                self.toasts
                    .error(format!("“{name}” was deleted in another tab"));
                None
            }
            Err(why) => {
                self.toasts
                    .error(format!("Could not open “{name}”: {}", why.reason()));
                None
            }
        }
    }

    fn new_scene(&mut self) {
        if !self.leave() {
            return;
        }
        let blank = self.doc.get().blank_file().map(|f| f.to_json());
        match self.slots.create(self.store, None, blank.as_deref()) {
            Ok(_) => {
                self.show(None, blank.unwrap_or_default());
                let name = self.slots.index.active_name().to_owned();
                self.toasts.info(format!("Started “{name}”"));
            }
            Err(why) => self
                .toasts
                .error(format!("Could not start a new scene: {}", why.reason())),
        }
    }

    fn duplicate(&mut self) {
        if !self.leave() {
            return;
        }
        let Some(json) = self
            .doc
            .get()
            .to_file(self.models.get())
            .map(|f| f.to_json())
        else {
            self.toasts
                .error("Nothing to copy yet; wait for an area to finish loading");
            return;
        };
        let name = slots::copy_name(self.slots.index.active_name());
        match self.slots.create(self.store, Some(name), Some(&json)) {
            Ok(_) => {
                // The same document, so its undo steps still apply.
                self.saver.opened(json, self.now);
                let name = self.slots.index.active_name().to_owned();
                self.toasts
                    .info(format!("Copied to “{name}”, which is now open"));
            }
            Err(why) => self
                .toasts
                .error(format!("Could not copy the scene: {}", why.reason())),
        }
    }

    fn delete(&mut self, id: u32) -> Option<LoadArea> {
        let name = self.slots.index.get(id)?.name.clone();
        if id != self.slots.index.active {
            match self.slots.delete(self.store, id) {
                Ok(()) => self.toasts.info(format!("Deleted “{name}”")),
                Err(why) => self
                    .toasts
                    .error(format!("Could not delete “{name}”: {}", why.reason())),
            }
            return None;
        }
        let Some(next) = self.slots.neighbour(id) else {
            self.toasts
                .error("This is the only scene; start a new one before deleting it");
            return None;
        };
        let next_name = self
            .slots
            .index
            .get(next)
            .map_or_else(String::new, |s| s.name.clone());
        let (file, stored) = self.read(next, &next_name)?;
        // One write removes this scene and opens the next, so if it is
        // refused this one is still open with its unsaved changes.
        match self.slots.delete_open(self.store, next) {
            Ok(true) => {
                let load = self.show(file, stored);
                self.toasts
                    .info(format!("Deleted “{name}”; “{next_name}” is open"));
                load
            }
            Ok(false) => {
                self.toasts
                    .error(format!("“{next_name}” was deleted in another tab"));
                None
            }
            Err(why) => {
                self.toasts
                    .error(format!("Could not delete “{name}”: {}", why.reason()));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::{STORAGE_KEY, SaveState};
    use crate::slots::INDEX_KEY;
    use crate::testing::{Counting, MemoryStore};
    use crate::toasts::{ToastAction, ToastKind};
    use observation_core::camera::{CameraPlacement, Catalog};
    use observation_core::geo::{BBox, LatLon};
    use observation_core::scene::AreaRef;

    fn osm() -> AreaRef {
        AreaRef::Osm {
            bbox: BBox {
                south: 51.5,
                west: -0.125,
                north: 51.5015,
                east: -0.1225,
            },
        }
    }

    fn camera(label: &str) -> CameraPlacement {
        CameraPlacement {
            id: 0,
            label: label.to_owned(),
            model_id: "m".to_owned(),
            position: [0.0, 3.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        }
    }

    /// A scene file on `area` with one camera called `label`.
    fn scene_json(area: AreaRef, label: &str) -> String {
        let mut file = SceneFile::new(area, LatLon::new(51.5, -0.12));
        file.cameras.push(CameraPlacement {
            id: 1,
            ..camera(label)
        });
        file.to_json()
    }

    /// Everything a scene change touches, over a store that may hold data.
    struct Fixture {
        store: MemoryStore,
        slots: Slots,
        saver: Autosaver,
        doc: Counting<Document>,
        models: Counting<Models>,
        editor: Editor,
        panels: Panels,
        history: History,
        notice: Option<DataChange>,
        toasts: Toasts,
        loading: bool,
    }

    impl Fixture {
        fn new(store: MemoryStore) -> Self {
            Self {
                store,
                slots: Slots::default(),
                saver: Autosaver::default(),
                doc: Counting::new(Document::default()),
                models: Counting::new(Models::new(Catalog::default())),
                editor: Editor::default(),
                panels: Panels::default(),
                history: History::default(),
                notice: None,
                toasts: Toasts::default(),
                loading: false,
            }
        }

        /// Starts the app over the store, as start-up does.
        fn restored(store: MemoryStore) -> (Self, LoadArea) {
            let mut f = Self::new(store);
            let load = restore(
                &f.store,
                &mut f.slots,
                &mut f.saver,
                &mut f.doc,
                &mut f.models,
                &mut f.panels,
                &mut f.toasts,
            );
            (f, load)
        }

        fn ctx(&mut self) -> SceneCtx<'_> {
            SceneCtx {
                store: &self.store,
                slots: &mut self.slots,
                saver: &mut self.saver,
                doc: &mut self.doc,
                models: &mut self.models,
                editor: &mut self.editor,
                history: &mut self.history,
                notice: &mut self.notice,
                toasts: &mut self.toasts,
                loading: self.loading,
                now: 10.0,
            }
        }

        fn apply(&mut self, action: SceneAction) -> Option<LoadArea> {
            self.ctx().apply(action)
        }

        fn import(&mut self, name: &str, text: &str) -> Option<LoadArea> {
            let mut pending = vec![PickedFile {
                name: name.to_owned(),
                text: text.to_owned(),
            }];
            let load = self.ctx().import_pending(&mut pending);
            assert!(pending.is_empty());
            load
        }

        fn autosave(&mut self) -> Result<Option<SlotError>, SaveFailure> {
            let json = self
                .doc
                .value
                .to_file(&self.models.value)
                .expect("an area is set")
                .to_json();
            autosave(&self.store, &mut self.slots, &mut self.saver, json, 1.0)
        }

        fn labels(&self) -> Vec<&str> {
            self.doc
                .value
                .cameras
                .iter()
                .map(|c| c.label.as_str())
                .collect()
        }

        fn names(&self) -> Vec<&str> {
            self.slots
                .index
                .slots
                .iter()
                .map(|s| s.name.as_str())
                .collect()
        }

        fn toasts(&self) -> Vec<(ToastKind, &str)> {
            self.toasts
                .iter()
                .map(|t| (t.kind, t.text.as_str()))
                .collect()
        }

        /// Adds a camera as an edit the history records as an undo step.
        fn edit(&mut self, label: &str) {
            // After a reset, the first settled document is only the start.
            let (doc, models) = (&self.doc.value, &self.models.value);
            assert!(self.history.observe(doc, models, false, &mut self.toasts));
            self.doc.value.add_camera(camera(label));
            let (doc, models) = (&self.doc.value, &self.models.value);
            assert!(self.history.observe(doc, models, false, &mut self.toasts));
        }
    }

    fn store_with(key: &str, value: &str) -> MemoryStore {
        let store = MemoryStore::default();
        store.set(key, value).expect("stored");
        store
    }

    // ----- restore ------------------------------------------------------

    #[test]
    fn a_first_visit_opens_the_help_over_the_demo_street() {
        let (f, load) = Fixture::restored(MemoryStore::default());
        assert_eq!(load, LoadArea::demo());
        assert!(f.panels.help_open);
        assert_eq!((f.doc.writes, f.models.writes), (0, 0));
        assert_eq!(f.toasts(), []);
        assert_eq!(f.names(), ["Scene 1"]);
    }

    #[test]
    fn a_returning_user_with_an_empty_scene_gets_the_demo_without_help() {
        let store = MemoryStore::default();
        store
            .set(
                INDEX_KEY,
                r#"{"version":1,"active":2,"slots":[{"id":0,"name":"A"},{"id":2,"name":"B"}]}"#,
            )
            .expect("stored");
        let (f, load) = Fixture::restored(store);
        assert_eq!(load, LoadArea::demo());
        assert!(!f.panels.help_open);
        assert_eq!(f.slots.index.active, 2);
    }

    #[test]
    fn a_scene_saved_before_scenes_existed_opens_as_the_first() {
        let json = scene_json(osm(), "Porch");
        let (f, load) = Fixture::restored(store_with(STORAGE_KEY, &json));
        assert_eq!(
            load,
            LoadArea {
                area: osm(),
                keep_cameras: true
            }
        );
        assert_eq!(f.labels(), ["Porch"]);
        assert_eq!((f.doc.writes, f.models.writes), (1, 1));
        assert_eq!(f.names(), ["Scene 1"]);
        assert!(!f.panels.help_open);
        assert_eq!(f.toasts(), []);
        // What is stored is not written back, over what another tab may
        // have stored since.
        let mut f = f;
        f.store.take_ops();
        assert_eq!(f.autosave(), Ok(None));
        assert_eq!(f.store.ops(), []);
    }

    #[test]
    fn the_scene_open_last_is_restored() {
        let store = store_with(STORAGE_KEY, &scene_json(osm(), "First"));
        store
            .set(&slot_key(4), &scene_json(osm(), "Fourth"))
            .expect("stored");
        store
            .set(
                INDEX_KEY,
                r#"{"version":1,"active":4,"slots":[{"id":0,"name":"A"},{"id":4,"name":"B"}]}"#,
            )
            .expect("stored");
        let (f, _) = Fixture::restored(store);
        assert_eq!(f.labels(), ["Fourth"]);
        assert_eq!(f.slots.index.active_name(), "B");
    }

    #[test]
    fn an_unreadable_scene_is_kept_and_the_demo_opens_as_a_new_scene() {
        let (mut f, load) = Fixture::restored(store_with(STORAGE_KEY, "{not json"));
        assert_eq!(load, LoadArea::demo());
        assert_eq!((f.doc.writes, f.models.writes), (0, 0));
        assert_eq!(f.names(), ["Scene 1", "Scene 2"]);
        assert_eq!(f.slots.index.active, 1);
        let toasts = f.toasts();
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].0, ToastKind::Error);
        assert!(
            toasts[0].1.starts_with("“Scene 1” could not be restored: ")
                && toasts[0]
                    .1
                    .ends_with("and the demo street opens as a new scene"),
            "{}",
            toasts[0].1
        );
        assert!(f.toasts.iter().all(|t| t.ttl.is_infinite()));
        // The demo is saved in the new scene, never over the unreadable one.
        f.doc.value.area = Some(AreaRef::Demo);
        f.autosave().expect("stored");
        assert_eq!(f.store.get(STORAGE_KEY).as_deref(), Some("{not json"));
        assert!(f.store.get(&slot_key(1)).is_some());
    }

    #[test]
    fn an_unreadable_scene_with_no_room_for_another_pauses_the_autosave() {
        let store = store_with(STORAGE_KEY, "{not json");
        store.refuse_writes(Some(SaveFailure::Full));
        let (mut f, load) = Fixture::restored(store);
        assert_eq!(load, LoadArea::demo());
        assert!(f.saver.is_paused());
        assert!(
            f.toasts()[0]
                .1
                .contains("is not saved, so the stored scene is kept")
        );
        f.store.refuse_writes(None);
        f.doc.value.area = Some(AreaRef::Demo);
        f.store.take_ops();
        f.autosave().expect("paused, not refused");
        assert_eq!(f.store.ops(), []);
    }

    #[test]
    fn a_list_in_a_newer_format_is_left_alone_and_nothing_is_saved() {
        let newer = r#"{"version":2,"active":4,"slots":[{"id":4,"name":"A"}]}"#;
        let (mut f, load) = Fixture::restored(store_with(INDEX_KEY, newer));
        assert_eq!(load, LoadArea::demo());
        assert!(f.saver.is_paused());
        assert!(
            f.toasts()[0]
                .1
                .contains("newer version of the app (format 2)")
        );
        f.doc.value.area = Some(AreaRef::Demo);
        f.store.take_ops();
        f.autosave().expect("paused");
        assert_eq!(f.store.ops(), []);
        assert_eq!(f.apply(SceneAction::New), None);
        assert_eq!(f.store.get(INDEX_KEY).as_deref(), Some(newer));
    }

    // ----- switching ----------------------------------------------------

    /// The app over one saved scene on the OSM area, with its edits so far
    /// recorded as undo steps.
    fn with_one_scene() -> Fixture {
        let (mut f, _) = Fixture::restored(store_with(STORAGE_KEY, &scene_json(osm(), "Porch")));
        f.edit("Gate");
        assert!(f.history.can_undo());
        f
    }

    #[test]
    fn a_new_scene_saves_the_open_one_and_starts_empty_on_the_same_area() {
        let mut f = with_one_scene();
        f.editor.selected = Some(1);
        f.editor.tool = crate::editor::Tool::Aim;
        f.notice = Some(DataChange {
            saved: Some(1),
            now: Some(2),
            saved_app: String::new(),
            now_app: String::new(),
        });
        f.toasts.info_with("Deleted", ToastAction::Undo);
        assert_eq!(f.apply(SceneAction::New), None, "no area to load");

        // The open scene was written first, with its latest edit.
        let first =
            SceneFile::from_json(&f.store.get(STORAGE_KEY).expect("saved")).expect("a scene");
        assert_eq!(first.cameras.len(), 2);
        assert_eq!(f.names(), ["Scene 1", "Scene 2"]);
        assert_eq!(f.slots.index.active, 1);
        assert_eq!(f.doc.value.area, Some(osm()));
        assert!(f.labels().is_empty());
        // Its blank file is stored, so the autosave has nothing to add.
        let blank = f.store.get(&slot_key(1)).expect("stored");
        f.store.take_ops();
        f.autosave().expect("stored");
        assert_eq!(f.store.ops(), []);
        assert_eq!(f.saver.status(), SaveState::Saved(10.0));
        // What belonged to the other scene is gone.
        assert!(!f.history.can_undo());
        assert_eq!(f.editor.selected, None);
        assert_eq!(f.editor.tool, crate::editor::Tool::Select);
        assert_eq!(f.notice, None);
        assert_eq!(f.toasts(), [(ToastKind::Info, "Started “Scene 2”")]);
        assert!(SceneFile::from_json(&blank).is_ok());
    }

    #[test]
    fn opening_a_scene_saves_this_one_and_loads_the_other_with_its_cameras() {
        let mut f = with_one_scene();
        f.apply(SceneAction::New);
        f.edit("Shed");
        assert_eq!(f.labels(), ["Shed"]);

        let load = f.apply(SceneAction::Open(0));
        assert_eq!(
            load,
            Some(LoadArea {
                area: osm(),
                keep_cameras: true
            })
        );
        assert_eq!(f.labels(), ["Porch", "Gate"]);
        assert!(!f.history.can_undo());
        let second =
            SceneFile::from_json(&f.store.get(&slot_key(1)).expect("saved")).expect("a scene");
        assert_eq!(second.cameras[0].label, "Shed");
        // The list remembers it for the next visit.
        assert_eq!(SlotIndex::load(&f.store).map(|i| i.active), Ok(0));
        // Opening the open scene does nothing.
        f.toasts = Toasts::default();
        assert_eq!(f.apply(SceneAction::Open(0)), None);
        assert_eq!(f.toasts(), []);
    }

    #[test]
    fn nothing_switches_while_an_area_loads() {
        let mut f = with_one_scene();
        f.apply(SceneAction::New);
        f.toasts = Toasts::default();
        f.loading = true;
        for action in [
            SceneAction::New,
            SceneAction::Duplicate,
            SceneAction::Open(0),
            SceneAction::Delete(1),
        ] {
            assert_eq!(f.apply(action.clone()), None, "{action:?}");
        }
        assert_eq!(f.names(), ["Scene 1", "Scene 2"]);
        assert_eq!(f.slots.index.active, 1);
        assert_eq!(
            f.toasts(),
            [(
                ToastKind::Error,
                "Wait for the area to finish loading, then try again"
            )]
        );
        // A rename does not switch, so it goes ahead.
        f.apply(SceneAction::Rename(0, "Front".to_owned()));
        assert_eq!(f.names(), ["Front", "Scene 2"]);
        // Nor does deleting a scene that is not open.
        f.toasts = Toasts::default();
        assert_eq!(f.apply(SceneAction::Delete(0)), None);
        assert_eq!(f.names(), ["Scene 2"]);
        assert_eq!(f.toasts(), [(ToastKind::Info, "Deleted “Front”")]);
    }

    #[test]
    fn a_refused_save_keeps_the_open_scene_open() {
        let mut f = with_one_scene();
        f.apply(SceneAction::New);
        f.edit("Shed");
        f.toasts = Toasts::default();
        f.store.refuse_writes(Some(SaveFailure::Full));
        assert_eq!(f.apply(SceneAction::Open(0)), None);
        assert_eq!(f.slots.index.active, 1);
        assert_eq!(f.labels(), ["Shed"]);
        assert!(f.history.can_undo(), "nothing was left");
        let toasts = f.toasts();
        assert!(
            toasts[0]
                .1
                .ends_with("Export it to keep your work, or delete a scene to make room"),
            "{toasts:?}"
        );
    }

    #[test]
    fn an_unreadable_scene_is_never_opened_or_written_over() {
        let mut f = with_one_scene();
        f.apply(SceneAction::New);
        f.store.set(STORAGE_KEY, "{broken").expect("stored");
        f.toasts = Toasts::default();
        assert_eq!(f.apply(SceneAction::Open(0)), None);
        assert_eq!(f.slots.index.active, 1);
        assert_eq!(f.store.get(STORAGE_KEY).as_deref(), Some("{broken"));
        assert!(
            f.toasts()[0]
                .1
                .starts_with("“Scene 1” could not be opened: ")
        );
    }

    #[test]
    fn duplicate_carries_on_in_a_copy_and_keeps_the_undo_steps() {
        let mut f = with_one_scene();
        f.apply(SceneAction::Duplicate);
        assert_eq!(f.names(), ["Scene 1", "Scene 1 copy"]);
        assert_eq!(f.slots.index.active, 1);
        assert_eq!(f.labels(), ["Porch", "Gate"]);
        assert!(f.history.can_undo());
        assert_eq!(f.store.get(&slot_key(1)), f.store.get(STORAGE_KEY));
        f.store.take_ops();
        f.autosave().expect("stored");
        assert_eq!(f.store.ops(), [], "the copy is already stored");
        f.edit("Shed");
        f.autosave().expect("stored");
        assert_ne!(f.store.get(&slot_key(1)), f.store.get(STORAGE_KEY));
    }

    #[test]
    fn deleting_the_open_scene_opens_the_next_and_the_only_one_stays() {
        let mut f = with_one_scene();
        f.toasts = Toasts::default();
        assert_eq!(f.apply(SceneAction::Delete(0)), None);
        assert_eq!(
            f.toasts(),
            [(
                ToastKind::Error,
                "This is the only scene; start a new one before deleting it"
            )]
        );
        f.apply(SceneAction::New);
        f.apply(SceneAction::Open(0));
        f.toasts = Toasts::default();
        // Open: the next one opens, on the area its blank file is on.
        assert_eq!(
            f.apply(SceneAction::Delete(0)),
            Some(LoadArea {
                area: osm(),
                keep_cameras: true
            })
        );
        assert_eq!(f.names(), ["Scene 2"]);
        assert_eq!(f.slots.index.active, 1);
        assert!(f.store.get(STORAGE_KEY).is_none());
        assert_eq!(
            f.toasts(),
            [(ToastKind::Info, "Deleted “Scene 1”; “Scene 2” is open")]
        );
        // Not open: just removed.
        f.apply(SceneAction::New);
        f.apply(SceneAction::Delete(1));
        assert_eq!(f.names(), ["Scene 1"]);
        assert!(f.store.get(&slot_key(1)).is_none());
    }

    // ----- import -------------------------------------------------------

    #[test]
    fn an_import_becomes_a_new_scene_named_after_the_file() {
        let mut f = with_one_scene();
        let load = f.import("rear garden.json", &scene_json(AreaRef::Demo, "Imported"));
        assert_eq!(
            load,
            Some(LoadArea {
                area: AreaRef::Demo,
                keep_cameras: true
            })
        );
        assert_eq!(f.names(), ["Scene 1", "rear garden"]);
        assert_eq!(f.labels(), ["Imported"]);
        assert!(!f.history.can_undo());
        // The scene left was saved with its latest edit.
        let first =
            SceneFile::from_json(&f.store.get(STORAGE_KEY).expect("saved")).expect("a scene");
        assert_eq!(first.cameras.len(), 2);
        assert_eq!(
            f.toasts().last(),
            Some(&(
                ToastKind::Info,
                "Imported “rear garden” with 1 camera(s) as a new scene"
            ))
        );
    }

    #[test]
    fn imports_wait_for_an_area_load() {
        let mut f = with_one_scene();
        f.loading = true;
        let picked = |name: &str| PickedFile {
            name: format!("{name}.json"),
            text: scene_json(AreaRef::Demo, name),
        };
        let mut pending = vec![picked("bad"), picked("A"), picked("B")];
        pending[0].text = "{ not json".to_owned();
        assert_eq!(f.ctx().import_pending(&mut pending), None);
        assert_eq!(pending.len(), 3, "all wait");
        f.loading = false;
        // The bad file is reported, A loads its area, and B waits for it.
        assert!(f.ctx().import_pending(&mut pending).is_some());
        assert_eq!(f.labels(), ["A"]);
        assert_eq!(pending, [picked("B")]);
        assert!(f.ctx().import_pending(&mut pending).is_some());
        assert_eq!(f.labels(), ["B"]);
        assert_eq!(f.names(), ["Scene 1", "A", "B"]);
    }

    #[test]
    fn a_bad_file_adds_no_scene() {
        let mut f = with_one_scene();
        assert_eq!(f.import("bad.json", "{ not json"), None);
        assert_eq!(f.names(), ["Scene 1"]);
        assert_eq!(f.labels(), ["Porch", "Gate"]);
        assert!(
            f.toasts()
                .last()
                .is_some_and(|t| t.1.starts_with("Import failed: "))
        );
    }

    #[test]
    fn without_storage_an_import_replaces_the_open_scene() {
        for why in [SaveFailure::Unavailable, SaveFailure::Blocked] {
            without_storage_an_import_replaces_the_open_scene_when(why);
        }
    }

    fn without_storage_an_import_replaces_the_open_scene_when(why: SaveFailure) {
        let mut f = with_one_scene();
        f.store.refuse_writes(Some(why));
        assert!(
            f.import("x.json", &scene_json(AreaRef::Demo, "Imported"))
                .is_some()
        );
        assert_eq!(f.names(), ["Scene 1"]);
        assert_eq!(f.labels(), ["Imported"]);
        assert!(f.history.can_undo(), "it can be undone");
        assert_eq!(
            f.toasts().last(),
            Some(&(ToastKind::Info, "Imported scene with 1 camera(s)"))
        );
    }

    #[test]
    fn a_full_store_refuses_an_import_and_says_how_to_make_room() {
        let mut f = with_one_scene();
        f.autosave().expect("stored");
        f.store.refuse_writes(Some(SaveFailure::Full));
        assert_eq!(f.import("x.json", &scene_json(AreaRef::Demo, "I")), None);
        assert_eq!(f.labels(), ["Porch", "Gate"]);
        assert!(
            f.toasts().last().is_some_and(|t| t
                .1
                .ends_with("Delete a scene to make room, or export the open one first")),
            "{:?}",
            f.toasts()
        );
    }

    // ----- autosave -----------------------------------------------------

    #[test]
    fn the_autosave_writes_to_the_open_scene_and_lists_it_again() {
        let mut f = with_one_scene();
        f.apply(SceneAction::New);
        // Another tab deletes this tab's open scene.
        let mut other = Slots::default();
        other.load(&f.store);
        other.open(&f.store, 0).expect("opened");
        other.delete(&f.store, 1).expect("deleted");
        assert!(
            SlotIndex::load(&f.store)
                .expect("readable")
                .get(1)
                .is_none()
        );

        f.edit("Shed");
        f.autosave().expect("stored");
        assert!(
            f.store
                .get(&slot_key(1))
                .is_some_and(|s| s.contains("Shed"))
        );
        assert!(
            SlotIndex::load(&f.store)
                .expect("readable")
                .get(1)
                .is_some()
        );
        assert!(f.slots.sizes[&1] > 0);
    }
}
