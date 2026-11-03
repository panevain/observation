//! Named autosave slots ("scenes") in browser storage.
//!
//! Each scene's JSON lives under its own key: the first one under
//! [`DEFAULT_KEY`], the key the single autosave slot always used, so a scene
//! saved before slots existed opens as the first slot; later ones under
//! `observation.scenes.<id>`. The list of scenes, their names and which one
//! was open last are kept under [`INDEX_KEY`]. Without an index (a first
//! visit, or a browser that last ran an older version) there is one scene, on
//! the default key.
//!
//! The index is read again before every change, so a scene created in another
//! tab is not dropped from the list by this one.

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

/// Key/value storage: `localStorage` in the browser.
pub trait Store {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str) -> Result<(), SaveFailure>;
    fn remove(&mut self, key: &str);
}

/// Where scene `id` is stored.
pub fn slot_key(id: u32) -> String {
    if id == 0 {
        DEFAULT_KEY.to_owned()
    } else {
        format!("{INDEX_KEY}.{id}")
    }
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

impl SlotIndex {
    /// The stored index, checked like any other stored data (it may have been
    /// edited by hand): names are cleaned, repeated ids dropped, and an
    /// unknown `active` falls back to the first scene. A missing or unreadable
    /// index is the default one.
    pub fn load(store: &impl Store) -> Self {
        let Some(text) = store.get(INDEX_KEY) else {
            return Self::default();
        };
        let parsed: Result<Self, _> = serde_json::from_str(&text);
        let Ok(mut index) = parsed.map_err(|e| {
            bevy::log::warn!("ignoring the unreadable list of scenes: {e}");
        }) else {
            return Self::default();
        };
        if index.version != INDEX_VERSION {
            bevy::log::warn!(
                "ignoring a list of scenes in format {} (this app reads {INDEX_VERSION})",
                index.version
            );
            return Self::default();
        }
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
            return Self::default();
        }
        if index.get(index.active).is_none() {
            index.active = index.slots[0].id;
        }
        index
    }

    /// Whether an index has ever been stored (false on a first visit).
    pub fn stored(store: &impl Store) -> bool {
        store.get(INDEX_KEY).is_some()
    }

    fn save(&self, store: &mut impl Store) -> Result<(), SaveFailure> {
        // Plain data: cannot fail.
        let json = serde_json::to_string(self).unwrap_or_default();
        store.set(INDEX_KEY, &json)
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
    fn free_id(&self, store: &impl Store) -> u32 {
        let mut id = self.slots.iter().map(|s| s.id).max().unwrap_or(0) + 1;
        while self.get(id).is_some() || store.get(&slot_key(id)).is_some() {
            id += 1;
        }
        id
    }
}

/// The scenes as this tab knows them, for the Scenes dialog and the autosave.
#[derive(Debug, Default, Resource)]
pub struct Slots {
    /// `index.active` is the scene open in this tab, which the autosave
    /// writes to.
    pub index: SlotIndex,
    /// Each scene's stored size in UTF-16 code units (what the browser
    /// counts), as of the last refresh.
    pub sizes: HashMap<u32, usize>,
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
    /// What the stored index says, keeping this tab's open scene in it (and
    /// open) even if another tab removed it: this tab still autosaves there.
    fn fresh(&self, store: &impl Store) -> SlotIndex {
        let mut index = SlotIndex::load(store);
        if index.get(self.index.active).is_none() {
            let name =
                clean_name(self.index.active_name()).unwrap_or_else(|| index.next_default_name());
            index.slots.push(Slot {
                id: self.index.active,
                name,
            });
        }
        index.active = self.index.active;
        index
    }

    /// Reads the index and every scene's size again.
    pub fn refresh(&mut self, store: &impl Store) {
        self.index = self.fresh(store);
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
        if self
            .renaming
            .as_ref()
            .is_some_and(|(id, _)| self.index.get(*id).is_none())
        {
            self.renaming = None;
        }
        if self
            .confirm_delete
            .is_some_and(|id| self.index.get(id).is_none())
        {
            self.confirm_delete = None;
        }
    }

    /// Total stored size of the scenes and the index.
    pub fn used(&self) -> usize {
        self.sizes.values().sum::<usize>()
            + stored_len(&serde_json::to_string(&self.index).unwrap_or_default())
    }

    /// Adds a scene holding `json` (nothing yet if `None`) and opens it. Named
    /// `name`, or `Scene N` if `None`. Nothing changes if the browser refuses
    /// either write.
    pub fn create(
        &mut self,
        store: &mut impl Store,
        name: Option<String>,
        json: Option<&str>,
    ) -> Result<u32, SaveFailure> {
        let mut index = self.fresh(store);
        let id = index.free_id(store);
        let key = slot_key(id);
        if let Some(json) = json {
            store.set(&key, json)?;
        }
        let name = name.unwrap_or_else(|| index.next_default_name());
        index.slots.push(Slot { id, name });
        index.active = id;
        if let Err(e) = index.save(store) {
            store.remove(&key);
            return Err(e);
        }
        self.index = index;
        self.sizes.insert(id, json.map_or(0, stored_len));
        Ok(id)
    }

    /// Records `id` as the open scene; the caller loads it. False if it is
    /// no longer in the list (deleted in another tab).
    pub fn open(&mut self, store: &mut impl Store, id: u32) -> Result<bool, SaveFailure> {
        let mut index = self.fresh(store);
        if index.get(id).is_none() {
            self.index = index;
            return Ok(false);
        }
        index.active = id;
        index.save(store)?;
        self.index = index;
        Ok(true)
    }

    pub fn rename(
        &mut self,
        store: &mut impl Store,
        id: u32,
        name: &str,
    ) -> Result<(), SaveFailure> {
        let Some(name) = clean_name(name) else {
            return Ok(());
        };
        let mut index = self.fresh(store);
        let Some(slot) = index.slots.iter_mut().find(|s| s.id == id) else {
            self.index = index;
            return Ok(());
        };
        slot.name = name;
        index.save(store)?;
        self.index = index;
        Ok(())
    }

    /// Removes a scene that is not open, with its stored data.
    pub fn delete(&mut self, store: &mut impl Store, id: u32) -> Result<(), SaveFailure> {
        let mut index = self.fresh(store);
        if id == index.active {
            // The caller opens another scene first.
            return Ok(());
        }
        index.slots.retain(|s| s.id != id);
        index.save(store)?;
        store.remove(&slot_key(id));
        self.index = index;
        self.sizes.remove(&id);
        Ok(())
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
    }

    fn names(s: &Slots) -> Vec<&str> {
        s.index.slots.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn without_an_index_there_is_one_scene_on_the_old_key() {
        let mut mem = Mem::default();
        mem.map.insert(DEFAULT_KEY.into(), "{}".into());
        let index = SlotIndex::load(&mem);
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
        assert_eq!(SlotIndex::load(&mem), s.index);

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
        assert_eq!(SlotIndex::load(&mem).active, 0);
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
        assert_eq!(s.create(&mut mem, None, Some("x")), Err(SaveFailure::Full));
        assert_eq!(s.index, SlotIndex::default());
        assert!(mem.map.is_empty());

        // The scene's data is written but the index is not: the data goes.
        mem.fail_key = Some(INDEX_KEY);
        assert_eq!(s.create(&mut mem, None, Some("x")), Err(SaveFailure::Full));
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
        let index = SlotIndex::load(&mem);
        assert_eq!(index.active, 3);
        assert_eq!(index.slots.len(), 2);
        assert_eq!(index.slots[0].name, "Scene 1");
        assert_eq!(index.slots[1].name.chars().count(), MAX_NAME_CHARS);

        for bad in [
            "not json",
            r#"{"version":2,"active":0,"slots":[]}"#,
            r#"{"version":1,"active":0,"slots":[]}"#,
        ] {
            mem.map.insert(INDEX_KEY.into(), bad.into());
            assert_eq!(SlotIndex::load(&mem), SlotIndex::default(), "{bad}");
        }
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
        assert_eq!(name_from_file(".json"), "Imported scene");
        assert_eq!(
            clean_name(&"é".repeat(70)).unwrap().chars().count(),
            MAX_NAME_CHARS
        );
    }
}
