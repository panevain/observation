//! Overpass API client and `out geom` JSON parsing.
//!
//! The client is deliberately polite: one request in flight at a time, a
//! descriptive `User-Agent`, and a bounded retry. A transient failure (connection
//! error, timeout, 429, 502-504) moves on to the next configured interpreter, or
//! retries a lone interpreter once after a short pause. An interpreter that
//! answers 429 or 504 then rests for a cool-down; while every interpreter rests,
//! new fetches fail fast with a 503.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use observation_core::address::Address;
use observation_core::building::infer_heights;
use observation_core::bundle::{OsmAddressPoint, OsmBuilding, OsmRoad, road_width_m};
use observation_core::geo::{BBox, LatLon};
use observation_core::provenance::parse_utc;
use reqwest::StatusCode;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::error::ApiError;

/// Largest Overpass response we are willing to buffer.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

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

/// Builds the single query used per bounding box.
pub fn build_query(bbox: &BBox) -> String {
    let b = bbox.overpass();
    format!(
        "[out:json][timeout:25];\n(\n  way[\"building\"]({b});\n  relation[\"building\"][\"type\"=\"multipolygon\"]({b});\n  way[\"highway\"]({b});\n  node[\"addr:housenumber\"]({b});\n  node[\"addr:housename\"]({b});\n);\nout geom qt;\n"
    )
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
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ----- client ----------------------------------------------------------------

/// Why one Overpass attempt failed, when it is worth trying again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transient {
    /// Connection error (including a connect timeout), 502 or 503.
    Failed,
    /// No answer in time, or Overpass reported that the query timed out.
    TimedOut,
    /// 429 or 504: the interpreter is overloaded; it rests for a cool-down.
    Busy,
}

impl Transient {
    fn of(e: &reqwest::Error) -> Self {
        // A connect timeout means the host was never reached, not that it was slow.
        if e.is_timeout() && !e.is_connect() {
            Self::TimedOut
        } else {
            Self::Failed
        }
    }
}

/// How one attempt failed.
#[derive(Debug)]
enum AttemptError {
    Retry(Transient),
    /// Not worth retrying (other statuses, oversized or unusable bodies).
    Fatal(ApiError),
}

/// Rate-limited Overpass client.
#[derive(Debug)]
pub struct OverpassClient {
    client: reqwest::Client,
    /// Interpreters tried in order; transient failures move on to the next one.
    urls: Vec<String>,
    in_flight: Semaphore,
    /// Requests waiting for `in_flight`; bounded so distinct boxes can't build a backlog.
    waiting: std::sync::atomic::AtomicUsize,
    /// Per interpreter: no requests to it until then, after it answered 429 or 504.
    cooldown_until: Mutex<Vec<Option<Instant>>>,
    retry_wait: Duration,
    cooldown: Duration,
    /// Longest single attempt.
    attempt_timeout: Duration,
    /// Longest whole fetch, queueing included, so that retries stop before the
    /// HTTP request they serve times out.
    budget: Duration,
}

/// Most Overpass requests allowed to queue behind the one in flight.
const MAX_WAITING: usize = 3;

/// Decrements the waiting counter when dropped.
struct WaitGuard<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl OverpassClient {
    /// `client` must already carry the project `User-Agent`.
    pub fn new(client: reqwest::Client, config: &Config) -> Self {
        let mut urls = config.overpass_urls.clone();
        if urls.is_empty() {
            urls.push(crate::config::DEFAULT_OVERPASS_URL.to_owned());
        }
        Self {
            client,
            cooldown_until: Mutex::new(vec![None; urls.len()]),
            urls,
            in_flight: Semaphore::new(1),
            waiting: std::sync::atomic::AtomicUsize::new(0),
            retry_wait: config.overpass_retry_wait,
            cooldown: config.overpass_cooldown,
            attempt_timeout: config.overpass_timeout,
            // Leave a tenth of the request's time to build and send the reply.
            budget: config.request_timeout.mul_f64(0.9),
        }
    }

    /// Fetches and parses buildings and roads for `bbox`.
    pub async fn fetch(&self, bbox: &BBox) -> Result<OsmData, ApiError> {
        self.fetch_since(bbox, Instant::now()).await
    }

    /// As [`Self::fetch`], for a request that began at `started` (time already
    /// spent, such as on a local region, counts against the budget).
    pub async fn fetch_since(&self, bbox: &BBox, started: Instant) -> Result<OsmData, ApiError> {
        let result = self.fetch_inner(bbox, started).await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(ApiError::RateLimited { .. }) => "rate_limited",
            Err(_) => "error",
        };
        metrics::counter!(crate::metrics::OVERPASS_FETCH, "outcome" => outcome).increment(1);
        result
    }

    async fn fetch_inner(&self, bbox: &BBox, started: Instant) -> Result<OsmData, ApiError> {
        let deadline = started + self.budget;
        self.check_cooldown()?;
        let guard = WaitGuard(&self.waiting);
        if self
            .waiting
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            >= MAX_WAITING
        {
            return Err(ApiError::RateLimited {
                retry_after_secs: 10,
            });
        }
        let _permit = self
            .in_flight
            .acquire()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        drop(guard);
        // We may have queued behind a request that got us rate limited.
        self.check_cooldown()?;

        let query = build_query(bbox);
        // Every interpreter once, in order; a lone one gets a second try, since a
        // public instance's load balancer often answers a retry from another backend.
        let plan: Vec<usize> = if self.urls.len() == 1 {
            vec![0, 0]
        } else {
            (0..self.urls.len()).collect()
        };
        // Too little time left for an attempt to be worth starting.
        let min_attempt = self.attempt_timeout / 10;
        let mut last = None;
        for (i, &endpoint) in plan.iter().enumerate() {
            if self.cooling(endpoint).is_some() {
                continue;
            }
            if last.is_some() {
                tokio::time::sleep(self.retry_wait).await;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left < min_attempt {
                last = Some(Transient::TimedOut);
                break;
            }
            match self
                .attempt(endpoint, &query, left.min(self.attempt_timeout))
                .await
            {
                Ok(data) => return Ok(data),
                Err(AttemptError::Fatal(e)) => return Err(e),
                Err(AttemptError::Retry(t)) => {
                    // A busy interpreter rests, unless this fetch is about to retry it.
                    if t == Transient::Busy && !plan[i + 1..].contains(&endpoint) {
                        self.start_cooldown(endpoint);
                    }
                    last = Some(t);
                }
            }
        }
        Err(match last {
            Some(Transient::TimedOut) => ApiError::UpstreamTimeout(
                "the map data provider took too long to answer; try again shortly, or draw a smaller area".into(),
            ),
            Some(Transient::Failed) => {
                ApiError::Upstream("could not reach the map data provider; try again shortly".into())
            }
            // Busy, or every interpreter started cooling down while we queued.
            Some(Transient::Busy) | None => {
                self.check_cooldown()
                    .err()
                    .unwrap_or(ApiError::RateLimited {
                        retry_after_secs: self.cooldown.as_secs().max(1),
                    })
            }
        })
    }

    /// One request to one interpreter, logged once if it fails.
    async fn attempt(
        &self,
        endpoint: usize,
        query: &str,
        timeout: Duration,
    ) -> Result<OsmData, AttemptError> {
        let resp = self
            .client
            .post(&self.urls[endpoint])
            .timeout(timeout)
            .form(&[("data", query)])
            .send()
            .await
            .map_err(|e| {
                let t = Transient::of(&e);
                tracing::warn!(endpoint, error = %e.without_url(), "overpass request failed");
                AttemptError::Retry(t)
            })?;
        let status = resp.status();
        if !status.is_success() {
            if status.is_redirection() {
                // Redirects are not followed (see `observation_upstream`); say where to.
                tracing::warn!(
                    endpoint,
                    %status,
                    location = ?resp.headers().get(reqwest::header::LOCATION),
                    "overpass redirected; point OBS_OVERPASS_URL at the final URL"
                );
                return Err(AttemptError::Retry(Transient::Failed));
            }
            tracing::warn!(endpoint, %status, "overpass returned an error status");
            return Err(match status {
                StatusCode::TOO_MANY_REQUESTS | StatusCode::GATEWAY_TIMEOUT => {
                    AttemptError::Retry(Transient::Busy)
                }
                StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE => {
                    AttemptError::Retry(Transient::Failed)
                }
                _ => AttemptError::Fatal(ApiError::Upstream(format!(
                    "the map data provider answered with an error (HTTP {})",
                    status.as_u16()
                ))),
            });
        }
        let body = crate::read_capped(resp, MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| {
                let t = Transient::of(&e);
                tracing::warn!(endpoint, error = %e.without_url(), "overpass body failed");
                AttemptError::Retry(t)
            })?
            .ok_or_else(|| {
                tracing::warn!(
                    endpoint,
                    max = MAX_RESPONSE_BYTES,
                    "overpass body too large"
                );
                AttemptError::Fatal(ApiError::Upstream(
                    "this area has more map data than the server accepts; draw a smaller area"
                        .into(),
                ))
            })?;
        parse_response(&body).map_err(|e| {
            tracing::warn!(endpoint, error = %e, "overpass response unusable");
            match e {
                // The query ran out of time on a loaded server: another may manage.
                ParseError::Remark(r) if r.contains("timed out") => {
                    AttemptError::Retry(Transient::TimedOut)
                }
                // Out of memory and the like: the same query fails the same way.
                ParseError::Remark(_) => AttemptError::Fatal(ApiError::Upstream(
                    "the map data provider could not process this area; try a smaller area".into(),
                )),
                ParseError::Json(_) => AttemptError::Fatal(ApiError::Upstream(
                    "the map data provider sent a reply the server could not read; try again shortly".into(),
                )),
            }
        })
    }

    /// Time left on `endpoint`'s cool-down, if any.
    fn cooling(&self, endpoint: usize) -> Option<Duration> {
        let until = self
            .cooldown_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[endpoint];
        until
            .map(|t| t.saturating_duration_since(Instant::now()))
            .filter(|left| !left.is_zero())
    }

    /// Fails fast while every interpreter is cooling down.
    fn check_cooldown(&self) -> Result<(), ApiError> {
        let left = (0..self.urls.len()).map(|e| self.cooling(e));
        match left.collect::<Option<Vec<Duration>>>() {
            Some(left) => Err(ApiError::RateLimited {
                retry_after_secs: left
                    .iter()
                    .min()
                    .map_or(1.0, Duration::as_secs_f64)
                    .ceil()
                    .max(1.0) as u64,
            }),
            None => Ok(()),
        }
    }

    fn start_cooldown(&self, endpoint: usize) {
        self.cooldown_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[endpoint] =
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
    fn an_empty_interpreter_list_falls_back_to_the_default() {
        let mut config = Config::for_tests("cache", "dist", "http://127.0.0.1:9/", "http://t/");
        config.overpass_urls.clear();
        let client = OverpassClient::new(reqwest::Client::new(), &config);
        assert_eq!(
            client.urls,
            vec![crate::config::DEFAULT_OVERPASS_URL.to_owned()]
        );
        assert_eq!(client.cooldown_until.lock().unwrap().len(), 1);
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
        assert!(q.contains("node[\"addr:housenumber\"](1.0000000,2.0000000,1.1000000,2.1000000);"));
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
        let d = parse_response(include_bytes!("../fixtures/demo-overpass.json")).unwrap();
        assert!(d.buildings.len() > 80, "{} buildings", d.buildings.len());
        assert!(d.roads.len() > 10, "{} roads", d.roads.len());
        assert!(
            d.buildings
                .iter()
                .all(|b| b.outer.len() >= 3 && b.height_m > 0.0)
        );
        let addressed = d.buildings.iter().filter(|b| b.address.is_some()).count();
        assert!(addressed > 50, "{addressed} addressed buildings");
        // Recorded with the fixture (see fixtures/README.md).
        assert_eq!(
            d.osm_base_unix
                .map(observation_core::provenance::format_utc),
            Some("2026-10-31T00:00:00Z".to_owned())
        );
    }
}
