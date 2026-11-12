//! The camera-independent layers of an area.

use super::facade::facade_cells;
use super::{FacadeCell, GroundGrid};
use crate::building::Footprint;
use crate::bundle::AreaBundle;
use crate::occlusion::Occluders;
use crate::road::Road;
use crate::terrain::Terrain;
use crate::util::planar::dist_to_segment;
use glam::{Vec2, Vec3};

/// The geometry the analysis layers are built from: an [`AreaBundle`], or
/// the [`LayerInputs`](crate::coverage_job::LayerInputs) the coverage worker
/// receives.
pub trait AreaGeometry {
    /// Building footprints. Their order numbers the façade cells, so it must
    /// be the same wherever an area's layers are built.
    fn footprints(&self) -> impl Iterator<Item = &Footprint>;
    fn roads(&self) -> &[Road];
    fn terrain(&self) -> &Terrain;
}

impl AreaGeometry for AreaBundle {
    /// In bundle order ([`AreaBundle::footprints`]).
    fn footprints(&self) -> impl Iterator<Item = &Footprint> {
        AreaBundle::footprints(self)
    }

    fn roads(&self) -> &[Road] {
        &self.roads
    }

    fn terrain(&self) -> &Terrain {
        &self.terrain
    }
}

/// Pre-computed, camera-independent analysis layers for a loaded area.
#[derive(Debug, Clone)]
pub struct StaticLayers {
    pub grid: GroundGrid,
    pub occluders: Occluders,
    /// Ground cells inside a building footprint.
    pub building_mask: Vec<bool>,
    /// Ground cells on a road surface (and not under a building).
    pub street_mask: Vec<bool>,
    pub facades: Vec<FacadeCell>,
    /// Private, so it cannot change under [`Self::terrain_top`].
    terrain: Terrain,
    /// [`Terrain::top`], for the sight-line tests.
    terrain_top: f32,
}

impl StaticLayers {
    pub fn new(bundle: &AreaBundle, ground_cell_m: f32, facade_cell_m: f32) -> Self {
        Self::from_area(Self::grid_for(bundle, ground_cell_m), bundle, facade_cell_m)
    }

    /// The ground grid covering a bundle's bounding box.
    pub fn grid_for(bundle: &AreaBundle, ground_cell_m: f32) -> GroundGrid {
        let (min, max) = bundle.projection().world_extent(&bundle.bbox);
        GroundGrid::covering(min, max, ground_cell_m)
    }

    /// The layers of an area's geometry on `grid`: [`Self::from_parts`] with
    /// the area's footprints (in its order), roads and terrain.
    pub fn from_area(grid: GroundGrid, area: &impl AreaGeometry, facade_cell_m: f32) -> Self {
        let footprints: Vec<&Footprint> = area.footprints().collect();
        Self::from_parts(
            grid,
            &footprints,
            area.roads(),
            facade_cell_m,
            area.terrain().clone(),
        )
    }

    pub fn from_parts(
        grid: GroundGrid,
        footprints: &[&Footprint],
        roads: &[Road],
        facade_cell_m: f32,
        terrain: Terrain,
    ) -> Self {
        let occluders = Occluders::new(footprints.iter().copied());
        let building_mask: Vec<bool> = (0..grid.len())
            .map(|i| occluders.inside_any(grid.center(i)))
            .collect();
        let mut street_mask = vec![false; grid.len()];
        for road in roads {
            let half = road.width_m / 2.0;
            for seg in road.points.windows(2) {
                let (a, b) = (Vec2::from_array(seg[0]), Vec2::from_array(seg[1]));
                let Some((x0, x1, z0, z1)) = grid.range(a.min(b) - half, a.max(b) + half) else {
                    continue;
                };
                for z in z0..=z1 {
                    for x in x0..=x1 {
                        let i = (z * grid.w + x) as usize;
                        if !building_mask[i] && dist_to_segment(grid.center(i), a, b) <= half {
                            street_mask[i] = true;
                        }
                    }
                }
            }
        }
        let facades = facade_cells(footprints.iter().copied(), facade_cell_m, &terrain);
        Self {
            grid,
            occluders,
            building_mask,
            street_mask,
            facades,
            terrain_top: terrain.top(),
            terrain,
        }
    }

    /// The ground the layers stand on.
    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }

    /// Whether the line from `a` to `b` is blocked by a building or the ground.
    pub fn segment_blocked(&self, a: Vec3, b: Vec3) -> bool {
        self.occluders.segment_blocked(a, b) || self.terrain.segment_blocked(a, b, self.terrain_top)
    }

    /// World point of a ground cell's subject, `subject_height_m` above the
    /// ground.
    pub fn ground_subject(&self, i: usize, subject_height_m: f32) -> Vec3 {
        let c = self.grid.center(i);
        Vec3::new(c.x, self.terrain.height_at(c) + subject_height_m, c.y)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::layers;
    use super::*;

    #[test]
    fn masks() {
        let l = layers();
        let street = l.street_mask.iter().filter(|b| **b).count();
        assert!(
            (550..=650).contains(&street),
            "≈100 m × 6 m of road, got {street}"
        );
        assert_eq!(l.building_mask.iter().filter(|b| **b).count(), 100);
        // 4 walls × 10 cols × 6 rows
        assert_eq!(l.facades.len(), 240);
        assert!(
            l.facades
                .iter()
                .all(|f| f.normal.dot(f.center - Vec3::new(0.0, 3.0, -20.0)) > 0.0)
        );
    }
}
