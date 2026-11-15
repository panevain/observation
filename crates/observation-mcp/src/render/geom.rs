//! Plan geometry the map needs: clipping a segment to a rectangle, and a
//! point inside a polygon.

/// The part of the segment `a`–`b` inside the rectangle `lo`–`hi`
/// (Liang–Barsky), if any. Outlines are clipped to the view with it before
/// they are sampled or dashed, so the work grows with what is shown, not
/// with how long the edges are.
pub fn clip_segment(
    a: [f32; 2],
    b: [f32; 2],
    lo: [f32; 2],
    hi: [f32; 2],
) -> Option<([f32; 2], [f32; 2])> {
    if !a.iter().chain(&b).all(|v| v.is_finite()) {
        return None;
    }
    let d = [b[0] - a[0], b[1] - a[1]];
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for axis in 0..2 {
        for (p, q) in [
            (-d[axis], a[axis] - lo[axis]),
            (d[axis], hi[axis] - a[axis]),
        ] {
            if p == 0.0 {
                // Parallel to this edge: inside its band or wholly out.
                if q < 0.0 {
                    return None;
                }
                continue;
            }
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return None;
            }
        }
    }
    if !(t0.is_finite() && t1.is_finite()) {
        return None;
    }
    let at = |t: f32| [a[0] + d[0] * t, a[1] + d[1] * t];
    Some((at(t0), at(t1)))
}

/// The polygon's corners without a closing duplicate of the first.
fn open_ring(points: &[[f32; 2]]) -> &[[f32; 2]] {
    match points {
        [first, .., last] if first == last => &points[..points.len() - 1],
        _ => points,
    }
}

/// Crossing-number test of `p` against the ring `ring`.
pub fn point_in_ring(p: [f32; 2], ring: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + n - 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
    }
    inside
}

/// A point that represents the polygon `points` on the map: its area
/// centroid when that lies inside it, otherwise the middle of the widest
/// stretch inside it along the line through the centroid (or through the
/// middle of its bounds), so a concave outline gets a point within it. A
/// closing duplicate corner is ignored. One or two corners give their mean;
/// none gives `None`.
pub fn inside_point(points: &[[f32; 2]]) -> Option<[f32; 2]> {
    let ring = open_ring(points);
    let n = ring.len();
    if n == 0 {
        return None;
    }
    let mean = {
        let (sx, sz) = ring
            .iter()
            .fold((0.0, 0.0), |(x, z), p| (x + p[0], z + p[1]));
        [sx / n as f32, sz / n as f32]
    };
    if n < 3 {
        return Some(mean);
    }
    // Shoelace centroid, relative to the first corner for precision.
    let o = ring[0];
    let (mut a2, mut cx, mut cz) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let p = [f64::from(ring[i][0] - o[0]), f64::from(ring[i][1] - o[1])];
        let q = [
            f64::from(ring[(i + 1) % n][0] - o[0]),
            f64::from(ring[(i + 1) % n][1] - o[1]),
        ];
        let cross = p[0] * q[1] - q[0] * p[1];
        a2 += cross;
        cx += (p[0] + q[0]) * cross;
        cz += (p[1] + q[1]) * cross;
    }
    let centroid = if a2.abs() > 1e-9 {
        [
            o[0] + (cx / (3.0 * a2)) as f32,
            o[1] + (cz / (3.0 * a2)) as f32,
        ]
    } else {
        mean
    };
    if point_in_ring(centroid, ring) {
        return Some(centroid);
    }
    let (lo, hi) = ring.iter().fold(
        ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]),
        |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1])],
                [hi[0].max(p[0]), hi[1].max(p[1])],
            )
        },
    );
    for z in [centroid[1], (lo[1] + hi[1]) / 2.0] {
        if let Some(x) = widest_run(ring, z) {
            return Some([x, z]);
        }
    }
    Some(centroid)
}

/// The middle of the widest stretch of the line at `z` inside the ring.
fn widest_run(ring: &[[f32; 2]], z: f32) -> Option<f32> {
    let n = ring.len();
    let mut xs: Vec<f32> = (0..n)
        .filter_map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            ((a[1] <= z) != (b[1] <= z)).then(|| a[0] + (z - a[1]) / (b[1] - a[1]) * (b[0] - a[0]))
        })
        .collect();
    xs.sort_by(f32::total_cmp);
    xs.as_chunks::<2>()
        .0
        .iter()
        .max_by(|p, q| (p[1] - p[0]).total_cmp(&(q[1] - q[0])))
        .map(|p| (p[0] + p[1]) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_cut_to_the_rectangle() {
        let (lo, hi) = ([0.0, 0.0], [10.0, 10.0]);
        assert_eq!(
            clip_segment([-10.0, 5.0], [20.0, 5.0], lo, hi),
            Some(([0.0, 5.0], [10.0, 5.0]))
        );
        assert_eq!(
            clip_segment([2.0, 2.0], [3.0, 4.0], lo, hi),
            Some(([2.0, 2.0], [3.0, 4.0]))
        );
        assert_eq!(clip_segment([-5.0, -5.0], [-1.0, 20.0], lo, hi), None);
        // A diagonal that passes the corner without touching the rectangle.
        assert_eq!(clip_segment([21.0, 1.0], [1.0, 21.0], lo, hi), None);
        let far = clip_segment([-2.0e4, 5.0], [2.0e4, 5.0], lo, hi).unwrap();
        assert!((far.0[0] - 0.0).abs() < 1e-2 && (far.1[0] - 10.0).abs() < 1e-2);
        assert_eq!(clip_segment([f32::NAN, 0.0], [1.0, 1.0], lo, hi), None);
    }

    #[test]
    fn a_concave_outline_gets_a_point_inside_it() {
        // A U: its centroid falls in the notch.
        let u = [
            [0.0, 0.0],
            [10.0, 0.0],
            [10.0, 10.0],
            [7.0, 10.0],
            [7.0, 2.0],
            [3.0, 2.0],
            [3.0, 10.0],
            [0.0, 10.0],
        ];
        let p = inside_point(&u).unwrap();
        assert!(point_in_ring(p, &u), "{p:?}");
        // A square, closed or not, gives its centre.
        let sq = [[0.0, 0.0], [4.0, 0.0], [4.0, 2.0], [0.0, 2.0]];
        assert_eq!(inside_point(&sq), Some([2.0, 1.0]));
        let closed = [sq[0], sq[1], sq[2], sq[3], sq[0]];
        assert_eq!(inside_point(&closed), Some([2.0, 1.0]));
        assert_eq!(inside_point(&[]), None);
        assert_eq!(inside_point(&[[1.0, 2.0]]), Some([1.0, 2.0]));
    }
}
