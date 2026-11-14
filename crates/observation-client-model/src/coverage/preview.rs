//! The coverage of a proposed scene, for the proposal card's before and
//! after figures (see [`crate::proposal`]).
//!
//! It is a second [`CoverageState`] over a copy of the proposed scene,
//! computed by the same [`CoverageBackend`] as the document's: in the
//! browser, in the coverage worker. Its cameras' ids carry
//! [`PREVIEW_ID_BIT`], so its jobs never collide with the document's (the
//! backend keeps one job per camera id), and every result the backend
//! returns is offered to both states, each keeping only its own. A camera
//! the proposal leaves as it is takes the document's sight lines, so only
//! new and changed cameras are computed.

use super::{CoverageState, key, update};
use crate::coverage::backend::CoverageBackend;
use crate::document::{Document, Models};
use observation_core::camera::{DoriLevel, Lighting};
use observation_core::coverage::{CoverageStats, StaticLayers};
use observation_core::coverage_job::JobResult;
use observation_core::scene::SceneFile;

/// Set in the ids of the preview's cameras. Scenes number their cameras
/// from 1 upwards, so the document's ids stay below it.
pub const PREVIEW_ID_BIT: u32 = 1 << 31;

/// A proposed scene to preview: proposal `id`'s scene at `revision`.
#[derive(Debug, Clone, Copy)]
pub struct Proposed<'a> {
    pub id: u64,
    pub revision: u64,
    pub scene: &'a SceneFile,
}

/// The preview of one proposed scene.
#[derive(Debug)]
struct Inner {
    /// The proposal and the revision of its proposed scene.
    source: (u64, u64),
    doc: Document,
    models: Models,
    state: CoverageState,
}

/// The coverage of the proposed scene, if one is being previewed.
#[derive(Debug, Default)]
pub struct Preview {
    inner: Option<Inner>,
}

impl Preview {
    /// Stops previewing (no proposal, or a new area).
    pub fn clear(&mut self) {
        self.inner = None;
    }

    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    /// Brings the preview up to date with `proposed`. `main` is the
    /// document's state, whose sight lines unchanged cameras take; `results`
    /// are this frame's results from `backend` (the document's state is
    /// offered them too).
    pub fn update(
        &mut self,
        proposed: Proposed,
        main: &CoverageState,
        models: &Models,
        layers: &StaticLayers,
        results: Vec<JobResult>,
        backend: &mut CoverageBackend,
    ) {
        let source = (proposed.id, proposed.revision);
        if self.inner.as_ref().is_none_or(|i| i.source != source) {
            let mut file = proposed.scene.clone();
            for c in &mut file.cameras {
                c.id |= PREVIEW_ID_BIT;
            }
            let mut preview_models = Models::new(models.catalog.clone());
            let mut doc = Document::default();
            doc.load_file(file, &mut preview_models);
            // Keep the sight lines already computed for this proposal.
            let state = match self.inner.take() {
                Some(old) if old.source.0 == source.0 => old.state,
                _ => CoverageState::default(),
            };
            self.inner = Some(Inner {
                source,
                doc,
                models: preview_models,
                state,
            });
        }
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        adopt_sights(inner, main);
        update(
            &mut inner.state,
            &inner.doc,
            &inner.models,
            layers,
            results,
            backend,
        );
    }

    /// The figures at the proposed scene's selected detail level and
    /// lighting, once every camera's sight lines are in (`None` while some
    /// are being computed).
    pub fn stats(&self, backend: &CoverageBackend) -> Option<CoverageStats> {
        let inner = self.inner.as_ref()?;
        let computing = inner.doc.cameras.iter().any(|c| backend.busy(c.id));
        (!computing && inner.state.counted()).then_some(inner.state.stats)
    }

    /// The proposed scene's detail level and lighting.
    pub fn settings(&self) -> Option<(DoriLevel, Lighting)> {
        self.inner
            .as_ref()
            .map(|i| (i.doc.params.dori, i.doc.params.lighting))
    }
}

/// Gives each preview camera the document's sight lines for the same
/// camera, when they were computed for the same pose, model and subject
/// height (the same key) and the preview has none for that key yet.
fn adopt_sights(inner: &mut Inner, main: &CoverageState) {
    let subject = inner.doc.params.subject_height_m;
    for c in &inner.doc.cameras {
        let Some(m) = inner.models.get(&c.model_id) else {
            continue;
        };
        let k = key(c, m, subject);
        if inner
            .state
            .cams
            .get(&c.id)
            .is_some_and(|(have, _)| *have == k)
        {
            continue;
        }
        if let Some((main_key, sight)) = main.cams.get(&(c.id & !PREVIEW_ID_BIT))
            && *main_key == k
        {
            inner.state.cams.insert(c.id, (k, sight.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::bundle::{OsmRoad, build_bundle};
    use observation_core::camera::{CameraKind, CameraModel, CameraPlacement, Catalog};
    use observation_core::geo::{BBox, LatLon};
    use observation_core::scene::AreaRef;
    use observation_core::terrain::Terrain;

    fn world() -> (StaticLayers, Models) {
        let bbox = BBox::new(51.5, -0.001, 51.5009, 0.001).unwrap();
        let road = OsmRoad {
            osm_id: 1,
            highway: "residential".into(),
            name: None,
            width_m: 8.0,
            points: vec![LatLon::new(51.50045, -0.001), LatLon::new(51.50045, 0.001)],
        };
        let bundle = build_bundle(bbox, &[], &[road], &[], Terrain::default(), "", 0, None);
        let layers = StaticLayers::new(&bundle, 0.5, 1.0);
        let mut models = Models::new(Catalog::default());
        models.custom.push(CameraModel {
            id: "custom-test-1".into(),
            vendor: String::new(),
            name: "Test".into(),
            kind: CameraKind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 55.0,
            resolution: [1920, 1080],
            ir_range_m: Some(12.0),
            source: None,
            notes: None,
        });
        (layers, models)
    }

    fn camera(id: u32, x: f32, yaw_deg: f32) -> CameraPlacement {
        CameraPlacement {
            id,
            label: String::new(),
            model_id: "custom-test-1".into(),
            position: [x, 3.0, 4.0],
            yaw_deg,
            pitch_deg: -10.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        }
    }

    #[test]
    fn the_preview_reuses_unchanged_sight_lines_and_counts_the_proposed_scene() {
        let (layers, models) = world();
        let mut doc = Document::default();
        doc.area = Some(AreaRef::Demo);
        doc.cameras = vec![camera(1, -60.0, 90.0)];
        let mut backend = CoverageBackend::default();
        let mut main = CoverageState::default();
        update(&mut main, &doc, &models, &layers, Vec::new(), &mut backend);
        assert_eq!(backend.submitted(), 1);
        let before = main.stats;

        // The proposal keeps camera 1 and adds camera 2 looking back along the street.
        let mut proposed = doc.to_file(&models).unwrap();
        proposed.cameras.push(camera(2, 60.0, 270.0));
        let mut preview = Preview::default();
        let at = |revision| Proposed {
            id: 1,
            revision,
            scene: &proposed,
        };
        preview.update(at(1), &main, &models, &layers, Vec::new(), &mut backend);
        assert_eq!(backend.submitted(), 2, "only the new camera is computed");
        let after = preview.stats(&backend).expect("computed inline");
        assert!(after.street_seen_m2 > before.street_seen_m2);
        assert_eq!(main.stats, before, "the document's figures are untouched");

        // The same proposal again computes nothing; a new revision of it
        // keeps what it has.
        preview.update(at(1), &main, &models, &layers, Vec::new(), &mut backend);
        preview.update(at(2), &main, &models, &layers, Vec::new(), &mut backend);
        assert_eq!(backend.submitted(), 2);
        assert!(preview.is_active());
        preview.clear();
        assert!(preview.stats(&backend).is_none());
    }
}
