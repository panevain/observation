//! Wall patches.

use crate::building::Footprint;
use crate::terrain::Terrain;
use crate::util::planar::{clean_ring, signed_area2};
use glam::{Vec2, Vec3};

/// How far façade sample points sit in front of the wall, metres.
pub(super) const FACADE_OFFSET_M: f32 = 0.05;

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
/// 128 up a wall (longer or taller walls get coarser patches). Walls standing
/// on the ground start where the ground meets them outside, so the part a
/// slope buries has no patches.
pub fn facade_cells<'a>(
    footprints: impl IntoIterator<Item = &'a Footprint>,
    cell_m: f32,
    terrain: &Terrain,
) -> Vec<FacadeCell> {
    let mut out = Vec::new();
    for fp in footprints {
        let (bottom, y1) = (fp.bottom_y(), fp.roof_y());
        if !(y1 - bottom).is_finite() {
            continue;
        }
        let on_ground = fp.min_height_m <= 0.05;
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
                    let y0 = if on_ground {
                        let outside = (p0 + p1) * 0.5 + n2 * FACADE_OFFSET_M;
                        bottom.max(terrain.height_at(outside))
                    } else {
                        bottom
                    };
                    if y1 - y0 < 0.1 {
                        continue;
                    }
                    let rows = ((y1 - y0) / cell_m).ceil().clamp(1.0, MAX_FACADE_ROWS) as usize;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absurd_walls_have_bounded_patches() {
        // Found by the coverage_geometry fuzz target: a 4e18 m wall tried to
        // allocate gigabytes of 1 m façade patches.
        let fp = Footprint {
            outer: vec![[0.0, 0.0], [4.0e18, 0.0], [0.0, 1.0]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 1.0e30,
            base_m: 0.0,
        };
        let cells = facade_cells([&fp], 1.0, &Terrain::default());
        assert!(cells.len() <= 3 * (MAX_FACADE_COLS * MAX_FACADE_ROWS) as usize);
        let nan = Footprint {
            height_m: f32::NAN,
            ..fp
        };
        assert!(facade_cells([&nan], 1.0, &Terrain::default()).is_empty());
    }
}
