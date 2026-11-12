//! Occlusion-aware coverage on a ground raster and on building façades.
//!
//! Every ground cell / façade cell centre inside a camera's view cone is
//! tested for line of sight with
//! [`Occluders::segment_blocked`](crate::occlusion::Occluders::segment_blocked)
//! and against the ground with
//! [`Terrain::segment_blocked`](crate::terrain::Terrain::segment_blocked). The
//! same results drive the on-screen overlay and the statistics, so the two
//! always agree.

mod facade;
mod grid;
mod layers;
mod params;
mod region;
mod sight;
mod stats;
mod tally;

pub use crate::util::planar::closest_on_segment;
pub use facade::{FacadeCell, facade_cells};
pub use grid::GroundGrid;
pub use layers::{AreaGeometry, StaticLayers};
pub use params::CoverageParams;
pub use region::{MAX_REGION_POINTS, Region, RegionMask, RegionProblem};
pub use sight::{Sight, Visibility};
pub use stats::CoverageStats;
pub use tally::{CoverageCounts, Tally};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::road::Road;
    use crate::terrain::Terrain;
    use crate::testing::box_footprint;
    use glam::Vec2;

    pub(super) fn layers() -> StaticLayers {
        // A 10×10 m, 6 m tall house centred at x=0, z=-20; a road along z=0.
        let house = box_footprint(-5.0, -25.0, 5.0, -15.0, 6.0);
        let road = Road {
            osm_id: 1,
            highway: "residential".into(),
            name: None,
            width_m: 6.0,
            points: vec![[-50.0, 0.0], [50.0, 0.0]],
        };
        let grid = GroundGrid::covering(Vec2::new(-50.0, -50.0), Vec2::new(50.0, 50.0), 1.0);
        StaticLayers::from_parts(grid, &[&house], &[road], 1.0, Terrain::default())
    }
}
