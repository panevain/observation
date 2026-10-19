//! Occlusion-aware coverage on a ground raster and on building façades.
//!
//! Every ground cell / façade cell centre inside a camera's view cone is
//! tested for line of sight with [`Occluders::segment_blocked`]. The same
//! results drive the on-screen overlay and the statistics, so the two always agree.

use crate::building::{Footprint, clean_ring, signed_area2};
use crate::bundle::AreaBundle;
use crate::camera::{DoriLevel, Lighting};
use crate::frustum::ViewCone;
use crate::occlusion::{Occluders, point_in_ring};
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

/// Most corners a [`Region`] may have.
pub const MAX_REGION_POINTS: usize = 256;

/// A user-drawn ground polygon the coverage figures can be limited to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    /// Corners in world `[x, z]`, metres, in drawing order (not repeated at the end).
    pub points: Vec<[f32; 2]>,
    /// Whether the figures are currently limited to the region.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Why a drawn outline cannot be used as a [`Region`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionProblem {
    TooFewCorners,
    /// The corners (nearly) lie on one line.
    NoArea,
    /// Two edges cross, so "inside" is ambiguous.
    SelfIntersecting,
}

impl RegionProblem {
    pub fn message(self) -> &'static str {
        match self {
            Self::TooFewCorners => "A region needs at least three corners",
            Self::NoArea => "The corners lie on one line, so the region has no area",
            Self::SelfIntersecting => {
                "The outline crosses itself; place the corners in order around the area"
            }
        }
    }
}

/// Wall patches count as inside a region when the ground this far in front of
/// them is, so a region traced along a building's walls is unambiguous.
const REGION_WALL_PROBE_M: f32 = 0.5;

impl Region {
    /// Why the outline cannot enclose an area, if it cannot.
    pub fn problem(&self) -> Option<RegionProblem> {
        if self.points.len() < 3 {
            Some(RegionProblem::TooFewCorners)
        } else if self.self_intersects() {
            // Checked first: a symmetric bow-tie also has a net area of zero.
            Some(RegionProblem::SelfIntersecting)
        } else if self.area_m2() <= 0.01 {
            Some(RegionProblem::NoArea)
        } else {
            None
        }
    }

    /// The figures should be limited to this region.
    pub fn is_active(&self) -> bool {
        self.enabled && self.problem().is_none()
    }

    /// Enclosed area of a simple outline (either winding), m².
    pub fn area_m2(&self) -> f32 {
        signed_area2(&self.points).abs() / 2.0
    }

    /// Two edges that do not share a corner cross or touch.
    fn self_intersects(&self) -> bool {
        let p: Vec<Vec2> = self.points.iter().map(|p| Vec2::from_array(*p)).collect();
        let n = p.len();
        let edge = |i: usize| (p[i], p[(i + 1) % n]);
        (0..n).any(|i| {
            // Edges i and j are adjacent when j == i + 1, or i == 0 and j == n - 1.
            (i + 2..n)
                .filter(|&j| !(i == 0 && j == n - 1))
                .any(|j| segments_cross(edge(i), edge(j)))
        })
    }
}

/// Segments `a` and `b` share a point (touching counts).
fn segments_cross((a0, a1): (Vec2, Vec2), (b0, b1): (Vec2, Vec2)) -> bool {
    let side = |p: Vec2, q: Vec2, r: Vec2| (q - p).perp_dot(r - p);
    let (d1, d2) = (side(b0, b1, a0), side(b0, b1, a1));
    let (d3, d4) = (side(a0, a1, b0), side(a0, a1, b1));
    let on = |p: Vec2, q: Vec2, r: Vec2| r.cmpge(p.min(q)).all() && r.cmple(p.max(q)).all();
    ((d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0) && d1 != 0.0 && d2 != 0.0)
        || (d1 == 0.0 && on(b0, b1, a0))
        || (d2 == 0.0 && on(b0, b1, a1))
        || (d3 == 0.0 && on(a0, a1, b0))
        || (d4 == 0.0 && on(a0, a1, b1))
}

/// Which analysis cells lie inside a [`Region`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionMask {
    pub ground: Vec<bool>,
    pub facade: Vec<bool>,
}

impl RegionMask {
    pub fn new(layers: &StaticLayers, region: &Region) -> Self {
        let ring: Vec<Vec2> = region.points.iter().map(|p| Vec2::from_array(*p)).collect();
        let grid = &layers.grid;
        // Scanline fill: on each row of cell centres, the cells between pairs
        // of edge crossings are inside (even-odd, like `point_in_ring`).
        let mut ground = vec![false; grid.len()];
        let mut xs = Vec::new();
        for row in 0..grid.h {
            let z = grid.min.y + (row as f32 + 0.5) * grid.cell_m;
            xs.clear();
            for (i, a) in ring.iter().enumerate() {
                let b = ring[(i + 1) % ring.len()];
                if (a.y > z) != (b.y > z) {
                    xs.push(a.x + (z - a.y) * (b.x - a.x) / (b.y - a.y));
                }
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.chunks_exact(2) {
                // Columns whose centre x lies in [pair[0], pair[1]).
                let col = |x: f32| {
                    ((x - grid.min.x) / grid.cell_m - 0.5)
                        .ceil()
                        .clamp(0.0, grid.w as f32) as usize
                };
                let start = row as usize * grid.w as usize;
                ground[start + col(pair[0])..start + col(pair[1])].fill(true);
            }
        }
        let facade = layers
            .facades
            .iter()
            .map(|f| {
                let probe = f.center + f.normal * REGION_WALL_PROBE_M;
                point_in_ring(Vec2::new(probe.x, probe.z), &ring)
            })
            .collect();
        Self { ground, facade }
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

/// Caps on patches per wall: real walls are far smaller, and absurd input
/// (bad data, fuzzing) gets coarser patches instead of unbounded allocation.
const MAX_FACADE_COLS: f32 = 256.0;
/// See `MAX_FACADE_COLS`.
const MAX_FACADE_ROWS: f32 = 128.0;

/// Splits every wall into roughly `cell_m`-sized patches, at most 256 along and
/// 128 up a wall (longer or taller walls get coarser patches).
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
    p.distance(closest_on_segment(p, a, b))
}

/// The point of segment `a`–`b` closest to `p`.
pub fn closest_on_segment(p: Vec2, a: Vec2, b: Vec2) -> Vec2 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-12 {
        0.0
    } else {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    };
    a + ab * t
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
        Self::compute_in(layers, counts, None)
    }

    /// Statistics over the cells inside `region` only (all cells if `None`).
    pub fn compute_in(
        layers: &StaticLayers,
        counts: &CoverageCounts,
        region: Option<&RegionMask>,
    ) -> Self {
        let a = layers.grid.cell_area_m2();
        let mut s = Self::default();
        for (i, &c) in counts.ground.iter().enumerate() {
            if region.is_some_and(|r| !r.ground[i]) {
                continue;
            }
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
        for (i, (f, &c)) in layers.facades.iter().zip(&counts.facade).enumerate() {
            if region.is_some_and(|r| !r.facade[i]) {
                continue;
            }
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
    fn region_limits_the_figures() {
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
        let mut counts = CoverageCounts::new(&l);
        counts.add(&l.visibility(&a, 1.0));
        let all = CoverageStats::compute(&l, &counts);

        // The road west of x = 0 (the camera looks east from x = -20, so it
        // sees part of the road in the east half only).
        let west = Region {
            points: vec![[-50.0, -5.0], [0.0, -5.0], [0.0, 5.0], [-50.0, 5.0]],
            enabled: true,
        };
        assert!(west.is_active());
        assert_eq!(west.area_m2(), 500.0);
        let mask = RegionMask::new(&l, &west);
        let s = CoverageStats::compute_in(&l, &counts, Some(&mask));
        assert!(
            (s.street_m2 - all.street_m2 / 2.0).abs() < 10.0,
            "half the road: {} of {}",
            s.street_m2,
            all.street_m2
        );
        assert!(s.street_seen_m2 < all.street_seen_m2);
        // No walls inside a box over the road.
        assert_eq!(s.facade_m2, 0.0);

        // Around the house: all its walls, none of the road.
        let house = Region {
            points: vec![[-10.0, -30.0], [10.0, -30.0], [10.0, -10.0], [-10.0, -10.0]],
            enabled: true,
        };
        let s = CoverageStats::compute_in(&l, &counts, Some(&RegionMask::new(&l, &house)));
        assert_eq!(s.street_m2, 0.0);
        assert!((s.facade_m2 - all.facade_m2).abs() < 1e-3);

        let line = Region {
            points: vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]],
            enabled: true,
        };
        assert_eq!(line.problem(), Some(RegionProblem::NoArea));
        let bow_tie = Region {
            points: vec![[0.0, 0.0], [10.0, 10.0], [10.0, 0.0], [0.0, 10.0]],
            enabled: true,
        };
        assert_eq!(bow_tie.problem(), Some(RegionProblem::SelfIntersecting));
        let two = Region {
            points: vec![[0.0, 0.0], [1.0, 0.0]],
            enabled: true,
        };
        assert_eq!(two.problem(), Some(RegionProblem::TooFewCorners));
        assert!(
            !Region {
                enabled: false,
                ..west
            }
            .is_active()
        );
    }

    #[test]
    fn region_mask_scanline_matches_point_test() {
        let l = layers();
        // A concave "L" with a corner on cell boundaries and slanted edges.
        let r = Region {
            points: vec![
                [-30.25, -40.0],
                [20.0, -35.5],
                [22.0, 10.0],
                [0.0, 10.0],
                [-3.0, -5.0],
                [-31.0, 0.0],
            ],
            enabled: true,
        };
        assert_eq!(r.problem(), None);
        let ring: Vec<Vec2> = r.points.iter().map(|p| Vec2::from_array(*p)).collect();
        let mask = RegionMask::new(&l, &r);
        for (i, &inside) in mask.ground.iter().enumerate() {
            assert_eq!(inside, point_in_ring(l.grid.center(i), &ring), "cell {i}");
        }
        assert!(mask.ground.iter().any(|b| *b));
    }

    #[test]
    fn walls_traced_by_a_region_count_by_the_ground_in_front() {
        let l = layers();
        // The house's own outline (x -5..5, z -25..-15): every wall patch faces
        // outward, so the ground in front of it is outside and none count.
        let house = Region {
            points: vec![[-5.0, -25.0], [5.0, -25.0], [5.0, -15.0], [-5.0, -15.0]],
            enabled: true,
        };
        assert!(RegionMask::new(&l, &house).facade.iter().all(|b| !b));
        // A lot one metre wider all round takes every wall.
        let lot = Region {
            points: vec![[-6.0, -26.0], [6.0, -26.0], [6.0, -14.0], [-6.0, -14.0]],
            enabled: true,
        };
        assert!(RegionMask::new(&l, &lot).facade.iter().all(|b| *b));
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
