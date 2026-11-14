//! Finding a spot on a building's wall near a point: where a wall target
//! goes when it is given as "the wall nearest here".

use super::WALL_TARGET_SIZE_M;
use crate::building::Footprint;
use crate::util::planar::{clean_ring, closest_on_segment, ring_bounds, signed_area2};
use glam::Vec2;

/// A spot on a wall.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallSpot {
    /// The building's index among the footprints searched (bundle order for
    /// a bundle's footprints). Only meaningful for the bundle at hand.
    pub building: u32,
    /// The spot on the wall's base line, world `[x, z]`.
    pub xz: [f32; 2],
    /// The direction the wall faces, out of the building: unit length,
    /// `[x, z]`.
    pub normal: [f32; 2],
    /// Horizontal distance from the point searched from to the spot, metres.
    pub distance_m: f32,
    /// World `y` of the wall's foot (before the ground outside is taken into
    /// account) and of its top.
    pub bottom_y: f32,
    pub top_y: f32,
}

/// The spot on a wall nearest world `[x, z]` `p`, among the walls within
/// `max_m` of it that `accept` accepts, if there is one (the first found on
/// a tie, in footprint and corner order).
///
/// Walls are the edges of each footprint's outline and of its holes, as the
/// façade patches are (edges shorter than 0.2 m have none), each facing out
/// of the building. The spot is the point of the wall nearest `p`, kept half
/// a [`WALL_TARGET_SIZE_M`] from the wall's ends where the wall is long
/// enough, so that a wall target there lies on the wall.
pub fn nearest_wall<'a>(
    footprints: impl IntoIterator<Item = &'a Footprint>,
    p: [f32; 2],
    max_m: f32,
    accept: impl Fn(&WallSpot) -> bool,
) -> Option<WallSpot> {
    let at = Vec2::from_array(p);
    let mut best: Option<WallSpot> = None;
    for (building, fp) in footprints.into_iter().enumerate() {
        let (lo, hi) = ring_bounds(&fp.outer);
        let outside = p[0] < lo[0] - max_m
            || p[0] > hi[0] + max_m
            || p[1] < lo[1] - max_m
            || p[1] > hi[1] + max_m;
        if outside {
            continue;
        }
        let building = u32::try_from(building).unwrap_or(u32::MAX);
        let rings = std::iter::once((clean_ring(&fp.outer), false))
            .chain(fp.holes.iter().map(|h| (clean_ring(h), true)));
        for (ring, is_hole) in rings {
            if ring.len() < 3 {
                continue;
            }
            // As the façade patches have it.
            let sign = signed_area2(&ring).signum() * if is_hole { -1.0 } else { 1.0 };
            for i in 0..ring.len() {
                let a = Vec2::from_array(ring[i]);
                let b = Vec2::from_array(ring[(i + 1) % ring.len()]);
                let d = b - a;
                let len = d.length();
                if !len.is_finite() || len < 0.2 {
                    continue;
                }
                let inset = (WALL_TARGET_SIZE_M / 2.0).min(len / 2.0);
                let dir = d / len;
                let foot = closest_on_segment(at, a + dir * inset, b - dir * inset);
                let distance_m = foot.distance(at);
                if distance_m > max_m || best.is_some_and(|w| w.distance_m <= distance_m) {
                    continue;
                }
                let spot = WallSpot {
                    building,
                    xz: foot.to_array(),
                    normal: (Vec2::new(d.y, -d.x) * (sign / len)).to_array(),
                    distance_m,
                    bottom_y: fp.bottom_y(),
                    top_y: fp.roof_y(),
                };
                if accept(&spot) {
                    best = Some(spot);
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::box_footprint;

    #[test]
    fn the_nearest_wall_faces_out_of_the_building() {
        // A 10 x 10 m house at x -5..5, z -25..-15.
        let house = box_footprint(-5.0, -25.0, 5.0, -15.0, 6.0);
        let south = nearest_wall([&house], [1.0, -12.0], 10.0, |_| true).expect("a wall");
        assert_eq!(south.building, 0);
        assert_eq!(south.xz, [1.0, -15.0]);
        assert!((south.distance_m - 3.0).abs() < 1e-5);
        assert!((Vec2::from_array(south.normal) - Vec2::Y).length() < 1e-5);
        assert_eq!(south.top_y, 6.0);
        // From inside: the nearest wall still faces out.
        let west = nearest_wall([&house], [-4.0, -20.0], 10.0, |_| true).expect("a wall");
        assert!((Vec2::from_array(west.normal) + Vec2::X).length() < 1e-5);
        // Near a corner, the spot keeps half a target from the wall's end.
        let corner = nearest_wall([&house], [6.0, -13.0], 10.0, |_| true).expect("a wall");
        assert!(
            corner.xz == [4.5, -15.0] || corner.xz == [5.0, -15.5],
            "{corner:?}"
        );
        // Too far, or every wall refused.
        assert!(nearest_wall([&house], [1.0, -2.0], 10.0, |_| true).is_none());
        let facing_north = |w: &WallSpot| w.normal[1] < -0.5;
        let north = nearest_wall([&house], [1.0, -12.0], 20.0, facing_north).expect("a wall");
        assert_eq!(north.xz, [1.0, -25.0]);
    }

    #[test]
    fn the_nearer_building_wins() {
        let a = box_footprint(0.0, 0.0, 4.0, 4.0, 3.0);
        let b = box_footprint(10.0, 0.0, 14.0, 4.0, 3.0);
        let w = nearest_wall([&a, &b], [8.0, 2.0], 5.0, |_| true).expect("a wall");
        assert_eq!((w.building, w.xz), (1, [10.0, 2.0]));
    }
}
