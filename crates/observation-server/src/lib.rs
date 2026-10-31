//! Bundle server for Observation.
//!
//! Serves pre-processed OSM area bundles (`/api/area`, `/api/demo`), a caching
//! tile proxy (`/api/tiles/...`), local region management (`/api/regions...`,
//! when a region agent is configured) and the static client, with security
//! headers, compression and request timeouts. [`app`] builds the whole
//! [`Router`] so tests can drive it without a socket.

pub mod bundles;
pub mod cache;
pub mod config;
pub mod demo;
pub mod error;
pub mod metrics;
pub mod overpass;
pub mod regions;
pub mod routes;
pub mod tiles;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use observation_upstream::{Redirects, client_builder};
use thiserror::Error;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::bundles::BundleService;
use crate::cache::{Cache, CacheCaps};
use crate::config::Config;
use crate::overpass::OverpassClient;
use crate::regions::Regions;
use crate::tiles::TileProxy;

/// `content-security-policy` sent with every response.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// Reads an upstream response body, giving up as soon as it grows past `max`
/// bytes. `Ok(None)` means it was too large; a `Content-Length` is not
/// required (chunked bodies are capped while streaming, not after buffering).
pub(crate) async fn read_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> Result<Option<Bytes>, reqwest::Error> {
    let declared = resp.content_length();
    if declared.is_some_and(|n| n > max as u64) {
        return Ok(None);
    }
    // At most `max` here, so the cast cannot truncate.
    let mut body = Vec::with_capacity(declared.map_or(0, |n| n as usize));
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > max {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(Bytes::from(body)))
}

/// The shared state could not be built.
#[derive(Debug, Error)]
pub enum StateError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("could not build HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

/// Shared, cheaply clonable application state.
#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    config: Config,
    bundles: Arc<BundleService>,
    tiles: TileProxy,
    /// `None` if the embedded fixture failed to build (logged at startup).
    demo: Option<Bytes>,
    /// `None` without `OBS_REGIONS_URL`.
    regions: Option<Arc<Regions>>,
    /// Flipped to `true` when the server starts shutting down, to end event streams.
    shutdown: tokio::sync::watch::Sender<bool>,
    /// Bounds the open region event streams.
    streams: Arc<tokio::sync::Semaphore>,
}

/// Most region event streams open at once.
pub const MAX_EVENT_STREAMS: usize = 256;

impl AppState {
    /// Builds the HTTP clients, opens the cache and prepares the demo bundle.
    /// With a region agent configured, also starts following its regions
    /// (so this must run inside a Tokio runtime).
    pub fn new(config: Config) -> Result<Self, StateError> {
        let user_agent = config.user_agent();
        config.validate()?;
        // No redirects: an upstream must not be able to send us elsewhere.
        let upstream = || client_builder(config.allow_private_upstreams, Redirects::None);
        let overpass_client = upstream()
            .user_agent(&user_agent)
            .timeout(config.overpass_timeout)
            .build()?;
        let tile_client = upstream()
            .user_agent(&user_agent)
            .timeout(Duration::from_secs(20))
            .build()?;

        let cache = Arc::new(Cache::open(
            &config.cache_dir,
            config.bundle_ttl,
            CacheCaps {
                bundle_bytes: config.bundle_cache_cap_bytes,
                tile_bytes: config.tile_cache_cap_bytes,
            },
        ));
        let overpass = Arc::new(OverpassClient::new(overpass_client, &config));
        let regions = match &config.regions_url {
            Some(url) => {
                // No overall timeout: snapshot requests long-poll (each request sets its own).
                // The agent normally sits on a private network and is trusted with
                // `OBS_REGIONS_TOKEN`, so addresses are not filtered, but it must not
                // redirect that token elsewhere.
                let client = client_builder(true, Redirects::None)
                    .user_agent(&user_agent)
                    .connect_timeout(Duration::from_secs(5))
                    .build()?;
                let regions = Arc::new(Regions::new(
                    client,
                    url.clone(),
                    config.regions_token.clone(),
                    config.admin_token.is_some(),
                ));
                tokio::spawn(Arc::clone(&regions).run_poller());
                Some(regions)
            }
            None => None,
        };
        let bundles = Arc::new(BundleService::new(
            Arc::clone(&cache),
            overpass,
            regions.clone(),
            config.attribution.clone(),
        ));
        let tiles = TileProxy::new(tile_client, &config, cache);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let demo = match demo::build_demo(&config.attribution, now) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::error!(error = %e, "demo bundle unavailable");
                None
            }
        };
        Ok(Self(Arc::new(Inner {
            config,
            bundles,
            tiles,
            demo,
            regions,
            shutdown: tokio::sync::watch::Sender::new(false),
            streams: Arc::new(tokio::sync::Semaphore::new(MAX_EVENT_STREAMS)),
        })))
    }

    /// The region agent client, if `OBS_REGIONS_URL` is set.
    pub fn regions(&self) -> Option<&Arc<Regions>> {
        self.0.regions.as_ref()
    }

    /// Ends open event streams so a graceful shutdown does not wait on them.
    pub fn begin_shutdown(&self) {
        self.0.shutdown.send_replace(true);
    }

    pub(crate) fn shutdown_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.0.shutdown.subscribe()
    }

    pub(crate) fn event_streams(&self) -> &Arc<tokio::sync::Semaphore> {
        &self.0.streams
    }

    pub fn config(&self) -> &Config {
        &self.0.config
    }

    pub fn bundles(&self) -> &Arc<BundleService> {
        &self.0.bundles
    }

    pub fn tiles(&self) -> &TileProxy {
        &self.0.tiles
    }

    /// The encoded demo bundle, if it could be built.
    pub fn demo(&self) -> Option<&Bytes> {
        self.0.demo.as_ref()
    }
}

/// Builds the complete application router with all middleware.
pub fn app(state: AppState) -> Router {
    let timeout = state.config().request_timeout;
    let sec = |name: HeaderName, value: &'static str| {
        SetResponseHeaderLayer::overriding(name, HeaderValue::from_static(value))
    };
    let request_id = HeaderName::from_static("x-request-id");
    let metrics_on = state.config().metrics_bind.is_some();
    // Layers added last are outermost: request id > trace > security headers >
    // compression > metrics > timeout. Metrics sit outside the timeout so timed-out
    // requests are counted too. Event streams stay open, so they skip the timeout.
    routes::router(state.clone())
        .layer(RequestBodyLimitLayer::new(16 * 1024))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ))
        .merge(routes::streams(state))
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| async move {
                if metrics_on {
                    metrics::track(req, next).await
                } else {
                    next.run(req).await
                }
            },
        ))
        .layer(CompressionLayer::new())
        .layer(sec(header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .layer(sec(
            header::REFERRER_POLICY,
            "strict-origin-when-cross-origin",
        ))
        .layer(sec(header::X_FRAME_OPTIONS, "DENY"))
        // Links the UI opens in new tabs (spec sheets, OSM copyright) get no
        // `window.opener` handle back to the app.
        .layer(sec(
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ))
        .layer(sec(
            header::CONTENT_SECURITY_POLICY,
            CONTENT_SECURITY_POLICY,
        ))
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
                let id = req
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-");
                // DEBUG under tower_http's target, like TraceLayer's own events, so
                // `RUST_LOG=tower_http=debug` shows the id and the default level logs none.
                tracing::debug_span!(
                    target: "tower_http::trace::make_span",
                    "request",
                    method = %req.method(),
                    uri = %req.uri().path(),
                    request_id = %id,
                )
            }),
        )
        // Echo the id on the response; the outermost layer assigns it first.
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(len: usize) -> reqwest::Response {
        reqwest::Response::from(axum::http::Response::new(reqwest::Body::from(vec![
            7u8;
            len
        ])))
    }

    #[tokio::test]
    async fn read_capped_boundary() {
        let body = read_capped(response(16), 16).await.unwrap();
        assert_eq!(body.map(|b| b.len()), Some(16), "exactly `max` is accepted");
        assert!(read_capped(response(0), 16).await.unwrap().is_some());
        // Declared length over the cap: refused before reading.
        assert!(read_capped(response(17), 16).await.unwrap().is_none());
    }
}
