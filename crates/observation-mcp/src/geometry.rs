//! Plan-view helpers for describing a scene in words: bearings and compass
//! words, distances to footprints, the street a point or a building is on,
//! footprint sizes and simplified centre lines.
//!
//! Positions are world `[x, z]` metres (x east, z south); bearings are
//! degrees clockwise from north. Numbers meant for an agent are rounded and
//! widened to `f64` here ([`m1`], [`xz1`]), so that their JSON reads `12.3`
//! rather than an `f32`'s widened digits (`12.300000190734863`).

use observation_core::address::distance_to_footprint;
use observation_core::building::{Footprint, signed_area2};
use observation_core::bundle::{AreaBundle, FOOT_WAYS};
pub use observation_core::coverage::closest_on_segment;
use observation_core::glam::Vec2;
use observation_core::occlusion::point_in_ring;

/// The eight compass points, from north clockwise.
pub const COMPASS: [&str; 8] = [
    "north",
    "north-east",
    "east",
    "south-east",
    "south",
    "south-west",
    "west",
    "north-west",
];

/// `v` to one decimal, as an `f64` that prints as such.
pub fn m1(v: f32) -> f64 {
    (f64::from(v) * 10.0).round() / 10.0
}

/// `[x, z]` to one decimal each (see [`m1`]).
pub fn xz1(p: [f32; 2]) -> [f64; 2] {
    [m1(p[0]), m1(p[1])]
}

/// `v` to three decimals, as an `f64`: a fraction such as 0.412.
pub fn r3(v: f32) -> f64 {
    (f64::from(v) * 1000.0).round() / 1000.0
}

/// `v` to the nearest whole number, as an `f64`.
pub fn m0(v: f32) -> f64 {
    f64::from(v).round()
}

/// Serialisers for `f32` fields of tool results. rmcp turns a result into
/// JSON with `serde_json::to_value`, which widens an `f32` to the `f64`
/// nearest it, so 8.2 would read 8.199999809265137; these write the `f64`
/// that the `f32`'s shortest decimal form names instead, so it reads 8.2.
pub mod widened {
    use serde::{Serialize, Serializer};

    /// The `f64` that `v`'s shortest decimal form names.
    pub fn widen(v: f32) -> f64 {
        v.to_string().parse().unwrap_or_else(|_| f64::from(v))
    }

    pub fn one<S: Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(widen(*v))
    }

    pub fn option<S: Serializer>(v: &Option<f32>, s: S) -> Result<S::Ok, S::Error> {
        v.map(widen).serialize(s)
    }

    pub fn array<S: Serializer, const N: usize>(v: &[f32; N], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(v.iter().map(|x| widen(*x)))
    }

    pub fn option_array<S: Serializer, const N: usize>(
        v: &Option<[f32; N]>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(a) => array(a, s),
            None => s.serialize_none(),
        }
    }
}

/// The compass bearing of the direction `v` (`[x, z]`), degrees clockwise
/// from north in `[0, 360)`; 0 for a zero vector.
pub fn direction_deg(v: [f32; 2]) -> f32 {
    if v[0] == 0.0 && v[1] == 0.0 {
        return 0.0;
    }
    let deg = v[0].atan2(-v[1]).to_degrees().rem_euclid(360.0);
    // `rem_euclid` can round up to 360 itself.
    if deg >= 360.0 { 0.0 } else { deg }
}

/// The compass bearing from `from` to `to`, degrees clockwise from north.
pub fn bearing_deg(from: [f32; 2], to: [f32; 2]) -> f32 {
    direction_deg([to[0] - from[0], to[1] - from[1]])
}

/// The nearest of the eight compass points to a bearing in degrees.
pub fn compass(bearing_deg: f32) -> &'static str {
    let i = (bearing_deg.rem_euclid(360.0) / 45.0).round() as usize % 8;
    COMPASS[i]
}

/// The unit direction `[x, z]` of a compass bearing.
pub fn unit_of_bearing(bearing_deg: f32) -> Vec2 {
    let (s, c) = bearing_deg.to_radians().sin_cos();
    Vec2::new(s, -c)
}

/// A compass word or a number of degrees, as an agent may write a bearing:
/// "north", "NE", "south-west", "135", "135 deg", "135 degrees", "135°".
/// `None` if it is neither.
pub fn parse_bearing(text: &str) -> Option<f32> {
    let lower = text.trim().to_ascii_lowercase();
    // Units first, before spaces become hyphens.
    let number = ["degrees", "degree", "deg", "°"]
        .iter()
        .find_map(|unit| lower.strip_suffix(unit))
        .unwrap_or(&lower)
        .trim();
    if let Ok(deg) = number.parse::<f32>() {
        return deg.is_finite().then(|| deg.rem_euclid(360.0));
    }
    let t = lower.replace([' ', '_'], "-");
    let short = ["n", "ne", "e", "se", "s", "sw", "w", "nw"];
    let joined = t.replace('-', "");
    (0..8)
        .find(|&i| t == COMPASS[i] || joined == COMPASS[i].replace('-', "") || t == short[i])
        .map(|i| i as f32 * 45.0)
}

/// The smallest angle between two bearings, degrees (0 to 180).
pub fn angle_between(a_deg: f32, b_deg: f32) -> f32 {
    let d = (a_deg - b_deg).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// The point of a ring's outline nearest `p`, and its distance.
pub fn nearest_on_ring(p: [f32; 2], ring: &[[f32; 2]]) -> Option<([f32; 2], f32)> {
    let at = Vec2::from_array(p);
    (0..ring.len())
        .map(|i| {
            let c = closest_on_segment(
                at,
                Vec2::from_array(ring[i]),
                Vec2::from_array(ring[(i + 1) % ring.len()]),
            );
            (c.to_array(), c.distance(at))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// The nearest point of a footprint to `p` and the distance to it (0, with
/// `p` itself, inside the footprint).
pub fn nearest_on_footprint(p: [f32; 2], f: &Footprint) -> ([f32; 2], f32) {
    if distance_to_footprint(p, f) == 0.0 {
        return (p, 0.0);
    }
    nearest_on_ring(p, &f.outer).unwrap_or((p, f32::INFINITY))
}

/// The enclosed area of a ring, m².
pub fn ring_area(ring: &[[f32; 2]]) -> f32 {
    signed_area2(ring).abs() / 2.0
}

/// A ring's centroid (its centre of area; the mean of its corners if it
/// encloses none).
pub fn centroid(ring: &[[f32; 2]]) -> [f32; 2] {
    if ring.is_empty() {
        return [0.0, 0.0];
    }
    let a2 = signed_area2(ring);
    if a2.abs() > 1e-6 {
        let (mut cx, mut cz) = (0.0f32, 0.0f32);
        for i in 0..ring.len() {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            let cross = a[0] * b[1] - b[0] * a[1];
            cx += (a[0] + b[0]) * cross;
            cz += (a[1] + b[1]) * cross;
        }
        return [cx / (3.0 * a2), cz / (3.0 * a2)];
    }
    let n = ring.len() as f32;
    let sum = ring
        .iter()
        .fold([0.0, 0.0], |s, p| [s[0] + p[0], s[1] + p[1]]);
    [sum[0] / n, sum[1] / n]
}

/// One edge of a footprint's outline, with its outward normal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wall {
    /// Midpoint, world `[x, z]`.
    pub mid: Vec2,
    /// Outward unit normal, `[x, z]`.
    pub normal: Vec2,
    pub length_m: f32,
}

/// The walls of a footprint's outer ring (edges of at least 0.2 m), each
/// facing out of the building.
pub fn walls(f: &Footprint) -> Vec<Wall> {
    let ring = &f.outer;
    let sign = signed_area2(ring).signum();
    (0..ring.len())
        .filter_map(|i| {
            let (a, b) = (
                Vec2::from_array(ring[i]),
                Vec2::from_array(ring[(i + 1) % ring.len()]),
            );
            let d = b - a;
            let len = d.length();
            (len.is_finite() && len >= 0.2).then(|| Wall {
                mid: (a + b) * 0.5,
                // As `observation_core::example` computes it.
                normal: Vec2::new(d.y, -d.x) * (sign / len),
                length_m: len,
            })
        })
        .collect()
}

/// A footprint's size as a rectangle along its longest wall: `(width,
/// depth)` in metres, width along that wall.
pub fn footprint_size(f: &Footprint) -> (f32, f32) {
    let Some(longest) = walls(f)
        .into_iter()
        .max_by(|a, b| a.length_m.total_cmp(&b.length_m))
    else {
        return (0.0, 0.0);
    };
    let along = Vec2::new(-longest.normal.y, longest.normal.x);
    let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for p in &f.outer {
        let p = Vec2::from_array(*p);
        let q = Vec2::new(p.dot(along), p.dot(longest.normal));
        lo = lo.min(q);
        hi = hi.max(q);
    }
    let size = hi - lo;
    (size.x, size.y)
}

/// A street near a point.
#[derive(Debug, Clone, PartialEq)]
pub struct StreetNear {
    /// The road's index in the bundle.
    pub index: usize,
    /// Its name, or what it is when it has none ("an unnamed service road").
    pub label: String,
    /// The point of its centre line nearest the point asked about.
    pub point: [f32; 2],
    pub distance_m: f32,
}

/// How a road is named in words: its name, or what it is ("an unnamed
/// service road", "an unnamed footway").
pub fn street_label(name: Option<&str>, highway: &str) -> String {
    match name {
        Some(n) => n.to_owned(),
        None if FOOT_WAYS.contains(&highway) => format!("an unnamed {highway}"),
        None => format!("an unnamed {highway} road"),
    }
}

/// The street a building or camera at `p` is on, as
/// [`AreaBundle::nearest_street`] chooses it (the nearest carriageway, or in
/// an area without one the nearest way that is not a footpath), so that the
/// descriptions and the mount points name the same street.
pub fn nearest_street(bundle: &AreaBundle, p: [f32; 2]) -> Option<StreetNear> {
    let near = bundle.nearest_street(p)?;
    let road = bundle.roads.get(near.index)?;
    Some(StreetNear {
        index: near.index,
        label: street_label(road.name.as_deref(), &road.highway),
        point: near.point,
        distance_m: near.distance_m,
    })
}

/// A building's street front: the wall that faces its nearest street, the
/// street, and the distance from the wall to the street's centre line.
#[derive(Debug, Clone, PartialEq)]
pub struct Front {
    pub wall: Wall,
    pub street: StreetNear,
}

/// The wall of footprint `f` that faces its nearest street (see
/// [`nearest_street`]): of the walls at least 2 m long (any wall if none
/// is), the one facing most nearly towards the street, a longer wall
/// winning a near tie.
pub fn front(bundle: &AreaBundle, f: &Footprint) -> Option<Front> {
    let c = centroid(&f.outer);
    let toward = nearest_street(bundle, c)?;
    let all = walls(f);
    let long: Vec<Wall> = all.iter().copied().filter(|w| w.length_m >= 2.0).collect();
    let pool = if long.is_empty() { all } else { long };
    let target = Vec2::from_array(toward.point);
    let wall = pool.into_iter().max_by(|a, b| {
        let score = |w: &Wall| {
            w.normal.dot((target - w.mid).normalize_or_zero()) + 0.02 * w.length_m.min(10.0)
        };
        score(a).total_cmp(&score(b))
    })?;
    let street = nearest_street(bundle, wall.mid.to_array())?;
    Some(Front { wall, street })
}

/// The length of a polyline, metres.
pub fn polyline_length(points: &[[f32; 2]]) -> f32 {
    points
        .windows(2)
        .map(|w| Vec2::from_array(w[0]).distance(Vec2::from_array(w[1])))
        .sum()
}

/// The corners Douglas–Peucker keeps of `points` within `tolerance_m`.
fn douglas_peucker(points: &[[f32; 2]], tolerance_m: f32) -> Vec<[f32; 2]> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        let (a, b) = (
            Vec2::from_array(points[first]),
            Vec2::from_array(points[last]),
        );
        let far = (first + 1..last)
            .map(|i| {
                let p = Vec2::from_array(points[i]);
                (i, closest_on_segment(p, a, b).distance(p))
            })
            .max_by(|x, y| x.1.total_cmp(&y.1));
        if let Some((i, d)) = far
            && d > tolerance_m
        {
            keep[i] = true;
            stack.push((first, i));
            stack.push((i, last));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

/// `points` simplified (Douglas–Peucker, from a 0.5 m tolerance, doubled
/// until it fits) to at most `max_points` corners, the ends kept.
pub fn simplify(points: &[[f32; 2]], max_points: usize) -> Vec<[f32; 2]> {
    let max_points = max_points.max(2);
    let mut tolerance = 0.5;
    loop {
        let kept = douglas_peucker(points, tolerance);
        if kept.len() <= max_points || tolerance > 1.0e4 {
            return kept;
        }
        tolerance *= 2.0;
    }
}

/// The smallest and largest `[x, z]` of a set of points.
pub fn bounds(points: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    let mut lo = [f32::MAX, f32::MAX];
    let mut hi = [f32::MIN, f32::MIN];
    for p in points {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    (lo, hi)
}

/// Whether `p` lies inside the polygon `ring` (even-odd).
pub fn inside_ring(p: [f32; 2], ring: &[[f32; 2]]) -> bool {
    point_in_ring(Vec2::from_array(p), ring)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f32, z0: f32, x1: f32, z1: f32) -> Footprint {
        Footprint {
            outer: vec![[x0, z0], [x1, z0], [x1, z1], [x0, z1]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.0,
            base_m: 0.0,
        }
    }

    #[test]
    fn bearings_run_clockwise_from_north() {
        assert_eq!(bearing_deg([0.0, 0.0], [0.0, -10.0]), 0.0);
        assert!((bearing_deg([0.0, 0.0], [10.0, 0.0]) - 90.0).abs() < 1e-4);
        assert!((bearing_deg([0.0, 0.0], [0.0, 10.0]) - 180.0).abs() < 1e-4);
        assert!((bearing_deg([0.0, 0.0], [-10.0, 0.0]) - 270.0).abs() < 1e-4);
        assert_eq!(compass(0.0), "north");
        assert_eq!(compass(22.0), "north");
        assert_eq!(compass(23.0), "north-east");
        assert_eq!(compass(180.0), "south");
        assert_eq!(compass(338.0), "north");
        assert_eq!(compass(-90.0), "west");
        let u = unit_of_bearing(90.0);
        assert!((u.x - 1.0).abs() < 1e-6 && u.y.abs() < 1e-6);
        assert_eq!(angle_between(350.0, 10.0), 20.0);
    }

    #[test]
    fn bearings_parse_from_words_and_numbers() {
        assert_eq!(parse_bearing("north"), Some(0.0));
        assert_eq!(parse_bearing("NE"), Some(45.0));
        assert_eq!(parse_bearing("South West"), Some(225.0));
        assert_eq!(parse_bearing("southwest"), Some(225.0));
        assert_eq!(parse_bearing("135"), Some(135.0));
        for text in [
            "135 deg",
            "135deg",
            "135 degrees",
            "135 Degrees",
            "135°",
            "135 °",
            " 135.0 ",
        ] {
            assert_eq!(parse_bearing(text), Some(135.0), "{text}");
        }
        assert_eq!(parse_bearing("north east"), Some(45.0));
        assert_eq!(parse_bearing("-90"), Some(270.0));
        assert_eq!(parse_bearing("street"), None);
        assert_eq!(parse_bearing("nan"), None);
    }

    #[test]
    fn rounding_prints_short() {
        assert_eq!(serde_json::to_string(&m1(12.3)).unwrap(), "12.3");
        assert_eq!(
            serde_json::to_string(&xz1([2.04, -7.96])).unwrap(),
            "[2.0,-8.0]"
        );
        assert_eq!(serde_json::to_string(&r3(0.41234)).unwrap(), "0.412");
    }

    #[test]
    fn widened_floats_read_as_written() {
        #[derive(serde::Serialize)]
        struct Out {
            #[serde(serialize_with = "widened::one")]
            gain: f32,
            #[serde(serialize_with = "widened::array")]
            at: [f32; 2],
            #[serde(serialize_with = "widened::option")]
            change: Option<f32>,
            #[serde(serialize_with = "widened::option_array")]
            none: Option<[f32; 2]>,
        }
        let out = Out {
            gain: 8.2,
            at: [2.15, -7.3],
            change: Some(-0.1),
            none: None,
        };
        // As rmcp builds a result: through a Value, which widens a plain f32.
        let value = serde_json::to_value(&out).unwrap();
        assert_eq!(
            value.to_string(),
            r#"{"at":[2.15,-7.3],"change":-0.1,"gain":8.2,"none":null}"#
        );
        assert_eq!(
            serde_json::to_value(8.2f32).unwrap().to_string(),
            "8.199999809265137"
        );
    }

    #[test]
    fn footprints_have_a_size_centroid_and_outward_walls() {
        let f = square(0.0, 0.0, 12.0, 8.0);
        let (w, d) = footprint_size(&f);
        assert!(
            (w - 12.0).abs() < 1e-4 && (d - 8.0).abs() < 1e-4,
            "{w} x {d}"
        );
        assert_eq!(centroid(&f.outer), [6.0, 4.0]);
        assert_eq!(ring_area(&f.outer), 96.0);
        for wall in walls(&f) {
            // Every wall faces away from the middle.
            assert!(
                wall.normal.dot(wall.mid - Vec2::new(6.0, 4.0)) > 0.0,
                "{wall:?}"
            );
        }
        let mut reversed = f.clone();
        reversed.outer.reverse();
        for wall in walls(&reversed) {
            assert!(
                wall.normal.dot(wall.mid - Vec2::new(6.0, 4.0)) > 0.0,
                "{wall:?}"
            );
        }
        assert_eq!(nearest_on_footprint([6.0, 4.0], &f), ([6.0, 4.0], 0.0));
        let (p, d) = nearest_on_footprint([6.0, -5.0], &f);
        assert_eq!(p, [6.0, 0.0]);
        assert_eq!(d, 5.0);
        assert!(inside_ring([1.0, 1.0], &f.outer));
        assert!(!inside_ring([13.0, 1.0], &f.outer));
    }

    #[test]
    fn simplifying_keeps_the_ends_and_the_shape() {
        // A straight line with a little noise, then a right-angle turn.
        let mut line: Vec<[f32; 2]> = (0..50)
            .map(|i| [i as f32, if i % 2 == 0 { 0.1 } else { -0.1 }])
            .collect();
        line.extend((1..50).map(|i| [49.0, i as f32]));
        let s = simplify(&line, 12);
        assert!(s.len() <= 12 && s.len() >= 3, "{s:?}");
        assert_eq!(s.first(), line.first());
        assert_eq!(s.last(), line.last());
        assert!(
            s.iter()
                .any(|p| (p[0] - 49.0).abs() < 1.5 && p[1].abs() < 1.5)
        );
        assert_eq!(simplify(&line[..2], 12), line[..2].to_vec());
        assert!((polyline_length(&[[0.0, 0.0], [3.0, 4.0], [3.0, 10.0]]) - 11.0).abs() < 1e-5);
        assert_eq!(
            bounds(&[[1.0, 5.0], [-2.0, 3.0]]),
            ([-2.0, 3.0], [1.0, 5.0])
        );
    }
}
