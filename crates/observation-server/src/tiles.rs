//! Caching tile proxy that follows the OSM tile usage policy
//! (<https://operations.osmfoundation.org/policies/tiles/>):
//! distinct `User-Agent`, `Referer` forwarded, upstream `Cache-Control`/`Expires`
//! honoured but never below the configured minimum (7 days), conditional
//! revalidation, at most 4 concurrent upstream fetches, no prefetching. Each
//! client's upstream fetches are also rate limited (see [`crate::ratelimit`]).
//! The same proxy, with its own cache directory, fetches the elevation tiles
//! areas are built on (see [`crate::elevation`]); those are not limited per
//! client, since the bundle builds that need them already are.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use observation_core::tiles::TileId;
use observation_upstream::{Body, HttpClient, HttpRequest, TransportErrorKind};
use tokio::sync::Semaphore;

use crate::clock::{Clock, unix_secs};
use crate::config::Config;
use crate::domain::freshness::tile_is_fresh;
use crate::error::ApiError;
use crate::ports::{TileMeta, TileSet, TileStore};
use crate::ratelimit::{ClientKey, RateLimiter};

/// Maximum number of simultaneous upstream tile requests.
pub const MAX_CONCURRENT_FETCHES: usize = 4;
/// Largest tile body we accept from upstream.
const MAX_TILE_BYTES: usize = 2 * 1024 * 1024;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The tile proxy.
#[derive(Debug)]
pub struct TileProxy {
    client: Arc<dyn HttpClient>,
    template: String,
    set: TileSet,
    /// Checks a downloaded tile beyond its PNG signature; one that fails is
    /// neither cached nor served.
    usable: fn(&[u8]) -> bool,
    cache: Arc<dyn TileStore>,
    permits: Semaphore,
    min_ttl: Duration,
    /// Per-client limit on upstream fetches (map tiles only).
    limiter: Option<RateLimiter>,
    /// "Now" for expiry.
    clock: Arc<dyn Clock>,
}

impl TileProxy {
    /// The map tile proxy. `client` must already carry the project
    /// `User-Agent` and a timeout.
    pub fn new(
        client: Arc<dyn HttpClient>,
        config: &Config,
        cache: Arc<dyn TileStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client,
            template: config.tile_url.clone(),
            set: TileSet::Map,
            usable: |_| true,
            cache,
            permits: Semaphore::new(MAX_CONCURRENT_FETCHES),
            min_ttl: config.tile_min_ttl,
            limiter: Some(RateLimiter::new("tiles", config.tile_rate)),
            clock,
        }
    }

    /// The elevation tile proxy for the `template` from `OBS_DEM_URL`, which
    /// keeps only tiles `usable` accepts.
    pub fn elevation(
        client: Arc<dyn HttpClient>,
        template: String,
        usable: fn(&[u8]) -> bool,
        config: &Config,
        cache: Arc<dyn TileStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client,
            template,
            set: TileSet::Dem,
            usable,
            cache,
            permits: Semaphore::new(MAX_CONCURRENT_FETCHES),
            min_ttl: config.dem_min_ttl,
            limiter: None,
            clock,
        }
    }

    /// The upstream, as named in errors.
    fn server(&self) -> &'static str {
        match self.set {
            TileSet::Map => "tile server",
            TileSet::Dem => "elevation server",
        }
    }

    fn count(&self, result: &'static str) {
        let name = match self.set {
            TileSet::Map => crate::metrics::TILE_CACHE,
            TileSet::Dem => crate::metrics::DEM_CACHE,
        };
        metrics::counter!(name, "result" => result).increment(1);
    }

    /// Returns the PNG for `id`, from cache when fresh, else from upstream.
    /// `referer` is the client's `Referer`, forwarded upstream as the policy asks.
    /// Only a request that goes upstream is charged to `client`; one over its
    /// limit is treated like an upstream failure (an expired copy, else the 429).
    pub async fn get(
        &self,
        id: TileId,
        referer: Option<&HeaderValue>,
        client: Option<ClientKey>,
    ) -> Result<Tile, ApiError> {
        let cached = self.cache.get(self.set, id).await;
        if let Some(c) = &cached
            && tile_is_fresh(&c.meta, self.clock.now())
        {
            self.count("hit");
            return Ok(Tile::fresh(c.bytes.clone()));
        }
        let allowed = match (&self.limiter, client) {
            (Some(limiter), Some(client)) => limiter.check(client),
            _ => Ok(()),
        };
        let fetched = match allowed {
            Ok(()) => {
                self.fetch(id, cached.as_ref().map(|c| &c.meta), referer)
                    .await
            }
            Err(e) => Err(e),
        };
        match fetched {
            Ok(Upstream::Fresh(bytes, meta)) => {
                // `refreshed`: an expired copy was replaced by a new body; `miss`: nothing cached.
                let result = if cached.is_some() {
                    "refreshed"
                } else {
                    "miss"
                };
                self.count(result);
                match self.cache.put(self.set, id, &bytes, &meta).await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, ?id, server = self.server(), "could not cache tile")
                    }
                }
                Ok(Tile::fresh(bytes))
            }
            Ok(Upstream::NotModified(meta)) => {
                let Some(c) = cached else {
                    return Err(ApiError::Upstream(format!(
                        "the {} sent an unexpected 'not modified' reply",
                        self.server()
                    )));
                };
                self.count("revalidated");
                if let Err(e) = TileStore::revalidated(&*self.cache, self.set, id, &meta).await {
                    tracing::warn!(error = %e, ?id, server = self.server(), "could not refresh tile metadata");
                }
                Ok(Tile::fresh(c.bytes))
            }
            Err(e) => match cached {
                Some(c) => {
                    self.count("stale_served");
                    // A client over its limit is routine, and counted on its own.
                    if !matches!(e, ApiError::TooManyRequests { .. }) {
                        tracing::warn!(error = %e, ?id, server = self.server(), "tile upstream failed, serving stale copy");
                    }
                    Ok(Tile {
                        bytes: c.bytes,
                        stale: true,
                    })
                }
                None => {
                    self.count("error");
                    Err(e)
                }
            },
        }
    }

    async fn fetch(
        &self,
        id: TileId,
        stale: Option<&TileMeta>,
        referer: Option<&HeaderValue>,
    ) -> Result<Upstream, ApiError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let unreachable = || {
            ApiError::Upstream(match self.set {
                TileSet::Map => "could not reach the tile server; map tiles may be missing".into(),
                TileSet::Dem => "could not reach the elevation server".into(),
            })
        };
        // Error bodies are never read; only the status and headers matter.
        let mut req = HttpRequest::get(id.url(&self.template), MAX_TILE_BYTES).error_max_bytes(0);
        if let Some(r) = referer {
            req = req.header(header::REFERER, r.clone());
        }
        if let Some(meta) = stale {
            let value = |v: &str| {
                HeaderValue::from_str(v).map_err(|e| {
                    tracing::warn!(error = %e, ?id, server = self.server(), "tile request failed");
                    unreachable()
                })
            };
            if let Some(etag) = &meta.etag {
                req = req.header(header::IF_NONE_MATCH, value(etag)?);
            }
            if let Some(lm) = &meta.last_modified {
                req = req.header(header::IF_MODIFIED_SINCE, value(lm)?);
            }
        }
        let resp = self.client.send(req).await.map_err(|e| {
            if matches!(
                e.kind,
                TransportErrorKind::Body | TransportErrorKind::BodyTimeout
            ) {
                tracing::warn!(error = %e, ?id, server = self.server(), "tile body failed");
                ApiError::Upstream(format!(
                    "the download from the {} was interrupted",
                    self.server()
                ))
            } else {
                tracing::warn!(error = %e, ?id, server = self.server(), "tile request failed");
                unreachable()
            }
        })?;
        let status = resp.status;
        let header_str =
            |h: &HeaderMap, name| h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let new_meta = |headers: &HeaderMap, old: Option<&TileMeta>| TileMeta {
            etag: header_str(headers, header::ETAG).or_else(|| old.and_then(|m| m.etag.clone())),
            last_modified: header_str(headers, header::LAST_MODIFIED)
                .or_else(|| old.and_then(|m| m.last_modified.clone())),
            expires_unix: expiry_unix(headers, self.min_ttl, self.clock.now()),
        };
        match status {
            StatusCode::OK => {
                let meta = new_meta(&resp.headers, None);
                let Body::Complete(body) = resp.body else {
                    tracing::warn!(
                        max = MAX_TILE_BYTES,
                        ?id,
                        server = self.server(),
                        "tile body too large"
                    );
                    return Err(ApiError::Upstream(format!(
                        "the {} returned a tile that is too large",
                        self.server()
                    )));
                };
                if !body.starts_with(PNG_MAGIC) {
                    return Err(ApiError::Upstream(format!(
                        "the {} returned something that is not a PNG",
                        self.server()
                    )));
                }
                if !(self.usable)(&body) {
                    tracing::warn!(?id, server = self.server(), "unusable tile");
                    return Err(ApiError::Upstream(format!(
                        "the {} sent an unusable tile",
                        self.server()
                    )));
                }
                Ok(Upstream::Fresh(body, meta))
            }
            StatusCode::NOT_MODIFIED => Ok(Upstream::NotModified(new_meta(&resp.headers, stale))),
            StatusCode::NOT_FOUND => Err(ApiError::NotFound(match self.set {
                TileSet::Map => "the tile server has no tile here",
                TileSet::Dem => "the elevation server has no tile here",
            })),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                tracing::warn!(%status, ?id, server = self.server(), "tile server is limiting us");
                Err(ApiError::RateLimited {
                    retry_after_secs: 30,
                })
            }
            other => {
                if other.is_redirection() {
                    // Redirects are not followed (see `observation_upstream`).
                    tracing::warn!(
                        status = %other,
                        location = ?resp.headers.get(header::LOCATION),
                        server = self.server(),
                        "upstream redirected; point OBS_TILE_URL or OBS_DEM_URL at the final URL"
                    );
                }
                tracing::warn!(status = %other, ?id, server = self.server(), "tile server error");
                Err(ApiError::Upstream(format!(
                    "the {} answered with an error (HTTP {})",
                    self.server(),
                    other.as_u16()
                )))
            }
        }
    }
}

/// A tile for the client.
#[derive(Debug)]
pub struct Tile {
    pub bytes: Bytes,
    /// An expired copy, served because the upstream failed or the client is
    /// over its limit; browsers should ask again soon.
    pub stale: bool,
}

impl Tile {
    fn fresh(bytes: Bytes) -> Self {
        Self {
            bytes,
            stale: false,
        }
    }
}

enum Upstream {
    Fresh(Bytes, TileMeta),
    NotModified(TileMeta),
}

/// Expiry (unix seconds) from `Cache-Control: max-age` or `Expires`, floored at
/// `min_ttl` from `now`.
fn expiry_unix(headers: &HeaderMap, min_ttl: Duration, now: SystemTime) -> u64 {
    let now_unix = unix_secs(now);
    let upstream_ttl = headers
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .and_then(max_age)
        .or_else(|| {
            let expires = headers.get(header::EXPIRES)?.to_str().ok()?;
            let at = httpdate::parse_http_date(expires).ok()?;
            Some(at.duration_since(now).map_or(0, |d| d.as_secs()))
        })
        .unwrap_or(0);
    // `max-age` is upstream input and parses up to `u64::MAX`.
    now_unix.saturating_add(upstream_ttl.max(min_ttl.as_secs()))
}

/// Extracts `max-age=N` (seconds) from a `Cache-Control` value.
fn max_age(value: &str) -> Option<u64> {
    value.split(',').find_map(|d| {
        let (k, v) = d.trim().split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("max-age")
            .then(|| v.trim().trim_matches('"').parse().ok())
            .flatten()
    })
}

#[cfg(test)]
mod proxy_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_substitution() {
        let id = TileId::new(17, 3, 5).unwrap();
        assert_eq!(
            id.url("https://{s}.t/{z}/{x}/{y}.png"),
            "https://a.t/17/3/5.png"
        );
    }

    #[test]
    fn max_age_parsing() {
        assert_eq!(max_age("max-age=7200"), Some(7200));
        assert_eq!(max_age("public, Max-Age=\"60\", immutable"), Some(60));
        assert_eq!(max_age("no-cache"), None);
        assert_eq!(max_age("max-age=abc"), None);
    }

    #[test]
    fn expiry_respects_minimum() {
        let week = Duration::from_secs(7 * 86_400);
        let now = SystemTime::now();
        let mut h = HeaderMap::new();
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        let e = expiry_unix(&h, week, now);
        assert!(
            e >= unix_secs(now) + week.as_secs() - 2,
            "short upstream TTLs are raised to 7 days"
        );

        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=2592000"),
        );
        assert!(
            expiry_unix(&h, week, now) >= unix_secs(now) + 2_592_000 - 2,
            "longer upstream TTLs are kept"
        );

        let mut h = HeaderMap::new();
        h.insert(
            header::EXPIRES,
            HeaderValue::from_static("Fri, 01 Jan 2100 00:00:00 GMT"),
        );
        assert!(expiry_unix(&h, week, now) > unix_secs(now) + 365 * 86_400);
        assert!(expiry_unix(&HeaderMap::new(), Duration::ZERO, now) <= unix_secs(now) + 1);

        let mut h = HeaderMap::new();
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=18446744073709551615"),
        );
        assert_eq!(
            expiry_unix(&h, week, now),
            u64::MAX,
            "huge max-age saturates"
        );
    }

    #[test]
    fn expiry_is_exact_for_a_given_now() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let week = Duration::from_secs(7 * 86_400);
        let with = |name, value: &'static str| {
            let mut h = HeaderMap::new();
            h.insert(name, HeaderValue::from_static(value));
            h
        };
        let cc = |v| with(header::CACHE_CONTROL, v);
        assert_eq!(expiry_unix(&cc("max-age=60"), week, now), 1_700_604_800);
        assert_eq!(expiry_unix(&cc("max-age=604801"), week, now), 1_700_604_801);
        assert_eq!(
            expiry_unix(&HeaderMap::new(), Duration::ZERO, now),
            1_700_000_000
        );
        // 1_700_000_000 is Tue, 14 Nov 2023 22:13:20 GMT.
        let expires = |v| with(header::EXPIRES, v);
        assert_eq!(
            expiry_unix(
                &expires("Tue, 14 Nov 2023 22:13:30 GMT"),
                Duration::ZERO,
                now
            ),
            1_700_000_010
        );
        assert_eq!(
            expiry_unix(
                &expires("Tue, 14 Nov 2023 22:13:10 GMT"),
                Duration::ZERO,
                now
            ),
            1_700_000_000,
            "an Expires in the past counts as no time"
        );
    }
}
