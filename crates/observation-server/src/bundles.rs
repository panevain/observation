//! Bundle production: disk cache, single-flight de-duplication, and the data
//! source: a local region when one holds the area, else public Overpass.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use observation_core::bundle::build_bundle;
use observation_core::geo::BBox;
use tracing::Instrument;

use crate::cache::Cache;
use crate::error::ApiError;
use crate::overpass::{OsmData, OverpassClient};
use crate::regions::Regions;

/// Longest wait for a local region (they answer in well under a second), so
/// most of the request's time is left for the Overpass fallback.
const LOCAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

type Flight = Shared<BoxFuture<'static, Result<Bytes, ApiError>>>;

/// Produces encoded bundles for (snapped) bounding boxes.
///
/// Concurrent requests for the same cache key share one in-flight job that is
/// spawned as its own task, so it completes (and fills the cache) even if every
/// waiting client disconnects.
#[derive(Debug)]
pub struct BundleService {
    cache: Arc<Cache>,
    overpass: Arc<OverpassClient>,
    regions: Option<Arc<Regions>>,
    attribution: String,
    in_flight: Mutex<HashMap<String, Flight>>,
}

impl BundleService {
    pub fn new(
        cache: Arc<Cache>,
        overpass: Arc<OverpassClient>,
        regions: Option<Arc<Regions>>,
        attribution: String,
    ) -> Self {
        Self {
            cache,
            overpass,
            regions,
            attribution,
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the encoded bundle for `bbox`, which should already be snapped.
    pub async fn get(self: &Arc<Self>, bbox: BBox) -> Result<Bytes, ApiError> {
        let key = bbox.cache_key();
        if let Some((bytes, true)) = self.cache.get_bundle(&key, self.valid_after(&bbox)).await {
            metrics::counter!(crate::metrics::BUNDLE_CACHE, "result" => "hit").increment(1);
            return Ok(bytes);
        }
        self.flight(key, bbox).await
    }

    fn flight(self: &Arc<Self>, key: String, bbox: BBox) -> Flight {
        let mut map = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(f) = map.get(&key) {
            return f.clone();
        }
        let this = Arc::clone(self);
        let task_key = key.clone();
        // Spawned while holding the lock: the task's own clean-up below cannot
        // run before the entry has been inserted.
        // The first caller's request span rides along, so upstream warnings carry its request id
        // (waiters that joined the flight share it).
        let handle = tokio::spawn(
            async move {
                let result = this.produce(&task_key, bbox).await;
                this.in_flight
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&task_key);
                result
            }
            .instrument(tracing::Span::current()),
        );
        let shared = async move {
            handle
                .await
                .unwrap_or_else(|e| Err(ApiError::Internal(format!("bundle task failed: {e}"))))
        }
        .boxed()
        .shared();
        map.insert(key, shared.clone());
        shared
    }

    /// Cache re-check, Overpass fetch, bundle build, cache write. Falls back to
    /// an expired cached bundle when the upstream fails.
    async fn produce(&self, key: &str, bbox: BBox) -> Result<Bytes, ApiError> {
        // A previous flight may have finished between the caller's check and now.
        if let Some((bytes, true)) = self.cache.get_bundle(key, self.valid_after(&bbox)).await {
            metrics::counter!(crate::metrics::BUNDLE_CACHE, "result" => "hit").increment(1);
            return Ok(bytes);
        }
        // One budget for both sources: Overpass gets what the local try left.
        let started = std::time::Instant::now();
        let local = self.fetch_local(&bbox).await;
        let fetched = match local {
            Some(d) => Ok(d),
            None => self.overpass.fetch_since(&bbox, started).await,
        };
        let data = match fetched {
            Ok(d) => d,
            Err(e) => {
                if let Some((stale, _)) = self.cache.get_bundle(key, None).await {
                    metrics::counter!(crate::metrics::BUNDLE_CACHE, "result" => "stale_served")
                        .increment(1);
                    tracing::warn!(key, error = %e, "serving expired bundle because upstream failed");
                    return Ok(stale);
                }
                metrics::counter!(crate::metrics::BUNDLE_CACHE, "result" => "error").increment(1);
                return Err(e);
            }
        };
        let generated = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let bundle = build_bundle(
            bbox,
            &data.buildings,
            &data.roads,
            &data.addresses,
            self.attribution.as_str(),
            generated,
        );
        let bytes = Bytes::from(bundle.encode());
        metrics::counter!(crate::metrics::BUNDLE_CACHE, "result" => "built").increment(1);
        // `Ok(false)` (not cached: too large for the cap) is logged by the cache.
        if let Err(e) = self.cache.put_bundle(key, &bytes).await {
            tracing::warn!(key, error = %e, "could not cache bundle");
        }
        tracing::info!(
            key,
            buildings = bundle.buildings.len(),
            roads = bundle.roads.len(),
            bytes = bytes.len(),
            "built bundle"
        );
        Ok(bytes)
    }

    /// When a local region holding `bbox` got its data: bundles cached before
    /// then are rebuilt.
    fn valid_after(&self, bbox: &BBox) -> Option<SystemTime> {
        self.regions.as_ref()?.data_since(bbox)
    }

    /// Data from a local region, if one holds `bbox` and answers.
    async fn fetch_local(&self, bbox: &BBox) -> Option<OsmData> {
        let regions = self.regions.as_ref()?;
        let outcome = regions.fetch(bbox, LOCAL_TIMEOUT).await;
        let label = match &outcome {
            Ok(Some(_)) => "ok",
            Ok(None) => "not_covered",
            Err(e) => {
                tracing::warn!(error = %e, "local region query failed; using Overpass");
                "error"
            }
        };
        metrics::counter!(crate::metrics::REGION_QUERY, "outcome" => label).increment(1);
        outcome.ok().flatten()
    }
}
