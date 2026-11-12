//! Parsing Overpass `out geom` JSON into buildings, roads and addresses.

use std::collections::{HashMap, HashSet};

use observation_core::address::Address;
use observation_core::building::infer_heights;
use observation_core::bundle::{OsmAddressPoint, OsmBuilding, OsmRoad, road_width_m};
use observation_core::geo::LatLon;
use observation_core::provenance::parse_utc;
use serde::Deserialize;
use thiserror::Error;

/// Buildings, roads and address nodes extracted from one Overpass response.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct OsmData {
    pub buildings: Vec<OsmBuilding>,
    pub roads: Vec<OsmRoad>,
    pub addresses: Vec<OsmAddressPoint>,
    /// When the data was current, Unix seconds: `osm3s.timestamp_osm_base`.
    /// `None` when the response has none, or an empty or unreadable one (a
    /// database imported without a data date reports `""`).
    pub osm_base_unix: Option<u64>,
}

/// The Overpass response could not be used.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid Overpass JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Overpass answered 200 but reported a runtime error (e.g. timeout) with a
    /// partial result in `remark`.
    #[error("Overpass reported an error: {0}")]
    Remark(String),
}

// ----- JSON model -----------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    elements: Vec<Element>,
    remark: Option<String>,
    #[serde(default)]
    osm3s: Osm3s,
}

/// The response's metadata block.
#[derive(Debug, Default, Deserialize)]
struct Osm3s {
    /// When the database's data was current, `YYYY-MM-DDTHH:MM:SSZ`.
    #[serde(default)]
    timestamp_osm_base: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Element {
    #[serde(rename = "type")]
    kind: String,
    id: i64,
    #[serde(default)]
    tags: HashMap<String, String>,
    /// Way geometry. Entries can be `null` when a node is missing.
    #[serde(default)]
    geometry: Vec<Option<Pt>>,
    #[serde(default)]
    members: Vec<Member>,
    /// Node position.
    lat: Option<f64>,
    lon: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Member {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "ref", default)]
    ref_: Option<i64>,
    #[serde(default)]
    role: String,
    #[serde(default)]
    geometry: Vec<Option<Pt>>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct Pt {
    lat: f64,
    lon: f64,
}

impl From<Pt> for LatLon {
    fn from(p: Pt) -> Self {
        LatLon::new(p.lat, p.lon)
    }
}

/// Converts raw geometry to points; `None` if any node is missing.
fn complete_geometry(g: &[Option<Pt>]) -> Option<Vec<LatLon>> {
    g.iter().map(|p| p.map(LatLon::from)).collect()
}

// ----- parsing --------------------------------------------------------------

/// Parses an Overpass `out geom` JSON document.
///
/// * Closed `building` ways with ≥ 4 points become buildings.
/// * `building` multipolygon relations are stitched into rings; every outer
///   ring becomes a building with the inner rings it contains as holes. Their
///   `osm_id` is the **negated relation id** so it can never clash with a way
///   id (all outers of one relation share it).
/// * `highway` ways become roads when [`road_width_m`] knows the type; `area=yes`
///   highways (plazas, car parks) are skipped.
/// * Buildings keep their `addr:*` tags, and nodes with a house number or
///   house name become address points.
/// * `osm3s.timestamp_osm_base` becomes [`OsmData::osm_base_unix`].
pub fn parse_response(json: &[u8]) -> Result<OsmData, ParseError> {
    let resp: Response = serde_json::from_slice(json)?;
    if let Some(remark) = resp.remark.filter(|r| r.contains("runtime error")) {
        return Err(ParseError::Remark(remark));
    }
    let mut out = OsmData {
        osm_base_unix: resp.osm3s.timestamp_osm_base.as_deref().and_then(parse_utc),
        ..OsmData::default()
    };

    // Outer-role ways of building relations are covered by the relation itself.
    let relation_member_ways: HashSet<i64> = resp
        .elements
        .iter()
        .filter(|e| e.kind == "relation" && e.tags.contains_key("building"))
        .flat_map(|e| e.members.iter())
        .filter(|m| m.kind == "way" && m.role == "outer")
        .filter_map(|m| m.ref_)
        .collect();

    for e in &resp.elements {
        match e.kind.as_str() {
            "way" if e.tags.contains_key("building") => {
                if relation_member_ways.contains(&e.id) {
                    continue;
                }
                let Some(ring) = complete_geometry(&e.geometry) else {
                    continue;
                };
                if ring.len() < 4 || ring.first() != ring.last() {
                    continue;
                }
                let (min_height_m, height_m) = heights(&e.tags);
                out.buildings.push(OsmBuilding {
                    osm_id: e.id,
                    outer: ring,
                    holes: vec![],
                    min_height_m,
                    height_m,
                    address: address(&e.tags),
                });
            }
            "way" => {
                if let Some(road) = parse_road(e) {
                    out.roads.push(road);
                }
            }
            "relation" if e.tags.contains_key("building") => parse_relation(e, &mut out.buildings),
            "node" => {
                let point = e
                    .lat
                    .zip(e.lon)
                    .filter(|(a, b)| a.is_finite() && b.is_finite());
                if let Some(((lat, lon), address)) = point.zip(address(&e.tags)) {
                    out.addresses.push(OsmAddressPoint {
                        address,
                        point: LatLon::new(lat, lon),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn heights(tags: &HashMap<String, String>) -> (f32, f32) {
    infer_heights(|k| tags.get(k).map(String::as_str))
}

fn address(tags: &HashMap<String, String>) -> Option<Address> {
    Address::from_tags(|k| tags.get(k).map(String::as_str))
}

fn parse_road(e: &Element) -> Option<OsmRoad> {
    let highway = e.tags.get("highway")?;
    if e.tags.get("area").is_some_and(|v| v == "yes") {
        return None;
    }
    let width_m = road_width_m(
        highway,
        e.tags.get("lanes").map(String::as_str),
        e.tags.get("width").map(String::as_str),
    )?;
    let points: Vec<LatLon> = e
        .geometry
        .iter()
        .flatten()
        .map(|p| LatLon::from(*p))
        .collect();
    (points.len() >= 2).then(|| OsmRoad {
        osm_id: e.id,
        highway: highway.clone(),
        name: e.tags.get("name").cloned(),
        width_m,
        points,
    })
}

fn parse_relation(e: &Element, out: &mut Vec<OsmBuilding>) {
    let ways = |role: &str| -> Vec<Vec<LatLon>> {
        e.members
            .iter()
            .filter(|m| m.kind == "way" && m.role == role)
            .filter_map(|m| complete_geometry(&m.geometry))
            .filter(|g| g.len() >= 2)
            .collect()
    };
    let outers = stitch_rings(ways("outer"));
    let inners = stitch_rings(ways("inner"));
    let (min_height_m, height_m) = heights(&e.tags);
    let address = address(&e.tags);

    let mut holes: Vec<Vec<Vec<LatLon>>> = vec![Vec::new(); outers.len()];
    for inner in inners {
        let Some(first) = inner.first().copied() else {
            continue;
        };
        if let Some(i) = outers.iter().position(|o| point_in_ring(first, o)) {
            holes[i].push(inner);
        }
    }
    for (outer, holes) in outers.into_iter().zip(holes) {
        out.push(OsmBuilding {
            osm_id: -e.id,
            outer,
            holes,
            min_height_m,
            height_m,
            address: address.clone(),
        });
    }
}

/// Joins way geometries end to end (reversing where needed) into closed rings
/// with ≥ 4 points (first == last). Segments that cannot be closed are dropped.
pub fn stitch_rings(mut segments: Vec<Vec<LatLon>>) -> Vec<Vec<LatLon>> {
    // Exact comparison is correct: shared nodes carry identical coordinates.
    let same = |a: &LatLon, b: &LatLon| a.lat == b.lat && a.lon == b.lon;
    let closed =
        |r: &[LatLon]| r.len() >= 4 && r.first().zip(r.last()).is_some_and(|(a, b)| same(a, b));

    let mut rings = Vec::new();
    while let Some(mut current) = segments.pop() {
        while !closed(&current) {
            let Some(end) = current.last().copied() else {
                break;
            };
            let next = segments.iter().position(|s| {
                s.first().is_some_and(|p| same(p, &end)) || s.last().is_some_and(|p| same(p, &end))
            });
            let Some(i) = next else { break };
            let mut seg = segments.swap_remove(i);
            if !seg.first().is_some_and(|p| same(p, &end)) {
                seg.reverse();
            }
            current.extend(seg.into_iter().skip(1));
        }
        if closed(&current) {
            rings.push(current);
        }
    }
    rings
}

/// Even-odd point-in-polygon test in lat/lon space (areas are tiny).
fn point_in_ring(p: LatLon, ring: &[LatLon]) -> bool {
    let mut inside = false;
    let mut j = ring.len().wrapping_sub(1);
    for (i, a) in ring.iter().enumerate() {
        let b = &ring[j];
        if (a.lat > p.lat) != (b.lat > p.lat)
            && p.lon < (b.lon - a.lon) * (p.lat - a.lat) / (b.lat - a.lat) + a.lon
        {
            inside.toggle();
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed ring cut into pieces, shuffled and some reversed, is
    /// stitched back into that one ring, its points in the ring's cyclic
    /// order; without one piece it is dropped. Neither the first point nor
    /// the direction is promised (they follow the last piece given), and
    /// the caller places holes by an even-odd test, which ignores both.
    #[test]
    fn rings_cut_into_pieces_are_stitched_back() {
        let mut rng = crate::test_rng::Rng::new(9);
        for _ in 0..2000 {
            let n = 3 + rng.below(10) as usize;
            let mut ring: Vec<LatLon> = (0..n)
                .map(|i| LatLon {
                    lat: i as f64 + rng.f64(0.0, 0.5),
                    lon: rng.f64(-1.0, 1.0),
                })
                .collect();
            ring.push(ring[0]);
            let mut cuts: Vec<usize> = (1..n).filter(|_| rng.below(3) == 0).collect();
            cuts.insert(0, 0);
            cuts.push(n);
            let mut pieces: Vec<Vec<LatLon>> = cuts
                .windows(2)
                .map(|w| ring[w[0]..=w[1]].to_vec())
                .collect();
            for p in &mut pieces {
                if rng.below(2) == 0 {
                    p.reverse();
                }
            }
            rng.shuffle(&mut pieces);
            let key = |p: &LatLon| (p.lat.to_bits(), p.lon.to_bits());
            let want: Vec<_> = ring[..n].iter().map(key).collect();

            let rings = stitch_rings(pieces.clone());
            assert_eq!(rings.len(), 1, "{pieces:?}");
            let got = &rings[0];
            assert_eq!(got.len(), n + 1, "{pieces:?}");
            assert_eq!(key(&got[0]), key(&got[n]), "closed");
            // From where `got` starts in `ring`, forward or backward.
            let start = want.iter().position(|p| *p == key(&got[0])).unwrap();
            let forward = Vec::from_fn(n, |i| want[(start + i) % n]);
            let backward = Vec::from_fn(n, |i| want[(start + n - i) % n]);
            let points: Vec<_> = got[..n].iter().map(key).collect();
            assert!(points == forward || points == backward, "{pieces:?}");

            if pieces.len() > 1 {
                pieces.remove(rng.below(pieces.len() as u64) as usize);
                assert!(stitch_rings(pieces.clone()).is_empty(), "{pieces:?}");
            }
        }
    }

    fn pts(v: &[(f64, f64)]) -> Vec<LatLon> {
        v.iter().map(|&(a, b)| LatLon::new(a, b)).collect()
    }

    #[test]
    fn stitches_split_reversed_and_drops_open() {
        let a = pts(&[(0.0, 0.0), (0.0, 1.0), (1.0, 1.0)]);
        // Second half is stored reversed.
        let b = pts(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]);
        let open = pts(&[(5.0, 5.0), (5.0, 6.0), (6.0, 6.0)]);
        let closed = pts(&[(9.0, 9.0), (9.0, 10.0), (10.0, 10.0), (9.0, 9.0)]);
        let rings = stitch_rings(vec![a, open, b, closed.clone()]);
        assert_eq!(rings.len(), 2);
        let big = rings.iter().find(|r| r.len() == 5).expect("stitched ring");
        assert_eq!(big.first(), big.last());
        assert!(rings.contains(&closed));
    }

    const MULTI: &str = r#"{
      "elements": [
        {"type":"way","id":1,"tags":{"building":"yes","building:levels":"2","addr:housenumber":"7","addr:street":"Main St"},
         "geometry":[{"lat":0,"lon":0},{"lat":0,"lon":1},{"lat":1,"lon":1},{"lat":1,"lon":0},{"lat":0,"lon":0}]},
        {"type":"way","id":2,"tags":{"building":"yes"},"geometry":[{"lat":0,"lon":0},{"lat":0,"lon":1},{"lat":1,"lon":1}]},
        {"type":"way","id":3,"tags":{"building":"yes"},"geometry":[{"lat":0,"lon":0},{"lat":0,"lon":1},{"lat":0,"lon":0}]},
        {"type":"way","id":30,"tags":{"building":"yes"},"geometry":[{"lat":20,"lon":20},null,{"lat":21,"lon":21},{"lat":20,"lon":21},{"lat":20,"lon":20}]},
        {"type":"way","id":4,"tags":{"highway":"residential","name":"Main St","lanes":"2"},"geometry":[{"lat":0,"lon":0},{"lat":0,"lon":1}]},
        {"type":"way","id":5,"tags":{"highway":"pedestrian","area":"yes"},"geometry":[{"lat":0,"lon":0},{"lat":0,"lon":1}]},
        {"type":"way","id":6,"tags":{"highway":"proposed"},"geometry":[{"lat":0,"lon":0},{"lat":0,"lon":1}]},
        {"type":"way","id":7,"tags":{"highway":"service"},"geometry":[{"lat":0,"lon":0}]},
        {"type":"way","id":50,"tags":{"building":"yes"},"geometry":[{"lat":10,"lon":10},{"lat":10,"lon":14},{"lat":14,"lon":14},{"lat":14,"lon":10},{"lat":10,"lon":10}]},
        {"type":"relation","id":100,"tags":{"type":"multipolygon","building":"apartments","height":"20"},
         "members":[
          {"type":"way","ref":50,"role":"outer","geometry":[{"lat":10,"lon":10},{"lat":10,"lon":14},{"lat":14,"lon":14}]},
          {"type":"way","ref":51,"role":"outer","geometry":[{"lat":14,"lon":14},{"lat":14,"lon":10},{"lat":10,"lon":10}]},
          {"type":"way","ref":52,"role":"inner","geometry":[{"lat":11,"lon":11},{"lat":11,"lon":12},{"lat":12,"lon":12},{"lat":12,"lon":11},{"lat":11,"lon":11}]},
          {"type":"way","ref":53,"role":"inner","geometry":[{"lat":30,"lon":30},{"lat":30,"lon":31},{"lat":31,"lon":31},{"lat":30,"lon":30}]}
         ]},
        {"type":"relation","id":101,"tags":{"type":"multipolygon","building":"yes"},
         "members":[{"type":"way","ref":60,"role":"outer","geometry":[{"lat":0,"lon":0},{"lat":0,"lon":1},{"lat":1,"lon":1}]}]},
        {"type":"node","id":9,"lat":0,"lon":0},
        {"type":"node","id":10,"lat":12,"lon":13,"tags":{"addr:housenumber":"3","addr:street":"Main St","entrance":"main"}},
        {"type":"node","id":11,"lat":12,"lon":13,"tags":{"addr:street":"Main St"}}
      ]
    }"#;

    #[test]
    fn parses_ways_roads_and_relations() {
        let d = parse_response(MULTI.as_bytes()).unwrap();
        let ids: Vec<i64> = d.buildings.iter().map(|b| b.osm_id).collect();
        // Way 1 only: 2 (open), 3 (<4 pts) and 30 (null node) are skipped; way 50 is
        // an outer member of relation 100 and covered by it; relation 101 cannot close.
        assert_eq!(ids, vec![1, -100]);
        // levels=2 -> 2 * 3 m + 1 m roof allowance; explicit height wins.
        assert_eq!(d.buildings[0].height_m, 7.0);
        assert_eq!(d.buildings[1].height_m, 20.0);
        // The inner ring inside the outer is a hole; the stray one is dropped.
        assert_eq!(d.buildings[1].outer.len(), 5);
        assert_eq!(d.buildings[1].holes.len(), 1);
        assert_eq!(
            d.buildings[0]
                .address
                .as_ref()
                .map(Address::label)
                .as_deref(),
            Some("7 Main St")
        );
        assert_eq!(d.buildings[1].address, None);
        // Node 9 has no tags and node 11 no house number.
        assert_eq!(d.addresses.len(), 1);
        assert_eq!(d.addresses[0].address.label(), "3 Main St");
        assert_eq!(d.addresses[0].point, LatLon::new(12.0, 13.0));

        assert_eq!(d.roads.len(), 1);
        let r = &d.roads[0];
        assert_eq!(
            (r.osm_id, r.highway.as_str(), r.name.as_deref()),
            (4, "residential", Some("Main St"))
        );
        assert_eq!(r.width_m, 6.5);
        assert_eq!(r.points.len(), 2);
    }

    #[test]
    fn rejects_garbage_and_runtime_errors() {
        assert!(matches!(
            parse_response(b"<html>"),
            Err(ParseError::Json(_))
        ));
        let timed_out =
            br#"{"elements":[],"remark":"runtime error: Query timed out in \"query\" at line 4"}"#;
        assert!(matches!(
            parse_response(timed_out),
            Err(ParseError::Remark(_))
        ));
        assert_eq!(
            parse_response(br#"{"elements":[]}"#).unwrap(),
            OsmData::default()
        );
    }

    #[test]
    fn reads_the_data_date() {
        let at = |osm3s: &str| {
            parse_response(format!(r#"{{"osm3s":{osm3s},"elements":[]}}"#).as_bytes())
                .unwrap()
                .osm_base_unix
        };
        assert_eq!(
            at(r#"{"timestamp_osm_base":"2026-10-31T19:43:02Z","copyright":"ODbL"}"#),
            Some(1_793_475_782)
        );
        // A database with no data date reports an empty one; some report none.
        assert_eq!(at(r#"{"timestamp_osm_base":""}"#), None);
        assert_eq!(at(r#"{"copyright":"ODbL"}"#), None);
        assert_eq!(at(r#"{"timestamp_osm_base":"last week"}"#), None);
        assert_eq!(at("{}"), None);
        // Area queries add `timestamp_areas_base`, which is not the data date.
        assert_eq!(
            at(
                r#"{"timestamp_osm_base":"2026-10-31T19:43:02Z","timestamp_areas_base":"2026-10-30T00:00:00Z"}"#
            ),
            Some(1_793_475_782)
        );
    }

    #[test]
    fn point_in_ring_works() {
        let ring = pts(&[(0.0, 0.0), (0.0, 4.0), (4.0, 4.0), (4.0, 0.0), (0.0, 0.0)]);
        assert!(point_in_ring(LatLon::new(2.0, 2.0), &ring));
        assert!(!point_in_ring(LatLon::new(5.0, 2.0), &ring));
    }

    #[test]
    fn demo_fixture_parses() {
        // Real OSM data (see fixtures/README.md): ~100 untagged houses + streets.
        let d = parse_response(include_bytes!("../../fixtures/demo-overpass.json")).unwrap();
        assert!(d.buildings.len() > 80, "{} buildings", d.buildings.len());
        assert!(d.roads.len() > 10, "{} roads", d.roads.len());
        assert!(
            d.buildings
                .iter()
                .all(|b| b.outer.len() >= 3 && b.height_m > 0.0)
        );
        let addressed = d.buildings.iter().filter(|b| b.address.is_some()).count();
        assert!(addressed > 50, "{addressed} addressed buildings");
        // Exact counts and a spot check, so a change to parsing shows here.
        let summary = (
            d.buildings.len(),
            d.buildings.iter().map(|b| b.holes.len()).sum::<usize>(),
            addressed,
            d.roads.len(),
            d.addresses.len(),
        );
        assert_eq!(summary, (108, 0, 61, 37, 205));
        let first = &d.buildings[0];
        assert_eq!(
            (
                first.osm_id,
                first.outer.len(),
                first.height_m,
                first.min_height_m
            ),
            (324_632_903, 13, 6.0, 0.0)
        );
        // Content of the other elements: ring and road points, heights,
        // road widths and addresses.
        let untagged = infer_heights(|_| None);
        let content = (
            d.buildings.iter().map(|b| b.outer.len()).sum::<usize>(),
            d.roads.iter().map(|r| r.points.len()).sum::<usize>(),
            d.buildings
                .iter()
                .filter(|b| (b.min_height_m, b.height_m) != untagged)
                .count(),
            (d.roads.iter().map(|r| f64::from(r.width_m)).sum::<f64>() * 10.0).round() as i64,
        );
        assert_eq!(content, (1198, 223, 0, 1530));
        let address = |number: &str, street: &str, unit: Option<&str>| Address {
            housenumber: Some(number.into()),
            street: Some(street.into()),
            housename: None,
            unit: unit.map(Into::into),
        };
        assert_eq!(
            d.addresses[0],
            OsmAddressPoint {
                address: address("11930", "100th Avenue Northeast", Some("B")),
                point: LatLon::new(47.7075056, -122.2076749),
            }
        );
        let addressed = d.buildings.iter().find_map(|b| b.address.as_ref());
        assert_eq!(
            addressed,
            Some(&address("10011", "Northeast 120th Street", None))
        );
        let road = &d.roads[0];
        assert_eq!(
            (
                road.osm_id,
                road.highway.as_str(),
                road.width_m,
                road.points.len()
            ),
            (6_359_964, "residential", 6.0, 9)
        );
        // Recorded with the fixture (see fixtures/README.md).
        assert_eq!(
            d.osm_base_unix
                .map(observation_core::provenance::format_utc),
            Some("2026-10-31T00:00:00Z".to_owned())
        );
    }
}
