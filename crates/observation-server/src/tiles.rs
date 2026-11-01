//! Caching tile proxy that follows the OSM tile usage policy
//! (<https://operations.osmfoundation.org/policies/tiles/>):
//! distinct `User-Agent`, `Referer` forwarded, upstream `Cache-Control`/`Expires`
//! honoured but never below the configured minimum (7 days), conditional
//! revalidation, at most 4 concurrent upstream fetches, no prefetching.
//! The same proxy, with its own cache directory, fetches the elevation tiles
//! areas are built on (see [`crate::elevation`]).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use observation_core::tiles::TileId;
use tokio::sync::Semaphore;

use crate::cache::{Cache, TileMeta, TileSet};
use crate::config::Config;
use crate::error::ApiError;

/// Maximum number of simultaneous upstream tile requests.
pub const MAX_CONCURRENT_FETCHES: usize = 4;
/// Largest tile body we accept from upstream.
const MAX_TILE_BYTES: usize = 2 * 1024 * 1024;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The tile proxy.
#[derive(Debug)]
pub struct TileProxy {
    client: reqwest::Client,
    template: String,
    set: TileSet,
    cache: Arc<Cache>,
    permits: Semaphore,
    min_ttl: Duration,
}

impl TileProxy {
    /// The map tile proxy. `client` must already carry the project
    /// `User-Agent` and a timeout.
    pub fn new(client: reqwest::Client, config: &Config, cache: Arc<Cache>) -> Self {
        Self {
            client,
            template: config.tile_url.clone(),
            set: TileSet::Map,
            cache,
            permits: Semaphore::new(MAX_CONCURRENT_FETCHES),
            min_ttl: config.tile_min_ttl,
        }
    }

    /// The elevation tile proxy for the `template` from `OBS_DEM_URL`.
    pub fn elevation(
        client: reqwest::Client,
        template: String,
        config: &Config,
        cache: Arc<Cache>,
    ) -> Self {
        Self {
            client,
            template,
            set: TileSet::Dem,
            cache,
            permits: Semaphore::new(MAX_CONCURRENT_FETCHES),
            min_ttl: config.dem_min_ttl,
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
    pub async fn get(&self, id: TileId, referer: Option<&HeaderValue>) -> Result<Bytes, ApiError> {
        let cached = self.cache.get_tile_in(self.set, id).await;
        if let Some(c) = &cached
            && c.meta.expires_unix > unix_now()
        {
            self.count("hit");
            return Ok(c.bytes.clone());
        }
        match self
            .fetch(id, cached.as_ref().map(|c| &c.meta), referer)
            .await
        {
            Ok(Upstream::Fresh(bytes, meta)) => {
                // `refreshed`: an expired copy was replaced by a new body; `miss`: nothing cached.
                let result = if cached.is_some() {
                    "refreshed"
                } else {
                    "miss"
                };
                self.count(result);
                match self.cache.put_tile_in(self.set, id, &bytes, &meta).await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, ?id, server = self.server(), "could not cache tile")
                    }
                }
                Ok(bytes)
            }
            Ok(Upstream::NotModified(meta)) => {
                let Some(c) = cached else {
                    return Err(ApiError::Upstream(format!(
                        "the {} sent an unexpected 'not modified' reply",
                        self.server()
                    )));
                };
                self.count("revalidated");
                if let Err(e) = self.cache.update_tile_meta_in(self.set, id, &meta).await {
                    tracing::warn!(error = %e, ?id, server = self.server(), "could not refresh tile metadata");
                }
                Ok(c.bytes)
            }
            Err(e) => match cached {
                Some(c) => {
                    self.count("stale_served");
                    tracing::warn!(error = %e, ?id, server = self.server(), "tile upstream failed, serving stale copy");
                    Ok(c.bytes)
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
        let mut req = self.client.get(tile_url(&self.template, id));
        if let Some(r) = referer {
            req = req.header(header::REFERER, r);
        }
        if let Some(meta) = stale {
            if let Some(etag) = &meta.etag {
                req = req.header(header::IF_NONE_MATCH, etag);
            }
            if let Some(lm) = &meta.last_modified {
                req = req.header(header::IF_MODIFIED_SINCE, lm);
            }
        }
        let resp = req.send().await.map_err(|e| {
            tracing::warn!(error = %e.without_url(), ?id, server = self.server(), "tile request failed");
            ApiError::Upstream(match self.set {
                TileSet::Map => "could not reach the tile server; map tiles may be missing".into(),
                TileSet::Dem => "could not reach the elevation server".into(),
            })
        })?;
        let status = resp.status();
        let header_str =
            |h: &HeaderMap, name| h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let new_meta = |headers: &HeaderMap, old: Option<&TileMeta>| TileMeta {
            etag: header_str(headers, header::ETAG).or_else(|| old.and_then(|m| m.etag.clone())),
            last_modified: header_str(headers, header::LAST_MODIFIED)
                .or_else(|| old.and_then(|m| m.last_modified.clone())),
            expires_unix: expiry_unix(headers, self.min_ttl),
        };
        match status {
            StatusCode::OK => {
                let meta = new_meta(resp.headers(), None);
                let body = crate::read_capped(resp, MAX_TILE_BYTES)
                    .await
                    .map_err(|e| {
                        tracing::warn!(error = %e.without_url(), ?id, server = self.server(), "tile body failed");
                        ApiError::Upstream(format!("the download from the {} was interrupted", self.server()))
                    })?
                    .ok_or_else(|| {
                        tracing::warn!(max = MAX_TILE_BYTES, ?id, server = self.server(), "tile body too large");
                        ApiError::Upstream(format!("the {} returned a tile that is too large", self.server()))
                    })?;
                if !body.starts_with(PNG_MAGIC) {
                    return Err(ApiError::Upstream(format!(
                        "the {} returned something that is not a PNG",
                        self.server()
                    )));
                }
                Ok(Upstream::Fresh(body, meta))
            }
            StatusCode::NOT_MODIFIED => Ok(Upstream::NotModified(new_meta(resp.headers(), stale))),
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
                        location = ?resp.headers().get(header::LOCATION),
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

enum Upstream {
    Fresh(Bytes, TileMeta),
    NotModified(TileMeta),
}

/// Expands `{z}`, `{x}`, `{y}` and `{s}` (always `a`) in the template.
pub fn tile_url(template: &str, id: TileId) -> String {
    template
        .replace("{z}", &id.z.to_string())
        .replace("{x}", &id.x.to_string())
        .replace("{y}", &id.y.to_string())
        .replace("{s}", "a")
}

/// Expiry (unix seconds) from `Cache-Control: max-age` or `Expires`, floored at
/// `min_ttl` from now.
fn expiry_unix(headers: &HeaderMap, min_ttl: Duration) -> u64 {
    let now = unix_now();
    let upstream_ttl = headers
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .and_then(max_age)
        .or_else(|| {
            let expires = headers.get(header::EXPIRES)?.to_str().ok()?;
            let at = httpdate::parse_http_date(expires).ok()?;
            Some(
                at.duration_since(SystemTime::now())
                    .map_or(0, |d| d.as_secs()),
            )
        })
        .unwrap_or(0);
    // `max-age` is upstream input and parses up to `u64::MAX`.
    now.saturating_add(upstream_ttl.max(min_ttl.as_secs()))
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_substitution() {
        let id = TileId::new(17, 3, 5).unwrap();
        assert_eq!(
            tile_url("https://{s}.t/{z}/{x}/{y}.png", id),
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
        let mut h = HeaderMap::new();
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        let e = expiry_unix(&h, week);
        assert!(
            e >= unix_now() + week.as_secs() - 2,
            "short upstream TTLs are raised to 7 days"
        );

        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=2592000"),
        );
        assert!(
            expiry_unix(&h, week) >= unix_now() + 2_592_000 - 2,
            "longer upstream TTLs are kept"
        );

        let mut h = HeaderMap::new();
        h.insert(
            header::EXPIRES,
            HeaderValue::from_static("Fri, 01 Jan 2100 00:00:00 GMT"),
        );
        assert!(expiry_unix(&h, week) > unix_now() + 365 * 86_400);
        assert!(expiry_unix(&HeaderMap::new(), Duration::ZERO) <= unix_now() + 1);

        let mut h = HeaderMap::new();
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=18446744073709551615"),
        );
        assert_eq!(expiry_unix(&h, week), u64::MAX, "huge max-age saturates");
    }
}
