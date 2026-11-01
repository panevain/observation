//! Exact line-of-sight tests against extruded (flat-roofed) building prisms,
//! accelerated by a uniform 2D grid.

use crate::building::Footprint;
use glam::{Vec2, Vec3};

/// Preferred grid cell size, metres.
const CELL_M: f32 = 8.0;
/// Upper bound on grid cells per axis; larger extents use bigger cells.
const MAX_CELLS_PER_AXIS: f32 = 512.0;

#[derive(Debug, Clone)]
struct Prism {
    rings: Vec<Vec<Vec2>>, // [0] = outer, rest = holes
    min_y: f32,
    max_y: f32,
}

impl Prism {
    fn contains_xz(&self, p: Vec2) -> bool {
        let mut inside = false;
        for ring in &self.rings {
            if point_in_ring(p, ring) {
                inside = !inside;
            }
        }
        inside
    }

    /// Whether segment `a → b` passes through the solid.
    fn blocks(&self, a: Vec3, b: Vec3) -> bool {
        let (a2, b2) = (Vec2::new(a.x, a.z), Vec2::new(b.x, b.z));
        let d = b2 - a2;
        // Parameter values where the 2D segment crosses a ring edge.
        let mut ts: Vec<f32> = vec![0.0, 1.0];
        for ring in &self.rings {
            for i in 0..ring.len() {
                let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
                if let Some(t) = segment_param(a2, d, p, q) {
                    ts.push(t);
                }
            }
        }
        ts.sort_by(f32::total_cmp);
        for w in ts.windows(2) {
            let (t0, t1) = (w[0], w[1]);
            if t1 - t0 < 1e-6 && d.length_squared() > 1e-12 {
                continue;
            }
            let mid = a2 + d * ((t0 + t1) * 0.5);
            if self.contains_xz(mid) {
                let (y0, y1) = (a.y + (b.y - a.y) * t0, a.y + (b.y - a.y) * t1);
                if y0.max(y1) >= self.min_y && y0.min(y1) <= self.max_y {
                    return true;
                }
            }
        }
        false
    }
}

/// Crossing-number test; the ring may or may not repeat its first point.
/// Takes `Vec2` rings or bundle `[f32; 2]` rings.
pub fn point_in_ring<P: Copy + Into<Vec2>>(p: Vec2, ring: &[P]) -> bool {
    let mut inside = false;
    let n = ring.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b): (Vec2, Vec2) = (ring[i].into(), ring[j].into());
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Parameter `t ∈ [0,1]` along `a + t·d` where it crosses segment `p–q`.
fn segment_param(a: Vec2, d: Vec2, p: Vec2, q: Vec2) -> Option<f32> {
    let e = q - p;
    let denom = d.perp_dot(e);
    if denom.abs() < 1e-9 {
        return None;
    }
    let w = p - a;
    let t = w.perp_dot(e) / denom;
    let u = w.perp_dot(d) / denom;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(t)
}

/// Spatial index over building prisms.
#[derive(Debug, Clone)]
pub struct Occluders {
    prisms: Vec<Prism>,
    origin: Vec2,
    cell: f32,
    w: i32,
    h: i32,
    cells: Vec<Vec<u32>>,
}

impl Occluders {
    pub fn new<'a>(footprints: impl IntoIterator<Item = &'a Footprint>) -> Self {
        let prisms: Vec<Prism> = footprints
            .into_iter()
            .filter(|f| f.outer.len() >= 3)
            // Non-finite input can't form a solid; skip it rather than poison the grid.
            .filter(|f| {
                f.outer
                    .iter()
                    .chain(f.holes.iter().flatten())
                    .flatten()
                    .all(|v| v.is_finite())
                    && f.bottom_y().is_finite()
                    && f.roof_y().is_finite()
            })
            .map(|f| Prism {
                rings: std::iter::once(&f.outer)
                    .chain(f.holes.iter())
                    .map(|r| r.iter().map(|p| Vec2::new(p[0], p[1])).collect())
                    .collect(),
                min_y: f.bottom_y(),
                max_y: f.roof_y(),
            })
            .collect();
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for p in prisms.iter().flat_map(|p| p.rings[0].iter()) {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        if prisms.is_empty() {
            (lo, hi) = (Vec2::ZERO, Vec2::ONE);
        }
        // Bound the grid size for absurd extents (bad data) by growing the cells.
        let extent = (hi - lo).max_element().min(f32::MAX);
        let cell = CELL_M.max(extent / MAX_CELLS_PER_AXIS);
        let dims =
            |len: f32| ((len / cell).floor() + 1.0).clamp(1.0, MAX_CELLS_PER_AXIS + 1.0) as i32;
        let (w, h) = (dims(hi.x - lo.x), dims(hi.y - lo.y));
        let mut cells = vec![Vec::new(); (w * h) as usize];
        for (i, p) in prisms.iter().enumerate() {
            let (mut plo, mut phi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
            for q in &p.rings[0] {
                plo = plo.min(*q);
                phi = phi.max(*q);
            }
            let c0 = ((plo - lo) / cell).floor().as_ivec2();
            let c1 = ((phi - lo) / cell).floor().as_ivec2();
            for cz in c0.y.max(0)..=c1.y.min(h - 1) {
                for cx in c0.x.max(0)..=c1.x.min(w - 1) {
                    cells[(cz * w + cx) as usize].push(i as u32);
                }
            }
        }
        Self {
            prisms,
            origin: lo,
            cell,
            w,
            h,
            cells,
        }
    }

    pub fn len(&self) -> usize {
        self.prisms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prisms.is_empty()
    }

    /// Whether `xz` is inside any building footprint.
    pub fn inside_any(&self, xz: Vec2) -> bool {
        let c = ((xz - self.origin) / self.cell).floor().as_ivec2();
        if c.x < 0 || c.y < 0 || c.x >= self.w || c.y >= self.h {
            return false;
        }
        self.cells[(c.y * self.w + c.x) as usize]
            .iter()
            .any(|&i| self.prisms[i as usize].contains_xz(xz))
    }

    /// Whether the straight line from `a` to `b` is blocked by any building.
    pub fn segment_blocked(&self, a: Vec3, b: Vec3) -> bool {
        let mut last = u32::MAX;
        let mut hit = false;
        self.walk_cells(Vec2::new(a.x, a.z), Vec2::new(b.x, b.z), |cell| {
            for &i in &self.cells[cell] {
                if i != last && self.prisms[i as usize].blocks(a, b) {
                    hit = true;
                    return false;
                }
                last = i;
            }
            true
        });
        hit
    }

    /// Amanatides–Woo traversal of grid cells touched by segment `a → b`.
    fn walk_cells(&self, a: Vec2, b: Vec2, mut visit: impl FnMut(usize) -> bool) {
        let (pa, pb) = ((a - self.origin) / self.cell, (b - self.origin) / self.cell);
        let d = pb - pa;
        // Clip the segment to the grid rectangle.
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        for (p, dd, max) in [(pa.x, d.x, self.w as f32), (pa.y, d.y, self.h as f32)] {
            if dd.abs() < 1e-12 {
                if p < 0.0 || p >= max {
                    return;
                }
            } else {
                let (mut ta, mut tb) = ((0.0 - p) / dd, (max - p) / dd);
                if ta > tb {
                    std::mem::swap(&mut ta, &mut tb);
                }
                t0 = t0.max(ta);
                t1 = t1.min(tb);
            }
        }
        if t0 > t1 {
            return;
        }
        let start = pa + d * t0;
        let end = pa + d * t1;
        let clampc = |v: f32, max: i32| (v.floor() as i32).clamp(0, max - 1);
        let (mut cx, mut cz) = (clampc(start.x, self.w), clampc(start.y, self.h));
        let (ex, ez) = (clampc(end.x, self.w), clampc(end.y, self.h));
        let step_x = if d.x > 0.0 { 1 } else { -1 };
        let step_z = if d.y > 0.0 { 1 } else { -1 };
        let next = |c: i32, step: i32| (c + i32::from(step > 0)) as f32;
        let mut t_max_x = if d.x.abs() > 1e-12 {
            (next(cx, step_x) - start.x) / d.x
        } else {
            f32::INFINITY
        };
        let mut t_max_z = if d.y.abs() > 1e-12 {
            (next(cz, step_z) - start.y) / d.y
        } else {
            f32::INFINITY
        };
        let t_dx = if d.x.abs() > 1e-12 {
            1.0 / d.x.abs()
        } else {
            f32::INFINITY
        };
        let t_dz = if d.y.abs() > 1e-12 {
            1.0 / d.y.abs()
        } else {
            f32::INFINITY
        };
        let max_steps = (self.w + self.h + 4) as usize;
        for _ in 0..max_steps {
            if !visit((cz * self.w + cx) as usize) || (cx == ex && cz == ez) {
                return;
            }
            if t_max_x < t_max_z {
                cx += step_x;
                t_max_x += t_dx;
            } else {
                cz += step_z;
                t_max_z += t_dz;
            }
            if cx < 0 || cz < 0 || cx >= self.w || cz >= self.h {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxfp(x0: f32, z0: f32, x1: f32, z1: f32, h: f32) -> Footprint {
        Footprint {
            outer: vec![[x0, z0], [x1, z0], [x1, z1], [x0, z1]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: h,
            base_m: 0.0,
        }
    }

    #[test]
    fn blocks_through_building_not_over_it() {
        let occ = Occluders::new(&[boxfp(10.0, -5.0, 20.0, 5.0, 6.0)]);
        let eye = Vec3::new(0.0, 2.0, 0.0);
        assert!(occ.segment_blocked(eye, Vec3::new(30.0, 1.0, 0.0)));
        assert!(
            !occ.segment_blocked(eye, Vec3::new(30.0, 1.0, 20.0)),
            "passes beside"
        );
        assert!(
            !occ.segment_blocked(Vec3::new(0.0, 10.0, 0.0), Vec3::new(30.0, 10.0, 0.0)),
            "passes above"
        );
        assert!(
            !occ.segment_blocked(eye, Vec3::new(9.0, 0.0, 0.0)),
            "stops short"
        );
    }

    #[test]
    fn courtyard_hole_and_raised_part() {
        let mut fp = boxfp(0.0, 0.0, 30.0, 30.0, 10.0);
        fp.holes
            .push(vec![[10.0, 10.0], [20.0, 10.0], [20.0, 20.0], [10.0, 20.0]]);
        let bridge = Footprint {
            min_height_m: 4.0,
            ..boxfp(40.0, -5.0, 45.0, 5.0, 8.0)
        };
        let occ = Occluders::new(&[fp, bridge]);
        // Inside the courtyard, looking at another courtyard point: clear.
        assert!(!occ.segment_blocked(Vec3::new(12.0, 1.0, 12.0), Vec3::new(18.0, 1.0, 18.0)));
        assert!(occ.segment_blocked(Vec3::new(15.0, 1.0, 15.0), Vec3::new(15.0, 1.0, 35.0)));
        assert!(occ.inside_any(Vec2::new(5.0, 5.0)));
        assert!(!occ.inside_any(Vec2::new(15.0, 15.0)));
        // Under the raised part at 1 m: clear; at 6 m: blocked.
        assert!(!occ.segment_blocked(Vec3::new(35.0, 1.0, 0.0), Vec3::new(50.0, 1.0, 0.0)));
        assert!(occ.segment_blocked(Vec3::new(35.0, 6.0, 0.0), Vec3::new(50.0, 6.0, 0.0)));
    }

    #[test]
    fn long_diagonal_through_grid() {
        let fps: Vec<Footprint> = (0..10)
            .map(|i| {
                boxfp(
                    i as f32 * 20.0,
                    i as f32 * 20.0,
                    i as f32 * 20.0 + 5.0,
                    i as f32 * 20.0 + 5.0,
                    5.0,
                )
            })
            .collect();
        let occ = Occluders::new(&fps);
        assert!(occ.segment_blocked(Vec3::new(-10.0, 1.0, -10.0), Vec3::new(250.0, 1.0, 250.0)));
        assert!(!occ.segment_blocked(Vec3::new(-10.0, 1.0, 10.0), Vec3::new(160.0, 1.0, 180.0)));
        assert!(
            !occ.segment_blocked(Vec3::new(-50.0, 1.0, 0.0), Vec3::new(-40.0, 1.0, 0.0)),
            "outside grid"
        );
    }

    #[test]
    fn extreme_and_non_finite_footprints_are_safe() {
        // Found by the coverage_geometry fuzz target: a huge extent overflowed
        // the grid dimensions.
        let huge = boxfp(-3.0e38, -3.0e38, 3.0e38, 3.0e38, 5.0);
        let nan = Footprint {
            outer: vec![[0.0, 0.0], [f32::NAN, 1.0], [1.0, 1.0]],
            ..boxfp(0.0, 0.0, 1.0, 1.0, 5.0)
        };
        let normal = boxfp(10.0, -5.0, 20.0, 5.0, 6.0);
        let occ = Occluders::new(&[huge, nan, normal]);
        assert_eq!(occ.len(), 2, "non-finite footprint skipped");
        // The absurd box only has to be harmless; the real one must still occlude.
        let _ = occ.inside_any(Vec2::new(f32::NAN, 0.0));
        let _ = occ.inside_any(Vec2::new(1.0e38, -1.0e38));
        assert!(occ.segment_blocked(Vec3::new(0.0, 2.0, 0.0), Vec3::new(30.0, 1.0, 0.0)));
    }

    #[test]
    fn vertical_segment() {
        let occ = Occluders::new(&[boxfp(0.0, 0.0, 10.0, 10.0, 5.0)]);
        assert!(occ.segment_blocked(Vec3::new(5.0, 10.0, 5.0), Vec3::new(5.0, 0.0, 5.0)));
        assert!(!occ.segment_blocked(Vec3::new(5.0, 10.0, 5.0), Vec3::new(5.0, 6.0, 5.0)));
    }
}
