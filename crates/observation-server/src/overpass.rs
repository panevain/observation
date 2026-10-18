//! Overpass API client and `out geom` JSON parsing.
//!
//! The client is deliberately polite: one request in flight at a time, a
//! descriptive `User-Agent`, at most one short inline retry on 429/504, then a
//! cool-down during which new fetches fail fast with a 503.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use observation_core::building::infer_heights;
use observation_core::bundle::{OsmBuilding, OsmRoad, road_width_m};
use observation_core::geo::{BBox, LatLon};
use reqwest::StatusCode;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::error::ApiError;

/// Largest Overpass response we are willing to buffer.
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// Buildings and roads extracted from one Overpass response.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct OsmData {
    pub buildings: Vec<OsmBuilding>,
    pub roads: Vec<OsmRoad>,
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

/// Builds the single query used per bounding box.
pub fn build_query(bbox: &BBox) -> String {
    let b = bbox.overpass();
    format!(
        "[out:json][timeout:25];\n(\n  way[\"building\"]({b});\n  relation[\"building\"][\"type\"=\"multipolygon\"]({b});\n  way[\"highway\"]({b});\n);\nout geom qt;\n"
    )
}

// ----- JSON model -----------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    elements: Vec<Element>,
    remark: Option<String>,
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
pub fn parse_response(json: &[u8]) -> Result<OsmData, ParseError> {
    let resp: Response = serde_json::from_slice(json)?;
    if let Some(remark) = resp.remark.filter(|r| r.contains("runtime error")) {
        return Err(ParseError::Remark(remark));
    }
    let mut out = OsmData::default();

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
                });
            }
            "way" => {
                if let Some(road) = parse_road(e) {
                    out.roads.push(road);
                }
            }
            "relation" if e.tags.contains_key("building") => parse_relation(e, &mut out.buildings),
            _ => {}
        }
    }
    Ok(out)
}

fn heights(tags: &HashMap<String, String>) -> (f32, f32) {
    infer_heights(|k| tags.get(k).map(String::as_str))
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
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ----- client ----------------------------------------------------------------

/// Rate-limited Overpass client.
#[derive(Debug)]
pub struct OverpassClient {
    client: reqwest::Client,
    url: String,
    in_flight: Semaphore,
    cooldown_until: Mutex<Option<Instant>>,
    retry_wait: Duration,
    cooldown: Duration,
}

impl OverpassClient {
    /// `client` must already carry the project `User-Agent` and timeout.
    pub fn new(client: reqwest::Client, config: &Config) -> Self {
        Self {
            client,
            url: config.overpass_url.clone(),
            in_flight: Semaphore::new(1),
            cooldown_until: Mutex::new(None),
            retry_wait: config.overpass_retry_wait,
            cooldown: config.overpass_cooldown,
        }
    }

    /// Fetches and parses buildings and roads for `bbox`.
    pub async fn fetch(&self, bbox: &BBox) -> Result<OsmData, ApiError> {
        self.check_cooldown()?;
        let _permit = self
            .in_flight
            .acquire()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        // We may have queued behind a request that got us rate limited.
        self.check_cooldown()?;

        let query = build_query(bbox);
        let mut retried = false;
        loop {
            let resp = self
                .client
                .post(&self.url)
                .form(&[("data", query.as_str())])
                .send()
                .await
                .map_err(|e| {
                    tracing::warn!(error = %e.without_url(), "overpass request failed");
                    ApiError::Upstream("could not reach the map data provider".into())
                })?;
            let status = resp.status();
            if status.is_success() {
                if resp
                    .content_length()
                    .is_some_and(|n| n > MAX_RESPONSE_BYTES)
                {
                    return Err(ApiError::Upstream("map data response too large".into()));
                }
                let body = resp.bytes().await.map_err(|e| {
                    tracing::warn!(error = %e.without_url(), "overpass body failed");
                    ApiError::Upstream("map data download failed".into())
                })?;
                return parse_response(&body).map_err(|e| {
                    tracing::warn!(error = %e, "overpass response unusable");
                    ApiError::Upstream("map data provider returned an unusable response".into())
                });
            }
            if matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::GATEWAY_TIMEOUT
            ) {
                if !retried {
                    retried = true;
                    tracing::warn!(%status, "overpass busy, retrying once");
                    tokio::time::sleep(self.retry_wait).await;
                    continue;
                }
                self.start_cooldown();
                return Err(ApiError::RateLimited {
                    retry_after_secs: self.cooldown.as_secs().max(1),
                });
            }
            tracing::warn!(%status, "overpass returned an error status");
            return Err(ApiError::Upstream(format!(
                "map data provider returned {}",
                status.as_u16()
            )));
        }
    }

    fn check_cooldown(&self) -> Result<(), ApiError> {
        let until = *self
            .cooldown_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match until.map(|t| t.saturating_duration_since(Instant::now())) {
            Some(left) if !left.is_zero() => Err(ApiError::RateLimited {
                retry_after_secs: left.as_secs_f64().ceil().max(1.0) as u64,
            }),
            _ => Ok(()),
        }
    }

    fn start_cooldown(&self) {
        *self
            .cooldown_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Instant::now() + self.cooldown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(v: &[(f64, f64)]) -> Vec<LatLon> {
        v.iter().map(|&(a, b)| LatLon::new(a, b)).collect()
    }

    #[test]
    fn query_matches_spec() {
        let q = build_query(&BBox {
            south: 1.0,
            west: 2.0,
            north: 1.1,
            east: 2.1,
        });
        assert!(q.starts_with("[out:json][timeout:25];"));
        assert!(q.contains("way[\"building\"](1.0000000,2.0000000,1.1000000,2.1000000);"));
        assert!(q.contains("relation[\"building\"][\"type\"=\"multipolygon\"]"));
        assert!(q.trim_end().ends_with("out geom qt;"));
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
        {"type":"way","id":1,"tags":{"building":"yes","building:levels":"2"},
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
        {"type":"node","id":9,"lat":0,"lon":0}
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
    fn point_in_ring_works() {
        let ring = pts(&[(0.0, 0.0), (0.0, 4.0), (4.0, 4.0), (4.0, 0.0), (0.0, 0.0)]);
        assert!(point_in_ring(LatLon::new(2.0, 2.0), &ring));
        assert!(!point_in_ring(LatLon::new(5.0, 2.0), &ring));
    }

    #[test]
    fn demo_fixture_parses() {
        // Real OSM data (see fixtures/README.md): ~100 untagged houses + streets.
        let d = parse_response(include_bytes!("../fixtures/demo-overpass.json")).unwrap();
        assert!(d.buildings.len() > 80, "{} buildings", d.buildings.len());
        assert!(d.roads.len() > 10, "{} roads", d.roads.len());
        assert!(
            d.buildings
                .iter()
                .all(|b| b.outer.len() >= 3 && b.height_m > 0.0)
        );
    }
}
