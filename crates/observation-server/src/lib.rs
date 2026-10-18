//! Bundle server for Observation.
//!
//! Serves pre-processed OSM area bundles (`/api/area`, `/api/demo`), a caching
//! tile proxy (`/api/tiles/...`) and the static client, with security headers,
//! compression and request timeouts. [`app`] builds the whole [`Router`] so
//! tests can drive it without a socket.

pub mod bundles;
pub mod cache;
pub mod config;
pub mod demo;
pub mod error;
pub mod overpass;
pub mod routes;
pub mod tiles;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use thiserror::Error;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::bundles::BundleService;
use crate::cache::Cache;
use crate::config::Config;
use crate::overpass::OverpassClient;
use crate::tiles::TileProxy;

/// `content-security-policy` sent with every response.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// The shared state could not be built.
#[derive(Debug, Error)]
pub enum StateError {
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
}

impl AppState {
    /// Builds the HTTP clients, opens the cache and prepares the demo bundle.
    pub fn new(config: Config) -> Result<Self, StateError> {
        let user_agent = config.user_agent();
        let overpass_client = reqwest::Client::builder()
            .user_agent(&user_agent)
            .timeout(Duration::from_secs(40))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        let tile_client = reqwest::Client::builder()
            .user_agent(&user_agent)
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .build()?;

        let cache = Arc::new(Cache::open(
            &config.cache_dir,
            config.bundle_ttl,
            config.tile_cache_cap_bytes,
        ));
        let overpass = Arc::new(OverpassClient::new(overpass_client, &config));
        let bundles = Arc::new(BundleService::new(
            Arc::clone(&cache),
            overpass,
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
        })))
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
    // Layers added last are outermost: trace > security headers > compression > timeout.
    routes::router(state)
        .layer(RequestBodyLimitLayer::new(16 * 1024))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
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
        .layer(TraceLayer::new_for_http())
}
