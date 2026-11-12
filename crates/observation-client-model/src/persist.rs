//! What the client keeps in browser storage: the autosaved scene and two
//! preferences, under fixed keys, and the rules for writing and restoring
//! the scene. The keys and the JSON are a frozen format: builds already in
//! use read what these write.

use crate::commands::Tracked;
use crate::document::{Document, LoadArea, Models};
use crate::editor::Panels;
use crate::platform::{KeyValueStore, SaveFailure};
use crate::toasts::Toasts;
use observation_core::scene::SceneFile;

/// The autosaved scene (a [`SceneFile`] as JSON).
pub const STORAGE_KEY: &str = "observation.scene.v1";
/// The chosen UI font (the client's font key).
pub const FONT_KEY: &str = "observation.font";
/// Whether an Address tool click scrolls the camera list (`on` or `off`).
pub const ADDRESS_JUMP_KEY: &str = "observation.address-jump";
/// File name of an exported scene.
pub const EXPORT_NAME: &str = "observation-scene.json";
/// What an exported scene is, and what the import dialog offers.
pub const SCENE_MIME: &str = "application/json";
pub const SCENE_ACCEPT: &str = ".json,application/json";
/// Seconds between autosave checks.
pub const AUTOSAVE_INTERVAL: f32 = 1.5;

/// Outcome of the autosave, shown in the status bar.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum SaveState {
    /// Nothing written yet this session.
    #[default]
    Pending,
    /// Written at this many seconds since start-up.
    Saved(f64),
    /// The browser refused the write.
    Failed(SaveFailure),
}

/// Writes the scene to storage at most once per [`AUTOSAVE_INTERVAL`], and
/// only when its JSON differs from what was last stored.
#[derive(Debug, Default)]
pub struct Autosaver {
    /// The JSON last stored; a refused write leaves it alone, so the next
    /// check tries again.
    last_saved: String,
    timer: f32,
    status: SaveState,
}

impl Autosaver {
    /// How the last write went.
    pub fn status(&self) -> SaveState {
        self.status
    }

    /// Advances the timer by `dt` seconds. Returns true, and starts the next
    /// interval from zero, once a whole interval has passed.
    pub fn due(&mut self, dt: f32) -> bool {
        self.timer += dt;
        if self.timer < AUTOSAVE_INTERVAL {
            return false;
        }
        self.timer = 0.0;
        true
    }

    /// Stores `json` under [`STORAGE_KEY`] unless it is what was last
    /// stored, recording the outcome; `now` is seconds since start-up.
    pub fn save(&mut self, json: String, now: f64, store: &dyn KeyValueStore) {
        if json == self.last_saved {
            return;
        }
        match store.set(STORAGE_KEY, &json) {
            Ok(()) => {
                self.status = SaveState::Saved(now);
                self.last_saved = json;
            }
            Err(why) => self.status = SaveState::Failed(why),
        }
    }
}

/// What start-up finds in storage.
#[derive(Debug)]
pub enum RestoreOutcome {
    /// An autosaved scene.
    Saved(Box<SceneFile>),
    /// Stored text that is not a scene this build reads, and why.
    Unreadable(String),
    /// Nothing stored (or storage could not be read): a first visit.
    FirstVisit,
}

impl RestoreOutcome {
    /// Reads the autosave from `store`.
    pub fn read(store: &dyn KeyValueStore) -> Self {
        match store.get(STORAGE_KEY).map(|s| SceneFile::from_json(&s)) {
            Some(Ok(file)) => Self::Saved(Box::new(file)),
            Some(Err(e)) => Self::Unreadable(e.to_string()),
            None => Self::FirstVisit,
        }
    }

    /// Puts the outcome into effect and returns the area to load: a saved
    /// scene becomes the document and keeps its cameras over its area; an
    /// unreadable one is reported and the demo street loads; a first visit
    /// opens the help over the demo street. The document and the models are
    /// written only for a saved scene.
    pub fn apply(
        self,
        doc: &mut dyn Tracked<Document>,
        models: &mut dyn Tracked<Models>,
        panels: &mut Panels,
        toasts: &mut Toasts,
    ) -> LoadArea {
        match self {
            Self::Saved(file) => {
                let area = file.area;
                doc.get_mut().load_file(*file, models.get_mut());
                LoadArea {
                    area,
                    keep_cameras: true,
                }
            }
            Self::Unreadable(e) => {
                toasts.error_sticky(format!(
                    "Your saved scene could not be restored: {e}. Starting with the demo street instead"
                ));
                LoadArea::demo()
            }
            Self::FirstVisit => {
                panels.help_open = true;
                LoadArea::demo()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Counting, MemoryStore, StoreOp};
    use crate::toasts::ToastKind;
    use observation_core::camera::Catalog;
    use observation_core::geo::BBox;
    use observation_core::scene::AreaRef;

    fn area() -> AreaRef {
        AreaRef::Osm {
            bbox: BBox {
                south: 51.5,
                west: -0.125,
                north: 51.5015,
                east: -0.1225,
            },
        }
    }

    fn set(json: &str) -> StoreOp {
        StoreOp::Set(STORAGE_KEY.to_owned(), json.to_owned())
    }

    #[test]
    fn a_check_is_due_once_a_whole_interval_has_passed() {
        let mut saver = Autosaver::default();
        // 0.25 s steps add up to exactly 1.5 s on the sixth.
        for _ in 0..5 {
            assert!(!saver.due(0.25));
        }
        assert!(saver.due(0.25), "due at exactly the interval");
        assert!(!saver.due(0.25), "the next interval starts over");
    }

    #[test]
    fn a_late_check_starts_the_next_interval_from_zero() {
        let mut saver = Autosaver::default();
        assert!(!saver.due(1.0));
        assert!(saver.due(1.0), "2 s: due, and the 0.5 s over is dropped");
        assert!(!saver.due(1.0), "1 s into the next interval");
        assert!(saver.due(0.5));
    }

    #[test]
    fn a_scene_is_written_once_until_it_changes() {
        let store = MemoryStore::default();
        let mut saver = Autosaver::default();
        assert_eq!(saver.status(), SaveState::Pending);

        saver.save("a".to_owned(), 2.0, &store);
        saver.save("a".to_owned(), 3.0, &store);
        assert_eq!(store.take_ops(), [set("a")]);
        assert_eq!(
            saver.status(),
            SaveState::Saved(2.0),
            "stamped at the write"
        );

        saver.save("b".to_owned(), 4.0, &store);
        assert_eq!(store.take_ops(), [set("b")]);
        assert_eq!(saver.status(), SaveState::Saved(4.0));
    }

    #[test]
    fn a_refused_write_is_tried_again_and_reported_until_it_is_stored() {
        let store = MemoryStore::default();
        let mut saver = Autosaver::default();
        saver.save("a".to_owned(), 1.0, &store);
        store.take_ops();

        store.refuse_writes(Some(SaveFailure::Blocked));
        let refused = StoreOp::Refused(STORAGE_KEY.to_owned(), "b".to_owned());
        saver.save("b".to_owned(), 2.0, &store);
        assert_eq!(saver.status(), SaveState::Failed(SaveFailure::Blocked));
        saver.save("b".to_owned(), 3.0, &store);
        assert_eq!(store.take_ops(), [refused.clone(), refused]);

        store.refuse_writes(None);
        saver.save("b".to_owned(), 4.0, &store);
        saver.save("b".to_owned(), 5.0, &store);
        assert_eq!(store.take_ops(), [set("b")]);
        assert_eq!(saver.status(), SaveState::Saved(4.0));
        assert_eq!(store.get(STORAGE_KEY).as_deref(), Some("b"));
    }

    struct Restored {
        load: LoadArea,
        doc: Counting<Document>,
        models: Counting<Models>,
        panels: Panels,
        toasts: Toasts,
    }

    fn restore(store: &MemoryStore) -> Restored {
        let mut doc = Counting::new(Document::default());
        let mut models = Counting::new(Models::new(Catalog::default()));
        let mut panels = Panels::default();
        let mut toasts = Toasts::default();
        let load =
            RestoreOutcome::read(store).apply(&mut doc, &mut models, &mut panels, &mut toasts);
        Restored {
            load,
            doc,
            models,
            panels,
            toasts,
        }
    }

    #[test]
    fn a_saved_scene_becomes_the_document_and_loads_its_area_with_its_cameras() {
        let file = SceneFile::new(area(), Document::default().origin);
        let store = MemoryStore::default();
        store.set(STORAGE_KEY, &file.to_json()).expect("stored");
        let r = restore(&store);
        assert_eq!(
            r.load,
            LoadArea {
                area: area(),
                keep_cameras: true
            }
        );
        assert_eq!(r.doc.value.area, Some(area()));
        assert_eq!((r.doc.writes, r.models.writes), (1, 1));
        assert!(!r.panels.help_open);
        assert_eq!(r.toasts.iter().count(), 0);
    }

    #[test]
    fn unreadable_text_is_reported_and_the_demo_street_loads() {
        let store = MemoryStore::default();
        store.set(STORAGE_KEY, "{not json").expect("stored");
        let r = restore(&store);
        assert_eq!(r.load, LoadArea::demo());
        assert_eq!((r.doc.writes, r.models.writes), (0, 0));
        assert!(!r.panels.help_open);
        let toasts: Vec<_> = r
            .toasts
            .iter()
            .map(|t| (t.kind, t.ttl.is_infinite()))
            .collect();
        assert_eq!(toasts, [(ToastKind::Error, true)]);
        let text = &r.toasts.iter().next().expect("one toast").text;
        assert!(
            text.starts_with("Your saved scene could not be restored: ")
                && text.ends_with(". Starting with the demo street instead"),
            "{text}"
        );
    }

    #[test]
    fn a_first_visit_opens_the_help_over_the_demo_street() {
        let store = MemoryStore::default();
        let r = restore(&store);
        assert_eq!(r.load, LoadArea::demo());
        assert_eq!((r.doc.writes, r.models.writes), (0, 0));
        assert!(r.panels.help_open);
        assert_eq!(r.toasts.iter().count(), 0);
    }

    #[test]
    fn storage_that_cannot_be_read_counts_as_a_first_visit() {
        let store = MemoryStore::default();
        store.set(STORAGE_KEY, "{not json").expect("stored");
        store.block_reads(true);
        assert!(matches!(
            RestoreOutcome::read(&store),
            RestoreOutcome::FirstVisit
        ));
    }
}
