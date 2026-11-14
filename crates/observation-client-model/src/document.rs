//! The user's document: the scene that is autosaved, exported and undone,
//! the camera models it uses, and requests to load its area.
//!
//! The client wraps [`Document`] and [`Models`] in Bevy resources whose
//! change detection drives coverage, painting and autosave, so a method
//! that changes nothing takes `&self`.

use observation_core::camera::{
    CameraKind, CameraModel, CameraPlacement, Catalog, fresh_custom_id,
};
use observation_core::coverage::{CoverageParams, Region};
use observation_core::footage::{
    LabelledArea, Target, area_by_id, area_by_id_mut, set_figures_area,
};
use observation_core::geo::{BBox, LatLon};
use observation_core::provenance::{DataChange, Provenance};
use observation_core::scene::agent_log::AgentLogEntry;
use observation_core::scene::{AreaRef, SceneFile};

pub mod area_load;

/// Curated + user-defined camera models.
#[derive(Debug)]
pub struct Models {
    pub catalog: Catalog,
    /// [`Catalog::fingerprint`], worked out once: the catalogue version a
    /// scene's provenance records.
    pub catalog_version: String,
    pub custom: Vec<CameraModel>,
}

impl Models {
    /// The shipped catalogue, and no custom models yet.
    pub fn new(catalog: Catalog) -> Self {
        Self {
            catalog_version: catalog.fingerprint(),
            catalog,
            custom: Vec::new(),
        }
    }

    pub fn get(&self, id: &str) -> Option<&CameraModel> {
        self.custom_model(id).or_else(|| self.catalog.get(id))
    }

    pub fn kind_of(&self, id: &str) -> CameraKind {
        self.get(id).map(|m| m.kind).unwrap_or_default()
    }

    /// A user-defined model (catalogue models cannot be edited).
    pub fn custom_model(&self, id: &str) -> Option<&CameraModel> {
        self.custom.iter().find(|m| m.id == id)
    }

    /// Replaces the custom model with `model`'s id; false if there is none.
    pub fn update_custom(&mut self, model: CameraModel) -> bool {
        match self.custom.iter_mut().find(|m| m.id == model.id) {
            Some(slot) => {
                *slot = model;
                true
            }
            None => false,
        }
    }

    /// Removes a custom model; false if there is none with this id.
    pub fn remove_custom(&mut self, id: &str) -> bool {
        let before = self.custom.len();
        self.custom.retain(|m| m.id != id);
        self.custom.len() != before
    }

    /// An id for a new custom model named `name` that no custom model has
    /// (models can be deleted, so the count alone can repeat one). Catalogue
    /// ids never start with
    /// [`observation_core::camera::CUSTOM_ID_PREFIX`], so they cannot clash.
    pub fn new_custom_id(&self, name: &str) -> String {
        fresh_custom_id(name, self.custom.len() + 1, |id| {
            self.custom_model(id).is_some()
        })
    }
}

/// The user's document: what gets autosaved and exported.
#[derive(Debug)]
pub struct Document {
    pub area: Option<AreaRef>,
    pub origin: LatLon,
    pub cameras: Vec<CameraPlacement>,
    pub params: CoverageParams,
    /// The scene's areas, labelled or not. Only the one the figures are
    /// limited to is edited yet (the Region tool, through [`Self::region`]
    /// and [`Self::set_region`]); the others are kept as loaded.
    pub areas: Vec<LabelledArea>,
    /// The id of the area the coverage figures are limited to.
    pub limit_figures_to: Option<u32>,
    /// The scene's targets, kept as loaded.
    pub targets: Vec<Target>,
    /// The cameras come from a scene saved before terrain and still stand on
    /// flat ground; they are lifted onto the terrain when the area loads.
    pub on_flat_ground: bool,
    /// Where the figures come from: the loaded area's data date and
    /// fingerprint, and the app and catalogue versions. Saved with the
    /// scene. Until a loaded file's area arrives it is the file's own (unless
    /// that area is already loaded), so an autosave in between keeps it.
    pub provenance: Option<Provenance>,
    /// The provenance a restored or imported file was saved with, checked
    /// against its area once that loads (see
    /// [`Provenance::data_change`]).
    pub saved_provenance: Option<Provenance>,
    /// The agent proposals accepted into this scene, oldest first (saved
    /// with it; see [`observation_core::scene::agent_log`]).
    pub agent_log: Vec<AgentLogEntry>,
    next_id: u32,
    /// Bumped on every change; drives sync, coverage and autosave.
    pub revision: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            area: None,
            origin: LatLon::new(0.0, 0.0),
            cameras: Vec::new(),
            params: CoverageParams::default(),
            areas: Vec::new(),
            limit_figures_to: None,
            targets: Vec::new(),
            on_flat_ground: false,
            provenance: None,
            saved_provenance: None,
            agent_log: Vec::new(),
            next_id: 1,
            revision: 0,
        }
    }
}

impl Document {
    pub fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn camera(&self, id: u32) -> Option<&CameraPlacement> {
        self.cameras.iter().find(|c| c.id == id)
    }

    pub fn camera_mut(&mut self, id: u32) -> Option<&mut CameraPlacement> {
        self.touch();
        self.cameras.iter_mut().find(|c| c.id == id)
    }

    pub fn add_camera(&mut self, mut c: CameraPlacement) -> u32 {
        c.id = self.next_id;
        self.next_id += 1;
        if c.label.is_empty() {
            c.label = format!("Camera {}", c.id);
        }
        let id = c.id;
        self.cameras.push(c);
        self.touch();
        id
    }

    pub fn remove_camera(&mut self, id: u32) {
        self.cameras.retain(|c| c.id != id);
        self.touch();
    }

    /// The area the coverage figures can be limited to (the single region
    /// the Region tool draws), if there is one.
    pub fn region(&self) -> Option<&LabelledArea> {
        area_by_id(&self.areas, self.limit_figures_to)
    }

    /// The same, to change (without a new revision).
    pub fn region_mut(&mut self) -> Option<&mut LabelledArea> {
        area_by_id_mut(&mut self.areas, self.limit_figures_to)
    }

    /// Replaces the region's outline and switch (keeping its id, label and
    /// name), adds one, or with `None` removes it; without a new revision.
    pub fn set_region(&mut self, region: Option<Region>) {
        set_figures_area(&mut self.areas, &mut self.limit_figures_to, region);
    }

    /// How many placed cameras use model `model_id`.
    pub fn cameras_using(&self, model_id: &str) -> usize {
        self.cameras
            .iter()
            .filter(|c| c.model_id == model_id)
            .count()
    }

    pub fn to_file(&self, models: &Models) -> Option<SceneFile> {
        let area = self.area?;
        let mut file = SceneFile::new(area, self.origin);
        file.cameras = self.cameras.clone();
        file.params = self.params;
        file.areas = self.areas.clone();
        file.limit_figures_to = self.limit_figures_to;
        file.targets = self.targets.clone();
        file.custom_models = models.custom.clone();
        file.provenance = self.provenance.clone();
        file.agent_log = self.agent_log.clone();
        if self.on_flat_ground {
            // Saved before the area loaded: still in the old frame.
            file.version = observation_core::scene::FLAT_GROUND_VERSION;
        }
        Some(file)
    }

    /// The undoable part of the document.
    pub fn snapshot(&self, models: &Models) -> Snapshot {
        Snapshot {
            area: self.area,
            origin: self.origin,
            cameras: self.cameras.clone(),
            params: self.params,
            areas: self.areas.clone(),
            limit_figures_to: self.limit_figures_to,
            targets: self.targets.clone(),
            custom_models: models.custom.clone(),
            agent_log: self.agent_log.clone(),
        }
    }

    /// Puts back a [`Self::snapshot`].
    pub fn restore(&mut self, s: &Snapshot, models: &mut Models) {
        if self.area != s.area {
            // It described the area being left; the one coming back sets its
            // own when it loads, and is not checked against a saved file.
            self.provenance = None;
            self.saved_provenance = None;
        }
        self.area = s.area;
        self.origin = s.origin;
        self.cameras = s.cameras.clone();
        self.params = s.params;
        self.areas = s.areas.clone();
        self.limit_figures_to = s.limit_figures_to;
        self.targets = s.targets.clone();
        self.agent_log = s.agent_log.clone();
        // Every step was recorded with its cameras on the terrain (none is
        // while a flat-ground scene waits for its area).
        self.on_flat_ground = false;
        // Never hand out an id the restored cameras already use (an import
        // resets `next_id`, so undoing past one can bring back higher ids).
        let max = self.cameras.iter().map(|c| c.id).max().unwrap_or(0);
        self.next_id = self.next_id.max(max + 1);
        models.custom = s.custom_models.clone();
        self.touch();
    }

    /// Replaces the document with a loaded file (custom models go to `models`).
    pub fn load_file(&mut self, file: SceneFile, models: &mut Models) {
        // Over the area already loaded, the provenance stays that of the data
        // on screen until the reload replaces it (should the reload fail,
        // the figures are still from that data). Otherwise the file's own is
        // kept, so an autosave before its area arrives does not drop it.
        if self.area != Some(file.area) {
            self.provenance.clone_from(&file.provenance);
        }
        self.area = Some(file.area);
        self.on_flat_ground = file.on_flat_ground();
        self.origin = file.origin;
        self.next_id = file.cameras.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        self.cameras = file.cameras;
        self.params = file.params;
        self.areas = file.areas;
        self.limit_figures_to = file.limit_figures_to;
        self.targets = file.targets;
        self.saved_provenance = file.provenance;
        self.agent_log = file.agent_log;
        models.custom = file.custom_models;
        self.touch();
    }

    /// Takes the items of `file`, a scene on this document's area: its
    /// cameras, settings, areas, targets, custom models and change log. The
    /// area, origin and provenance stay as they are. This is how an accepted
    /// proposal ([`crate::proposal`]), applied to this document's own
    /// scene, comes back.
    pub fn take_items(&mut self, file: SceneFile, models: &mut Models) {
        let max = file.cameras.iter().map(|c| c.id).max().unwrap_or(0);
        self.next_id = self.next_id.max(max.saturating_add(1));
        self.cameras = file.cameras;
        self.params = file.params;
        self.areas = file.areas;
        self.limit_figures_to = file.limit_figures_to;
        self.targets = file.targets;
        self.agent_log = file.agent_log;
        models.custom = file.custom_models;
        self.touch();
    }

    /// An empty scene on the current area (no cameras, areas, targets or
    /// custom models, default settings), or `None` before an area has loaded.
    pub fn blank_file(&self) -> Option<SceneFile> {
        let mut file = SceneFile::new(self.area?, self.origin);
        file.provenance = self.provenance.clone();
        Some(file)
    }

    /// Empties the document, keeping the area it is on: what
    /// [`Self::blank_file`] describes.
    pub fn clear_document(&mut self, models: &mut Models) {
        self.cameras.clear();
        self.params = CoverageParams::default();
        self.areas.clear();
        self.limit_figures_to = None;
        self.targets.clear();
        self.agent_log.clear();
        self.on_flat_ground = false;
        self.saved_provenance = None;
        self.next_id = 1;
        models.custom.clear();
        self.touch();
    }

    /// Records the provenance of an area that just loaded, and when it
    /// brings back a restored or imported scene (`kept`), how its map data
    /// changed since that scene was saved.
    pub fn area_loaded(&mut self, now: Provenance, kept: bool) -> Option<DataChange> {
        let saved = self.saved_provenance.take();
        let change = saved.filter(|_| kept).and_then(|s| s.data_change(&now));
        self.provenance = Some(now);
        change
    }
}

/// One undo step: everything [`Document`] saves, plus the custom models.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub area: Option<AreaRef>,
    pub origin: LatLon,
    pub cameras: Vec<CameraPlacement>,
    pub params: CoverageParams,
    pub areas: Vec<LabelledArea>,
    pub limit_figures_to: Option<u32>,
    pub targets: Vec<Target>,
    pub custom_models: Vec<CameraModel>,
    /// The change log, so undoing an accepted proposal takes its entry out.
    pub agent_log: Vec<AgentLogEntry>,
}

/// Request to (re)load an area.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadArea {
    pub area: AreaRef,
    /// Keep the current cameras (used when restoring a saved scene).
    pub keep_cameras: bool,
}

impl LoadArea {
    pub fn demo() -> Self {
        Self {
            area: AreaRef::Demo,
            keep_cameras: false,
        }
    }

    pub fn osm(bbox: BBox) -> Self {
        Self {
            area: AreaRef::Osm { bbox },
            keep_cameras: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(fingerprint: &str, date: u64) -> Provenance {
        Provenance {
            osm_data_unix: Some(date),
            osm_fingerprint: Some(fingerprint.to_owned()),
            app_version: "0.1.0".into(),
            ..Provenance::default()
        }
    }

    fn file(p: Option<Provenance>) -> SceneFile {
        let mut f = SceneFile::new(AreaRef::Demo, LatLon::new(0.0, 0.0));
        f.provenance = p;
        f
    }

    #[test]
    fn areas_and_targets_survive_load_edit_undo_and_save() {
        use observation_core::footage::{AreaLabel, TargetPlace};
        let mut models = Models::new(Catalog::default());
        let mut f = file(None);
        let square = |x: f32| vec![[x, 0.0], [x + 5.0, 0.0], [x + 5.0, 5.0]];
        f.areas = vec![
            LabelledArea {
                id: 4,
                label: Some(AreaLabel::Own),
                name: Some("Garden".into()),
                points: square(0.0),
                enabled: true,
            },
            LabelledArea {
                id: 9,
                label: Some(AreaLabel::Neighbour),
                name: None,
                points: square(10.0),
                enabled: true,
            },
        ];
        f.limit_figures_to = Some(4);
        f.targets = vec![Target {
            id: 1,
            name: "Gate".into(),
            at: TargetPlace::Ground {
                points: square(1.0),
            },
        }];
        let mut doc = Document::default();
        doc.load_file(f.clone(), &mut models);
        assert_eq!(doc.to_file(&models), Some(f.clone()));
        let before = doc.snapshot(&models);
        // The Region tool redraws the limiting area: its label, its name and
        // the other areas stay.
        doc.set_region(Some(Region {
            points: square(2.0),
            enabled: false,
        }));
        let saved = doc.to_file(&models).expect("an area");
        assert_eq!(saved.areas[0].label, Some(AreaLabel::Own));
        assert_eq!(saved.areas[0].name.as_deref(), Some("Garden"));
        assert_eq!(
            (saved.areas[0].points[0], saved.areas[0].enabled),
            ([2.0, 0.0], false)
        );
        assert_eq!(saved.areas[1], f.areas[1]);
        assert_eq!(saved.targets, f.targets);
        doc.restore(&before, &mut models);
        assert_eq!(doc.to_file(&models), Some(f));
        doc.clear_document(&mut models);
        assert!(doc.areas.is_empty() && doc.targets.is_empty() && doc.region().is_none());
    }

    #[test]
    fn a_loaded_scene_is_checked_once_against_its_area() {
        let mut models = Models::new(Catalog::default());
        let mut scene = Document::default();
        let saved = provenance("00000000000000aa", 1_793_475_782);
        scene.load_file(file(Some(saved.clone())), &mut models);
        // Until the area loads, the file's provenance is kept for autosave.
        assert_eq!(
            scene.to_file(&models).and_then(|f| f.provenance),
            Some(saved.clone())
        );
        // The same features on a newer data date: nothing to report.
        let same = provenance("00000000000000aa", 1_794_000_000);
        assert_eq!(scene.area_loaded(same.clone(), true), None);
        assert_eq!(scene.provenance, Some(same));

        // Different features: reported once, and only for that load.
        scene.load_file(file(Some(saved.clone())), &mut models);
        let changed = provenance("00000000000000bb", 1_794_000_000);
        let change = scene.area_loaded(changed.clone(), true).unwrap();
        assert!(change.message().contains("saved: 2026-10-31"), "{change:?}");
        assert_eq!(scene.provenance, Some(changed.clone()));
        assert_eq!(scene.area_loaded(changed.clone(), true), None);

        // A different area replacing the scene is not compared with it.
        scene.load_file(file(Some(saved)), &mut models);
        assert_eq!(scene.area_loaded(changed.clone(), false), None);
        assert_eq!(scene.saved_provenance, None);

        // A file without provenance has nothing to compare.
        scene.load_file(file(None), &mut models);
        assert_eq!(scene.saved_provenance, None);
        assert_eq!(scene.area_loaded(changed, true), None);
    }

    #[test]
    fn importing_over_the_loaded_area_keeps_its_provenance_until_reload() {
        let mut models = Models::new(Catalog::default());
        let mut scene = Document::default();
        scene.load_file(file(None), &mut models);
        let on_screen = provenance("00000000000000aa", 1_794_000_000);
        scene.area_loaded(on_screen.clone(), true);
        // A file for the same area: if its reload fails, the figures are
        // still from the data on screen, and so is what gets saved.
        scene.load_file(
            file(Some(provenance("00000000000000bb", 1_793_475_782))),
            &mut models,
        );
        assert_eq!(scene.provenance, Some(on_screen.clone()));
        // When the reload arrives, the file is checked against it.
        assert!(scene.area_loaded(on_screen, true).is_some());
    }

    fn camera() -> CameraPlacement {
        CameraPlacement {
            id: 0,
            label: "Porch".to_owned(),
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

    #[test]
    fn a_cleared_document_is_its_blank_file() {
        let mut models = Models::new(Catalog::default());
        let mut scene = Document::default();
        assert!(scene.blank_file().is_none(), "no area yet");
        let mut f = file(Some(provenance("00000000000000aa", 0)));
        f.cameras.push(CameraPlacement { id: 7, ..camera() });
        scene.load_file(f, &mut models);
        let blank = scene.blank_file().expect("an area is set");
        assert!(blank.cameras.is_empty());
        let revision = scene.revision;
        scene.clear_document(&mut models);
        assert_ne!(scene.revision, revision, "a change for autosave");
        assert_eq!(
            scene.to_file(&models).map(|f| f.to_json()),
            Some(blank.to_json())
        );
        assert_eq!(scene.saved_provenance, None);
        assert_eq!(scene.add_camera(camera()), 1, "ids start again");
    }

    #[test]
    fn undoing_into_another_area_forgets_the_provenance() {
        let mut models = Models::new(Catalog::default());
        let mut scene = Document::default();
        scene.load_file(file(None), &mut models);
        scene.area_loaded(provenance("00000000000000aa", 0), true);
        let mut other = scene.snapshot(&models);
        other.area = Some(AreaRef::Osm {
            bbox: BBox::around(LatLon::new(1.0, 1.0), 100.0),
        });
        scene.restore(&other, &mut models);
        assert_eq!(scene.provenance, None, "it described the demo street");
        // Undoing within the area keeps it.
        scene.area_loaded(provenance("00000000000000cc", 0), true);
        let same = scene.snapshot(&models);
        scene.restore(&same, &mut models);
        assert!(scene.provenance.is_some());
    }
}
