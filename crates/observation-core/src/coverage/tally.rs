//! Per-cell camera counts, and figures kept up to date with them.

use super::{CoverageStats, RegionMask, Sight, StaticLayers, Visibility};
use std::collections::HashMap;

/// Per-cell count of cameras that see it.
///
/// `ground` and `facade` are the counts as shown, capped at 255. The cameras
/// beyond 255 on a cell are kept too, so taking cameras away from a cell
/// more than 255 cameras see leaves the right figure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageCounts {
    pub ground: Vec<u8>,
    pub facade: Vec<u8>,
    /// Cameras beyond the 255 shown, for each ground cell that has any.
    over_ground: HashMap<usize, u16>,
    /// The same for façade patches.
    over_facade: HashMap<usize, u16>,
}

/// One camera more (`UP`) or fewer on cell `i`; returns its shown count
/// before and after. Only a cell already showing 255 touches `over`.
#[inline]
fn bump<const UP: bool>(shown: &mut [u8], over: &mut HashMap<usize, u16>, i: usize) -> (u8, u8) {
    let before = shown[i];
    if before == u8::MAX && beyond_255::<UP>(over, i) {
        return (before, before);
    }
    shown[i] = if UP {
        before + 1
    } else {
        before.saturating_sub(1)
    };
    (before, shown[i])
}

/// For a cell showing 255: counts one camera more (`UP`) or fewer among
/// those beyond 255, and returns whether that absorbed the change (always
/// when adding; when removing, only if there were any).
#[cold]
#[inline(never)]
fn beyond_255<const UP: bool>(over: &mut HashMap<usize, u16>, i: usize) -> bool {
    if UP {
        let n = over.entry(i).or_insert(0);
        *n = n.saturating_add(1);
        return true;
    }
    let Some(n) = over.get_mut(&i) else {
        return false;
    };
    // Entries are removed at zero, so `n` is at least 1.
    *n -= 1;
    if *n == 0 {
        over.remove(&i);
    }
    true
}

impl CoverageCounts {
    pub fn new(layers: &StaticLayers) -> Self {
        Self {
            ground: vec![0; layers.grid().len()],
            facade: vec![0; layers.facades().len()],
            over_ground: HashMap::new(),
            over_facade: HashMap::new(),
        }
    }

    pub fn add(&mut self, v: &Visibility) {
        for &i in &v.ground {
            self.ground_step::<true>(i as usize);
        }
        for &i in &v.facade {
            self.facade_step::<true>(i as usize);
        }
    }

    pub fn remove(&mut self, v: &Visibility) {
        for &i in &v.ground {
            self.ground_step::<false>(i as usize);
        }
        for &i in &v.facade {
            self.facade_step::<false>(i as usize);
        }
    }

    /// One camera more (`UP`) or fewer on ground cell `i`; the shown count
    /// before and after.
    fn ground_step<const UP: bool>(&mut self, i: usize) -> (u8, u8) {
        bump::<UP>(&mut self.ground, &mut self.over_ground, i)
    }

    /// One camera more (`UP`) or fewer on façade patch `i`; the shown count
    /// before and after.
    fn facade_step<const UP: bool>(&mut self, i: usize) -> (u8, u8) {
        bump::<UP>(&mut self.facade, &mut self.over_facade, i)
    }
}

/// [`CoverageCounts`] that keep their [`CoverageStats`] up to date as cameras
/// are counted and taken away, so the figures cost the cells that changed
/// rather than a pass over the whole area. The ground figures are kept as cell
/// counts, so they equal [`CoverageStats::compute_in`]'s; the wall areas are
/// summed in `f64`, so they agree with it to rounding.
#[derive(Debug, Clone, PartialEq)]
pub struct Tally {
    pub counts: CoverageCounts,
    street: u32,
    street_seen: u32,
    street_seen_multi: u32,
    open_ground_seen: u32,
    facade_m2: f64,
    facade_seen_m2: f64,
}

impl Tally {
    /// No cameras counted yet; the figures are limited to `region` (all cells
    /// if `None`).
    pub fn new(layers: &StaticLayers, region: Option<&RegionMask>) -> Self {
        let mut t = Self {
            counts: CoverageCounts::new(layers),
            street: 0,
            street_seen: 0,
            street_seen_multi: 0,
            open_ground_seen: 0,
            facade_m2: 0.0,
            facade_seen_m2: 0.0,
        };
        t.limit_to(layers, region);
        t
    }

    /// The per-cell camera counts the figures are kept for.
    pub fn counts(&self) -> &CoverageCounts {
        &self.counts
    }

    /// Works the figures out afresh for another region (a pass over every
    /// cell).
    pub fn limit_to(&mut self, layers: &StaticLayers, region: Option<&RegionMask>) {
        let counts = std::mem::replace(
            &mut self.counts,
            CoverageCounts {
                ground: Vec::new(),
                facade: Vec::new(),
                over_ground: HashMap::new(),
                over_facade: HashMap::new(),
            },
        );
        (self.street, self.street_seen, self.street_seen_multi) = (0, 0, 0);
        self.open_ground_seen = 0;
        (self.facade_m2, self.facade_seen_m2) = (0.0, 0.0);
        for (i, &c) in counts.ground.iter().enumerate() {
            if region.is_none_or(|r| r.ground[i]) && layers.street_mask()[i] {
                self.street += 1;
            }
            self.ground_moved(layers, region, i, 0, c);
        }
        for (i, &c) in counts.facade.iter().enumerate() {
            if region.is_none_or(|r| r.facade[i]) {
                self.facade_m2 += f64::from(layers.facades()[i].area_m2());
            }
            self.facade_moved(layers, region, i, 0, c);
        }
        self.counts = counts;
    }

    /// Counts the cells `s` sees out to `range` (with the same `region` as
    /// [`Self::limit_to`] was given).
    pub fn add_within(
        &mut self,
        layers: &StaticLayers,
        region: Option<&RegionMask>,
        s: &Sight,
        range: f32,
    ) {
        self.step::<true>(layers, region, s, range);
    }

    /// Takes away the cells `s` sees out to `range`.
    pub fn remove_within(
        &mut self,
        layers: &StaticLayers,
        region: Option<&RegionMask>,
        s: &Sight,
        range: f32,
    ) {
        self.step::<false>(layers, region, s, range);
    }

    fn step<const UP: bool>(
        &mut self,
        layers: &StaticLayers,
        region: Option<&RegionMask>,
        s: &Sight,
        range: f32,
    ) {
        // The figures only depend on whether a count is at least 1 or 2,
        // which the shown count tells as well as the exact one.
        for i in s.ground_within(range) {
            let i = i as usize;
            let (before, after) = self.counts.ground_step::<UP>(i);
            self.ground_moved(layers, region, i, before, after);
        }
        for i in s.facade_within(range) {
            let i = i as usize;
            let (before, after) = self.counts.facade_step::<UP>(i);
            self.facade_moved(layers, region, i, before, after);
        }
    }

    /// Ground cell `i`'s count went from `before` to `after`.
    fn ground_moved(
        &mut self,
        layers: &StaticLayers,
        region: Option<&RegionMask>,
        i: usize,
        before: u8,
        after: u8,
    ) {
        if region.is_some_and(|r| !r.ground[i]) {
            return;
        }
        // +1 when the count reaches `k`, -1 when it drops below it.
        let crossed = |k: u8| i32::from(after >= k) - i32::from(before >= k);
        self.open_ground_seen = self.open_ground_seen.wrapping_add_signed(crossed(1));
        if layers.street_mask()[i] {
            self.street_seen = self.street_seen.wrapping_add_signed(crossed(1));
            self.street_seen_multi = self.street_seen_multi.wrapping_add_signed(crossed(2));
        }
    }

    /// Façade patch `i`'s count went from `before` to `after`.
    fn facade_moved(
        &mut self,
        layers: &StaticLayers,
        region: Option<&RegionMask>,
        i: usize,
        before: u8,
        after: u8,
    ) {
        if region.is_some_and(|r| !r.facade[i]) || (before >= 1) == (after >= 1) {
            return;
        }
        let a = f64::from(layers.facades()[i].area_m2());
        self.facade_seen_m2 += if after >= 1 { a } else { -a };
    }

    pub fn stats(&self, layers: &StaticLayers) -> CoverageStats {
        let a = layers.grid().cell_area_m2();
        CoverageStats {
            street_m2: self.street as f32 * a,
            street_seen_m2: self.street_seen as f32 * a,
            street_seen_multi_m2: self.street_seen_multi as f32 * a,
            open_ground_seen_m2: self.open_ground_seen as f32 * a,
            facade_m2: self.facade_m2 as f32,
            // Never below zero, whatever the rounding.
            facade_seen_m2: self.facade_seen_m2.max(0.0) as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::layers;
    use super::*;
    use crate::coverage::Region;
    use crate::frustum::ViewCone;
    use glam::Vec3;

    #[test]
    fn counts_stay_exact_beyond_255_cameras() {
        let l = layers();
        let cone = ViewCone::from_angles(
            Vec3::new(0.0, 2.5, 10.0),
            0.0,
            -10.0,
            0.0,
            100.0,
            60.0,
            40.0,
        );
        let sight = l.sight(&cone, 1.0);
        let vis = sight.within(cone.range);
        let (g, f) = (vis.ground[0] as usize, vis.facade[0] as usize);
        for cameras in [255usize, 256, 300] {
            let mut counts = CoverageCounts::new(&l);
            let mut tally = Tally::new(&l, None);
            for _ in 0..cameras {
                counts.add(&vis);
                tally.add_within(&l, None, &sight, cone.range);
            }
            assert_eq!(
                (counts.ground[g], counts.facade[f]),
                (255, 255),
                "{cameras}"
            );
            assert_eq!(tally.counts(), &counts, "{cameras}");
            // Take away all but one: the cells are still seen, by one camera.
            for left in (1..cameras).rev() {
                counts.remove(&vis);
                tally.remove_within(&l, None, &sight, cone.range);
                let shown = left.min(255) as u8;
                assert_eq!(
                    (counts.ground[g], counts.facade[f]),
                    (shown, shown),
                    "{cameras} -> {left}"
                );
            }
            assert_eq!(tally.counts(), &counts, "{cameras}");
            assert_eq!(
                tally.stats(&l),
                CoverageStats::compute(&l, &counts),
                "{cameras}"
            );
            assert!(tally.stats(&l).open_ground_seen_m2 > 0.0, "{cameras}");
            counts.remove(&vis);
            assert_eq!((counts.ground[g], counts.facade[f]), (0, 0), "{cameras}");
        }
    }

    #[test]
    fn tally_keeps_the_figures_a_full_pass_gives() {
        let l = layers();
        let cone = |x: f32, yaw: f32| {
            ViewCone::from_angles(Vec3::new(x, 2.5, 10.0), yaw, -10.0, 0.0, 100.0, 60.0, 40.0)
        };
        let sights: Vec<Sight> = [(-20.0, 90.0), (20.0, 270.0), (0.0, 0.0), (-5.0, 120.0)]
            .iter()
            .map(|&(x, yaw)| l.sight(&cone(x, yaw), 1.0))
            .collect();
        let west = RegionMask::new(
            &l,
            &Region {
                points: vec![[-50.0, -30.0], [5.0, -30.0], [5.0, 30.0], [-50.0, 30.0]],
                enabled: true,
            },
        );
        let check = |t: &Tally, region: Option<&RegionMask>| {
            let (got, want) = (
                t.stats(&l),
                CoverageStats::compute_in(&l, t.counts(), region),
            );
            let ground = |s: CoverageStats| {
                (
                    s.street_m2,
                    s.street_seen_m2,
                    s.street_seen_multi_m2,
                    s.open_ground_seen_m2,
                )
            };
            assert_eq!(ground(got), ground(want));
            assert!(
                (got.facade_m2 - want.facade_m2).abs() < 1e-2,
                "{got:?} {want:?}"
            );
            assert!(
                (got.facade_seen_m2 - want.facade_seen_m2).abs() < 1e-2,
                "{got:?} {want:?}"
            );
        };
        for region in [None, Some(&west)] {
            let mut t = Tally::new(&l, region);
            check(&t, region);
            // Counted at different ranges, overlapping, then taken away in
            // another order.
            let ranges = [40.0, 25.0, 40.0, 12.0];
            for (s, &r) in sights.iter().zip(&ranges) {
                t.add_within(&l, region, s, r);
                check(&t, region);
            }
            assert!(t.stats(&l).street_seen_multi_m2 > 0.0);
            // Moving the region recounts what is there.
            let other = if region.is_some() { None } else { Some(&west) };
            t.limit_to(&l, other);
            check(&t, other);
            t.limit_to(&l, region);
            for i in [2, 0, 3, 1] {
                t.remove_within(&l, region, &sights[i], ranges[i]);
                check(&t, region);
            }
            assert!(
                t.counts()
                    .ground
                    .iter()
                    .chain(&t.counts().facade)
                    .all(|c| *c == 0)
            );
            assert_eq!(t.stats(&l).facade_seen_m2, 0.0);
        }
    }
}
