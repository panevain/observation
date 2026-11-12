//! Plan-view (`[x, z]`) geometry on rings and segments.

use glam::Vec2;

/// Crossing-number test; the ring may or may not repeat its first point.
/// Takes `Vec2` rings or bundle `[f32; 2]` rings.
pub fn point_in_ring<P: Copy + Into<Vec2>>(p: Vec2, ring: &[P]) -> bool {
    let mut inside = false;
    let n = ring.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b): (Vec2, Vec2) = (ring[i].into(), ring[j].into());
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside.toggle();
        }
        j = i;
    }
    inside
}

/// The ring's bounding box, `(min, max)`.
pub fn ring_bounds(ring: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
    for q in ring {
        for i in 0..2 {
            lo[i] = lo[i].min(q[i]);
            hi[i] = hi[i].max(q[i]);
        }
    }
    (lo, hi)
}

/// Removes a closing duplicate point and consecutive duplicates.
pub fn clean_ring(ring: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(ring.len());
    for &p in ring {
        if out
            .last()
            .is_none_or(|q| (q[0] - p[0]).abs() > 1e-4 || (q[1] - p[1]).abs() > 1e-4)
        {
            out.push(p);
        }
    }
    while out.len() > 1 {
        let (f, l) = (out[0], out[out.len() - 1]);
        if (f[0] - l[0]).abs() <= 1e-4 && (f[1] - l[1]).abs() <= 1e-4 {
            out.pop();
        } else {
            break;
        }
    }
    out
}

/// Twice the signed area of a ring in the `[x, z]` plane.
pub fn signed_area2(ring: &[[f32; 2]]) -> f32 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

/// Distance between two plan-view points.
pub(crate) fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Distance from `p` to the segment `a`-`b`, on `[f32; 2]` points (the
/// address code's version).
pub(crate) fn distance_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, [a[0] + t * dx, a[1] + t * dz])
}

/// Distance from `p` to the segment `a`-`b`, on `Vec2` (the coverage code's
/// version). Not bit-identical to [`distance_to_segment`]: it treats segments
/// shorter than 1e-6 m as points and takes a square root where the other
/// takes `hypot`, so the two stay apart to keep coverage results unchanged.
pub(crate) fn dist_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
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
