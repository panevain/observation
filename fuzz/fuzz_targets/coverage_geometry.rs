//! Coverage maths on arbitrary geometry and camera poses (including NaN,
//! infinities and degenerate polygons, as a hostile scene file or odd OSM
//! data could produce): occlusion, view cones and rasterisation must not
//! panic, and results must stay inside the grid.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use observation_core::building::Footprint;
use observation_core::bundle::Road;
use observation_core::coverage::{CoverageCounts, CoverageStats, GroundGrid, StaticLayers};
use observation_core::frustum::ViewCone;
use observation_core::glam::{Vec2, Vec3};

#[derive(Debug, Arbitrary)]
struct Input {
    rings: Vec<Vec<(f32, f32)>>,
    heights: (f32, f32),
    eye: (f32, f32, f32),
    angles: (f32, f32, f32),
    fov: (f32, f32),
    range: f32,
    subject_height: f32,
}

fuzz_target!(|input: Input| {
    // Keep each run cheap: a 40 m × 40 m grid at 1 m and a handful of rings.
    let footprints: Vec<Footprint> = input
        .rings
        .iter()
        .take(4)
        .map(|r| Footprint {
            outer: r.iter().take(16).map(|&(x, z)| [x, z]).collect(),
            holes: vec![],
            min_height_m: input.heights.0,
            height_m: input.heights.1,
        })
        .collect();
    let refs: Vec<&Footprint> = footprints.iter().collect();
    let road = Road {
        osm_id: 1,
        highway: "residential".into(),
        name: None,
        width_m: 6.0,
        points: vec![[-20.0, 0.0], [20.0, 0.0]],
    };
    let grid = GroundGrid::covering(Vec2::splat(-20.0), Vec2::splat(20.0), 1.0);
    let layers = StaticLayers::from_parts(grid, &refs, &[road], 1.0);

    let cone = ViewCone::from_angles(
        Vec3::new(input.eye.0, input.eye.1, input.eye.2),
        input.angles.0,
        input.angles.1,
        input.angles.2,
        input.fov.0.clamp(1.0, 170.0),
        input.fov.1.clamp(1.0, 170.0),
        input.range.clamp(0.0, 100.0),
    );
    let vis = layers.visibility(&cone, input.subject_height);
    assert!(vis.ground.iter().all(|&i| (i as usize) < layers.grid.len()));
    assert!(
        vis.facade
            .iter()
            .all(|&i| (i as usize) < layers.facades.len())
    );

    let mut counts = CoverageCounts::new(&layers);
    counts.add(&vis);
    let _ = CoverageStats::compute(&layers, &counts);
    counts.remove(&vis);
    assert!(counts.ground.iter().all(|&c| c == 0));
});
