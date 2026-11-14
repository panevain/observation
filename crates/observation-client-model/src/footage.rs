//! Where footage lands, as the client shows it (issue #89): each enabled
//! camera's split over the labelled areas, and each camera's verdicts on
//! each target, kept up to date as the scene changes.
//!
//! The answers are `observation_core::footage`'s; this module only decides
//! what to work out again. A split is counted over the cells the coverage
//! already has for the camera ([`CoverageState::visibility`]), so it costs no
//! sight lines of its own and agrees with the overlay. A verdict tests a
//! target's few points from the camera, and is kept until the camera, its
//! model, the analysis settings or the target change, so dragging one camera
//! tests only that camera again.
//!
//! The panels call [`FootageState::update_splits`] and
//! [`FootageState::update_verdicts`] only while they show the results, so
//! nothing is worked out for a hidden panel.

use crate::coverage::CoverageState;
use crate::document::{Document, Models};
use observation_core::camera::{CameraModel, CameraPlacement};
use observation_core::coverage::{CoverageParams, StaticLayers};
use observation_core::footage::{
    AreaMasks, CameraVerdicts, DEFAULT_OFFSET_M, FootageCamera, Probe, Split, Target, TargetPlace,
    camera_verdicts, split_seen,
};
use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// The splits and verdicts worked out so far, each under a key of what it
/// was worked out from.
#[derive(Debug, Default)]
pub struct FootageState {
    /// The active areas' cells, under a key of their ids, labels and
    /// outlines.
    masks: Option<(u64, AreaMasks)>,
    /// Each enabled camera's split at the selected level, by camera id.
    splits: BTreeMap<u32, (u64, Split)>,
    /// The points each target is tested at, by target id.
    probes: BTreeMap<u32, (u64, Probe)>,
    /// Each camera's verdicts on each target, by (target id, camera id).
    verdicts: BTreeMap<(u32, u32), (u64, CameraVerdicts)>,
}

impl FootageState {
    /// Forgets everything: another area loaded, and the cells and building
    /// indices of the old one mean nothing in it.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Brings the split of every enabled camera whose cells the coverage has
    /// up to date with the document's areas and the selected detail level.
    pub fn update_splits(
        &mut self,
        doc: &Document,
        models: &Models,
        layers: &StaticLayers,
        coverage: &CoverageState,
    ) {
        let mut h = DefaultHasher::new();
        for a in doc.areas.iter().filter(|a| a.is_active()) {
            (a.id, a.label).hash(&mut h);
            hash_points(&a.points, &mut h);
        }
        let masks_key = h.finish();
        if self.masks.as_ref().is_none_or(|(k, _)| *k != masks_key) {
            self.masks = Some((
                masks_key,
                AreaMasks::new(layers, &doc.areas, DEFAULT_OFFSET_M),
            ));
        }
        let Some((_, masks)) = &self.masks else {
            return;
        };
        let mut old = std::mem::take(&mut self.splits);
        for c in doc.cameras.iter().filter(|c| c.enabled) {
            if models.get(&c.model_id).is_none() {
                continue;
            }
            let Some(vis_key) = coverage.visibility_key(c.id) else {
                continue;
            };
            let mut h = DefaultHasher::new();
            masks_key.hash(&mut h);
            vis_key.0.hash(&mut h);
            (vis_key.1.to_bits(), vis_key.2.to_bits()).hash(&mut h);
            hash_points(&[c.position], &mut h);
            let key = h.finish();
            let kept = old.remove(&c.id).filter(|(k, _)| *k == key);
            let entry = match kept {
                Some(e) => e,
                None => {
                    let Some(seen) = coverage.visibility(c.id) else {
                        continue;
                    };
                    (key, split_seen(layers, masks, c.eye(), &seen))
                }
            };
            self.splits.insert(c.id, entry);
        }
    }

    /// Camera `id`'s split at the selected level, once worked out.
    pub fn split(&self, id: u32) -> Option<&Split> {
        self.splits.get(&id).map(|(_, s)| s)
    }

    /// Brings every enabled camera's verdicts on every target up to date
    /// with the document.
    pub fn update_verdicts(&mut self, doc: &Document, models: &Models, layers: &StaticLayers) {
        let params = doc.params;
        let mut old_probes = std::mem::take(&mut self.probes);
        for t in &doc.targets {
            let key = target_key(t, &params);
            let entry = match old_probes.remove(&t.id).filter(|(k, _)| *k == key) {
                Some(e) => e,
                None => (key, Probe::target(layers, t, params.subject_height_m)),
            };
            self.probes.insert(t.id, entry);
        }
        let mut old = std::mem::take(&mut self.verdicts);
        for t in &doc.targets {
            let Some((target_key, probe)) = self.probes.get(&t.id) else {
                continue;
            };
            for c in doc.cameras.iter().filter(|c| c.enabled) {
                let Some(m) = models.get(&c.model_id) else {
                    continue;
                };
                let key = verdict_key(*target_key, c, m, &params);
                let entry = match old.remove(&(t.id, c.id)).filter(|(k, _)| *k == key) {
                    Some(e) => e,
                    None => {
                        let camera = FootageCamera {
                            placement: c,
                            model: m,
                        };
                        (key, camera_verdicts(layers, &params, &camera, probe))
                    }
                };
                self.verdicts.insert((t.id, c.id), entry);
            }
        }
    }

    /// Camera `camera`'s verdicts on target `target`, once worked out.
    pub fn verdicts(&self, target: u32, camera: u32) -> Option<&CameraVerdicts> {
        self.verdicts.get(&(target, camera)).map(|(_, v)| v)
    }
}

fn hash_points<const N: usize>(points: &[[f32; N]], h: &mut DefaultHasher) {
    points.len().hash(h);
    for p in points {
        for v in p {
            v.to_bits().hash(h);
        }
    }
}

/// Everything a target's test points depend on.
fn target_key(t: &Target, params: &CoverageParams) -> u64 {
    let mut h = DefaultHasher::new();
    params.subject_height_m.to_bits().hash(&mut h);
    match &t.at {
        TargetPlace::Ground { points } => {
            0u8.hash(&mut h);
            hash_points(points, &mut h);
        }
        TargetPlace::Wall { point, normal } => {
            1u8.hash(&mut h);
            hash_points(&[*point], &mut h);
            hash_points(&[*normal], &mut h);
        }
    }
    h.finish()
}

/// Everything one camera's verdicts on one target depend on: the target's
/// points, the camera's pose, the model's fields of view, resolution and
/// night range, and the ladder of detail levels.
fn verdict_key(target: u64, c: &CameraPlacement, m: &CameraModel, p: &CoverageParams) -> u64 {
    let mut h = DefaultHasher::new();
    target.hash(&mut h);
    hash_points(&[c.position], &mut h);
    hash_points(&[[c.yaw_deg, c.pitch_deg, c.roll_deg]], &mut h);
    hash_points(&[[m.hfov_deg, m.vfov_deg]], &mut h);
    m.resolution.hash(&mut h);
    m.ir_range_m.map(f32::to_bits).hash(&mut h);
    (p.dori, p.custom_ppm.to_bits()).hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::backend::CoverageBackend;
    use observation_core::building::Footprint;
    use observation_core::bundle::Road;
    use observation_core::camera::{CameraKind, Catalog, DoriLevel, Lighting};
    use observation_core::coverage::GroundGrid;
    use observation_core::footage::{
        AreaLabel, LabelledArea, Reason, Verdict, camera_splits, target_verdicts,
    };
    use observation_core::glam::Vec2;
    use observation_core::terrain::Terrain;

    /// A street running east-west through the middle of the area, a house
    /// north of it, and the house's camera looking south over the street.
    struct Scene {
        layers: StaticLayers,
        doc: Document,
        models: Models,
        coverage: CoverageState,
        backend: CoverageBackend,
    }

    impl Scene {
        fn new() -> Self {
            let house = Footprint {
                outer: vec![[-5.0, -20.0], [5.0, -20.0], [5.0, -12.0], [-5.0, -12.0]],
                holes: Vec::new(),
                min_height_m: 0.0,
                height_m: 6.0,
                base_m: 0.0,
            };
            let road = Road {
                osm_id: 1,
                highway: "residential".into(),
                name: None,
                width_m: 8.0,
                points: vec![[-50.0, 0.0], [50.0, 0.0]],
            };
            let grid = GroundGrid::covering(Vec2::splat(-50.0), Vec2::splat(50.0), 0.5);
            let layers =
                StaticLayers::from_parts(grid, &[&house], &[road], 1.0, Terrain::default());
            let mut models = Models::new(Catalog::default());
            models.custom.push(CameraModel {
                id: "test".into(),
                vendor: String::new(),
                name: "Test".into(),
                kind: CameraKind::Bullet,
                hfov_deg: 90.0,
                vfov_deg: 60.0,
                resolution: [1920, 1080],
                ir_range_m: Some(8.0),
                source: None,
                notes: None,
            });
            let mut doc = Document::default();
            doc.add_camera(CameraPlacement {
                id: 0,
                label: "Porch".into(),
                model_id: "test".into(),
                position: [0.0, 3.0, -11.5],
                yaw_deg: 180.0,
                pitch_deg: -15.0,
                roll_deg: 0.0,
                color: None,
                enabled: true,
                group: None,
            });
            let square = |x: f32, z: f32, size: f32| {
                vec![[x, z], [x + size, z], [x + size, z + size], [x, z + size]]
            };
            doc.areas = vec![
                LabelledArea {
                    id: 1,
                    label: Some(AreaLabel::Own),
                    name: Some("Front garden".into()),
                    points: square(-5.0, -11.0, 6.0),
                    enabled: true,
                },
                LabelledArea {
                    id: 2,
                    label: Some(AreaLabel::Neighbour),
                    name: None,
                    points: square(1.0, -11.0, 6.0),
                    enabled: true,
                },
            ];
            doc.targets = vec![
                Target {
                    id: 1,
                    name: "Gate".into(),
                    at: TargetPlace::Ground {
                        points: square(-1.0, -6.0, 1.0),
                    },
                },
                Target {
                    id: 2,
                    name: "Behind".into(),
                    at: TargetPlace::Ground {
                        points: square(-1.0, -40.0, 1.0),
                    },
                },
            ];
            let mut scene = Self {
                layers,
                doc,
                models,
                coverage: CoverageState::default(),
                backend: CoverageBackend::default(),
            };
            scene.count();
            scene
        }

        /// Brings the coverage up to date, inline.
        fn count(&mut self) {
            crate::coverage::update(
                &mut self.coverage,
                &self.doc,
                &self.models,
                &self.layers,
                Vec::new(),
                &mut self.backend,
            );
        }

        fn cameras(&self) -> Vec<FootageCamera<'_>> {
            self.doc
                .cameras
                .iter()
                .map(|c| FootageCamera {
                    placement: c,
                    model: self.models.get(&c.model_id).expect("a model"),
                })
                .collect()
        }

        fn splits(&mut self, state: &mut FootageState) {
            state.update_splits(&self.doc, &self.models, &self.layers, &self.coverage);
        }
    }

    #[test]
    fn the_splits_are_the_cores_from_the_cells_the_coverage_has() {
        let mut s = Scene::new();
        let mut state = FootageState::default();
        s.splits(&mut state);
        let masks = AreaMasks::new(&s.layers, &s.doc.areas, DEFAULT_OFFSET_M);
        let direct = camera_splits(&s.layers, &s.doc.params, &s.cameras(), &masks);
        let split = state.split(1).expect("worked out");
        assert_eq!(*split, direct[0].split);
        assert!(split.street_m2 > 0.0 && split.areas.len() == 2, "{split:?}");
        assert!(split.areas[0].seen.as_drawn_m2 > 0.0, "{split:?}");

        // At another level, the cells counted there.
        s.doc.params.dori = DoriLevel::Recognise;
        s.count();
        s.splits(&mut state);
        let direct = camera_splits(&s.layers, &s.doc.params, &s.cameras(), &masks);
        assert_eq!(state.split(1), Some(&direct[0].split));

        // An area turned off leaves the split.
        s.doc.areas[1].enabled = false;
        s.splits(&mut state);
        assert_eq!(state.split(1).map(|s| s.areas.len()), Some(1));

        // A camera turned off has none.
        s.doc.cameras[0].enabled = false;
        s.count();
        s.splits(&mut state);
        assert_eq!(state.split(1), None);
    }

    #[test]
    fn a_split_waits_for_the_cameras_cells() {
        let s = Scene::new();
        let mut state = FootageState::default();
        state.update_splits(&s.doc, &s.models, &s.layers, &CoverageState::default());
        assert_eq!(state.split(1), None, "nothing counted yet");
    }

    #[test]
    fn the_verdicts_are_the_cores_and_follow_the_scene() {
        let mut s = Scene::new();
        let mut state = FootageState::default();
        state.update_verdicts(&s.doc, &s.models, &s.layers);
        let direct = target_verdicts(&s.layers, &s.doc.params, &s.cameras(), &s.doc.targets);
        for t in &direct {
            assert_eq!(state.verdicts(t.target_id, 1), Some(&t.cameras[0]));
        }
        let gate = state.verdicts(1, 1).expect("worked out");
        assert_eq!(
            gate.at(DoriLevel::Detect, Lighting::Day),
            Some(Verdict::NotRuledOut)
        );
        let behind = state.verdicts(2, 1).expect("worked out");
        assert_eq!(
            behind.at(DoriLevel::Detect, Lighting::Day),
            Some(Verdict::RuledOut(Reason::OutsideView)),
            "behind the camera"
        );

        // Turned round, the camera faces the target behind it, through its
        // own house.
        s.doc.cameras[0].yaw_deg = 0.0;
        state.update_verdicts(&s.doc, &s.models, &s.layers);
        let behind = state.verdicts(2, 1).expect("worked out");
        assert_eq!(
            behind.at(DoriLevel::Detect, Lighting::Day),
            Some(Verdict::RuledOut(Reason::BlockedByBuilding))
        );

        // Deleted targets and cameras drop out.
        s.doc.targets.remove(0);
        s.doc.cameras[0].enabled = false;
        state.update_verdicts(&s.doc, &s.models, &s.layers);
        assert_eq!(state.verdicts(1, 1), None);
        assert_eq!(state.verdicts(2, 1), None);
    }

    #[test]
    fn verdicts_are_kept_until_what_they_depend_on_changes() {
        let mut s = Scene::new();
        let mut state = FootageState::default();
        state.update_verdicts(&s.doc, &s.models, &s.layers);
        let key = |state: &FootageState| state.verdicts.get(&(1, 1)).map(|(k, _)| *k);
        let first = key(&state);
        // The lighting only picks among the verdicts already there; a camera
        // label changes nothing.
        s.doc.params.lighting = Lighting::Night;
        s.doc.cameras[0].label = "Front".into();
        state.update_verdicts(&s.doc, &s.models, &s.layers);
        assert_eq!(key(&state), first);
        s.doc.params.subject_height_m = 1.6;
        state.update_verdicts(&s.doc, &s.models, &s.layers);
        assert_ne!(key(&state), first);
        state.reset();
        assert_eq!(key(&state), None);
    }
}
