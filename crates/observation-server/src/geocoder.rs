//! Place-name search through a Nominatim-compatible geocoder, following
//! Nominatim's usage policy (<https://operations.osmfoundation.org/policies/nominatim/>):
//! an identifying `User-Agent`, at most one upstream request a second for the
//! whole server, answers cached, a small fixed number of results, and no
//! autocomplete (the client searches only when the user submits; the server
//! limits each client as well, see [`crate::ratelimit`]).
//!
//! It is off unless `OBS_GEOCODER_URL` is set: searches then go to that third
//! party. Queries are never logged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::http::{HeaderValue, StatusCode, header};
use observation_upstream::{
    BoundedQueue, HttpClient, HttpRequest, QueueError, TransportErrorKind, Url,
};
use serde::Serialize;
use tokio::time::Instant;

use crate::config::GeocoderConfig;
use crate::error::ApiError;
use crate::ratelimit::{ClientKey, RateLimiter};

/// Shortest query accepted, in characters (after trimming).
pub const MIN_QUERY_CHARS: usize = 2;
/// Longest query accepted, in characters (after trimming).
pub const MAX_QUERY_CHARS: usize = 200;
/// Results asked for and returned.
pub const MAX_RESULTS: usize = 5;
/// Least time between the starts of two upstream requests, server-wide.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// How long an answer is reused.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);
/// Most answers kept; past it the oldest go first.
pub const CACHE_ENTRIES: usize = 1000;
/// Most searches waiting for their turn upstream; more are refused (503).
pub const MAX_WAITING: usize = 8;
/// How long new searches fail fast after the geocoder limited us.
pub const COOLDOWN: Duration = Duration::from_secs(30);
/// Time allowed for one upstream request, body included.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// Largest answer read (five results are a few kilobytes).
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Longest place label passed on, in characters.
const MAX_LABEL_CHARS: usize = 300;

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Place {
    /// The geocoder's full name of the place.
    pub label: String,
    pub lat: f64,
    pub lon: f64,
    /// `[south, west, north, east]`, when the geocoder gave a usable one.
    pub bbox: Option<[f64; 4]>,
}

/// The answer to one query, shared by everyone who asks it while cached.
pub type Places = Arc<[Place]>;

/// The place search.
#[derive(Debug)]
pub struct Geocoder {
    client: Arc<dyn HttpClient>,
    endpoint: Url,
    user_agent: HeaderValue,
    email: Option<String>,
    attribution: String,
    cache: Mutex<HashMap<String, Cached>>,
    /// One upstream request at a time, with a bounded queue behind it.
    queue: BoundedQueue,
    pace: Mutex<Pace>,
    limiter: RateLimiter,
}

#[derive(Debug)]
struct Cached {
    places: Places,
    at: Instant,
}

#[derive(Debug, Default)]
struct Pace {
    /// When the last upstream request started.
    last: Option<Instant>,
    /// No upstream request before this (the geocoder limited us).
    blocked_until: Option<Instant>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Geocoder {
    /// The search over `client`, or `None` when `config` turns it off. The
    /// endpoint must already have been validated ([`crate::config::Config::validate`]).
    pub fn new(client: Arc<dyn HttpClient>, config: &GeocoderConfig) -> Option<Self> {
        let endpoint = Url::parse(config.url.as_deref()?).ok()?;
        let user_agent = HeaderValue::from_str(&config.user_agent()).ok()?;
        Some(Self {
            client,
            endpoint,
            user_agent,
            email: config.email().map(str::to_owned),
            attribution: config.attribution.clone(),
            cache: Mutex::default(),
            queue: BoundedQueue::new(1, MAX_WAITING),
            pace: Mutex::default(),
            limiter: RateLimiter::new("geocode", config.rate),
        })
    }

    /// Credit to show with the results.
    pub fn attribution(&self) -> &str {
        &self.attribution
    }

    /// The places matching `query`, from the cache or the geocoder. Only a
    /// search that goes upstream is charged to `client`.
    pub async fn search(&self, query: &str, client: ClientKey) -> Result<Places, ApiError> {
        let key = normalise(query)?;
        if let Some(hit) = self.cached(&key) {
            count("hit");
            return Ok(hit);
        }
        if let Some(wait) = self.blocked_for() {
            count("busy");
            return Err(busy(wait));
        }
        self.limiter.check(client).map_err(|e| match e {
            ApiError::TooManyRequests { retry_after_secs } => {
                ApiError::TooManySearches { retry_after_secs }
            }
            other => other,
        })?;
        let _turn = match self.queue.enter().await {
            Ok(permit) => permit,
            Err(QueueError::Full) => {
                self.limiter.refund(client);
                count("busy");
                return Err(ApiError::SearchBusy {
                    retry_after_secs: 5,
                });
            }
            Err(QueueError::Closed(e)) => return Err(ApiError::Internal(e.to_string())),
        };
        // Someone ahead in the queue may have asked the same.
        if let Some(hit) = self.cached(&key) {
            self.limiter.refund(client);
            count("hit");
            return Ok(hit);
        }
        if let Some(wait) = self.blocked_for() {
            self.limiter.refund(client);
            count("busy");
            return Err(busy(wait));
        }
        let due = lock(&self.pace).last.map(|t| t + MIN_INTERVAL);
        if let Some(due) = due {
            tokio::time::sleep_until(due).await;
        }
        lock(&self.pace).last = Some(Instant::now());
        match self.fetch(&key).await {
            Ok(places) => {
                count("fetched");
                self.store(key, Arc::clone(&places));
                Ok(places)
            }
            Err(e) => {
                count("error");
                if let ApiError::SearchBusy { .. } = e {
                    lock(&self.pace).blocked_until = Some(Instant::now() + COOLDOWN);
                }
                Err(e)
            }
        }
    }

    fn cached(&self, key: &str) -> Option<Places> {
        let now = Instant::now();
        lock(&self.cache)
            .get(key)
            .filter(|c| now.saturating_duration_since(c.at) < CACHE_TTL)
            .map(|c| Arc::clone(&c.places))
    }

    fn store(&self, key: String, places: Places) {
        let now = Instant::now();
        let mut cache = lock(&self.cache);
        if cache.len() >= CACHE_ENTRIES && !cache.contains_key(&key) {
            cache.retain(|_, c| now.saturating_duration_since(c.at) < CACHE_TTL);
            if cache.len() >= CACHE_ENTRIES
                && let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, c)| c.at)
                    .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, Cached { places, at: now });
    }

    /// How long upstream requests are still held back after the geocoder
    /// limited us.
    fn blocked_for(&self) -> Option<Duration> {
        let until = lock(&self.pace).blocked_until?;
        let left = until.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    async fn fetch(&self, key: &str) -> Result<Places, ApiError> {
        let mut url = self.endpoint.clone();
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("q", key)
                .append_pair("format", "jsonv2")
                .append_pair("limit", &MAX_RESULTS.to_string());
            if let Some(email) = &self.email {
                q.append_pair("email", email);
            }
        }
        let req = HttpRequest::get(url.as_str(), MAX_BODY_BYTES)
            .error_max_bytes(0)
            .body_only_for(StatusCode::OK)
            .timeout(TIMEOUT)
            .header(header::USER_AGENT, self.user_agent.clone())
            .header(header::ACCEPT, HeaderValue::from_static("application/json"));
        // The query is never logged: only the kind of failure.
        let resp = self.client.send(req).await.map_err(|e| {
            tracing::warn!(kind = ?e.kind, "place search request failed");
            match e.kind {
                TransportErrorKind::ConnectTimeout
                | TransportErrorKind::Timeout
                | TransportErrorKind::BodyTimeout => {
                    ApiError::UpstreamTimeout("the place search did not answer in time".into())
                }
                _ => ApiError::Upstream("could not reach the place search".into()),
            }
        })?;
        match resp.status {
            StatusCode::OK => {
                let places = resp.bytes().and_then(|b| parse_places(b)).ok_or_else(|| {
                    tracing::warn!("place search answer could not be read");
                    ApiError::Upstream(
                        "the place search sent an answer that could not be read".into(),
                    )
                })?;
                Ok(places.into())
            }
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                tracing::warn!(status = %resp.status, "place search is limiting us");
                Err(busy(COOLDOWN))
            }
            other => {
                if other.is_redirection() {
                    tracing::warn!(
                        status = %other,
                        "place search redirected; point OBS_GEOCODER_URL at the final URL"
                    );
                } else {
                    tracing::warn!(status = %other, "place search error");
                }
                Err(ApiError::Upstream(format!(
                    "the place search answered with an error (HTTP {})",
                    other.as_u16()
                )))
            }
        }
    }
}

fn count(result: &'static str) {
    metrics::counter!(crate::metrics::GEOCODER, "result" => result).increment(1);
}

fn busy(wait: Duration) -> ApiError {
    ApiError::SearchBusy {
        retry_after_secs: wait.as_secs_f64().ceil().max(1.0) as u64,
    }
}

/// The cache key and upstream query for `query`: trimmed, runs of
/// whitespace made one space, lower case. Refuses an empty, too short or too
/// long query, or one with control characters.
pub fn normalise(query: &str) -> Result<String, ApiError> {
    if query.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return Err(ApiError::BadRequest(
            "the search text contains control characters".into(),
        ));
    }
    let key = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let n = key.chars().count();
    if !(MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&n) {
        return Err(ApiError::BadRequest(format!(
            "type a place to search for, {MIN_QUERY_CHARS} to {MAX_QUERY_CHARS} characters long"
        )));
    }
    Ok(key)
}

/// The places in a Nominatim `jsonv2` (or `json`) answer: an array of
/// objects with `display_name`, `lat`, `lon` and `boundingbox` (`[south,
/// north, west, east]`), numbers as strings or numbers. Entries without a
/// usable position are skipped; at most [`MAX_RESULTS`] are kept. `None` if
/// the answer is not such an array.
pub fn parse_places(body: &[u8]) -> Option<Vec<Place>> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let num = |v: Option<&serde_json::Value>| -> Option<f64> {
        let v = v?;
        let n = v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())?;
        n.is_finite().then_some(n)
    };
    let places = v
        .as_array()?
        .iter()
        .filter_map(|p| {
            let lat = num(p.get("lat")).filter(|l| (-90.0..=90.0).contains(l))?;
            let lon = num(p.get("lon")).filter(|l| (-180.0..=180.0).contains(l))?;
            let label: String = p
                .get("display_name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|l| !l.is_empty())?
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_LABEL_CHARS)
                .collect();
            let bbox = p
                .get("boundingbox")
                .and_then(serde_json::Value::as_array)
                .filter(|b| b.len() == 4)
                .and_then(|b| {
                    let (s, n, w, e) = (
                        num(b.first())?,
                        num(b.get(1))?,
                        num(b.get(2))?,
                        num(b.get(3))?,
                    );
                    let ok = (-90.0..=90.0).contains(&s)
                        && (-90.0..=90.0).contains(&n)
                        && (-180.0..=180.0).contains(&w)
                        && (-180.0..=180.0).contains(&e)
                        && s <= n
                        && w <= e;
                    ok.then_some([s, w, n, e])
                });
            Some(Place {
                label,
                lat,
                lon,
                bbox,
            })
        })
        .take(MAX_RESULTS)
        .collect();
    Some(places)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_normalised_and_bounded() {
        assert_eq!(
            normalise("  High  Street,\tKirkland ").unwrap(),
            "high street, kirkland"
        );
        assert_eq!(normalise("Zürich").unwrap(), "zürich");
        for bad in ["", "  ", "x", "a\u{0}b"] {
            assert!(
                matches!(normalise(bad), Err(ApiError::BadRequest(_))),
                "{bad:?}"
            );
        }
        assert!(normalise(&"é".repeat(MAX_QUERY_CHARS)).is_ok());
        assert!(normalise(&"é".repeat(MAX_QUERY_CHARS + 1)).is_err());
    }

    #[test]
    fn nominatim_answers_become_places() {
        let body = br#"[
            {"display_name": "Westminster, London", "lat": "51.4994", "lon": "-0.1273",
             "boundingbox": ["51.48", "51.52", "-0.15", "-0.11"]},
            {"display_name": "Numbers as numbers", "lat": 47.7, "lon": -122.2,
             "boundingbox": ["47.8", "47.6", "-122.3", "-122.1"]},
            {"display_name": "Out of range", "lat": "95", "lon": "0"},
            {"display_name": "", "lat": "1", "lon": "1"},
            {"lat": "1", "lon": "1"},
            {"display_name": "No bbox", "lat": "1", "lon": "2"}
        ]"#;
        let places = parse_places(body).unwrap();
        assert_eq!(
            places,
            vec![
                Place {
                    label: "Westminster, London".into(),
                    lat: 51.4994,
                    lon: -0.1273,
                    bbox: Some([51.48, -0.15, 51.52, -0.11]),
                },
                Place {
                    label: "Numbers as numbers".into(),
                    lat: 47.7,
                    lon: -122.2,
                    bbox: None,
                },
                Place {
                    label: "No bbox".into(),
                    lat: 1.0,
                    lon: 2.0,
                    bbox: None,
                },
            ]
        );
        let many = format!(
            "[{}]",
            [r#"{"display_name": "x", "lat": "1", "lon": "1"}"#; 9].join(",")
        );
        assert_eq!(parse_places(many.as_bytes()).unwrap().len(), MAX_RESULTS);
        for bad in [&b"{\"error\": \"x\"}"[..], b"<html>", b""] {
            assert_eq!(parse_places(bad), None);
        }
        let long = format!(
            r#"[{{"display_name": "{}", "lat": "1", "lon": "1"}}]"#,
            "a".repeat(1000)
        );
        assert_eq!(
            parse_places(long.as_bytes()).unwrap()[0].label.len(),
            MAX_LABEL_CHARS
        );
    }
}
