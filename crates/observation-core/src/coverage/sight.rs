//! What one camera sees.

use super::StaticLayers;
use super::facade::FACADE_OFFSET_M;
use crate::frustum::ViewCone;
use glam::Vec2;
use serde::{Deserialize, Serialize};

impl StaticLayers {
    /// Ground and façade cells a single camera can see.
    pub fn visibility(&self, cone: &ViewCone, subject_height_m: f32) -> Visibility {
        self.sight(cone, subject_height_m).within(cone.range)
    }

    /// Ground and façade cells a single camera can see out to `cone.range`,
    /// each with its squared distance from the eye. Range only ever enters as
    /// a distance cut-off, so the cells seen at any shorter range (another
    /// detail level, or night) follow from these without another
    /// line-of-sight pass: see [`Sight::within`].
    pub fn sight(&self, cone: &ViewCone, subject_height_m: f32) -> Sight {
        let mut s = Sight {
            range: cone.range,
            ..Sight::default()
        };
        // Candidates: the cells in the cone's square bounds, one cell wider,
        // so a cell within range is a candidate whatever the rounding, and
        // which cells are seen does not depend on the bounds (or the range
        // they were taken at).
        let (lo, hi) = cone.xz_bounds();
        let pad = Vec2::splat(self.grid.cell_m);
        let (lo, hi) = (Vec2::from_array(lo) - pad, Vec2::from_array(hi) + pad);
        if let Some((x0, x1, z0, z1)) = self.grid.range(lo, hi) {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    let i = (z * self.grid.w + x) as usize;
                    if self.building_mask[i] {
                        continue;
                    }
                    let p = self.ground_subject(i, subject_height_m);
                    if cone.contains(p) && !self.segment_blocked(cone.eye, p) {
                        s.ground.push(i as u32);
                        // As `ViewCone::contains` computes it, so thresholding
                        // agrees with it exactly.
                        s.ground_d2.push((p - cone.eye).length_squared());
                    }
                }
            }
        }
        let r2 = cone.range * cone.range;
        for (i, f) in self.facades.iter().enumerate() {
            let to_eye = cone.eye - f.center;
            let centre_d2 = to_eye.length_squared();
            if !(centre_d2 <= r2 && to_eye.dot(f.normal) > 0.0) {
                continue;
            }
            let p = f.center + f.normal * FACADE_OFFSET_M;
            if cone.contains(p) && !self.segment_blocked(cone.eye, p) {
                s.facade.push(i as u32);
                // A patch passes two range tests, on its centre and on the
                // sample point in front of it: it is in range when both are.
                s.facade_d2
                    .push(centre_d2.max((p - cone.eye).length_squared()));
            }
        }
        s
    }
}

/// Cells seen by one camera (sorted, unique indices).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visibility {
    pub ground: Vec<u32>,
    pub facade: Vec<u32>,
}

/// Cells seen by one camera out to [`Self::range`], each with its squared
/// distance from the eye (parallel to the index lists), from which the cells
/// seen at any range up to that one follow.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sight {
    /// The range these were computed out to, metres.
    pub range: f32,
    /// Sorted, unique ground cell indices.
    pub ground: Vec<u32>,
    pub ground_d2: Vec<f32>,
    /// Sorted, unique façade patch indices.
    pub facade: Vec<u32>,
    /// For each patch, the larger of its two range tests' squared distances.
    pub facade_d2: Vec<f32>,
}

impl Sight {
    /// Whether [`Self::within`] is exact at `range`: it is no longer than the
    /// range these were computed out to.
    pub fn covers(&self, range: f32) -> bool {
        range <= self.range
    }

    /// Ground cells seen out to `range` (all of them beyond [`Self::range`]).
    pub fn ground_within(&self, range: f32) -> impl Iterator<Item = u32> + '_ {
        let r2 = range * range;
        self.ground
            .iter()
            .zip(&self.ground_d2)
            .filter(move |(_, d2)| **d2 <= r2)
            .map(|(i, _)| *i)
    }

    /// Façade patches seen out to `range` (all of them beyond [`Self::range`]).
    pub fn facade_within(&self, range: f32) -> impl Iterator<Item = u32> + '_ {
        let r2 = range * range;
        self.facade
            .iter()
            .zip(&self.facade_d2)
            .filter(move |(_, d2)| **d2 <= r2)
            .map(|(i, _)| *i)
    }

    /// The cells seen out to `range`: for a range these cover, exactly what
    /// [`StaticLayers::visibility`] gives for the same cone cut to `range`.
    pub fn within(&self, range: f32) -> Visibility {
        Visibility {
            ground: self.ground_within(range).collect(),
            facade: self.facade_within(range).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::layers;
    use super::*;
    use crate::building::Footprint;
    use crate::camera::{DoriLevel, Lighting};
    use crate::coverage::{CoverageParams, GroundGrid, facade_cells};
    use crate::terrain::Terrain;
    use crate::testing::box_footprint;
    use glam::Vec3;

    #[test]
    fn hills_hide_the_ground_behind_them() {
        // A 6 m mound across x = 10 to 20 on otherwise level ground.
        let mound = Terrain::sample_from(
            Vec2::splat(-60.0),
            Vec2::splat(60.0),
            |p| {
                Some(if (10.0..=20.0).contains(&p.x) {
                    6.0
                } else {
                    0.0
                })
            },
            "",
        )
        .unwrap();
        let grid = GroundGrid::covering(Vec2::new(-50.0, -50.0), Vec2::new(50.0, 50.0), 1.0);
        let flat = StaticLayers::from_parts(grid, &[], &[], 1.0, Terrain::default());
        let hilly = StaticLayers::from_parts(grid, &[], &[], 1.0, mound.clone());
        // Looking east along z = 0 from 2.5 m.
        let cone =
            ViewCone::from_angles(Vec3::new(0.0, 2.5, 0.0), 90.0, -5.0, 0.0, 40.0, 40.0, 40.0);
        let seen = |l: &StaticLayers, x: f32| {
            let i = l.grid.cell_at(Vec2::new(x, 0.0)).unwrap() as u32;
            l.visibility(&cone, 1.0).ground.contains(&i)
        };
        assert!(seen(&flat, 30.0) && seen(&hilly, 5.0));
        assert!(!seen(&hilly, 30.0), "behind the mound");
        // The subject on top of the mound stands 6 m up.
        let i = hilly.grid.cell_at(Vec2::new(15.5, 0.5)).unwrap();
        assert!((hilly.ground_subject(i, 1.0).y - 7.0).abs() < 0.01);

        // A wall half buried by the mound has patches only above the ground.
        let shed = Footprint {
            outer: vec![[16.0, -2.0], [24.0, -2.0], [24.0, 2.0], [16.0, 2.0]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 10.0,
            base_m: 0.0,
        };
        let cells = facade_cells([&shed], 1.0, &mound);
        let west: Vec<_> = cells.iter().filter(|c| c.normal.x < -0.5).collect();
        assert!(!west.is_empty());
        assert!(
            west.iter()
                .all(|c| c.corners.iter().all(|p| p.y >= 6.0 - 1e-3))
        );
        // The east wall stands on the foot of the slope (interpolated
        // between the 5 m samples).
        let east = cells.iter().filter(|c| c.normal.x > 0.5);
        assert!(east.clone().any(|c| c.corners[0].y < 2.0));
    }

    #[test]
    fn house_shadows_the_road_behind_it() {
        let l = layers();
        // Camera north of the house looking south, over the house toward the road.
        let behind = ViewCone::from_angles(
            Vec3::new(0.0, 2.0, -40.0),
            180.0,
            -5.0,
            0.0,
            90.0,
            60.0,
            60.0,
        );
        let v = l.visibility(&behind, 1.0);
        let shadowed = l.grid.cell_at(Vec2::new(0.0, 0.0)).unwrap() as u32;
        let visible = l.grid.cell_at(Vec2::new(30.0, 0.0)).unwrap() as u32;
        assert!(
            !v.ground.contains(&shadowed),
            "road directly behind the house is hidden"
        );
        assert!(
            v.ground.contains(&visible),
            "road off to the side is visible"
        );
        // Sees the house's north wall but not its south wall.
        assert!(
            v.facade
                .iter()
                .any(|&i| l.facades[i as usize].normal.z < -0.9)
        );
        assert!(
            !v.facade
                .iter()
                .any(|&i| l.facades[i as usize].normal.z > 0.9)
        );
    }

    /// The visibility computed directly at the cone's range, as it was before
    /// [`Sight`]: the reference the thresholded results must equal.
    fn direct(l: &StaticLayers, cone: &ViewCone, subject_height_m: f32) -> Visibility {
        let mut ground = Vec::new();
        let (lo, hi) = cone.xz_bounds();
        if let Some((x0, x1, z0, z1)) = l.grid.range(Vec2::from_array(lo), Vec2::from_array(hi)) {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    let i = (z * l.grid.w + x) as usize;
                    if l.building_mask[i] {
                        continue;
                    }
                    let p = l.ground_subject(i, subject_height_m);
                    if cone.contains(p) && !l.segment_blocked(cone.eye, p) {
                        ground.push(i as u32);
                    }
                }
            }
        }
        let r2 = cone.range * cone.range;
        let facade = l
            .facades
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                let to_eye = cone.eye - f.center;
                to_eye.length_squared() <= r2 && to_eye.dot(f.normal) > 0.0
            })
            .filter(|(_, f)| {
                let p = f.center + f.normal * FACADE_OFFSET_M;
                cone.contains(p) && !l.segment_blocked(cone.eye, p)
            })
            .map(|(i, _)| i as u32)
            .collect();
        Visibility { ground, facade }
    }

    /// xorshift64*: enough randomness for scene generation, without a dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32
        }

        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.next()
        }
    }

    fn model(rng: &mut Rng) -> crate::camera::CameraModel {
        crate::camera::CameraModel {
            id: "random".into(),
            vendor: String::new(),
            name: "Random".into(),
            kind: crate::camera::CameraKind::Bullet,
            hfov_deg: rng.range(30.0, 150.0),
            vfov_deg: rng.range(20.0, 110.0),
            resolution: [rng.range(640.0, 3840.0) as u32, 1080],
            ir_range_m: (rng.next() < 0.7).then(|| rng.range(3.0, 40.0)),
            source: None,
            notes: None,
        }
    }

    #[test]
    fn every_level_and_lighting_follows_from_one_sight() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut checked = (0, 0);
        for scene in 0..12 {
            let boxes: Vec<Footprint> = (0..6)
                .map(|_| {
                    let (x, z) = (rng.range(-50.0, 45.0), rng.range(-50.0, 45.0));
                    let (w, d) = (rng.range(3.0, 15.0), rng.range(3.0, 15.0));
                    box_footprint(x, z, x + w, z + d, rng.range(3.0, 15.0))
                })
                .collect();
            let refs: Vec<&Footprint> = boxes.iter().collect();
            let road = crate::bundle::Road {
                osm_id: 1,
                highway: "residential".into(),
                name: None,
                width_m: rng.range(4.0, 10.0),
                points: vec![
                    [-60.0, rng.range(-40.0, 40.0)],
                    [60.0, rng.range(-40.0, 40.0)],
                ],
            };
            // Every other scene on a slope with a ridge, so the ground also
            // blocks sight lines.
            let terrain = if scene % 2 == 1 {
                let (tilt, ridge) = (rng.range(-0.1, 0.1), rng.range(-30.0, 30.0));
                Terrain::sample_from(
                    Vec2::splat(-70.0),
                    Vec2::splat(70.0),
                    |p| Some(p.x * tilt + 5.0 * (-((p.y - ridge) / 8.0).powi(2)).exp()),
                    "",
                )
                .unwrap()
            } else {
                Terrain::default()
            };
            let grid = GroundGrid::covering(Vec2::splat(-60.0), Vec2::splat(60.0), 1.0);
            let l = StaticLayers::from_parts(grid, &refs, &[road], 1.0, terrain);
            for _ in 0..4 {
                let m = model(&mut rng);
                let at = Vec2::new(rng.range(-55.0, 55.0), rng.range(-55.0, 55.0));
                let eye = Vec3::new(at.x, l.terrain().height_at(at) + rng.range(1.0, 8.0), at.y);
                let (yaw, pitch) = (rng.range(0.0, 360.0), rng.range(-40.0, 10.0));
                let params = CoverageParams {
                    dori: if rng.next() < 0.3 {
                        DoriLevel::Custom
                    } else {
                        DoriLevel::Observe
                    },
                    custom_ppm: rng.range(5.0, 400.0),
                    ..CoverageParams::default()
                };
                let cone_at = |range| {
                    ViewCone::from_angles(
                        eye,
                        yaw,
                        pitch,
                        0.0,
                        m.hfov_clamped(),
                        m.vfov_clamped(),
                        range,
                    )
                };
                let h = rng.range(0.0, 2.0);
                let sight = l.sight(&cone_at(params.sight_range_m(&m)), h);
                for (_, ppm) in params.ladder() {
                    for lighting in [Lighting::Day, Lighting::Night] {
                        let r = m.effective_range_m(ppm, lighting);
                        assert!(sight.covers(r), "{r} beyond {}", sight.range);
                        let want = direct(&l, &cone_at(r), h);
                        assert_eq!(
                            sight.within(r),
                            want,
                            "scene {scene}, {r} m of {}",
                            sight.range
                        );
                        checked.0 += want.ground.len();
                        checked.1 += want.facade.len();
                    }
                }
            }
        }
        // The scenes are not all empty views.
        assert!(checked.0 > 10_000 && checked.1 > 500, "{checked:?}");
    }

    #[test]
    fn grazing_patches_are_cut_by_the_farther_of_their_two_tests() {
        // A long wall facing south (+z), seen almost edge-on from just in front
        // of its plane: the sample point 5 cm out from each patch is then
        // farther from the eye than the patch centre.
        let wall = box_footprint(0.0, -2.0, 60.0, 0.0, 4.0);
        let grid = GroundGrid::covering(Vec2::splat(-10.0), Vec2::new(70.0, 10.0), 1.0);
        let l = StaticLayers::from_parts(grid, &[&wall], &[], 1.0, Terrain::default());
        let eye = Vec3::new(-1.0, 2.0, 0.01);
        let cone = |range| ViewCone::from_angles(eye, 90.0, 0.0, 0.0, 60.0, 60.0, range);
        let sight = l.sight(&cone(80.0), 1.0);
        // Patches whose two tests straddle a range: their centres are within
        // it and their sample points are not.
        let straddles: Vec<(usize, f32)> = sight
            .facade
            .iter()
            .zip(&sight.facade_d2)
            .filter_map(|(&i, &d2)| {
                let centre = (eye - l.facades[i as usize].center).length_squared();
                (centre < d2).then(|| (i as usize, (centre.sqrt() + d2.sqrt()) / 2.0))
            })
            .collect();
        assert!(!straddles.is_empty());
        for (i, r) in straddles {
            let cut = sight.within(r);
            assert_eq!(cut, direct(&l, &cone(r), 1.0));
            assert!(!cut.facade.contains(&(i as u32)), "patch {i} at {r} m");
        }
    }
}
