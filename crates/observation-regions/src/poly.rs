//! Region boundaries in the Osmosis `.poly` format that Geofabrik publishes
//! next to every extract, and the test of whether an area lies inside one.
//!
//! An extract holds complete data only inside its boundary, so an area is
//! answered locally only when it lies wholly within it; an area that straddles
//! the edge would come back half empty.

use observation_core::geo::{BBox, LatLon};
use thiserror::Error;

/// A `.poly` file could not be read.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolyError {
    #[error("the boundary file is empty or has no rings")]
    Empty,
    #[error("the boundary file has an invalid line: {0}")]
    Line(String),
    #[error("the boundary file ends inside a ring")]
    Unterminated,
}

/// A multipolygon: outer rings with holes, as listed in the file (a hole is a
/// section whose name starts with `!`).
#[derive(Debug, Clone, PartialEq)]
pub struct Boundary {
    outers: Vec<Vec<LatLon>>,
    holes: Vec<Vec<LatLon>>,
}

/// Largest boundary file accepted (Geofabrik's largest are a few MB).
pub const MAX_POLY_BYTES: usize = 32 * 1024 * 1024;

impl Boundary {
    /// Parses a `.poly` file: a name line, then sections of `lon lat` lines,
    /// each closed by `END`, and a final `END`.
    pub fn parse(text: &str) -> Result<Self, PolyError> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        lines.next().ok_or(PolyError::Empty)?; // file name
        let mut outers = Vec::new();
        let mut holes = Vec::new();
        loop {
            let Some(header) = lines.next() else {
                return Err(PolyError::Unterminated);
            };
            if header == "END" {
                break;
            }
            let mut ring = Vec::new();
            loop {
                let line = lines.next().ok_or(PolyError::Unterminated)?;
                if line == "END" {
                    break;
                }
                let mut nums = line.split_whitespace().map(str::parse::<f64>);
                match (nums.next(), nums.next(), nums.next()) {
                    (Some(Ok(lon)), Some(Ok(lat)), None) if lon.is_finite() && lat.is_finite() => {
                        ring.push(LatLon::new(lat, lon));
                    }
                    _ => return Err(PolyError::Line(line.chars().take(80).collect())),
                }
            }
            if ring.len() >= 3 {
                if header.starts_with('!') {
                    holes.push(ring);
                } else {
                    outers.push(ring);
                }
            }
        }
        if outers.is_empty() {
            return Err(PolyError::Empty);
        }
        Ok(Self { outers, holes })
    }

    /// Bounds of the outer rings; `None` for a boundary that crosses the
    /// antimeridian (Alaska, Russia, Fiji), whose bounds would span the globe.
    pub fn bbox(&self) -> Option<BBox> {
        let pts = self.outers.iter().flatten();
        let (mut s, mut w, mut n, mut e) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in pts {
            s = s.min(p.lat);
            n = n.max(p.lat);
            w = w.min(p.lon);
            e = e.max(p.lon);
        }
        if w < -180.0 || e > 180.0 || e - w > 180.0 {
            return None;
        }
        // Boundaries may reach past the Web Mercator latitudes `BBox` accepts.
        BBox::new(s.max(-85.0), w, n.min(85.0), e).ok()
    }

    /// Whether `b` lies wholly inside the boundary: every corner inside, and no
    /// boundary edge crossing the box or vertex within it (a hole or an inlet
    /// reaching into the box).
    pub fn contains_bbox(&self, b: &BBox) -> bool {
        let corners = [
            LatLon::new(b.south, b.west),
            LatLon::new(b.south, b.east),
            LatLon::new(b.north, b.east),
            LatLon::new(b.north, b.west),
        ];
        if !corners.iter().all(|&c| self.contains(c)) {
            return false;
        }
        let edges = [
            (corners[0], corners[1]),
            (corners[1], corners[2]),
            (corners[2], corners[3]),
            (corners[3], corners[0]),
        ];
        self.outers.iter().chain(&self.holes).all(|ring| {
            ring.iter().all(|p| !b.contains(*p))
                && ring_edges(ring)
                    .all(|(a, c)| edges.iter().all(|&(p, q)| !segments_cross(a, c, p, q)))
        })
    }

    /// Even-odd test over all rings, so holes (and a ring nested in a hole) work.
    fn contains(&self, p: LatLon) -> bool {
        let crossings = self
            .outers
            .iter()
            .chain(&self.holes)
            .filter(|r| point_in_ring(p, r))
            .count();
        crossings % 2 == 1
    }
}

/// Edges of a ring, closing it if the file did not repeat the first point.
fn ring_edges(ring: &[LatLon]) -> impl Iterator<Item = (LatLon, LatLon)> + '_ {
    ring.iter()
        .zip(ring.iter().cycle().skip(1))
        .map(|(a, b)| (*a, *b))
}

fn point_in_ring(p: LatLon, ring: &[LatLon]) -> bool {
    let mut inside = false;
    for (a, b) in ring_edges(ring) {
        if (a.lat > p.lat) != (b.lat > p.lat)
            && p.lon < (b.lon - a.lon) * (p.lat - a.lat) / (b.lat - a.lat) + a.lon
        {
            inside.toggle();
        }
    }
    inside
}

/// Whether segments `ab` and `cd` intersect (touching counts).
fn segments_cross(a: LatLon, b: LatLon, c: LatLon, d: LatLon) -> bool {
    let orient = |p: LatLon, q: LatLon, r: LatLon| {
        (q.lon - p.lon) * (r.lat - p.lat) - (q.lat - p.lat) * (r.lon - p.lon)
    };
    let on_segment = |p: LatLon, q: LatLon, r: LatLon| {
        r.lon >= p.lon.min(q.lon)
            && r.lon <= p.lon.max(q.lon)
            && r.lat >= p.lat.min(q.lat)
            && r.lat <= p.lat.max(q.lat)
    };
    let (d1, d2) = (orient(c, d, a), orient(c, d, b));
    let (d3, d4) = (orient(a, b, c), orient(a, b, d));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    (d1 == 0.0 && on_segment(c, d, a))
        || (d2 == 0.0 && on_segment(c, d, b))
        || (d3 == 0.0 && on_segment(a, b, c))
        || (d4 == 0.0 && on_segment(a, b, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seeded xorshift64* value in `[lo, hi)`: the same inputs every run.
    /// The same generator as `observation-server`'s `test_rng`; keep them alike.
    fn uniform(state: &mut u64, lo: f64, hi: f64) -> f64 {
        *state ^= *state >> 12;
        *state ^= *state << 25;
        *state ^= *state >> 27;
        let x = state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11;
        lo + (hi - lo) * x as f64 / (1u64 << 53) as f64
    }

    /// The `.poly` text of the rectangle `(s, w, n, e)`, with the rectangle
    /// `hole` cut out of it.
    fn rectangle(r: (f64, f64, f64, f64), hole: (f64, f64, f64, f64)) -> String {
        let ring = |(s, w, n, e): (f64, f64, f64, f64)| {
            format!(" {w} {s}\n {e} {s}\n {e} {n}\n {w} {n}\n {w} {s}\nEND\n")
        };
        format!("box\n1\n{}!2\n{}END\n", ring(r), ring(hole))
    }

    /// For any rectangle with a hole: its bounds are the rectangle's; a box
    /// inside it and clear of the hole is inside; a box in the hole, across
    /// the hole's edge, across the outer edge or outside is not.
    #[test]
    fn rectangles_with_holes_hold_exactly_the_boxes_clear_of_the_hole() {
        let mut st = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..2000 {
            let (s, w) = (
                // Up to 85°, the Web Mercator limit of the bounds.
                uniform(&mut st, -84.0, 75.0),
                uniform(&mut st, -170.0, 150.0),
            );
            let (n, e) = (
                s + uniform(&mut st, 1.0, 10.0),
                w + uniform(&mut st, 1.0, 10.0),
            );
            // The hole: the middle third of the rectangle.
            let (hs, hn) = (s + (n - s) / 3.0, n - (n - s) / 3.0);
            let (hw, he) = (w + (e - w) / 3.0, e - (e - w) / 3.0);
            let b = Boundary::parse(&rectangle((s, w, n, e), (hs, hw, hn, he))).unwrap();
            assert_eq!(b.bbox(), BBox::new(s, w, n, e).ok());

            let bx = |s: f64, w: f64, n: f64, e: f64| BBox::new(s, w, n, e).unwrap();
            // South of the hole, anywhere across the rectangle's width.
            let lo = uniform(&mut st, s + 1e-6, hs - 2e-6);
            let hi = uniform(&mut st, lo + 1e-7, hs - 1e-6);
            let left = uniform(&mut st, w + 1e-6, e - 2e-6);
            let right = uniform(&mut st, left + 1e-7, e - 1e-6);
            assert!(b.contains_bbox(&bx(lo, left, hi, right)), "{s} {w} {n} {e}");
            // In the hole, and across its south edge.
            let (cs, cw) = (
                uniform(&mut st, hs, hn - 1e-3),
                uniform(&mut st, hw, he - 1e-3),
            );
            assert!(
                !b.contains_bbox(&bx(cs, cw, cs + 1e-4, cw + 1e-4)),
                "{s} {w} {n} {e}"
            );
            assert!(
                !b.contains_bbox(&bx(lo, cw, hs + 1e-4, cw + 1e-4)),
                "{s} {w} {n} {e}"
            );
            // Around the whole hole, corners clear of it.
            let (aw, ae) = ((w + hw) / 2.0, (he + e) / 2.0);
            assert!(
                !b.contains_bbox(&bx(lo, aw, (hn + n) / 2.0, ae)),
                "{s} {w} {n} {e}"
            );
            // Touching counts as crossing: a box whose north edge lies on the
            // hole's south edge, one with a corner on the hole's south-west
            // vertex, and one whose south edge lies on the outer edge are not
            // inside.
            assert!(
                !b.contains_bbox(&bx(lo, cw, hs, cw + 1e-4)),
                "{s} {w} {n} {e}"
            );
            assert!(!b.contains_bbox(&bx(lo, aw, hs, hw)), "{s} {w} {n} {e}");
            assert!(!b.contains_bbox(&bx(s, left, hi, right)), "{s} {w} {n} {e}");
            // Across the outer edge, and outside.
            assert!(
                !b.contains_bbox(&bx(lo, e - 1e-4, hi, e + 1e-4)),
                "{s} {w} {n} {e}"
            );
            assert!(
                !b.contains_bbox(&bx(s - 0.2, w, s - 0.1, e)),
                "{s} {w} {n} {e}"
            );
        }
    }

    /// A 10° square (0..10, 0..10) with a 2° hole at 4..6, plus an island at 20..21.
    const POLY: &str = "test
1
   0.0   0.0
   10.0  0.0
   10.0  10.0
   0.0   10.0
   0.0   0.0
END
!2
   4.0 4.0
   6.0 4.0
   6.0 6.0
   4.0 6.0
END
island
   20.0 20.0
   21.0 20.0
   21.0 21.0
END
END
";

    fn bbox(s: f64, w: f64, n: f64, e: f64) -> BBox {
        BBox::new(s, w, n, e).unwrap()
    }

    #[test]
    fn parses_rings_and_holes() {
        let b = Boundary::parse(POLY).unwrap();
        assert_eq!(b.outers.len(), 2);
        assert_eq!(b.holes.len(), 1);
        let bb = b.bbox().unwrap();
        assert_eq!(
            (bb.south, bb.west, bb.north, bb.east),
            (0.0, 0.0, 21.0, 21.0)
        );
    }

    #[test]
    fn containment() {
        let b = Boundary::parse(POLY).unwrap();
        assert!(b.contains_bbox(&bbox(1.0, 1.0, 2.0, 2.0)));
        // In the hole, overlapping the hole, straddling the edge, outside.
        assert!(!b.contains_bbox(&bbox(4.5, 4.5, 5.0, 5.0)));
        assert!(!b.contains_bbox(&bbox(3.0, 3.0, 4.5, 4.5)));
        assert!(!b.contains_bbox(&bbox(9.0, 9.0, 11.0, 11.0)));
        assert!(!b.contains_bbox(&bbox(30.0, 30.0, 31.0, 31.0)));
        // The hole lies wholly inside this box: no corner is in it, but it is.
        assert!(!b.contains_bbox(&bbox(3.0, 3.0, 7.0, 7.0)));
        // Inside the triangular island.
        assert!(b.contains_bbox(&bbox(20.1, 20.5, 20.2, 20.6)));
    }

    #[test]
    fn antimeridian_boundaries_have_no_bbox() {
        let fiji = "fiji\n1\n 177 -19\n 180.5 -19\n 180.5 -16\n 177 -16\nEND\n2\n -180 -19\n -179 -19\n -179 -16\nEND\nEND\n";
        assert_eq!(Boundary::parse(fiji).unwrap().bbox(), None);
    }

    #[test]
    fn rejects_bad_files() {
        assert_eq!(Boundary::parse(""), Err(PolyError::Empty));
        assert_eq!(Boundary::parse("x\nEND\n"), Err(PolyError::Empty));
        assert_eq!(
            Boundary::parse("x\n1\n 1 2\n"),
            Err(PolyError::Unterminated)
        );
        assert!(matches!(
            Boundary::parse("x\n1\n 1 nope\nEND\nEND\n"),
            Err(PolyError::Line(_))
        ));
    }

    #[test]
    fn parses_geofabrik_scientific_notation() {
        let text = "none\n1\n   7.456283E+00   4.374326E+01\n   7.595671E+00   4.352067E+01\n   7.487181E+00   4.348382E+01\nEND\nEND\n";
        let b = Boundary::parse(text).unwrap();
        assert!((b.outers[0][0].lat - 43.74326).abs() < 1e-9);
    }
}
