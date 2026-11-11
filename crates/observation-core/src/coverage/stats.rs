//! The figures shown in the UI.

use super::{CoverageCounts, RegionMask, StaticLayers};
use serde::{Deserialize, Serialize};

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
    use super::super::tests::layers;
    use super::*;
    use crate::frustum::ViewCone;
    use glam::Vec3;

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
