//! Bundle production: disk cache, single-flight de-duplication and Overpass.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use observation_core::bundle::build_bundle;
use observation_core::geo::BBox;

use crate::cache::Cache;
use crate::error::ApiError;
use crate::overpass::OverpassClient;

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
    attribution: String,
    in_flight: Mutex<HashMap<String, Flight>>,
}

impl BundleService {
    pub fn new(cache: Arc<Cache>, overpass: Arc<OverpassClient>, attribution: String) -> Self {
        Self {
            cache,
            overpass,
            attribution,
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the encoded bundle for `bbox`, which should already be snapped.
    pub async fn get(self: &Arc<Self>, bbox: BBox) -> Result<Bytes, ApiError> {
        let key = bbox.cache_key();
        if let Some((bytes, true)) = self.cache.get_bundle(&key).await {
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
        let handle = tokio::spawn(async move {
            let result = this.produce(&task_key, bbox).await;
            this.in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&task_key);
            result
        });
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
        if let Some((bytes, true)) = self.cache.get_bundle(key).await {
            return Ok(bytes);
        }
        let data = match self.overpass.fetch(&bbox).await {
            Ok(d) => d,
            Err(e) => {
                if let Some((stale, _)) = self.cache.get_bundle(key).await {
                    tracing::warn!(key, error = %e, "serving expired bundle because upstream failed");
                    return Ok(stale);
                }
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
            self.attribution.as_str(),
            generated,
        );
        let bytes = Bytes::from(bundle.encode());
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
}
