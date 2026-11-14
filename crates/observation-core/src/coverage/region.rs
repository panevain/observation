//! A drawn region the figures can be limited to.

use super::{GroundGrid, StaticLayers};
use crate::util::planar::{point_in_ring, signed_area2};
use glam::Vec2;
use serde::{Deserialize, Serialize};

/// Most corners a [`Region`] may have.
pub const MAX_REGION_POINTS: usize = 256;

/// A user-drawn ground polygon the coverage figures can be limited to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    /// Corners in world `[x, z]`, metres, in drawing order (not repeated at the end).
    pub points: Vec<[f32; 2]>,
    /// Whether the figures are currently limited to the region.
    #[serde(default = "crate::util::default_true")]
    pub enabled: bool,
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
            Self::TooFewCorners => "A region needs at least three corners; click to add more",
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
        outline_problem(&self.points)
    }

    /// The figures should be limited to this region.
    pub fn is_active(&self) -> bool {
        self.enabled && self.problem().is_none()
    }

    /// Enclosed area of a simple outline (either winding), m².
    pub fn area_m2(&self) -> f32 {
        outline_area_m2(&self.points)
    }
}

/// Why a drawn outline (corners in world `[x, z]`, not repeated at the end)
/// cannot enclose an area, if it cannot. Regions, labelled areas and ground
/// targets share the rule.
pub fn outline_problem(points: &[[f32; 2]]) -> Option<RegionProblem> {
    if points.len() < 3 {
        Some(RegionProblem::TooFewCorners)
    } else if self_intersects(points) {
        // Checked first: a symmetric bow-tie also has a net area of zero.
        Some(RegionProblem::SelfIntersecting)
    } else if outline_area_m2(points) <= 0.01 {
        Some(RegionProblem::NoArea)
    } else {
        None
    }
}

/// Enclosed area of a simple outline (either winding), m².
pub fn outline_area_m2(points: &[[f32; 2]]) -> f32 {
    signed_area2(points).abs() / 2.0
}

/// Two edges that do not share a corner cross or touch.
fn self_intersects(points: &[[f32; 2]]) -> bool {
    let p: Vec<Vec2> = points.iter().map(|p| Vec2::from_array(*p)).collect();
    let n = p.len();
    let edge = |i: usize| (p[i], p[(i + 1) % n]);
    (0..n).any(|i| {
        // Edges i and j are adjacent when j == i + 1, or i == 0 and j == n - 1.
        (i + 2..n)
            .filter(|&j| !(i == 0 && j == n - 1))
            .any(|j| segments_cross(edge(i), edge(j)))
    })
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
    /// The cells inside the outline `points` (world `[x, z]`): ground cells
    /// by their centres, wall patches by the ground just in front of them.
    pub fn new(layers: &StaticLayers, points: &[[f32; 2]]) -> Self {
        let grid = layers.grid();
        let mut ground = vec![false; grid.len()];
        scan_inside(grid, points, |start, end| ground[start..end].fill(true));
        let ring: Vec<Vec2> = points.iter().map(|p| Vec2::from_array(*p)).collect();
        let facade = layers
            .facades()
            .iter()
            .map(|f| {
                let probe = f.center + f.normal * REGION_WALL_PROBE_M;
                point_in_ring(Vec2::new(probe.x, probe.z), &ring)
            })
            .collect();
        Self { ground, facade }
    }
}

/// Calls `inside(start, end)` for each run of ground cells, `start..end` in
/// grid index order, whose centres lie inside the outline `points`.
///
/// Scanline fill: on each row of cell centres, the cells between pairs of
/// edge crossings are inside (even-odd, like `point_in_ring`).
pub(crate) fn scan_inside(
    grid: &GroundGrid,
    points: &[[f32; 2]],
    mut inside: impl FnMut(usize, usize),
) {
    let ring: Vec<Vec2> = points.iter().map(|p| Vec2::from_array(*p)).collect();
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
        for pair in xs.as_chunks::<2>().0 {
            // Columns whose centre x lies in [pair[0], pair[1]).
            let col = |x: f32| {
                ((x - grid.min.x) / grid.cell_m - 0.5)
                    .ceil()
                    .clamp(0.0, grid.w as f32) as usize
            };
            let start = row as usize * grid.w as usize;
            let (a, b) = (col(pair[0]), col(pair[1]));
            if a < b {
                inside(start + a, start + b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::layers;
    use super::*;
    use crate::coverage::{CoverageCounts, CoverageStats};
    use crate::frustum::ViewCone;
    use glam::Vec3;

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
        let mask = RegionMask::new(&l, &west.points);
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
        let s = CoverageStats::compute_in(&l, &counts, Some(&RegionMask::new(&l, &house.points)));
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
        let mask = RegionMask::new(&l, &r.points);
        for (i, &inside) in mask.ground.iter().enumerate() {
            assert_eq!(inside, point_in_ring(l.grid().center(i), &ring), "cell {i}");
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
        assert!(RegionMask::new(&l, &house.points).facade.iter().all(|b| !b));
        // A lot one metre wider all round takes every wall.
        let lot = Region {
            points: vec![[-6.0, -26.0], [6.0, -26.0], [6.0, -14.0], [-6.0, -14.0]],
            enabled: true,
        };
        assert!(RegionMask::new(&l, &lot.points).facade.iter().all(|b| *b));
    }
}
