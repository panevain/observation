//! The ground raster.

use glam::Vec2;
use serde::{Deserialize, Serialize};

/// Regular raster over the ground (in plan), row-major with rows along `+Z` (south).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    pub(super) fn range(&self, lo: Vec2, hi: Vec2) -> Option<(u32, u32, u32, u32)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_indexing() {
        let g = GroundGrid::covering(Vec2::new(-10.0, -10.0), Vec2::new(10.0, 10.0), 0.5);
        assert_eq!((g.w, g.h), (40, 40));
        let i = g.cell_at(Vec2::new(0.1, 0.1)).unwrap();
        assert!(g.center(i).distance(Vec2::new(0.25, 0.25)) < 1e-5);
        assert_eq!(g.cell_at(Vec2::new(10.5, 0.0)), None);
    }
}
