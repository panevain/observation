//! The production ports and background tasks behind [`AppState::new`].

use std::sync::Arc;
use std::time::Duration;

use observation_regions_api::{HttpRegionAgent, RegionAgent};
use observation_upstream::{GuardedHttp, HttpClient, Redirects};

use crate::cache::{Cache, CacheCaps};
use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::{AppState, Ports, StateError};

/// The production ports, and the disk cache behind both stores, which
/// [`spawn_background`] trims.
pub(crate) struct Production {
    pub(crate) ports: Ports,
    pub(crate) cache: Arc<Cache>,
}

/// Builds the upstream clients and the region agent client, and opens the
/// cache. Spawns nothing.
pub(crate) fn production_ports(config: &Config) -> Result<Production, StateError> {
    let user_agent = config.user_agent();
    // No redirects: an upstream must not be able to send us elsewhere.
    let upstream = || GuardedHttp::builder(config.allow_private_upstreams, Redirects::None);
    // Every attempt sets its own timeout (at most this one), as before.
    let overpass_http: Arc<dyn HttpClient> = Arc::new(
        upstream()
            .user_agent(&user_agent)
            .timeout(config.overpass_timeout)
            .build()?,
    );
    // Tile requests set none of their own: this one covers each fetch.
    let tile_http: Arc<dyn HttpClient> = Arc::new(
        upstream()
            .user_agent(&user_agent)
            .timeout(Duration::from_secs(20))
            .build()?,
    );
    // Opened before the agent's client is built, as before.
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let cache = Arc::new(Cache::open_with_clock(
        &config.cache_dir,
        CacheCaps {
            bundle_bytes: config.bundle_cache_cap_bytes,
            tile_bytes: config.tile_cache_cap_bytes,
            dem_bytes: config.dem_cache_cap_bytes,
        },
        Arc::clone(&clock),
    ));
    let agent = match &config.regions_url {
        Some(url) => {
            // No overall timeout: snapshot requests long-poll (each request sets its own).
            // The agent normally sits on a private network and is trusted with
            // `OBS_REGIONS_TOKEN`, so addresses are not filtered, but it must not
            // redirect that token elsewhere.
            let client = GuardedHttp::builder(true, Redirects::None)
                .user_agent(&user_agent)
                .connect_timeout(Duration::from_secs(5))
                .build()?;
            let agent: Arc<dyn RegionAgent> = Arc::new(HttpRegionAgent::new(
                Arc::new(client),
                url.clone(),
                config.regions_token.clone(),
            ));
            Some(agent)
        }
        None => None,
    };
    Ok(Production {
        ports: Ports {
            overpass_http,
            tile_http,
            agent,
            bundle_store: cache.clone(),
            tile_store: cache.clone(),
            clock,
        },
        cache,
    })
}

/// Starts the cache trim and, with a region agent, the poller following it,
/// in that order. The poller needs a Tokio runtime.
pub(crate) fn spawn_background(state: &AppState, cache: Arc<Cache>) {
    // Off the request path: a cache from before a cap can be far over it.
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move { cache.trim().await });
    }
    if let Some(regions) = state.regions() {
        tokio::spawn(Arc::clone(regions).run_poller());
    }
}
