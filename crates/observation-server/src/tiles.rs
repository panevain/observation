//! Caching tile proxy that follows the OSM tile usage policy
//! (<https://operations.osmfoundation.org/policies/tiles/>):
//! distinct `User-Agent`, `Referer` forwarded, upstream `Cache-Control`/`Expires`
//! honoured but never below the configured minimum (7 days), conditional
//! revalidation, at most 4 concurrent upstream fetches, no prefetching.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use observation_core::tiles::TileId;
use tokio::sync::Semaphore;

use crate::cache::{Cache, TileMeta};
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
    cache: Arc<Cache>,
    permits: Semaphore,
    min_ttl: Duration,
}

impl TileProxy {
    /// `client` must already carry the project `User-Agent` and a timeout.
    pub fn new(client: reqwest::Client, config: &Config, cache: Arc<Cache>) -> Self {
        Self {
            client,
            template: config.tile_url.clone(),
            cache,
            permits: Semaphore::new(MAX_CONCURRENT_FETCHES),
            min_ttl: config.tile_min_ttl,
        }
    }

    /// Returns the PNG for `id`, from cache when fresh, else from upstream.
    /// `referer` is the client's `Referer`, forwarded upstream as the policy asks.
    pub async fn get(&self, id: TileId, referer: Option<&HeaderValue>) -> Result<Bytes, ApiError> {
        let cached = self.cache.get_tile(id).await;
        if let Some(c) = &cached
            && c.meta.expires_unix > unix_now()
        {
            metrics::counter!(crate::metrics::TILE_CACHE, "result" => "hit").increment(1);
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
                metrics::counter!(crate::metrics::TILE_CACHE, "result" => result).increment(1);
                match self.cache.put_tile(id, &bytes, &meta).await {
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, ?id, "could not cache tile"),
                }
                Ok(bytes)
            }
            Ok(Upstream::NotModified(meta)) => {
                let Some(c) = cached else {
                    return Err(ApiError::Upstream(
                        "the tile server sent an unexpected 'not modified' reply".into(),
                    ));
                };
                metrics::counter!(crate::metrics::TILE_CACHE, "result" => "revalidated")
                    .increment(1);
                if let Err(e) = self.cache.update_tile_meta(id, &meta).await {
                    tracing::warn!(error = %e, ?id, "could not refresh tile metadata");
                }
                Ok(c.bytes)
            }
            Err(e) => match cached {
                Some(c) => {
                    metrics::counter!(crate::metrics::TILE_CACHE, "result" => "stale_served")
                        .increment(1);
                    tracing::warn!(error = %e, ?id, "tile upstream failed, serving stale copy");
                    Ok(c.bytes)
                }
                None => {
                    metrics::counter!(crate::metrics::TILE_CACHE, "result" => "error").increment(1);
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
        let mut req = self.client.get(id.url(&self.template));
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
            tracing::warn!(error = %e.without_url(), ?id, "tile request failed");
            ApiError::Upstream("could not reach the tile server; map tiles may be missing".into())
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
                        tracing::warn!(error = %e.without_url(), ?id, "tile body failed");
                        ApiError::Upstream("the tile download was interrupted".into())
                    })?
                    .ok_or_else(|| {
                        tracing::warn!(max = MAX_TILE_BYTES, ?id, "tile body too large");
                        ApiError::Upstream("tile server returned a tile that is too large".into())
                    })?;
                if !body.starts_with(PNG_MAGIC) {
                    return Err(ApiError::Upstream(
                        "tile server returned something that is not a PNG".into(),
                    ));
                }
                Ok(Upstream::Fresh(body, meta))
            }
            StatusCode::NOT_MODIFIED => Ok(Upstream::NotModified(new_meta(resp.headers(), stale))),
            StatusCode::NOT_FOUND => Err(ApiError::NotFound("the tile server has no tile here")),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                tracing::warn!(%status, ?id, "tile server is limiting us");
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
                        "tile server redirected; point OBS_TILE_URL at the final URL"
                    );
                }
                tracing::warn!(status = %other, ?id, "tile server error");
                Err(ApiError::Upstream(format!(
                    "the tile server answered with an error (HTTP {})",
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
