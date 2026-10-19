//! Occlusion-aware coverage on a ground raster and on building façades.
//!
//! Every ground cell / façade cell centre inside a camera's view cone is
//! tested for line of sight with [`Occluders::segment_blocked`]. The same
//! results drive the on-screen overlay and the statistics, so the two always agree.

use crate::building::{Footprint, clean_ring, signed_area2};
use crate::bundle::AreaBundle;
use crate::camera::{DoriLevel, Lighting};
use crate::frustum::ViewCone;
use crate::occlusion::Occluders;
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// How far façade sample points sit in front of the wall, metres.
const FACADE_OFFSET_M: f32 = 0.05;

/// User-adjustable analysis settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CoverageParams {
    pub dori: DoriLevel,
    pub lighting: Lighting,
    /// Height above ground at which people are tested, metres
    /// (0 = ground, ~1.0 = torso, ~1.6 = face).
    pub subject_height_m: f32,
}

impl Default for CoverageParams {
    fn default() -> Self {
        Self {
            dori: DoriLevel::Detect,
            lighting: Lighting::Day,
            subject_height_m: 1.0,
        }
    }
}

/// Regular raster over the ground plane, row-major with rows along `+Z` (south).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroundGrid {
    /// North-west (min x, min z) corner, metres.
    pub min: Vec2,
    pub cell_m: f32,
    pub w: u32,
    pub h: u32,
}

impl GroundGrid {
    pub fn covering(min: Vec2, max: Vec2, cell_m: f32) -> Self {
        let size = (max - min).max(Vec2::splat(cell_m));
        Self {
            min,
            cell_m,
            w: (size.x / cell_m).ceil() as u32,
            h: (size.y / cell_m).ceil() as u32,
        }
    }

    pub fn len(&self) -> usize {
        self.w as usize * self.h as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn max(&self) -> Vec2 {
        self.min + Vec2::new(self.w as f32, self.h as f32) * self.cell_m
    }

    pub fn center(&self, i: usize) -> Vec2 {
        let (x, z) = ((i % self.w as usize) as f32, (i / self.w as usize) as f32);
        self.min + (Vec2::new(x, z) + 0.5) * self.cell_m
    }

    pub fn cell_at(&self, p: Vec2) -> Option<usize> {
        let c = ((p - self.min) / self.cell_m).floor();
        (c.x >= 0.0 && c.y >= 0.0 && (c.x as u32) < self.w && (c.y as u32) < self.h)
            .then(|| c.y as usize * self.w as usize + c.x as usize)
    }

    pub fn cell_area_m2(&self) -> f32 {
        self.cell_m * self.cell_m
    }

    /// Inclusive cell index ranges covering an `x, z` rectangle (clamped).
    fn range(&self, lo: Vec2, hi: Vec2) -> Option<(u32, u32, u32, u32)> {
        let a = ((lo - self.min) / self.cell_m).floor();
        let b = ((hi - self.min) / self.cell_m).floor();
        if b.x < 0.0 || b.y < 0.0 || a.x >= self.w as f32 || a.y >= self.h as f32 {
            return None;
        }
        let cl = |v: f32, n: u32| (v.max(0.0) as u32).min(n - 1);
        Some((
            cl(a.x, self.w),
            cl(b.x, self.w),
            cl(a.y, self.h),
            cl(b.y, self.h),
        ))
    }
}

/// One rectangular patch of wall.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FacadeCell {
    pub center: Vec3,
    pub normal: Vec3,
    /// Bottom-left, bottom-right, top-right, top-left (seen from outside).
    pub corners: [Vec3; 4],
}

impl FacadeCell {
    pub fn area_m2(&self) -> f32 {
        (self.corners[1] - self.corners[0]).length() * (self.corners[3] - self.corners[0]).length()
    }
}

/// Splits every wall into roughly `cell_m`-sized patches.
/// Caps on patches per wall: real walls are far smaller, and absurd input
/// (bad data, fuzzing) gets coarser patches instead of unbounded allocation.
const MAX_FACADE_COLS: f32 = 256.0;
const MAX_FACADE_ROWS: f32 = 128.0;

pub fn facade_cells<'a>(
    footprints: impl IntoIterator<Item = &'a Footprint>,
    cell_m: f32,
) -> Vec<FacadeCell> {
    let mut out = Vec::new();
    for fp in footprints {
        let (y0, y1) = (fp.min_height_m, fp.height_m);
        if !(y1 - y0).is_finite() {
            continue;
        }
        let rows = ((y1 - y0) / cell_m).ceil().clamp(1.0, MAX_FACADE_ROWS) as usize;
        let rings = std::iter::once((clean_ring(&fp.outer), false))
            .chain(fp.holes.iter().map(|h| (clean_ring(h), true)));
        for (ring, is_hole) in rings {
            if ring.len() < 3 {
                continue;
            }
            let sign = signed_area2(&ring).signum() * if is_hole { -1.0 } else { 1.0 };
            for i in 0..ring.len() {
                let (a, b) = (
                    Vec2::from_array(ring[i]),
                    Vec2::from_array(ring[(i + 1) % ring.len()]),
                );
                let d = b - a;
                let len = d.length();
                if !len.is_finite() || len < 0.2 {
                    continue;
                }
                let n2 = Vec2::new(d.y, -d.x) * (sign / len);
                let normal = Vec3::new(n2.x, 0.0, n2.y);
                let cols = (len / cell_m).ceil().clamp(1.0, MAX_FACADE_COLS) as usize;
                // Orient corners so that (bl → br) runs left-to-right seen from outside.
                for c in 0..cols {
                    let (s0, s1) = (c as f32 / cols as f32, (c + 1) as f32 / cols as f32);
                    let (p0, p1) = (a + d * s0, a + d * s1);
                    for r in 0..rows {
                        let ya = y0 + (y1 - y0) * r as f32 / rows as f32;
                        let yb = y0 + (y1 - y0) * (r + 1) as f32 / rows as f32;
                        let mut corners = [
                            Vec3::new(p0.x, ya, p0.y),
                            Vec3::new(p1.x, ya, p1.y),
                            Vec3::new(p1.x, yb, p1.y),
                            Vec3::new(p0.x, yb, p0.y),
                        ];
                        let face = (corners[1] - corners[0]).cross(corners[3] - corners[0]);
                        if face.dot(normal) < 0.0 {
                            corners.swap(0, 1);
                            corners.swap(2, 3);
                        }
                        let center = (corners[0] + corners[2]) * 0.5;
                        out.push(FacadeCell {
                            center,
                            normal,
                            corners,
                        });
                    }
                }
            }
        }
    }
    out
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
}

impl StaticLayers {
    pub fn new(bundle: &AreaBundle, ground_cell_m: f32, facade_cell_m: f32) -> Self {
        let proj = bundle.projection();
        let corner = |lat, lon| {
            Vec2::from_array(
                proj.to_world(crate::geo::LatLon::new(lat, lon))
                    .map(|v| v as f32),
            )
        };
        let nw = corner(bundle.bbox.north, bundle.bbox.west);
        let se = corner(bundle.bbox.south, bundle.bbox.east);
        let grid = GroundGrid::covering(nw.min(se), nw.max(se), ground_cell_m);
        let footprints: Vec<&Footprint> = bundle.footprints().collect();
        Self::from_parts(grid, &footprints, &bundle.roads, facade_cell_m)
    }

    pub fn from_parts(
        grid: GroundGrid,
        footprints: &[&Footprint],
        roads: &[crate::bundle::Road],
        facade_cell_m: f32,
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
        let facades = facade_cells(footprints.iter().copied(), facade_cell_m);
        Self {
            grid,
            occluders,
            building_mask,
            street_mask,
            facades,
        }
    }

    /// Ground and façade cells a single camera can see.
    pub fn visibility(&self, cone: &ViewCone, subject_height_m: f32) -> Visibility {
        let mut ground = Vec::new();
        let (lo, hi) = cone.xz_bounds();
        if let Some((x0, x1, z0, z1)) = self.grid.range(Vec2::from_array(lo), Vec2::from_array(hi))
        {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    let i = (z * self.grid.w + x) as usize;
                    if self.building_mask[i] {
                        continue;
                    }
                    let c = self.grid.center(i);
                    let p = Vec3::new(c.x, subject_height_m, c.y);
                    if cone.contains(p) && !self.occluders.segment_blocked(cone.eye, p) {
                        ground.push(i as u32);
                    }
                }
            }
        }
        let r2 = cone.range * cone.range;
        let facade = self
            .facades
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                let to_eye = cone.eye - f.center;
                to_eye.length_squared() <= r2 && to_eye.dot(f.normal) > 0.0
            })
            .filter(|(_, f)| {
                let p = f.center + f.normal * FACADE_OFFSET_M;
                cone.contains(p) && !self.occluders.segment_blocked(cone.eye, p)
            })
            .map(|(i, _)| i as u32)
            .collect();
        Visibility { ground, facade }
    }
}

fn dist_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-12 {
        0.0
    } else {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    };
    p.distance(a + ab * t)
}

/// Cells seen by one camera (sorted, unique indices).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Visibility {
    pub ground: Vec<u32>,
    pub facade: Vec<u32>,
}

/// Per-cell count of cameras that see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageCounts {
    pub ground: Vec<u8>,
    pub facade: Vec<u8>,
}

impl CoverageCounts {
    pub fn new(layers: &StaticLayers) -> Self {
        Self {
            ground: vec![0; layers.grid.len()],
            facade: vec![0; layers.facades.len()],
        }
    }

    pub fn add(&mut self, v: &Visibility) {
        for &i in &v.ground {
            self.ground[i as usize] = self.ground[i as usize].saturating_add(1);
        }
        for &i in &v.facade {
            self.facade[i as usize] = self.facade[i as usize].saturating_add(1);
        }
    }

    pub fn remove(&mut self, v: &Visibility) {
        for &i in &v.ground {
            self.ground[i as usize] = self.ground[i as usize].saturating_sub(1);
        }
        for &i in &v.facade {
            self.facade[i as usize] = self.facade[i as usize].saturating_sub(1);
        }
    }
}

/// Summary numbers shown in the UI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CoverageStats {
    pub street_m2: f32,
    pub street_seen_m2: f32,
    pub street_seen_multi_m2: f32,
    pub open_ground_seen_m2: f32,
    pub facade_m2: f32,
    pub facade_seen_m2: f32,
}

impl CoverageStats {
    pub fn compute(layers: &StaticLayers, counts: &CoverageCounts) -> Self {
        let a = layers.grid.cell_area_m2();
        let mut s = Self::default();
        for (i, &c) in counts.ground.iter().enumerate() {
            if layers.street_mask[i] {
                s.street_m2 += a;
                if c >= 1 {
                    s.street_seen_m2 += a;
                }
                if c >= 2 {
                    s.street_seen_multi_m2 += a;
                }
            }
            if c >= 1 {
                s.open_ground_seen_m2 += a;
            }
        }
        for (f, &c) in layers.facades.iter().zip(&counts.facade) {
            let fa = f.area_m2();
            s.facade_m2 += fa;
            if c >= 1 {
                s.facade_seen_m2 += fa;
            }
        }
        s
    }

    /// Share of the street surface seen by at least one camera, `0..=1`.
    pub fn street_fraction(&self) -> f32 {
        if self.street_m2 > 0.0 {
            self.street_seen_m2 / self.street_m2
        } else {
            0.0
        }
    }

    pub fn street_multi_fraction(&self) -> f32 {
        if self.street_m2 > 0.0 {
            self.street_seen_multi_m2 / self.street_m2
        } else {
            0.0
        }
    }

    pub fn facade_fraction(&self) -> f32 {
        if self.facade_m2 > 0.0 {
            self.facade_seen_m2 / self.facade_m2
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::Road;

    fn layers() -> StaticLayers {
        // A 10×10 m, 6 m tall house centred at x=0, z=-20; a road along z=0.
        let house = Footprint {
            outer: vec![[-5.0, -25.0], [5.0, -25.0], [5.0, -15.0], [-5.0, -15.0]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.0,
        };
        let road = Road {
            osm_id: 1,
            highway: "residential".into(),
            name: None,
            width_m: 6.0,
            points: vec![[-50.0, 0.0], [50.0, 0.0]],
        };
        let grid = GroundGrid::covering(Vec2::new(-50.0, -50.0), Vec2::new(50.0, 50.0), 1.0);
        StaticLayers::from_parts(grid, &[&house], &[road], 1.0)
    }

    #[test]
    fn absurd_walls_have_bounded_patches() {
        // Found by the coverage_geometry fuzz target: a 4e18 m wall tried to
        // allocate gigabytes of 1 m façade patches.
        let fp = Footprint {
            outer: vec![[0.0, 0.0], [4.0e18, 0.0], [0.0, 1.0]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 1.0e30,
        };
        let cells = facade_cells([&fp], 1.0);
        assert!(cells.len() <= 3 * (MAX_FACADE_COLS * MAX_FACADE_ROWS) as usize);
        let nan = Footprint {
            height_m: f32::NAN,
            ..fp
        };
        assert!(facade_cells([&nan], 1.0).is_empty());
    }

    #[test]
    fn grid_indexing() {
        let g = GroundGrid::covering(Vec2::new(-10.0, -10.0), Vec2::new(10.0, 10.0), 0.5);
        assert_eq!((g.w, g.h), (40, 40));
        let i = g.cell_at(Vec2::new(0.1, 0.1)).unwrap();
        assert!(g.center(i).distance(Vec2::new(0.25, 0.25)) < 1e-5);
        assert_eq!(g.cell_at(Vec2::new(10.5, 0.0)), None);
    }

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

    #[test]
    fn counts_and_stats() {
        let l = layers();
        let a = ViewCone::from_angles(
            Vec3::new(-20.0, 2.5, 10.0),
            90.0,
            -10.0,
            0.0,
            100.0,
            60.0,
            30.0,
        );
        let b = ViewCone::from_angles(
            Vec3::new(20.0, 2.5, 10.0),
            270.0,
            -10.0,
            0.0,
            100.0,
            60.0,
            30.0,
        );
        let (va, vb) = (l.visibility(&a, 1.0), l.visibility(&b, 1.0));
        let mut counts = CoverageCounts::new(&l);
        counts.add(&va);
        counts.add(&vb);
        let s = CoverageStats::compute(&l, &counts);
        assert!(
            s.street_fraction() > 0.15 && s.street_fraction() <= 1.0,
            "{s:?}"
        );
        assert!(s.street_multi_fraction() > 0.0 && s.street_multi_fraction() < s.street_fraction());
        counts.remove(&vb);
        let s2 = CoverageStats::compute(&l, &counts);
        assert!(s2.street_seen_m2 < s.street_seen_m2);
        counts.remove(&va);
        assert!(counts.ground.iter().all(|c| *c == 0));
    }
}
