//! Per-cell camera counts, and figures kept up to date with them.

use super::{CoverageStats, RegionMask, Sight, StaticLayers, Visibility};

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

    /// Works the figures out afresh for another region (a pass over every
    /// cell).
    pub fn limit_to(&mut self, layers: &StaticLayers, region: Option<&RegionMask>) {
        let counts = std::mem::replace(
            &mut self.counts,
            CoverageCounts {
                ground: Vec::new(),
                facade: Vec::new(),
            },
        );
        (self.street, self.street_seen, self.street_seen_multi) = (0, 0, 0);
        self.open_ground_seen = 0;
        (self.facade_m2, self.facade_seen_m2) = (0.0, 0.0);
        for (i, &c) in counts.ground.iter().enumerate() {
            if region.is_none_or(|r| r.ground[i]) && layers.street_mask[i] {
                self.street += 1;
            }
            self.ground_moved(layers, region, i, 0, c);
        }
        for (i, &c) in counts.facade.iter().enumerate() {
            if region.is_none_or(|r| r.facade[i]) {
                self.facade_m2 += f64::from(layers.facades[i].area_m2());
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
        self.step(layers, region, s, range, |c| c.saturating_add(1));
    }

    /// Takes away the cells `s` sees out to `range`.
    pub fn remove_within(
        &mut self,
        layers: &StaticLayers,
        region: Option<&RegionMask>,
        s: &Sight,
        range: f32,
    ) {
        self.step(layers, region, s, range, |c| c.saturating_sub(1));
    }

    fn step(
        &mut self,
        layers: &StaticLayers,
        region: Option<&RegionMask>,
        s: &Sight,
        range: f32,
        f: impl Fn(u8) -> u8,
    ) {
        for i in s.ground_within(range) {
            let i = i as usize;
            let before = self.counts.ground[i];
            self.counts.ground[i] = f(before);
            self.ground_moved(layers, region, i, before, self.counts.ground[i]);
        }
        for i in s.facade_within(range) {
            let i = i as usize;
            let before = self.counts.facade[i];
            self.counts.facade[i] = f(before);
            self.facade_moved(layers, region, i, before, self.counts.facade[i]);
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
        if layers.street_mask[i] {
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
        let a = f64::from(layers.facades[i].area_m2());
        self.facade_seen_m2 += if after >= 1 { a } else { -a };
    }

    pub fn stats(&self, layers: &StaticLayers) -> CoverageStats {
        let a = layers.grid.cell_area_m2();
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
                CoverageStats::compute_in(&l, &t.counts, region),
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
                t.counts
                    .ground
                    .iter()
                    .chain(&t.counts.facade)
                    .all(|c| *c == 0)
            );
            assert_eq!(t.stats(&l).facade_seen_m2, 0.0);
        }
    }
}
