//! Bundle server for Observation.
//!
//! Serves pre-processed OSM area bundles (`/api/area`, `/api/demo`), a caching
//! tile proxy (`/api/tiles/...`), local region management (`/api/regions...`,
//! when a region agent is configured) and the static client, with security
//! headers, compression and request timeouts. [`app`] builds the whole
//! [`Router`] so tests can drive it without a socket.

pub mod bundles;
pub mod cache;
pub mod clock;
pub mod config;
pub mod demo;
pub mod domain;
pub mod elevation;
pub mod error;
pub mod metrics;
pub mod overpass;
pub mod ports;
pub mod ratelimit;
pub mod regions;
pub mod routes;
#[cfg(test)]
mod test_rng;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tiles;
mod wiring;

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use observation_regions_api::RegionAgent;
use observation_upstream::HttpClient;
use thiserror::Error;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::bundles::BundleService;
use crate::clock::{Clock, unix_secs};
use crate::config::Config;
use crate::elevation::Elevation;
use crate::overpass::OverpassClient;
use crate::ports::{BundleStore, TileStore};
use crate::ratelimit::RateLimiter;
use crate::regions::Regions;
use crate::tiles::TileProxy;

/// `content-security-policy` sent with every response.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// The shared state could not be built.
#[derive(Debug, Error)]
pub enum StateError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("could not build HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

/// Shared, cheaply cloneable application state.
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

/// What [`AppState`] is built on: the upstreams, the region agent, the
/// stores and the clock.
#[derive(Debug, Clone)]
pub struct Ports {
    /// Overpass, with the configured Overpass timeout as its default.
    pub overpass_http: Arc<dyn HttpClient>,
    /// Map tiles and elevation tiles alike (one client for both).
    pub tile_http: Arc<dyn HttpClient>,
    /// The region agent; `None` without one.
    pub agent: Option<Arc<dyn RegionAgent>>,
    pub bundle_store: Arc<dyn BundleStore>,
    pub tile_store: Arc<dyn TileStore>,
    pub clock: Arc<dyn Clock>,
}

impl AppState {
    /// Builds the HTTP clients, opens the cache (starting its trim) and
    /// prepares the demo bundle. With a region agent configured, then starts
    /// following its regions (so this must run inside a Tokio runtime).
    pub fn new(config: Config) -> Result<Self, StateError> {
        config.validate()?;
        let production = wiring::production_ports(&config)?;
        let state = Self::from_ports(config, production.ports);
        wiring::spawn_background(&state);
        Ok(state)
    }

    /// The state over `ports`, with the services built on them and the demo
    /// bundle dated by the ports' clock. Spawns nothing: the cache trim and
    /// the region poller are [`AppState::new`]'s. `config` must already be
    /// validated ([`Config::validate`]), as [`AppState::new`] does first.
    pub fn from_ports(config: Config, ports: Ports) -> Self {
        let Ports {
            overpass_http,
            tile_http,
            agent,
            bundle_store,
            tile_store,
            clock,
        } = ports;
        let overpass = Arc::new(OverpassClient::new(overpass_http, &config));
        let regions =
            agent.map(|agent| Arc::new(Regions::new(agent, config.admin_token.is_some())));
        let elevation = Arc::new(Elevation::new(
            Arc::clone(&tile_http),
            &config,
            Arc::clone(&tile_store),
            Arc::clone(&clock),
        ));
        let bundles = Arc::new(BundleService::new(
            bundle_store,
            overpass,
            regions.clone(),
            elevation,
            config.attribution.clone(),
            RateLimiter::new("area", config.area_rate),
            Arc::clone(&clock),
            config.bundle_ttl,
        ));
        let now = unix_secs(clock.now());
        let tiles = TileProxy::new(tile_http, &config, tile_store, clock);

        let demo = match demo::build_demo(&config.attribution, &config.dem_attribution, now) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::error!(error = %e, "demo bundle unavailable");
                None
            }
        };
        Self(Arc::new(Inner {
            config,
            bundles,
            tiles,
            demo,
            regions,
            shutdown: tokio::sync::watch::Sender::new(false),
            streams: Arc::new(tokio::sync::Semaphore::new(MAX_EVENT_STREAMS)),
        }))
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
