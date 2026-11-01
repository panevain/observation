//! Street addresses from OSM `addr:*` tags, and which address a camera is at.

use crate::building::Footprint;
use crate::occlusion::point_in_ring;
use glam::Vec2;
use serde::{Deserialize, Serialize};

/// A camera farther than this from every addressed building or address point
/// has no address, metres.
pub const MAX_ADDRESS_DISTANCE_M: f32 = 25.0;

/// An address node this close to a building's outline lies on it, metres.
pub const ON_OUTLINE_M: f32 = 0.5;

/// An address with neither `addr:street` nor `addr:place` takes its street
/// from the nearest named road within this distance, metres.
pub const STREET_SEARCH_M: f32 = 100.0;

/// Longest tag value kept; OSM allows 255 characters, addresses need far fewer.
const MAX_TAG_CHARS: usize = 80;

fn clip(v: &str) -> String {
    v.chars().take(MAX_TAG_CHARS).collect()
}

/// The parts of an OSM address that tell neighbouring houses apart. City,
/// postcode and country are left out: every address in an area shares them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Address {
    /// `addr:housenumber`.
    pub housenumber: Option<String>,
    /// `addr:street`, or `addr:place` where there is no street, or else the
    /// nearest road's name (see [`Address::fill_street`]).
    pub street: Option<String>,
    /// `addr:housename`.
    pub housename: Option<String>,
    /// `addr:unit`.
    pub unit: Option<String>,
}

impl Address {
    /// Reads `addr:*` tags; `None` unless there is a house number or house name.
    pub fn from_tags<'a>(tag: impl Fn(&str) -> Option<&'a str>) -> Option<Self> {
        let get = |k: &str| tag(k).map(str::trim).filter(|v| !v.is_empty()).map(clip);
        let a = Self {
            housenumber: get("addr:housenumber"),
            street: get("addr:street").or_else(|| get("addr:place")),
            housename: get("addr:housename"),
            unit: get("addr:unit"),
        };
        (a.housenumber.is_some() || a.housename.is_some()).then_some(a)
    }

    /// Without a street, takes the name of the road nearest `p` within
    /// [`STREET_SEARCH_M`], as geocoders such as Nominatim do. Some mappers put
    /// the street on an `associatedStreet` relation instead, which the query
    /// does not fetch; without this, two houses tagged only `12` would share
    /// one label and so one group. `streets` are names with world polylines.
    pub fn fill_street<'a>(
        &mut self,
        p: [f32; 2],
        streets: impl IntoIterator<Item = (&'a str, &'a [[f32; 2]])>,
    ) {
        if self.street.is_some() {
            return;
        }
        let mut best: Option<(f32, &str)> = None;
        for (name, points) in streets {
            let d = points
                .windows(2)
                .map(|w| distance_to_segment(p, w[0], w[1]))
                .fold(f32::INFINITY, f32::min);
            if d <= STREET_SEARCH_M && best.is_none_or(|(b, _)| d < b) {
                best = Some((d, name));
            }
        }
        self.street = best
            .map(|(_, n)| n.trim())
            .filter(|n| !n.is_empty())
            .map(clip);
    }

    /// One line, such as `10011 Northeast 120th Street` or `Rose Cottage`.
    ///
    /// The number comes before the street (the North American and British
    /// order); the bundle does not say which country an area is in.
    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(4);
        match (&self.housenumber, &self.housename) {
            (Some(n), _) => parts.push(n),
            (None, Some(name)) => parts.push(name),
            (None, None) => {}
        }
        parts.extend(self.street.as_deref());
        let mut s = parts.join(" ");
        if let Some(u) = &self.unit {
            s.push_str(", unit ");
            s.push_str(u);
        }
        s
    }
}

/// An address mapped as a point (an OSM node), typically an entrance or one
/// of several addresses inside a building.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddressPoint {
    pub address: Address,
    /// World `[x, z]`.
    pub position: [f32; 2],
    /// Index into the bundle's buildings of the building the point lies in.
    pub building: Option<u32>,
}

/// Something in the area that carries an address.
#[derive(Debug, Clone, Copy)]
pub enum AddressSource<'a> {
    /// A building with its own `addr:*` tags.
    Building(&'a Footprint, &'a Address),
    Point(&'a AddressPoint),
}

/// The address a camera at world `[x, z]` belongs to.
///
/// The camera belongs to whatever is nearest: a building with an address
/// (distance 0 inside it), an address point inside a building (measured to
/// that building, so a camera on a wall finds the building's address even
/// when the address is a node; with several, the closest node wins), or a free
/// address point. A building's own address beats points inside it. Nothing
/// within [`MAX_ADDRESS_DISTANCE_M`] gives `None`.
///
/// `footprint` looks up a building by its index in the bundle, for points that
/// lie in one.
pub fn address_at<'a>(
    p: [f32; 2],
    footprint: impl Fn(u32) -> Option<&'a Footprint>,
    sources: impl IntoIterator<Item = AddressSource<'a>>,
) -> Option<&'a Address> {
    let max = MAX_ADDRESS_DISTANCE_M;
    let mut best: Option<((f32, f32), &Address)> = None;
    for s in sources {
        let (score, address) = match s {
            AddressSource::Building(f, a) => {
                if !near_bounds(p, &f.outer, max) {
                    continue;
                }
                ((distance_to_footprint(p, f), 0.0), a)
            }
            AddressSource::Point(ap) => {
                let to_point = dist(p, ap.position);
                let building = ap
                    .building
                    .and_then(&footprint)
                    .filter(|f| near_bounds(p, &f.outer, max));
                match building {
                    Some(f) => ((distance_to_footprint(p, f), to_point), &ap.address),
                    None if ap.building.is_none() => ((to_point, 0.0), &ap.address),
                    None => continue,
                }
            }
        };
        if score.0 <= max && best.is_none_or(|(b, _)| score < b) {
            best = Some((score, address));
        }
    }
    best.map(|(_, a)| a)
}

pub(crate) fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Cheap reject: `p` is more than `margin` outside the ring's bounding box.
pub(crate) fn near_bounds(p: [f32; 2], ring: &[[f32; 2]], margin: f32) -> bool {
    within_bounds(p, ring_bounds(ring), margin)
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

/// `p` lies in the box `(lo, hi)` grown by `margin`.
fn within_bounds(p: [f32; 2], (lo, hi): ([f32; 2], [f32; 2]), margin: f32) -> bool {
    (0..2).all(|i| p[i] >= lo[i] - margin && p[i] <= hi[i] + margin)
}

/// Plan-view distance from `p` to the footprint's outer ring; 0 inside it.
pub fn distance_to_footprint(p: [f32; 2], f: &Footprint) -> f32 {
    if point_in_ring(Vec2::from(p), &f.outer) {
        return 0.0;
    }
    distance_to_ring(p, &f.outer)
}

/// Distance from `p` to the nearest edge of `ring`, inside or out.
pub fn distance_to_ring(p: [f32; 2], ring: &[[f32; 2]]) -> f32 {
    ring_edges(ring)
        .map(|(a, b)| distance_to_segment(p, a, b))
        .fold(f32::INFINITY, f32::min)
}

/// Index of the footprint an address node at `p` belongs to, given each
/// footprint's [`ring_bounds`]: the first whose outer ring holds `p` outside
/// its holes, else the first whose outline `p` lies on (within
/// [`ON_OUTLINE_M`]). Entrance nodes are vertices of the outline, where the
/// even-odd test can go either way.
pub fn building_of(
    p: [f32; 2],
    footprints: &[&Footprint],
    bounds: &[([f32; 2], [f32; 2])],
) -> Option<usize> {
    let candidates = || footprints.iter().zip(bounds).enumerate();
    candidates()
        .find(|(_, (f, b))| {
            within_bounds(p, **b, 0.0)
                && point_in_ring(Vec2::from(p), &f.outer)
                && !f.holes.iter().any(|h| point_in_ring(Vec2::from(p), h))
        })
        .or_else(|| {
            candidates().find(|(_, (f, b))| {
                within_bounds(p, **b, ON_OUTLINE_M) && distance_to_ring(p, &f.outer) <= ON_OUTLINE_M
            })
        })
        .map(|(i, _)| i)
}

fn ring_edges(ring: &[[f32; 2]]) -> impl Iterator<Item = ([f32; 2], [f32; 2])> + '_ {
    let n = ring.len();
    (0..n).map(move |i| (ring[i], ring[(i + 1) % n]))
}

fn distance_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, [a[0] + t * dx, a[1] + t * dz])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tags(v: &[(&str, &str)]) -> HashMap<String, String> {
        v.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn parse(v: &[(&str, &str)]) -> Option<Address> {
        let t = tags(v);
        Address::from_tags(|k| t.get(k).map(String::as_str))
    }

    fn square(x: f32, z: f32, side: f32) -> Footprint {
        Footprint {
            outer: vec![[x, z], [x + side, z], [x + side, z + side], [x, z + side]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.0,
            base_m: 0.0,
        }
    }

    fn at<'a>(
        p: [f32; 2],
        fps: &[&'a Footprint],
        src: impl IntoIterator<Item = AddressSource<'a>>,
    ) -> Option<&'a Address> {
        address_at(p, |i| fps.get(i as usize).copied(), src)
    }

    fn addr(n: &str) -> Address {
        Address {
            housenumber: Some(n.into()),
            street: Some("Main St".into()),
            ..Address::default()
        }
    }

    #[test]
    fn reads_and_formats_tags() {
        let a = parse(&[
            ("addr:housenumber", " 10011 "),
            ("addr:street", "Northeast 120th Street"),
            ("addr:city", "Kirkland"),
        ])
        .unwrap();
        assert_eq!(a.label(), "10011 Northeast 120th Street");
        let a = parse(&[("addr:housename", "Rose Cottage"), ("addr:place", "Upton")]).unwrap();
        assert_eq!(a.label(), "Rose Cottage Upton");
        let a = parse(&[
            ("addr:housenumber", "12"),
            ("addr:street", "High St"),
            ("addr:unit", "B"),
        ])
        .unwrap();
        assert_eq!(a.label(), "12 High St, unit B");
        // A street alone does not tell houses apart.
        assert_eq!(parse(&[("addr:street", "High St")]), None);
        assert_eq!(parse(&[("addr:housenumber", "  ")]), None);
        let long = "9".repeat(500);
        let a = parse(&[("addr:housenumber", &long)]).unwrap();
        assert_eq!(a.housenumber.unwrap().len(), MAX_TAG_CHARS);
    }

    #[test]
    fn missing_streets_come_from_the_nearest_road() {
        let (main, high) = (
            vec![[0.0, 0.0], [100.0, 0.0]],
            vec![[0.0, 50.0], [100.0, 50.0]],
        );
        let streets = || [("Main St", &main[..]), ("High St", &high[..])];
        let mut a = parse(&[("addr:housenumber", "12")]).unwrap();
        let mut b = a.clone();
        a.fill_street([30.0, 10.0], streets());
        b.fill_street([30.0, 45.0], streets());
        assert_eq!(a.label(), "12 Main St");
        assert_eq!(b.label(), "12 High St");
        // A tagged street or place is kept, and a road too far away is not used.
        let mut c = parse(&[("addr:housenumber", "3"), ("addr:place", "Upton")]).unwrap();
        c.fill_street([30.0, 10.0], streets());
        assert_eq!(c.label(), "3 Upton");
        let mut d = parse(&[("addr:housenumber", "4")]).unwrap();
        d.fill_street([30.0, 200.0], streets());
        assert_eq!(d.label(), "4");
    }

    #[test]
    fn distances() {
        let f = square(0.0, 0.0, 10.0);
        assert_eq!(distance_to_footprint([5.0, 5.0], &f), 0.0);
        assert!((distance_to_footprint([-0.15, 5.0], &f) - 0.15).abs() < 1e-6);
        assert!((distance_to_footprint([13.0, 14.0], &f) - 5.0).abs() < 1e-5);
    }

    #[test]
    fn nearest_building_wins() {
        let (a, b) = (square(0.0, 0.0, 10.0), square(20.0, 0.0, 10.0));
        let (a1, a2) = (addr("1"), addr("2"));
        let fps = [&a, &b];
        let src = || {
            [
                AddressSource::Building(&a, &a1),
                AddressSource::Building(&b, &a2),
            ]
        };
        // On a's east wall, nearer a than b.
        assert_eq!(at([10.15, 5.0], &fps, src()), Some(&a1));
        // A pole in b's front yard.
        assert_eq!(at([25.0, 14.0], &fps, src()), Some(&a2));
        // Far from everything.
        assert_eq!(at([200.0, 0.0], &fps, src()), None);
    }

    #[test]
    fn points_inside_buildings_count_as_the_building() {
        // A terrace mapped as one building with two entrance nodes, and a
        // house with its own address 2 m away.
        let terrace = square(0.0, 0.0, 20.0);
        let house = square(22.0, 0.0, 10.0);
        let fps = [&terrace, &house];
        let p1 = AddressPoint {
            address: addr("1"),
            position: [5.0, 19.0],
            building: Some(0),
        };
        let p3 = AddressPoint {
            address: addr("3"),
            position: [15.0, 19.0],
            building: Some(0),
        };
        let h = addr("5");
        let src = || {
            [
                AddressSource::Point(&p1),
                AddressSource::Point(&p3),
                AddressSource::Building(&house, &h),
            ]
        };
        // On the terrace's front wall: the nearer entrance.
        assert_eq!(at([4.0, 20.15], &fps, src()), Some(&p1.address));
        assert_eq!(at([16.0, 20.15], &fps, src()), Some(&p3.address));
        // On the terrace's east wall, 1.85 m from the house but 0.15 m from
        // the terrace, whose entrance nodes are 5 m away: still the terrace.
        assert_eq!(at([20.15, 10.0], &fps, src()), Some(&p3.address));
        // A building's own address beats nodes inside it.
        let shop = AddressPoint {
            address: addr("5A"),
            position: [27.0, 5.0],
            building: Some(1),
        };
        let both = [
            AddressSource::Point(&shop),
            AddressSource::Building(&house, &h),
        ];
        assert_eq!(at([27.0, 10.15], &fps, both), Some(&h));
    }

    #[test]
    fn free_points_use_their_own_position() {
        let p = AddressPoint {
            address: addr("9"),
            position: [0.0, 0.0],
            building: None,
        };
        let src = [AddressSource::Point(&p)];
        assert_eq!(at([3.0, 4.0], &[], src), Some(&p.address));
        assert_eq!(at([30.0, 0.0], &[], src), None);
    }

    #[test]
    fn entrance_nodes_on_the_outline_belong_to_the_building() {
        // A front wall at z = 20 with an entrance vertex at x = 5: the
        // even-odd test calls that vertex outside.
        let f = Footprint {
            outer: vec![
                [0.0, 0.0],
                [20.0, 0.0],
                [20.0, 20.0],
                [5.0, 20.0],
                [0.0, 20.0],
            ],
            ..square(0.0, 0.0, 20.0)
        };
        let other = square(30.0, 0.0, 10.0);
        let fps = [&f, &other];
        let bounds: Vec<_> = fps.iter().map(|f| ring_bounds(&f.outer)).collect();
        assert!(!point_in_ring(Vec2::new(5.0, 20.0), &f.outer));
        assert_eq!(building_of([5.0, 20.0], &fps, &bounds), Some(0));
        assert_eq!(building_of([5.0, 10.0], &fps, &bounds), Some(0));
        assert_eq!(building_of([35.0, 5.0], &fps, &bounds), Some(1));
        assert_eq!(building_of([25.0, 5.0], &fps, &bounds), None);
    }
}
