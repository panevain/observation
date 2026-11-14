//! Gaps: the connected runs of street that no camera sees.
//!
//! A gap is a set of ground cells on the street mask, none seen by an
//! enabled camera at the level and lighting asked about, each sharing an
//! edge with another (cells that touch only at a corner are apart). Gaps are
//! ranked by area, so the first is the longest stretch of street a person
//! could walk unrecorded in the model. Like the coverage figures, a gap is an
//! estimate: what the model leaves out (trees, fences, vehicles) can only
//! make gaps larger.

use super::FootageCamera;
use crate::coverage::{CoverageParams, GroundGrid, RegionMask, StaticLayers};
use crate::frustum::ViewCone;
use glam::Vec2;

/// One connected run of ground cells.
#[derive(Debug, Clone, PartialEq)]
pub struct Gap {
    /// The run's ground cells, as sorted grid indices.
    pub cells: Vec<u32>,
    /// Its area, m².
    pub area_m2: f32,
    /// The mean of its cell centres, world `[x, z]`. It may lie outside the
    /// run where the street bends: see [`Self::inside`].
    pub centroid: [f32; 2],
    /// The centre of the run's cell nearest [`Self::centroid`] (the first in
    /// grid order on a tie), world `[x, z]`: a point that lies in the run.
    pub inside: [f32; 2],
    /// The smallest `[x, z]` of the run's cells (their outer edges), metres.
    pub min: [f32; 2],
    /// The largest `[x, z]` of the run's cells (their outer edges), metres.
    pub max: [f32; 2],
}

impl Gap {
    /// The distance from world `[x, z]` `p` to the nearest cell centre of
    /// the run, metres, and that centre (the first in grid order on a tie).
    pub fn nearest_to(&self, grid: &GroundGrid, p: [f32; 2]) -> (f32, [f32; 2]) {
        let p = Vec2::from_array(p);
        let mut best = (f32::INFINITY, [0.0; 2]);
        for &i in &self.cells {
            let c = grid.center(i as usize);
            let d = c.distance_squared(p);
            if d < best.0 {
                best = (d, c.to_array());
            }
        }
        (best.0.sqrt(), best.1)
    }
}

/// Every connected run of cells of `grid` for which `open` is true, largest
/// first; runs of equal size come in the grid order of their first cell, so
/// the result does not depend on anything but the inputs.
pub fn gaps_where(grid: &GroundGrid, open: impl Fn(usize) -> bool) -> Vec<Gap> {
    let (w, h) = (grid.w as usize, grid.h as usize);
    let cell_m2 = f64::from(grid.cell_area_m2());
    let mut done = vec![false; grid.len()];
    let mut stack = Vec::new();
    let mut gaps = Vec::new();
    for start in 0..grid.len() {
        if done[start] || !open(start) {
            continue;
        }
        done[start] = true;
        stack.push(start);
        let mut cells = Vec::new();
        while let Some(i) = stack.pop() {
            cells.push(i as u32);
            let (x, z) = (i % w, i / w);
            let neighbours = [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (z > 0).then(|| i - w),
                (z + 1 < h).then(|| i + w),
            ];
            for n in neighbours.into_iter().flatten() {
                if !done[n] && open(n) {
                    done[n] = true;
                    stack.push(n);
                }
            }
        }
        cells.sort_unstable();
        gaps.push(gap_of(grid, cells, cell_m2));
    }
    // Stable, so equal sizes keep the order of their first cells.
    gaps.sort_by_key(|g| std::cmp::Reverse(g.cells.len()));
    gaps
}

fn gap_of(grid: &GroundGrid, cells: Vec<u32>, cell_m2: f64) -> Gap {
    let n = cells.len() as f64;
    let (mut sx, mut sz) = (0f64, 0f64);
    let (mut lo, mut hi) = (Vec2::INFINITY, Vec2::NEG_INFINITY);
    for &i in &cells {
        let c = grid.center(i as usize);
        sx += f64::from(c.x);
        sz += f64::from(c.y);
        lo = lo.min(c);
        hi = hi.max(c);
    }
    let centroid = Vec2::new((sx / n) as f32, (sz / n) as f32);
    let half = grid.cell_m / 2.0;
    let mut gap = Gap {
        area_m2: (n * cell_m2) as f32,
        centroid: centroid.to_array(),
        inside: [0.0; 2],
        min: (lo - half).to_array(),
        max: (hi + half).to_array(),
        cells,
    };
    gap.inside = gap.nearest_to(grid, gap.centroid).1;
    gap
}

/// The connected runs of street that `seen` (per ground cell) leaves unseen,
/// largest first, as [`gaps_where`] ranks them. Only cells on the street mask
/// count, and with `region` only those inside it, as for the coverage
/// figures.
pub fn unseen_street(
    layers: &StaticLayers,
    region: Option<&RegionMask>,
    seen: impl Fn(usize) -> bool,
) -> Vec<Gap> {
    let street = layers.street_mask();
    gaps_where(layers.grid(), |i| {
        street[i] && region.is_none_or(|r| r.ground[i]) && !seen(i)
    })
}

/// Which ground cells some enabled camera in `cameras` sees at the level and
/// lighting `params` select.
pub fn seen_ground(
    layers: &StaticLayers,
    params: &CoverageParams,
    cameras: &[FootageCamera],
) -> Vec<bool> {
    let mut seen = vec![false; layers.grid().len()];
    for c in cameras.iter().filter(|c| c.placement.enabled) {
        let range = c
            .model
            .effective_range_m(params.pixels_per_metre(), params.lighting);
        let cone = ViewCone::new(c.placement, c.model, range);
        for &i in &layers.visibility(&cone, params.subject_height_m).ground {
            seen[i as usize] = true;
        }
    }
    seen
}

/// The connected runs of street no enabled camera in `cameras` sees at the
/// level and lighting `params` select, largest first: [`seen_ground`], then
/// [`unseen_street`].
pub fn street_gaps(
    layers: &StaticLayers,
    params: &CoverageParams,
    cameras: &[FootageCamera],
    region: Option<&RegionMask>,
) -> Vec<Gap> {
    let seen = seen_ground(layers, params, cameras);
    unseen_street(layers, region, |i| seen[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid of 1 m cells with its north-west corner at the origin, from
    /// rows of `#` (open) and `.` (closed).
    fn grid(rows: &[&str]) -> (GroundGrid, Vec<bool>) {
        let (w, h) = (rows[0].len(), rows.len());
        let g = GroundGrid::covering(Vec2::ZERO, Vec2::new(w as f32, h as f32), 1.0);
        let open = rows
            .iter()
            .flat_map(|r| r.bytes().map(|b| b == b'#'))
            .collect();
        (g, open)
    }

    fn gaps(rows: &[&str]) -> (GroundGrid, Vec<Gap>) {
        let (g, open) = grid(rows);
        (g, gaps_where(&g, |i| open[i]))
    }

    #[test]
    fn runs_are_joined_by_edges_not_corners() {
        let (_, found) = gaps(&[
            "##..#", //
            "##...", //
            "..#..", //
            "...##", //
        ]);
        let sizes: Vec<usize> = found.iter().map(|g| g.cells.len()).collect();
        assert_eq!(sizes, [4, 2, 1, 1]);
        // Equal sizes in grid order of their first cell: (4, 0) before (2, 2).
        assert_eq!(found[2].cells, [4]);
        assert_eq!(found[3].cells, [12]);
        assert_eq!(found[1].cells, [18, 19]);
    }

    #[test]
    fn a_gap_has_its_area_centroid_box_and_a_point_inside() {
        // An L: the centroid of its cell centres lies off the L itself.
        let (g, found) = gaps(&[
            "#...", //
            "#...", //
            "#...", //
            "####", //
        ]);
        assert_eq!(found.len(), 1);
        let l = &found[0];
        assert_eq!(l.area_m2, 7.0);
        assert_eq!((l.min, l.max), ([0.0, 0.0], [4.0, 4.0]));
        let (cx, cz) = (
            (0.5 * 4.0 + 1.5 + 2.5 + 3.5) / 7.0,
            (0.5 + 1.5 + 2.5 + 3.5 * 4.0) / 7.0,
        );
        assert!((l.centroid[0] - cx).abs() < 1e-5 && (l.centroid[1] - cz).abs() < 1e-5);
        let off = g
            .cell_at(Vec2::from_array(l.centroid))
            .expect("on the grid");
        assert!(!l.cells.contains(&(off as u32)), "{:?}", l.centroid);
        let inside = g.cell_at(Vec2::from_array(l.inside)).expect("on the grid");
        assert!(l.cells.contains(&(inside as u32)), "{:?}", l.inside);
        let (d, at) = l.nearest_to(&g, [10.5, 3.5]);
        assert_eq!((d, at), (7.0, [3.5, 3.5]));
    }

    #[test]
    fn nothing_open_gives_no_gaps_and_everything_open_one() {
        let (_, none) = gaps(&["...", "..."]);
        assert!(none.is_empty());
        let (_, all) = gaps(&["###", "###"]);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].cells, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_ring_is_one_gap_around_a_hole() {
        let (_, found) = gaps(&[
            "#####", //
            "#...#", //
            "#.#.#", //
            "#...#", //
            "#####", //
        ]);
        let sizes: Vec<usize> = found.iter().map(|g| g.cells.len()).collect();
        assert_eq!(sizes, [16, 1]);
    }
}
