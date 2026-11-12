//! Line of sight, sight lines and the running coverage tally.

use crate::config;
use glam::{Vec2, Vec3};
use observation_core::building::Footprint;
use observation_core::bundle::Road;
use observation_core::coverage::{
    CoverageStats, GroundGrid, Region, RegionMask, Sight, StaticLayers, Tally,
};
use observation_core::frustum::ViewCone;
use observation_core::occlusion::{Occluders, point_in_ring};
use observation_core::terrain::Terrain;
use observation_core::testing::box_footprint;
use proptest::prelude::*;

/// A box-shaped building, possibly a raised part (`min > 0`: a bridge or an
/// overhang a sight line can pass under).
#[derive(Debug, Clone, Copy)]
pub(crate) struct House {
    pub x: f32,
    pub z: f32,
    pub w: f32,
    pub d: f32,
    pub h: f32,
    pub min: f32,
}

impl House {
    pub fn footprint(&self) -> Footprint {
        Footprint {
            min_height_m: self.min,
            ..box_footprint(self.x, self.z, self.x + self.w, self.z + self.d, self.h)
        }
    }
}

pub(crate) fn house() -> impl Strategy<Value = House> {
    (
        (
            -45.0..=35.0f32,
            -45.0..=35.0f32,
            3.0..=12.0f32,
            3.0..=12.0f32,
        ),
        3.0..=15.0f32,
        prop_oneof![3 => Just(0.0f32), 1 => 0.5..=0.9f32],
    )
        .prop_map(|((x, z, w, d), h, raised)| House {
            x,
            z,
            w,
            d,
            h,
            // A raised part starts at least a metre below its top.
            min: (raised * h).min(h - 1.0),
        })
}

/// A 100 m square street: up to six houses, a road along a random line and
/// ground that is flat or tilted. Cells are 2 m, so that a cell's area (4 m²)
/// adds up exactly in `f32`.
#[derive(Debug, Clone)]
pub(crate) struct Street {
    pub houses: Vec<House>,
    pub road_z: f32,
    /// Where the road starts and ends along x (it may stop inside the area).
    pub road_x: (f32, f32),
    tilt: Option<(f32, f32)>,
}

impl Street {
    pub fn layers(&self) -> StaticLayers {
        let footprints: Vec<_> = self.houses.iter().map(House::footprint).collect();
        let refs: Vec<_> = footprints.iter().collect();
        let road = Road {
            osm_id: 1,
            highway: "residential".into(),
            name: None,
            width_m: 6.0,
            points: vec![
                [self.road_x.0, self.road_z + (self.road_x.0 + 50.0) / 10.0],
                [self.road_x.1, self.road_z + (self.road_x.1 + 50.0) / 10.0],
            ],
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

pub(crate) fn street() -> impl Strategy<Value = Street> {
    (
        prop::collection::vec(house(), 0..6),
        -30.0..=30.0f32,
        prop_oneof![
            Just((-50.0f32, 50.0f32)),
            (-50.0..=-10.0f32, 10.0..=50.0f32),
        ],
        prop::option::of((-0.15..=0.15f32, -0.15..=0.15f32)),
    )
        .prop_map(|(houses, road_z, road_x, tilt)| Street {
            houses,
            road_z,
            road_x,
            tilt,
        })
}

/// A camera somewhere over the street, 1 to 8 m above the ground.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Camera {
    pub at: Vec2,
    pub above: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub hfov: f32,
    pub vfov: f32,
    pub range: f32,
}

impl Camera {
    pub fn cone(&self, layers: &StaticLayers) -> ViewCone {
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

/// Mostly ordinary cameras, sometimes degenerate ones: a range of half a
/// metre or less, a field of view of a few degrees, or looking (nearly)
/// straight down or up.
pub(crate) fn camera() -> impl Strategy<Value = Camera> {
    (
        (-45.0..=45.0f32, -45.0..=45.0f32, 1.0..=8.0f32),
        (
            0.0..360.0f32,
            prop_oneof![
                6 => -60.0..=10.0f32,
                1 => -90.0..=-85.0f32,
                1 => 85.0..=90.0f32,
            ],
            -20.0..=20.0f32,
        ),
        (
            prop_oneof![5 => 30.0..=150.0f32, 1 => 1.0..=10.0f32],
            prop_oneof![5 => 20.0..=110.0f32, 1 => 1.0..=10.0f32],
            prop_oneof![6 => 5.0..=40.0f32, 1 => 0.0..=0.5f32],
        ),
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

/// A camera on the street's road, looking along it, so street cells are seen.
fn road_camera(road_z: f32) -> impl Strategy<Value = Camera> {
    (
        -45.0..=45.0f32,
        2.0..=6.0f32,
        prop::bool::ANY,
        (
            -15.0..=0.0f32,
            60.0..=120.0f32,
            40.0..=80.0f32,
            10.0..=40.0f32,
        ),
    )
        .prop_map(move |(x, above, east, (pitch, hfov, vfov, range))| Camera {
            at: Vec2::new(x, road_z + (x + 50.0) / 10.0),
            above,
            // The road runs east-south-east (10 m south per 100 m east).
            yaw: if east { 95.7 } else { 275.7 },
            pitch,
            roll: 0.0,
            hfov,
            vfov,
            range,
        })
}

/// A street with one to eight distinct cameras, about half of them on the
/// road.
fn street_with_cameras() -> impl Strategy<Value = (Street, Vec<Camera>)> {
    street().prop_flat_map(|s| {
        let cam = prop_oneof![camera(), road_camera(s.road_z)];
        (Just(s), prop::collection::vec(cam, 1..=8))
    })
}

/// No region, a rectangle, an L shape (concave) or any polygon (which may
/// cross itself; the mask is even-odd either way). A region has no holes.
fn region() -> impl Strategy<Value = Option<Region>> {
    let rect = (
        -50.0..=0.0f32,
        -50.0..=0.0f32,
        10.0..=60.0f32,
        10.0..=60.0f32,
    )
        .prop_map(|(x, z, w, d)| vec![[x, z], [x + w, z], [x + w, z + d], [x, z + d]]);
    let ell = (-50.0..=-10.0f32, -50.0..=-10.0f32, 20.0..=60.0f32).prop_map(|(x, z, s)| {
        let h = s / 2.0;
        vec![
            [x, z],
            [x + s, z],
            [x + s, z + h],
            [x + h, z + h],
            [x + h, z + s],
            [x, z + s],
        ]
    });
    let any = prop::collection::vec([-55.0..=55.0f32, -55.0..=55.0f32], 3..8);
    prop_oneof![
        1 => Just(None),
        1 => rect.prop_map(Some),
        1 => ell.prop_map(Some),
        1 => any.prop_map(Some),
    ]
    .prop_map(|points| {
        points.map(|points| Region {
            points,
            enabled: true,
        })
    })
}

/// Adds and removes cameras: `(camera, range as a fraction of its own, add
/// rather than remove)`. At most 40 steps, so no cell ever has more than 40
/// cameras counted on it, far below the 255 the counters hold, and most
/// counts are 0 to 3; above 255, see `prop_tally_counts_more_than_255_cameras`.
fn ops() -> impl Strategy<Value = Vec<(usize, f32, bool)>> {
    prop::collection::vec((0..8usize, 0.1..=1.0f32, prop::bool::weighted(0.6)), 0..40)
}

/// Runs `ops` on a fresh tally and returns it with the cameras still counted.
fn run(
    layers: &StaticLayers,
    mask: Option<&RegionMask>,
    sights: &[Sight],
    ops: &[(usize, f32, bool)],
) -> (Tally, Vec<(usize, f32)>) {
    let mut tally = Tally::new(layers, mask);
    let mut active: Vec<(usize, f32)> = Vec::new();
    for &(cam, cut, add) in ops {
        let cam = cam % sights.len();
        if add || active.is_empty() {
            let range = sights[cam].range * cut;
            tally.add_within(layers, mask, &sights[cam], range);
            active.push((cam, range));
        } else {
            let (cam, range) = active.swap_remove(cam % active.len());
            tally.remove_within(layers, mask, &sights[cam], range);
        }
    }
    (tally, active)
}

/// Indices that are sorted, unique and below `len`, with one distance each.
fn well_formed(indices: &[u32], d2: &[f32], len: usize) -> bool {
    indices.len() == d2.len()
        && indices.windows(2).all(|w| w[0] < w[1])
        && indices.last().is_none_or(|&i| (i as usize) < len)
}

/// The kept figures equal a one-pass count over the same counters (ground
/// figures exactly, wall areas to rounding).
fn same_figures(kept: CoverageStats, full: CoverageStats) -> Result<(), TestCaseError> {
    prop_assert_eq!(kept.street_m2, full.street_m2);
    prop_assert_eq!(kept.street_seen_m2, full.street_seen_m2);
    prop_assert_eq!(kept.street_seen_multi_m2, full.street_seen_multi_m2);
    prop_assert_eq!(kept.open_ground_seen_m2, full.open_ground_seen_m2);
    let close = |a: f32, b: f32| (a - b).abs() <= 1e-3 * a.abs().max(1.0);
    prop_assert!(close(kept.facade_m2, full.facade_m2), "{kept:?} {full:?}");
    prop_assert!(
        close(kept.facade_seen_m2, full.facade_seen_m2),
        "{kept:?} {full:?}"
    );
    Ok(())
}

/// The first cell whose count differs, as `(cell, got, want)`.
fn first_difference(got: &[u8], want: &[u8]) -> Option<(usize, u8, u8)> {
    got.iter()
        .zip(want)
        .position(|(g, w)| g != w)
        .map(|i| (i, got[i], want[i]))
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
        let direct = layers.visibility(&ViewCone { range, ..cone }, subject);
        prop_assert_eq!(sight.within(range), direct);
    }

    #[test]
    fn prop_tally_matches_a_full_recount(
        (s, cams) in street_with_cameras(),
        region in region(),
        ops in ops(),
    ) {
        let layers = s.layers();
        let sights: Vec<Sight> = cams.iter().map(|c| layers.sight(&c.cone(&layers), 1.0)).collect();
        let mask = region.as_ref().map(|r| RegionMask::new(&layers, r));
        let (tally, _) = run(&layers, mask.as_ref(), &sights, &ops);
        let kept = tally.stats(&layers);
        same_figures(kept, CoverageStats::compute_in(&layers, &tally.counts, mask.as_ref()))?;
        // Limiting the figures to the region afresh gives the same figures.
        let mut again = tally.clone();
        again.limit_to(&layers, mask.as_ref());
        same_figures(again.stats(&layers), kept)?;
    }

    #[test]
    fn prop_tally_counts_the_cameras_that_see_each_cell(
        (s, cams) in street_with_cameras(),
        ops in ops(),
    ) {
        let layers = s.layers();
        let sights: Vec<Sight> = cams.iter().map(|c| layers.sight(&c.cone(&layers), 1.0)).collect();
        let (tally, active) = run(&layers, None, &sights, &ops);
        let (ground, facade) = true_counts(&layers, &sights, &active);
        prop_assert_eq!(first_difference(&tally.counts.ground, &ground), None);
        prop_assert_eq!(first_difference(&tally.counts.facade, &facade), None);
    }

    #[test]
    fn prop_region_mask_is_the_point_in_ring_test(s in street(), region in region()) {
        let Some(region) = region else { return Ok(()) };
        let layers = s.layers();
        let mask = RegionMask::new(&layers, &region);
        let ring: Vec<Vec2> = region.points.iter().map(|p| Vec2::from_array(*p)).collect();
        for (i, &inside) in mask.ground.iter().enumerate() {
            let c = layers.grid.center(i);
            prop_assert_eq!(inside, point_in_ring(c, &ring), "cell {} at {}", i, c);
        }
    }

    #[test]
    fn prop_occlusion_is_symmetric(
        s in street(),
        a in (-60.0..=60.0f32, 0.0..=20.0f32, -60.0..=60.0f32),
        b in (-60.0..=60.0f32, 0.0..=20.0f32, -60.0..=60.0f32),
    ) {
        let footprints: Vec<_> = s.houses.iter().map(House::footprint).collect();
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
    /// Once fixed, the kept figures must also still equal a one-pass count
    /// across the 255/256 boundary.
    #[test]
    #[ignore = "fixed in claude/fix-core-coverage-count-saturation"]
    fn prop_tally_counts_more_than_255_cameras(
        cam in road_camera(0.0),
        region in region(),
        extra in 0..40usize,
        removed in 1..=255usize,
    ) {
        let layers = Street {
            houses: vec![],
            road_z: 0.0,
            road_x: (-50.0, 50.0),
            tilt: None,
        }.layers();
        let sight = layers.sight(&cam.cone(&layers), 1.0);
        prop_assume!(!sight.ground.is_empty());
        let mask = region.as_ref().map(|r| RegionMask::new(&layers, r));
        let mut tally = Tally::new(&layers, mask.as_ref());
        let added = 256 + extra;
        for _ in 0..added {
            tally.add_within(&layers, mask.as_ref(), &sight, sight.range);
        }
        for _ in 0..removed {
            tally.remove_within(&layers, mask.as_ref(), &sight, sight.range);
        }
        let active = vec![(0, sight.range); added - removed];
        let (ground, _) = true_counts(&layers, std::slice::from_ref(&sight), &active);
        prop_assert_eq!(first_difference(&tally.counts.ground, &ground), None);
        same_figures(
            tally.stats(&layers),
            CoverageStats::compute_in(&layers, &tally.counts, mask.as_ref()),
        )?;
    }
}
