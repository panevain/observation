//! Named autosave slots ("scenes") in browser storage.
//!
//! Each scene's JSON lives under its own key: the first one under
//! [`DEFAULT_KEY`], the key the single autosave slot always used, so a scene
//! saved before slots existed opens as the first slot; later ones under
//! `observation.scenes.<id>`. The list of scenes, their names and which one
//! was open last are kept under [`INDEX_KEY`]. Without a list (a first visit,
//! or a browser that last ran an older version) there is one scene, on the
//! default key; an unreadable list is rebuilt from the keys that hold scenes,
//! and one in a newer format is left alone and nothing is saved.
//!
//! The list is read again before every change, and whenever the open scene is
//! autosaved, so a scene created or removed in another tab is neither dropped
//! nor lost by this one.

use crate::state::SaveFailure;
use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Where the first scene is stored: the key of the original single slot.
pub const DEFAULT_KEY: &str = "observation.scene.v1";
/// The list of scenes ([`SlotIndex`]).
pub const INDEX_KEY: &str = "observation.scenes";
const INDEX_VERSION: u32 = 1;
/// Longest scene name kept, in characters.
pub const MAX_NAME_CHARS: usize = 60;
/// What most browsers let one site keep in `localStorage` (about 5 MiB,
/// counted in UTF-16 code units), for the usage shown in the Scenes dialog.
/// The browser's own quota is what counts; this is only a guide.
pub const STORAGE_BUDGET: usize = 5 * 1024 * 1024;
const COPY_SUFFIX: &str = " copy";

/// Key/value storage: `localStorage` in the browser.
pub trait Store {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str) -> Result<(), SaveFailure>;
    fn remove(&mut self, key: &str);
    /// Every stored key.
    fn keys(&self) -> Vec<String>;
}

/// Why a change to the scenes was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotError {
    /// The browser refused a write.
    Storage(SaveFailure),
    /// The stored list is in a newer format than this app reads, and is left
    /// alone.
    Newer(u32),
    /// No id is free (only possible with a hand-made list).
    NoId,
}

impl From<SaveFailure> for SlotError {
    fn from(f: SaveFailure) -> Self {
        Self::Storage(f)
    }
}

impl SlotError {
    pub fn reason(self) -> String {
        match self {
            Self::Storage(f) => f.reason().to_owned(),
            Self::Newer(v) => format!(
                "The list of scenes was saved by a newer version of the app (format {v}); reload the page to update."
            ),
            Self::NoId => "The list of scenes has no room for another.".to_owned(),
        }
    }

    pub fn is_full(self) -> bool {
        self == Self::Storage(SaveFailure::Full)
    }
}

/// Where scene `id` is stored.
pub fn slot_key(id: u32) -> String {
    if id == 0 {
        DEFAULT_KEY.to_owned()
    } else {
        format!("{INDEX_KEY}.{id}")
    }
}

/// The scene stored under `key`, if it is a scene key.
fn slot_id(key: &str) -> Option<u32> {
    if key == DEFAULT_KEY {
        return Some(0);
    }
    let id: u32 = key
        .strip_prefix(INDEX_KEY)?
        .strip_prefix('.')?
        .parse()
        .ok()?;
    // `observation.scenes.0` is never written; `+1` or `01` do not round-trip.
    (id != 0 && slot_key(id) == key).then_some(id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub id: u32,
    pub name: String,
}

/// The stored list of scenes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotIndex {
    version: u32,
    /// The scene open last, which the next visit restores.
    pub active: u32,
    pub slots: Vec<Slot>,
}

impl Default for SlotIndex {
    /// One scene, on the default key.
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            active: 0,
            slots: vec![Slot {
                id: 0,
                name: default_name(1),
            }],
        }
    }
}

fn default_name(n: usize) -> String {
    format!("Scene {n}")
}

/// Trims a name and cuts it to [`MAX_NAME_CHARS`]; `None` if nothing is left.
pub fn clean_name(name: &str) -> Option<String> {
    let name: String = name.trim().chars().take(MAX_NAME_CHARS).collect();
    let name = name.trim_end();
    (!name.is_empty()).then(|| name.to_owned())
}

/// A name for an imported file: its name without `.json`.
pub fn name_from_file(file_name: &str) -> String {
    let stem = file_name
        .strip_suffix(".json")
        .or_else(|| file_name.strip_suffix(".JSON"))
        .unwrap_or(file_name);
    clean_name(stem).unwrap_or_else(|| "Imported scene".to_owned())
}

/// `<name> copy`, the name cut short if need be so the suffix always shows.
pub fn copy_name(name: &str) -> String {
    let base: String = name
        .chars()
        .take(MAX_NAME_CHARS - COPY_SUFFIX.len())
        .collect();
    format!("{}{COPY_SUFFIX}", base.trim_end())
}

impl SlotIndex {
    /// The stored index, checked like any other stored data (it may have been
    /// edited by hand): names are cleaned, repeated ids dropped, and an
    /// unknown `active` falls back to the first scene. A missing index is the
    /// default one, and an unreadable one is rebuilt from the scene keys. One
    /// in a newer format is `Err(version)`, to be left alone.
    pub fn load(store: &impl Store) -> Result<Self, u32> {
        let Some(text) = store.get(INDEX_KEY) else {
            return Ok(Self::default());
        };
        let mut index = match serde_json::from_str::<Self>(&text) {
            Ok(index) if index.version > INDEX_VERSION => return Err(index.version),
            Ok(index) if index.version == INDEX_VERSION => index,
            Ok(_) => return Ok(Self::recover(store)),
            Err(e) => {
                bevy::log::warn!("rebuilding the unreadable list of scenes: {e}");
                return Ok(Self::recover(store));
            }
        };
        let mut seen = Vec::new();
        index.slots.retain_mut(|s| {
            if seen.contains(&s.id) {
                return false;
            }
            seen.push(s.id);
            s.name = clean_name(&s.name).unwrap_or_else(|| default_name(seen.len()));
            true
        });
        if index.slots.is_empty() {
            return Ok(Self::recover(store));
        }
        if index.get(index.active).is_none() {
            index.active = index.slots[0].id;
        }
        Ok(index)
    }

    /// A list of every key that holds a scene, so none is lost with the list.
    fn recover(store: &impl Store) -> Self {
        let mut ids: Vec<u32> = store.keys().iter().filter_map(|k| slot_id(k)).collect();
        ids.sort_unstable();
        if ids.is_empty() {
            return Self::default();
        }
        Self {
            version: INDEX_VERSION,
            active: ids[0],
            slots: ids
                .iter()
                .enumerate()
                .map(|(i, &id)| Slot {
                    id,
                    name: default_name(i + 1),
                })
                .collect(),
        }
    }

    /// Whether an index has ever been stored (false on a first visit).
    pub fn stored(store: &impl Store) -> bool {
        store.get(INDEX_KEY).is_some()
    }

    fn to_json(&self) -> String {
        // Plain data: cannot fail.
        serde_json::to_string(self).unwrap_or_default()
    }

    fn save(&self, store: &mut impl Store) -> Result<(), SaveFailure> {
        store.set(INDEX_KEY, &self.to_json())
    }

    pub fn get(&self, id: u32) -> Option<&Slot> {
        self.slots.iter().find(|s| s.id == id)
    }

    /// The open scene's name.
    pub fn active_name(&self) -> &str {
        self.get(self.active).map_or("", |s| s.name.as_str())
    }

    /// `Scene N` for the lowest N no scene is called yet.
    fn next_default_name(&self) -> String {
        (1..)
            .map(default_name)
            .find(|n| self.slots.iter().all(|s| &s.name != n))
            .unwrap_or_default()
    }

    /// An id not in the list and whose key holds nothing, so a scene whose
    /// entry was lost from the list is never written over.
    fn free_id(&self, store: &impl Store) -> Option<u32> {
        let max = self.slots.iter().map(|s| s.id).max().unwrap_or(0);
        let start = max.checked_add(1).unwrap_or(1);
        (start..=u32::MAX)
            .chain(1..start)
            .find(|&id| self.get(id).is_none() && store.get(&slot_key(id)).is_none())
    }
}

/// The scenes as this tab knows them, for the Scenes dialog and the autosave.
#[derive(Debug, Default, Resource)]
pub struct Slots {
    /// `index.active` is the scene open in this tab, which the autosave
    /// writes to.
    pub index: SlotIndex,
    /// The stored list is in this newer format: nothing is changed or saved.
    pub newer: Option<u32>,
    /// Each scene's stored size in UTF-16 code units (what the browser
    /// counts), as of the last refresh.
    pub sizes: HashMap<u32, usize>,
    /// The stored size of the list itself.
    index_len: usize,
    /// The scene being renamed in the dialog, and the name typed so far.
    pub renaming: Option<(u32, String)>,
    /// Focus the name field on its first frame.
    pub rename_focus: bool,
    /// The scene whose Delete was clicked once, waiting for confirmation.
    pub confirm_delete: Option<u32>,
}

/// The storage size of a value, in UTF-16 code units.
pub fn stored_len(value: &str) -> usize {
    value.encode_utf16().count()
}

impl Slots {
    fn set_index(&mut self, index: SlotIndex) {
        self.index_len = stored_len(&index.to_json());
        self.index = index;
    }

    /// Reads the stored list at start-up.
    pub fn load(&mut self, store: &impl Store) {
        match SlotIndex::load(store) {
            Ok(index) => self.set_index(index),
            Err(v) => {
                self.newer = Some(v);
                self.set_index(SlotIndex::default());
            }
        }
    }

    /// What the stored index says, keeping this tab's open scene in it (and
    /// open) even if another tab removed it: this tab still autosaves there.
    fn fresh(&self, store: &impl Store) -> Result<SlotIndex, SlotError> {
        let mut index = SlotIndex::load(store).map_err(SlotError::Newer)?;
        if index.get(self.index.active).is_none() {
            let name =
                clean_name(self.index.active_name()).unwrap_or_else(|| index.next_default_name());
            index.slots.push(Slot {
                id: self.index.active,
                name,
            });
        }
        index.active = self.index.active;
        Ok(index)
    }

    /// Puts the open scene back in the stored list if another tab removed
    /// it, so what the autosave writes there stays reachable.
    pub fn ensure_listed(&mut self, store: &mut impl Store) -> Result<(), SlotError> {
        let stored = SlotIndex::load(store).map_err(SlotError::Newer)?;
        if stored.get(self.index.active).is_some() {
            return Ok(());
        }
        let index = self.fresh(store)?;
        index.save(store)?;
        self.set_index(index);
        Ok(())
    }

    /// Reads the index and every scene's size again, and forgets a half-done
    /// rename or delete (the dialog was opened).
    pub fn refresh(&mut self, store: &impl Store) {
        match self.fresh(store) {
            Ok(index) => {
                self.newer = None;
                self.set_index(index);
            }
            Err(SlotError::Newer(v)) => self.newer = Some(v),
            Err(_) => {}
        }
        self.sizes = self
            .index
            .slots
            .iter()
            .map(|s| {
                (
                    s.id,
                    store.get(&slot_key(s.id)).map_or(0, |v| stored_len(&v)),
                )
            })
            .collect();
        self.renaming = None;
        self.confirm_delete = None;
    }

    /// Total stored size of the scenes and the index.
    pub fn used(&self) -> usize {
        self.sizes.values().sum::<usize>() + self.index_len
    }

    /// Adds a scene holding `json` (nothing yet if `None`) and opens it. Named
    /// `name`, or `Scene N` if `None`. Nothing changes if the browser refuses
    /// either write.
    pub fn create(
        &mut self,
        store: &mut impl Store,
        name: Option<String>,
        json: Option<&str>,
    ) -> Result<u32, SlotError> {
        let mut index = self.fresh(store)?;
        let id = index.free_id(store).ok_or(SlotError::NoId)?;
        let key = slot_key(id);
        if let Some(json) = json {
            store.set(&key, json)?;
        }
        let name = name.unwrap_or_else(|| index.next_default_name());
        index.slots.push(Slot { id, name });
        index.active = id;
        if let Err(e) = index.save(store) {
            store.remove(&key);
            return Err(e.into());
        }
        self.set_index(index);
        self.sizes.insert(id, json.map_or(0, stored_len));
        Ok(id)
    }

    /// Records `id` as the open scene; the caller loads it. False if it is
    /// no longer in the list (deleted in another tab).
    pub fn open(&mut self, store: &mut impl Store, id: u32) -> Result<bool, SlotError> {
        let mut index = self.fresh(store)?;
        if index.get(id).is_none() {
            self.set_index(index);
            return Ok(false);
        }
        index.active = id;
        index.save(store)?;
        self.set_index(index);
        Ok(true)
    }

    pub fn rename(&mut self, store: &mut impl Store, id: u32, name: &str) -> Result<(), SlotError> {
        let Some(name) = clean_name(name) else {
            return Ok(());
        };
        let mut index = self.fresh(store)?;
        if let Some(slot) = index.slots.iter_mut().find(|s| s.id == id) {
            slot.name = name;
            index.save(store)?;
        }
        self.set_index(index);
        Ok(())
    }

    /// Removes a scene that is not open, with its stored data.
    pub fn delete(&mut self, store: &mut impl Store, id: u32) -> Result<(), SlotError> {
        let mut index = self.fresh(store)?;
        if id == index.active {
            return Ok(());
        }
        index.slots.retain(|s| s.id != id);
        index.save(store)?;
        store.remove(&slot_key(id));
        self.set_index(index);
        self.sizes.remove(&id);
        Ok(())
    }

    /// Removes the open scene and opens `next` in one write, so a refused
    /// write leaves both as they were; the caller loads `next`. False if
    /// `next` is no longer in the list.
    pub fn delete_open(&mut self, store: &mut impl Store, next: u32) -> Result<bool, SlotError> {
        let mut index = self.fresh(store)?;
        let id = index.active;
        if next == id || index.get(next).is_none() {
            self.set_index(index);
            return Ok(false);
        }
        index.slots.retain(|s| s.id != id);
        index.active = next;
        index.save(store)?;
        store.remove(&slot_key(id));
        self.set_index(index);
        self.sizes.remove(&id);
        Ok(true)
    }

    /// The scene to open when the open one is deleted: the next in the list,
    /// else the one before.
    pub fn neighbour(&self, id: u32) -> Option<u32> {
        let slots = &self.index.slots;
        let at = slots.iter().position(|s| s.id == id)?;
        slots
            .get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|i| slots.get(i)))
            .map(|s| s.id)
    }

    /// The JSON stored for scene `id`, if any.
    pub fn read(store: &impl Store, id: u32) -> Option<String> {
        store.get(&slot_key(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory storage that can be told to refuse writes.
    #[derive(Default)]
    struct Mem {
        map: HashMap<String, String>,
        fail: Option<SaveFailure>,
        /// Refuse only writes to this key.
        fail_key: Option<&'static str>,
    }

    impl Store for Mem {
        fn get(&self, key: &str) -> Option<String> {
            self.map.get(key).cloned()
        }
        fn set(&mut self, key: &str, value: &str) -> Result<(), SaveFailure> {
            if let Some(f) = self.fail
                && self.fail_key.is_none_or(|k| k == key)
            {
                return Err(f);
            }
            self.map.insert(key.to_owned(), value.to_owned());
            Ok(())
        }
        fn remove(&mut self, key: &str) {
            self.map.remove(key);
        }
        fn keys(&self) -> Vec<String> {
            self.map.keys().cloned().collect()
        }
    }

    fn names(s: &Slots) -> Vec<&str> {
        s.index.slots.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn without_an_index_there_is_one_scene_on_the_old_key() {
        let mut mem = Mem::default();
        mem.map.insert(DEFAULT_KEY.into(), "{}".into());
        let index = SlotIndex::load(&mem).unwrap();
        assert_eq!(index, SlotIndex::default());
        assert_eq!(slot_key(index.active), DEFAULT_KEY);
        assert!(!SlotIndex::stored(&mem));
    }

    #[test]
    fn create_open_rename_delete_round_trip() {
        let mut mem = Mem::default();
        let mut s = Slots::default();
        let id = s.create(&mut mem, None, Some("two")).unwrap();
        assert_eq!(id, 1);
        assert_eq!(s.index.active, 1);
        assert_eq!(names(&s), ["Scene 1", "Scene 2"]);
        assert_eq!(mem.get(&slot_key(1)).as_deref(), Some("two"));
        assert_eq!(mem.get(&slot_key(1)).unwrap(), "two");
        assert_eq!(slot_key(1), "observation.scenes.1");

        // The stored index matches, and a new tab would open scene 2.
        assert_eq!(SlotIndex::load(&mem).unwrap(), s.index);

        s.rename(&mut mem, 0, "  Front of house  ").unwrap();
        assert_eq!(names(&s), ["Front of house", "Scene 2"]);
        // Blank names are ignored.
        s.rename(&mut mem, 0, "   ").unwrap();
        assert_eq!(names(&s), ["Front of house", "Scene 2"]);

        // The open scene is not deleted; another is, with its data.
        s.delete(&mut mem, 1).unwrap();
        assert_eq!(names(&s), ["Front of house", "Scene 2"]);
        assert!(s.open(&mut mem, 0).unwrap());
        s.delete(&mut mem, 1).unwrap();
        assert_eq!(names(&s), ["Front of house"]);
        // Gone: nothing to open.
        assert!(!s.open(&mut mem, 1).unwrap());
        assert!(mem.get(&slot_key(1)).is_none());
        assert_eq!(SlotIndex::load(&mem).unwrap().active, 0);
    }

    #[test]
    fn default_names_skip_taken_ones() {
        let mut mem = Mem::default();
        let mut s = Slots::default();
        s.create(&mut mem, Some("Scene 3".into()), None).unwrap();
        s.create(&mut mem, None, None).unwrap();
        assert_eq!(names(&s), ["Scene 1", "Scene 3", "Scene 2"]);
        s.create(&mut mem, None, None).unwrap();
        assert_eq!(names(&s), ["Scene 1", "Scene 3", "Scene 2", "Scene 4"]);
    }

    #[test]
    fn a_refused_write_changes_nothing() {
        let mut mem = Mem::default();
        let mut s = Slots::default();
        mem.fail = Some(SaveFailure::Full);
        assert_eq!(
            s.create(&mut mem, None, Some("x")),
            Err(SlotError::Storage(SaveFailure::Full))
        );
        assert_eq!(s.index, SlotIndex::default());
        assert!(mem.map.is_empty());

        // The scene's data is written but the index is not: the data goes.
        mem.fail_key = Some(INDEX_KEY);
        assert_eq!(
            s.create(&mut mem, None, Some("x")),
            Err(SlotError::Storage(SaveFailure::Full))
        );
        assert_eq!(s.index, SlotIndex::default());
        assert!(mem.map.is_empty());
    }

    #[test]
    fn new_ids_never_reuse_a_key_that_holds_data() {
        let mut mem = Mem::default();
        // A scene whose entry was lost from the list.
        mem.map.insert(slot_key(1), "orphan".into());
        let mut s = Slots::default();
        let id = s.create(&mut mem, None, Some("new")).unwrap();
        assert_eq!(id, 2);
        assert_eq!(mem.get(&slot_key(1)).as_deref(), Some("orphan"));
    }

    #[test]
    fn changes_keep_scenes_made_in_another_tab() {
        let mut mem = Mem::default();
        let mut a = Slots::default();
        let mut b = Slots::default();
        a.create(&mut mem, Some("From A".into()), Some("a"))
            .unwrap();
        // B never refreshed, but its rename keeps A's scene, and B's own
        // open scene stays open in B.
        b.rename(&mut mem, 0, "Renamed in B").unwrap();
        assert_eq!(names(&b), ["Renamed in B", "From A"]);
        assert_eq!(b.index.active, 0);

        // A deletes B's open scene (after opening another): B puts it back
        // on its next change, since it still autosaves there.
        a.open(&mut mem, 1).unwrap();
        a.delete(&mut mem, 0).unwrap();
        b.refresh(&mem);
        assert_eq!(names(&b), ["From A", "Renamed in B"]);
        assert_eq!(b.index.active, 0);
    }

    #[test]
    fn a_hand_edited_index_is_cleaned() {
        let mut mem = Mem::default();
        let long = "x".repeat(200);
        mem.map.insert(
            INDEX_KEY.into(),
            format!(
                r#"{{"version":1,"active":9,"slots":[{{"id":3,"name":"  "}},{{"id":3,"name":"dup"}},{{"id":4,"name":"{long}"}}]}}"#
            ),
        );
        let index = SlotIndex::load(&mem).unwrap();
        assert_eq!(index.active, 3);
        assert_eq!(index.slots.len(), 2);
        assert_eq!(index.slots[0].name, "Scene 1");
        assert_eq!(index.slots[1].name.chars().count(), MAX_NAME_CHARS);

        for bad in ["not json", r#"{"version":1,"active":0,"slots":[]}"#] {
            mem.map.insert(INDEX_KEY.into(), bad.into());
            assert_eq!(
                SlotIndex::load(&mem).unwrap(),
                SlotIndex::default(),
                "{bad}"
            );
        }
    }

    #[test]
    fn an_unreadable_index_is_rebuilt_from_the_scene_keys() {
        let mut mem = Mem::default();
        mem.map.insert(INDEX_KEY.into(), "{broken".into());
        for key in [
            "observation.scenes.7",
            DEFAULT_KEY,
            "observation.scenes.3",
            // Not scene keys.
            "observation.scenes.0",
            "observation.scenes.03",
            "observation.scenes.x",
            "observation.font",
        ] {
            mem.map.insert(key.into(), "{}".into());
        }
        let index = SlotIndex::load(&mem).unwrap();
        let ids: Vec<u32> = index.slots.iter().map(|s| s.id).collect();
        assert_eq!(ids, [0, 3, 7]);
        assert_eq!(index.active, 0);
        assert_eq!(index.slots[2].name, "Scene 3");
    }

    #[test]
    fn a_newer_index_is_left_alone() {
        let mut mem = Mem::default();
        let newer = r#"{"version":2,"active":4,"slots":[{"id":4,"name":"A"}]}"#;
        mem.map.insert(INDEX_KEY.into(), newer.into());
        assert_eq!(SlotIndex::load(&mem), Err(2));
        let mut s = Slots::default();
        s.load(&mem);
        assert_eq!(s.newer, Some(2));
        assert_eq!(
            s.create(&mut mem, None, Some("x")),
            Err(SlotError::Newer(2))
        );
        assert_eq!(s.rename(&mut mem, 0, "B"), Err(SlotError::Newer(2)));
        assert_eq!(s.ensure_listed(&mut mem), Err(SlotError::Newer(2)));
        assert_eq!(mem.get(INDEX_KEY).as_deref(), Some(newer));
        assert_eq!(mem.map.len(), 1);
    }

    #[test]
    fn the_autosave_relists_a_scene_another_tab_removed() {
        let mut mem = Mem::default();
        let mut a = Slots::default();
        let mut b = Slots::default();
        a.create(&mut mem, None, Some("a")).unwrap();
        b.load(&mem);
        // B has scene 1 open too; A opens scene 0 and deletes scene 1.
        a.open(&mut mem, 0).unwrap();
        a.delete(&mut mem, 1).unwrap();
        assert!(SlotIndex::load(&mem).unwrap().get(1).is_none());
        b.ensure_listed(&mut mem).unwrap();
        assert!(SlotIndex::load(&mem).unwrap().get(1).is_some());
        // Already listed: nothing written.
        mem.fail = Some(SaveFailure::Full);
        b.ensure_listed(&mut mem).unwrap();
    }

    #[test]
    fn deleting_the_open_scene_opens_the_next_in_one_write() {
        let mut mem = Mem::default();
        let mut s = Slots::default();
        s.create(&mut mem, None, Some("two")).unwrap();
        mem.fail = Some(SaveFailure::Full);
        assert!(s.delete_open(&mut mem, 0).is_err());
        assert_eq!(s.index.active, 1);
        assert_eq!(mem.get(&slot_key(1)).as_deref(), Some("two"));
        mem.fail = None;
        assert!(!s.delete_open(&mut mem, 1).unwrap());
        assert!(s.delete_open(&mut mem, 0).unwrap());
        assert_eq!(names(&s), ["Scene 1"]);
        assert_eq!(SlotIndex::load(&mem).unwrap().active, 0);
        assert!(mem.get(&slot_key(1)).is_none());
    }

    #[test]
    fn ids_near_the_top_wrap_round() {
        let mut mem = Mem::default();
        mem.map.insert(
            INDEX_KEY.into(),
            format!(r#"{{"version":1,"active":0,"slots":[{{"id":0,"name":"a"}},{{"id":{},"name":"b"}}]}}"#, u32::MAX),
        );
        let mut s = Slots::default();
        s.load(&mem);
        assert_eq!(s.create(&mut mem, None, None), Ok(1));
    }

    #[test]
    fn neighbour_prefers_the_next_scene() {
        let mut mem = Mem::default();
        let mut s = Slots::default();
        s.create(&mut mem, None, None).unwrap();
        s.create(&mut mem, None, None).unwrap();
        assert_eq!(s.neighbour(0), Some(1));
        assert_eq!(s.neighbour(1), Some(2));
        assert_eq!(s.neighbour(2), Some(1));
        s.delete(&mut mem, 1).unwrap();
        s.open(&mut mem, 0).unwrap();
        s.delete(&mut mem, 2).unwrap();
        assert_eq!(s.neighbour(0), None);
    }

    #[test]
    fn sizes_and_names() {
        let mut mem = Mem::default();
        mem.map.insert(DEFAULT_KEY.into(), "abc".into());
        let mut s = Slots::default();
        s.refresh(&mem);
        assert_eq!(s.sizes[&0], 3);
        assert!(s.used() > 3);
        assert_eq!(stored_len("é😀"), 3);

        assert_eq!(name_from_file("kirkland.json"), "kirkland");
        assert_eq!(copy_name("Front"), "Front copy");
        let long = copy_name(&"x".repeat(MAX_NAME_CHARS));
        assert_eq!(long.chars().count(), MAX_NAME_CHARS);
        assert!(long.ends_with(" copy"));
        assert_eq!(name_from_file(".json"), "Imported scene");
        assert_eq!(
            clean_name(&"é".repeat(70)).unwrap().chars().count(),
            MAX_NAME_CHARS
        );
    }
}
