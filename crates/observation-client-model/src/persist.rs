//! What the client keeps in browser storage: the autosaved scenes and two
//! preferences, under fixed keys, and the rules for writing the open scene.
//! The keys and the JSON are a frozen format: builds already in use read
//! what these write. Which scenes there are, and under which keys, is
//! [`crate::slots`]; opening and restoring them is [`crate::scenes`].

use crate::platform::{KeyValueStore, SaveFailure};
use observation_core::scene::SceneFile;

/// The first autosaved scene (a [`SceneFile`] as JSON), and the only one
/// before a browser kept several: see [`crate::slots`].
pub const STORAGE_KEY: &str = crate::slots::DEFAULT_KEY;
/// The chosen UI font (the client's font key).
pub const FONT_KEY: &str = "observation.font";
/// Whether an Address tool click scrolls the camera list (`on` or `off`).
pub const ADDRESS_JUMP_KEY: &str = "observation.address-jump";
/// File name of an exported scene whose name leaves nothing to use (see
/// [`export_name`]).
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

/// Writes the open scene to storage at most once per [`AUTOSAVE_INTERVAL`],
/// and only when its JSON differs from what was last stored.
#[derive(Debug, Default)]
pub struct Autosaver {
    /// The JSON last stored for the open scene; a refused write leaves it
    /// alone, so the next check tries again.
    last_saved: String,
    timer: f32,
    status: SaveState,
    /// Nothing is written: the open scene's key holds a scene that could not
    /// be read (and no other could be opened), or the list of scenes is in
    /// a newer format. Opening or starting a scene ends it.
    paused: bool,
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

    /// Stores `json` under `key` unless it is what was last stored or the
    /// autosave is paused, recording the outcome; `now` is seconds since
    /// start-up. `Ok(true)` when it was written.
    pub fn save(
        &mut self,
        store: &dyn KeyValueStore,
        key: &str,
        json: String,
        now: f64,
    ) -> Result<bool, SaveFailure> {
        if self.paused || json == self.last_saved {
            return Ok(false);
        }
        match store.set(key, &json) {
            Ok(()) => {
                self.status = SaveState::Saved(now);
                self.last_saved = json;
                Ok(true)
            }
            Err(why) => {
                self.status = SaveState::Failed(why);
                Err(why)
            }
        }
    }

    /// Another scene was opened: `stored` is what its key holds (empty if
    /// nothing), so the autosave writes it only once it changes. Ends a
    /// pause. The status is `Saved` only if something is stored.
    pub fn opened(&mut self, stored: String, now: f64) {
        self.status = if stored.is_empty() {
            SaveState::Pending
        } else {
            SaveState::Saved(now)
        };
        self.last_saved = stored;
        self.paused = false;
    }

    /// The restored scene's key already holds `json`, so the first check
    /// does not write it back (over what another tab may have stored since).
    pub fn restored(&mut self, json: String) {
        self.last_saved = json;
    }

    /// Stops writing until another scene is opened.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }
}

/// A download file name for a scene: its name with anything a file system
/// might object to replaced, then `.json`; [`EXPORT_NAME`] if nothing is left.
pub fn export_name(scene: &str) -> String {
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
        EXPORT_NAME.to_owned()
    } else {
        format!("{stem}.json")
    }
}

/// What start-up finds under the open scene's key.
#[derive(Debug)]
pub enum RestoreOutcome {
    /// An autosaved scene.
    Saved(Box<SceneFile>),
    /// Stored text that is not a scene this build reads, and why.
    Unreadable(String),
    /// Nothing stored (or storage could not be read).
    Nothing,
}

impl RestoreOutcome {
    /// Reads the scene stored under `key`.
    pub fn read(store: &dyn KeyValueStore, key: &str) -> Self {
        match store.get(key).map(|s| SceneFile::from_json(&s)) {
            Some(Ok(file)) => Self::Saved(Box::new(file)),
            Some(Err(e)) => Self::Unreadable(e.to_string()),
            None => Self::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MemoryStore, StoreOp};

    fn set(json: &str) -> StoreOp {
        StoreOp::Set(STORAGE_KEY.to_owned(), json.to_owned())
    }

    fn save(saver: &mut Autosaver, json: &str, now: f64, store: &MemoryStore) {
        // The outcome is in the status; tests check that.
        let _ = saver.save(store, STORAGE_KEY, json.to_owned(), now);
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

        assert_eq!(
            saver.save(&store, STORAGE_KEY, "a".to_owned(), 2.0),
            Ok(true)
        );
        assert_eq!(
            saver.save(&store, STORAGE_KEY, "a".to_owned(), 3.0),
            Ok(false)
        );
        assert_eq!(store.take_ops(), [set("a")]);
        assert_eq!(
            saver.status(),
            SaveState::Saved(2.0),
            "stamped at the write"
        );

        save(&mut saver, "b", 4.0, &store);
        assert_eq!(store.take_ops(), [set("b")]);
        assert_eq!(saver.status(), SaveState::Saved(4.0));
    }

    #[test]
    fn a_refused_write_is_tried_again_and_reported_until_it_is_stored() {
        let store = MemoryStore::default();
        let mut saver = Autosaver::default();
        save(&mut saver, "a", 1.0, &store);
        store.take_ops();

        store.refuse_writes(Some(SaveFailure::Blocked));
        let refused = StoreOp::Refused(STORAGE_KEY.to_owned(), "b".to_owned());
        assert_eq!(
            saver.save(&store, STORAGE_KEY, "b".to_owned(), 2.0),
            Err(SaveFailure::Blocked)
        );
        assert_eq!(saver.status(), SaveState::Failed(SaveFailure::Blocked));
        save(&mut saver, "b", 3.0, &store);
        assert_eq!(store.take_ops(), [refused.clone(), refused]);

        store.refuse_writes(None);
        save(&mut saver, "b", 4.0, &store);
        save(&mut saver, "b", 5.0, &store);
        assert_eq!(store.take_ops(), [set("b")]);
        assert_eq!(saver.status(), SaveState::Saved(4.0));
        assert_eq!(store.get(STORAGE_KEY).as_deref(), Some("b"));
    }

    #[test]
    fn a_paused_autosave_writes_nothing_until_a_scene_is_opened() {
        let store = MemoryStore::default();
        let mut saver = Autosaver::default();
        saver.pause();
        assert!(saver.is_paused());
        save(&mut saver, "a", 1.0, &store);
        assert_eq!(store.take_ops(), []);
        assert_eq!(saver.status(), SaveState::Pending);

        // Opening a scene ends the pause, and what its key holds already
        // is not written again.
        saver.opened("a".to_owned(), 2.0);
        assert!(!saver.is_paused());
        assert_eq!(saver.status(), SaveState::Saved(2.0));
        save(&mut saver, "a", 3.0, &store);
        assert_eq!(store.take_ops(), []);
        save(&mut saver, "b", 4.0, &store);
        assert_eq!(store.take_ops(), [set("b")]);

        // A scene with nothing stored is not reported as saved.
        saver.opened(String::new(), 5.0);
        assert_eq!(saver.status(), SaveState::Pending);
    }

    #[test]
    fn export_names_follow_the_scene_name() {
        assert_eq!(export_name("Front of house"), "Front of house.json");
        assert_eq!(
            export_name("12/14 High St: rear"),
            "12-14 High St- rear.json"
        );
        assert_eq!(export_name("  ..hidden.. "), "hidden.json");
        assert_eq!(export_name(""), EXPORT_NAME);
        assert_eq!(export_name(" . "), EXPORT_NAME);
    }

    #[test]
    fn storage_that_cannot_be_read_restores_nothing() {
        let store = MemoryStore::default();
        store.set(STORAGE_KEY, "{not json").expect("stored");
        assert!(matches!(
            RestoreOutcome::read(&store, STORAGE_KEY),
            RestoreOutcome::Unreadable(_)
        ));
        store.block_reads(true);
        assert!(matches!(
            RestoreOutcome::read(&store, STORAGE_KEY),
            RestoreOutcome::Nothing
        ));
    }
}
