//! Line of sight, sight lines and the running coverage tally.

use crate::config;
use glam::{Vec2, Vec3};
use observation_core::bundle::Road;
use observation_core::coverage::{
    CoverageStats, GroundGrid, Region, RegionMask, Sight, StaticLayers, Tally,
};
use observation_core::frustum::ViewCone;
use observation_core::occlusion::Occluders;
use observation_core::terrain::Terrain;
use observation_core::testing::box_footprint;
use proptest::prelude::*;

/// A 100 m square street: up to six houses, a road along a random line and
/// ground that is flat or tilted. Cells are 2 m, so that a cell's area (4 m²)
/// adds up exactly in `f32`.
#[derive(Debug, Clone)]
struct Street {
    boxes: Vec<(f32, f32, f32, f32, f32)>,
    road_z: f32,
    tilt: Option<(f32, f32)>,
}

impl Street {
    fn layers(&self) -> StaticLayers {
        let footprints: Vec<_> = self
            .boxes
            .iter()
            .map(|&(x, z, w, d, h)| box_footprint(x, z, x + w, z + d, h))
            .collect();
        let refs: Vec<_> = footprints.iter().collect();
        let road = Road {
            osm_id: 1,
            highway: "residential".into(),
            name: None,
            width_m: 6.0,
            points: vec![[-50.0, self.road_z], [50.0, self.road_z + 10.0]],
        };
        let terrain = self.tilt.map_or_else(Terrain::default, |(a, b)| {
            Terrain::sample_from(
                Vec2::splat(-60.0),
                Vec2::splat(60.0),
                |p| Some(100.0 + a * p.x + b * p.y),
                "",
            )
            .unwrap_or_default()
        });
        let grid = GroundGrid::covering(Vec2::splat(-50.0), Vec2::splat(50.0), 2.0);
        StaticLayers::from_parts(grid, &refs, &[road], 1.0, terrain)
    }
}

fn street() -> impl Strategy<Value = Street> {
    let house = (
        -45.0..=35.0f32,
        -45.0..=35.0f32,
        3.0..=12.0f32,
        3.0..=12.0f32,
        3.0..=15.0f32,
    );
    (
        prop::collection::vec(house, 0..6),
        -30.0..=30.0f32,
        prop::option::of((-0.15..=0.15f32, -0.15..=0.15f32)),
    )
        .prop_map(|(boxes, road_z, tilt)| Street {
            boxes,
            road_z,
            tilt,
        })
}

/// A camera somewhere over the street, 1 to 8 m above the ground.
#[derive(Debug, Clone, Copy)]
struct Camera {
    at: Vec2,
    above: f32,
    yaw: f32,
    pitch: f32,
    roll: f32,
    hfov: f32,
    vfov: f32,
    range: f32,
}

impl Camera {
    fn cone(&self, layers: &StaticLayers) -> ViewCone {
        let eye = Vec3::new(
            self.at.x,
            layers.terrain().height_at(self.at) + self.above,
            self.at.y,
        );
        ViewCone::from_angles(
            eye, self.yaw, self.pitch, self.roll, self.hfov, self.vfov, self.range,
        )
    }
}

fn camera() -> impl Strategy<Value = Camera> {
    (
        (-45.0..=45.0f32, -45.0..=45.0f32, 1.0..=8.0f32),
        (0.0..360.0f32, -60.0..=10.0f32, -20.0..=20.0f32),
        (30.0..=150.0f32, 20.0..=110.0f32, 5.0..=40.0f32),
    )
        .prop_map(
            |((x, z, above), (yaw, pitch, roll), (hfov, vfov, range))| Camera {
                at: Vec2::new(x, z),
                above,
                yaw,
                pitch,
                roll,
                hfov,
                vfov,
                range,
            },
        )
}

/// Half the time, the figures are limited to a rectangle.
fn region() -> impl Strategy<Value = Option<Region>> {
    prop::option::of(
        (
            -50.0..=0.0f32,
            -50.0..=0.0f32,
            10.0..=60.0f32,
            10.0..=60.0f32,
        )
            .prop_map(|(x, z, w, d)| Region {
                points: vec![[x, z], [x + w, z], [x + w, z + d], [x, z + d]],
                enabled: true,
            }),
    )
}

/// Indices that are sorted, unique and below `len`, with one distance each.
fn well_formed(indices: &[u32], d2: &[f32], len: usize) -> bool {
    indices.len() == d2.len()
        && indices.windows(2).all(|w| w[0] < w[1])
        && indices.last().is_none_or(|&i| (i as usize) < len)
}

proptest! {
    #![proptest_config(config(24))]

    #[test]
    fn prop_sight_within_a_range_is_visibility_at_that_range(
        s in street(),
        cam in camera(),
        cut in 0.05..=1.0f32,
        subject in 0.0..=2.0f32,
    ) {
        let layers = s.layers();
        let cone = cam.cone(&layers);
        let sight = layers.sight(&cone, subject);
        prop_assert_eq!(sight.range, cone.range);
        prop_assert!(well_formed(&sight.ground, &sight.ground_d2, layers.grid.len()));
        prop_assert!(well_formed(&sight.facade, &sight.facade_d2, layers.facades.len()));
        prop_assert!(sight.ground.iter().all(|&i| !layers.building_mask[i as usize]));
        let range = cone.range * cut;
        prop_assert!(sight.covers(range));
        let direct = layers.visibility(&ViewCone { range, ..cone }, subject);
        prop_assert_eq!(sight.within(range), direct);
    }

    #[test]
    fn prop_tally_matches_a_full_recount(
        s in street(),
        cams in prop::collection::vec(camera(), 1..4),
        region in region(),
        // (camera, range as a fraction of its own, add rather than remove).
        ops in prop::collection::vec((0..4usize, 0.1..=1.0f32, prop::bool::weighted(0.7)), 0..300),
    ) {
        let layers = s.layers();
        let sights: Vec<Sight> = cams.iter().map(|c| layers.sight(&c.cone(&layers), 1.0)).collect();
        let mask = region.as_ref().map(|r| RegionMask::new(&layers, r));
        let mut tally = Tally::new(&layers, mask.as_ref());
        let mut active: Vec<(usize, f32)> = Vec::new();
        for (cam, cut, add) in ops {
            let cam = cam % sights.len();
            if add || active.is_empty() {
                let range = sights[cam].range * cut;
                tally.add_within(&layers, mask.as_ref(), &sights[cam], range);
                active.push((cam, range));
            } else {
                let (cam, range) = active.swap_remove(cam % active.len());
                tally.remove_within(&layers, mask.as_ref(), &sights[cam], range);
            }
        }
        let kept = tally.stats(&layers);
        let full = CoverageStats::compute_in(&layers, &tally.counts, mask.as_ref());
        prop_assert_eq!(kept.street_m2, full.street_m2);
        prop_assert_eq!(kept.street_seen_m2, full.street_seen_m2);
        prop_assert_eq!(kept.street_seen_multi_m2, full.street_seen_multi_m2);
        prop_assert_eq!(kept.open_ground_seen_m2, full.open_ground_seen_m2);
        let close = |a: f32, b: f32| (a - b).abs() <= 1e-3 * a.abs().max(1.0);
        prop_assert!(close(kept.facade_m2, full.facade_m2), "{kept:?} {full:?}");
        prop_assert!(close(kept.facade_seen_m2, full.facade_seen_m2), "{kept:?} {full:?}");
        // Limiting the figures to the region afresh gives the same figures.
        let mut again = tally.clone();
        again.limit_to(&layers, mask.as_ref());
        prop_assert_eq!(again.stats(&layers).street_seen_m2, kept.street_seen_m2);
    }

    #[test]
    fn prop_tally_counts_the_cameras_that_see_each_cell(
        s in street(),
        cams in prop::collection::vec(camera(), 1..4),
        ops in prop::collection::vec((0..4usize, 0.1..=1.0f32, prop::bool::weighted(0.7)), 0..200),
    ) {
        let layers = s.layers();
        let sights: Vec<Sight> = cams.iter().map(|c| layers.sight(&c.cone(&layers), 1.0)).collect();
        let mut tally = Tally::new(&layers, None);
        let mut active: Vec<(usize, f32)> = Vec::new();
        for (cam, cut, add) in ops {
            let cam = cam % sights.len();
            if add || active.is_empty() {
                let range = sights[cam].range * cut;
                tally.add_within(&layers, None, &sights[cam], range);
                active.push((cam, range));
            } else {
                let (cam, range) = active.swap_remove(cam % active.len());
                tally.remove_within(&layers, None, &sights[cam], range);
            }
        }
        prop_assert_eq!(&tally.counts.ground, &true_counts(&layers, &sights, &active).0);
        prop_assert_eq!(&tally.counts.facade, &true_counts(&layers, &sights, &active).1);
    }

    #[test]
    fn prop_occlusion_is_symmetric(
        s in street(),
        a in (-60.0..=60.0f32, 0.0..=20.0f32, -60.0..=60.0f32),
        b in (-60.0..=60.0f32, 0.0..=20.0f32, -60.0..=60.0f32),
    ) {
        let footprints: Vec<_> = s
            .boxes
            .iter()
            .map(|&(x, z, w, d, h)| box_footprint(x, z, x + w, z + d, h))
            .collect();
        let occ = Occluders::new(&footprints);
        let (a, b) = (Vec3::new(a.0, a.1, a.2), Vec3::new(b.0, b.1, b.2));
        prop_assert_eq!(occ.segment_blocked(a, b), occ.segment_blocked(b, a));
    }
}

/// How many of the `active` cameras see each ground cell and façade patch,
/// as the `u8` the counters hold (saturating at 255).
fn true_counts(
    layers: &StaticLayers,
    sights: &[Sight],
    active: &[(usize, f32)],
) -> (Vec<u8>, Vec<u8>) {
    let mut ground = vec![0usize; layers.grid.len()];
    let mut facade = vec![0usize; layers.facades.len()];
    for &(cam, range) in active {
        for i in sights[cam].ground_within(range) {
            ground[i as usize] += 1;
        }
        for i in sights[cam].facade_within(range) {
            facade[i as usize] += 1;
        }
    }
    let cap = |v: Vec<usize>| v.into_iter().map(|c| c.min(255) as u8).collect();
    (cap(ground), cap(facade))
}

proptest! {
    #![proptest_config(config(16))]

    /// The counters are `u8`: adding saturates at 255, but taking a camera
    /// away always subtracts one, so with more than 255 cameras on a cell its
    /// count drifts low and a cell some camera still sees can read as unseen.
    /// The smallest case (as shrunk): one camera's sight added 256 times and
    /// removed once. Its cells read 254, though 255 cameras still see them.
    /// Removed 255 times instead, they read 0 though one camera sees them.
    #[test]
    #[ignore = "fixed in claude/fix-core-coverage-count-saturation"]
    fn prop_tally_counts_more_than_255_cameras(
        cam in camera(),
        extra in 0..40usize,
        removed in 1..=255usize,
    ) {
        let layers = Street { boxes: vec![], road_z: 0.0, tilt: None }.layers();
        let sight = layers.sight(&cam.cone(&layers), 1.0);
        prop_assume!(!sight.ground.is_empty());
        let mut tally = Tally::new(&layers, None);
        let added = 256 + extra;
        for _ in 0..added {
            tally.add_within(&layers, None, &sight, sight.range);
        }
        for _ in 0..removed {
            tally.remove_within(&layers, None, &sight, sight.range);
        }
        let active = vec![(0, sight.range); added - removed];
        let (ground, _) = true_counts(&layers, std::slice::from_ref(&sight), &active);
        prop_assert_eq!(&tally.counts.ground, &ground);
    }
}
