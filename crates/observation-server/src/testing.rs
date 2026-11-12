//! Test doubles for the server's ports, and the contract every store
//! implementation must meet. Built for this crate's tests and, with the
//! `testing` feature, for tests elsewhere.
//!
//! The memory stores never evict: size caps, estimates and eviction are
//! properties of the disk [`Cache`](crate::cache::Cache) and are tested on it.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::body::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use observation_core::tiles::TileId;

use crate::clock::Clock;
pub use crate::clock::ManualClock;
use crate::ports::{BundleStore, CachedTile, StoredBundle, TileMeta, TileSet, TileStore};

/// How a memory store answers writes.
#[derive(Debug, Default)]
struct WriteMode {
    /// `put` returns `Ok(false)` and keeps nothing, as a full store would.
    refuse: AtomicBool,
    /// `put` (and `revalidated`) fail with an I/O error.
    fail: AtomicBool,
}

impl WriteMode {
    /// `Some(result)` when the write must not happen.
    fn check(&self) -> Option<io::Result<bool>> {
        if self.fail.load(Ordering::SeqCst) {
            Some(Err(io::Error::other("the store failed (as told)")))
        } else if self.refuse.load(Ordering::SeqCst) {
            Some(Ok(false))
        } else {
            None
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A [`BundleStore`] in memory, stamping each write with its clock's time.
#[derive(Debug)]
pub struct MemBundleStore {
    bundles: Mutex<HashMap<String, StoredBundle>>,
    clock: Arc<dyn Clock>,
    mode: WriteMode,
}

impl MemBundleStore {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            bundles: Mutex::default(),
            clock,
            mode: WriteMode::default(),
        }
    }

    /// From now on, writes are declined (`Ok(false)`) while `refuse` is set.
    pub fn refuse_writes(&self, refuse: bool) {
        self.mode.refuse.store(refuse, Ordering::SeqCst);
    }

    /// From now on, writes fail while `fail` is set.
    pub fn fail_writes(&self, fail: bool) {
        self.mode.fail.store(fail, Ordering::SeqCst);
    }

    /// Stores `stored` as is (any write time), bypassing the write mode.
    pub fn insert(&self, key: &str, stored: StoredBundle) {
        lock(&self.bundles).insert(key.to_owned(), stored);
    }
}

impl BundleStore for MemBundleStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<StoredBundle>> {
        let found = lock(&self.bundles).get(key).cloned();
        async move { found }.boxed()
    }

    fn put<'a>(&'a self, key: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, io::Result<bool>> {
        let result = if !crate::cache::is_safe_key(key) {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe cache key",
            ))
        } else if let Some(r) = self.mode.check() {
            r
        } else {
            let stored = StoredBundle {
                bytes: Bytes::copy_from_slice(bytes),
                written: Some(self.clock.now()),
            };
            lock(&self.bundles).insert(key.to_owned(), stored);
            Ok(true)
        };
        async move { result }.boxed()
    }
}

/// A [`TileStore`] in memory.
#[derive(Debug, Default)]
pub struct MemTileStore {
    tiles: Mutex<HashMap<(TileSetKey, TileId), CachedTile>>,
    revalidations: AtomicU64,
    mode: WriteMode,
}

/// `TileSet` as a map key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TileSetKey {
    Map,
    Dem,
}

impl From<TileSet> for TileSetKey {
    fn from(set: TileSet) -> Self {
        match set {
            TileSet::Map => Self::Map,
            TileSet::Dem => Self::Dem,
        }
    }
}

impl MemTileStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// From now on, writes are declined (`Ok(false)`) while `refuse` is set.
    pub fn refuse_writes(&self, refuse: bool) {
        self.mode.refuse.store(refuse, Ordering::SeqCst);
    }

    /// From now on, writes fail while `fail` is set.
    pub fn fail_writes(&self, fail: bool) {
        self.mode.fail.store(fail, Ordering::SeqCst);
    }

    /// How many times `revalidated` found a tile to update.
    pub fn revalidations(&self) -> u64 {
        self.revalidations.load(Ordering::SeqCst)
    }
}

impl TileStore for MemTileStore {
    fn get(&self, set: TileSet, id: TileId) -> BoxFuture<'_, Option<CachedTile>> {
        let found = lock(&self.tiles).get(&(set.into(), id)).cloned();
        async move { found }.boxed()
    }

    fn put<'a>(
        &'a self,
        set: TileSet,
        id: TileId,
        png: &'a [u8],
        meta: &'a TileMeta,
    ) -> BoxFuture<'a, io::Result<bool>> {
        let result = self.mode.check().unwrap_or_else(|| {
            let tile = CachedTile {
                bytes: Bytes::copy_from_slice(png),
                meta: meta.clone(),
            };
            lock(&self.tiles).insert((set.into(), id), tile);
            Ok(true)
        });
        async move { result }.boxed()
    }

    fn revalidated<'a>(
        &'a self,
        set: TileSet,
        id: TileId,
        meta: &'a TileMeta,
    ) -> BoxFuture<'a, io::Result<()>> {
        let result = match self.mode.check() {
            Some(Err(e)) => Err(e),
            _ => {
                if let Some(tile) = lock(&self.tiles).get_mut(&(set.into(), id)) {
                    tile.meta = meta.clone();
                    self.revalidations.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            }
        };
        async move { result }.boxed()
    }
}

/// Checks what every [`BundleStore`] must do: missing keys, round trips,
/// overwrites, write times and unusable keys. `store` must start empty and
/// accept small writes.
pub async fn bundle_store_contract(store: &dyn BundleStore) {
    assert_eq!(store.get("1.5_2.5").await, None);
    assert!(
        store
            .put("1.5_2.5", b"first")
            .await
            .expect("the store answers")
    );
    let first = store.get("1.5_2.5").await.expect("the store answers");
    assert_eq!(first.bytes.as_ref(), b"first");
    assert!(first.written.is_some());
    // Reading does not change the write time.
    assert_eq!(
        store
            .get("1.5_2.5")
            .await
            .expect("the store answers")
            .written,
        first.written
    );
    assert!(
        store
            .put("1.5_2.5", b"second, longer")
            .await
            .expect("the store answers")
    );
    assert_eq!(
        store
            .get("1.5_2.5")
            .await
            .expect("the store answers")
            .bytes
            .as_ref(),
        b"second, longer"
    );
    assert_eq!(store.get("1.5_2.6").await, None, "keys are exact");
    for bad in ["", "../x", "a/b"] {
        assert!(store.put(bad, b"x").await.is_err(), "{bad:?}");
        assert_eq!(store.get(bad).await, None);
    }
}

/// Checks a [`BundleStore`] that declines `bytes`: `put` answers `Ok(false)`
/// and keeps nothing.
pub async fn bundle_store_declines(store: &dyn BundleStore, bytes: &[u8]) {
    assert!(!store.put("7.5", bytes).await.expect("the store answers"));
    assert_eq!(store.get("7.5").await, None);
}

/// Checks what every [`TileStore`] must do. `store` must start empty and
/// accept small writes.
pub async fn tile_store_contract(store: &dyn TileStore) {
    let id = TileId::new(5, 3, 7).expect("valid tile id");
    let meta = |etag: &str, expires_unix| TileMeta {
        etag: Some(etag.into()),
        last_modified: None,
        expires_unix,
    };
    assert!(store.get(TileSet::Map, id).await.is_none());
    // Revalidating a tile that is not there keeps it not there.
    store
        .revalidated(TileSet::Map, id, &meta("\"x\"", 1))
        .await
        .expect("the store answers");
    assert!(store.get(TileSet::Map, id).await.is_none());

    assert!(
        store
            .put(TileSet::Map, id, b"png-1", &meta("\"a\"", 10))
            .await
            .expect("the store answers")
    );
    let got = store
        .get(TileSet::Map, id)
        .await
        .expect("the store answers");
    assert_eq!(
        (got.bytes.as_ref(), got.meta),
        (&b"png-1"[..], meta("\"a\"", 10))
    );
    assert!(
        store.get(TileSet::Dem, id).await.is_none(),
        "sets are apart"
    );
    assert!(
        store
            .get(TileSet::Map, TileId::new(5, 3, 6).expect("valid tile id"))
            .await
            .is_none()
    );

    // A revalidation replaces the metadata only.
    store
        .revalidated(TileSet::Map, id, &meta("\"a\"", 20))
        .await
        .expect("the store answers");
    let got = store
        .get(TileSet::Map, id)
        .await
        .expect("the store answers");
    assert_eq!(
        (got.bytes.as_ref(), got.meta),
        (&b"png-1"[..], meta("\"a\"", 20))
    );

    // An overwrite replaces both.
    assert!(
        store
            .put(TileSet::Map, id, b"png-2", &meta("\"b\"", 30))
            .await
            .expect("the store answers")
    );
    let got = store
        .get(TileSet::Map, id)
        .await
        .expect("the store answers");
    assert_eq!(
        (got.bytes.as_ref(), got.meta),
        (&b"png-2"[..], meta("\"b\"", 30))
    );
}

/// Checks a [`TileStore`] that declines `png`: `put` answers `Ok(false)` and
/// keeps nothing.
pub async fn tile_store_declines(store: &dyn TileStore, png: &[u8]) {
    let id = TileId::new(6, 1, 1).expect("valid tile id");
    assert!(
        !store
            .put(TileSet::Map, id, png, &TileMeta::default())
            .await
            .expect("the store answers")
    );
    assert!(store.get(TileSet::Map, id).await.is_none());
}

/// A [`metrics::Recorder`] that keeps counter values, for tests that assert
/// metric labels (set it with `metrics::set_default_local_recorder` on a
/// current-thread runtime). Gauges and histograms are dropped.
#[derive(Debug, Default)]
pub struct MetricCounts(Mutex<BTreeMap<String, Arc<CountFn>>>);

impl MetricCounts {
    /// `(labels, value)` of every counter named `name`, labels as
    /// `k=v,k=v` in the order given at the call site, sorted by labels.
    pub fn counters(&self, name: &str) -> Vec<(String, u64)> {
        let prefix = format!("{name}{{");
        lock(&self.0)
            .iter()
            .filter_map(|(k, v)| {
                let labels = k.strip_prefix(&prefix)?.strip_suffix('}')?;
                Some((labels.to_owned(), v.0.load(Ordering::SeqCst)))
            })
            .collect()
    }
}

/// One counter's value.
#[derive(Debug, Default)]
pub struct CountFn(AtomicU64);

impl metrics::CounterFn for CountFn {
    fn increment(&self, value: u64) {
        self.0.fetch_add(value, Ordering::SeqCst);
    }
    fn absolute(&self, value: u64) {
        self.0.store(value, Ordering::SeqCst);
    }
}

impl metrics::Recorder for MetricCounts {
    fn describe_counter(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_gauge(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_histogram(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn register_counter(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Counter {
        let labels: Vec<String> = key
            .labels()
            .map(|l| format!("{}={}", l.key(), l.value()))
            .collect();
        let name = format!("{}{{{}}}", key.name(), labels.join(","));
        let counter = Arc::clone(lock(&self.0).entry(name).or_default());
        metrics::Counter::from_arc(counter)
    }
    fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
        metrics::Gauge::noop()
    }
    fn register_histogram(
        &self,
        _: &metrics::Key,
        _: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        metrics::Histogram::noop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> Arc<ManualClock> {
        Arc::new(ManualClock::at_unix(1_700_000_000))
    }

    #[tokio::test]
    async fn memory_bundle_store_meets_the_contract() {
        bundle_store_contract(&MemBundleStore::new(clock())).await;
        let refusing = MemBundleStore::new(clock());
        refusing.refuse_writes(true);
        bundle_store_declines(&refusing, b"anything").await;
    }

    #[tokio::test]
    async fn memory_tile_store_meets_the_contract() {
        tile_store_contract(&MemTileStore::new()).await;
        let refusing = MemTileStore::new();
        refusing.refuse_writes(true);
        tile_store_declines(&refusing, b"anything").await;
    }

    #[tokio::test]
    async fn memory_stores_stamp_writes_with_their_clock_and_can_fail() {
        let clock = clock();
        let store = MemBundleStore::new(clock.clone());
        assert!(store.put("1.0", b"b").await.unwrap());
        assert_eq!(store.get("1.0").await.unwrap().written, Some(clock.now()));
        clock.advance(std::time::Duration::from_secs(60));
        assert!(store.put("1.0", b"b").await.unwrap());
        assert_eq!(store.get("1.0").await.unwrap().written, Some(clock.now()));
        store.fail_writes(true);
        assert!(store.put("1.0", b"c").await.is_err());
        assert_eq!(store.get("1.0").await.unwrap().bytes.as_ref(), b"b");

        let tiles = MemTileStore::new();
        let id = TileId::new(1, 0, 0).unwrap();
        tiles.fail_writes(true);
        assert!(
            tiles
                .put(TileSet::Map, id, b"p", &TileMeta::default())
                .await
                .is_err()
        );
        assert!(
            tiles
                .revalidated(TileSet::Map, id, &TileMeta::default())
                .await
                .is_err()
        );
        tiles.fail_writes(false);
        assert!(
            tiles
                .put(TileSet::Map, id, b"p", &TileMeta::default())
                .await
                .unwrap()
        );
        tiles
            .revalidated(TileSet::Map, id, &TileMeta::default())
            .await
            .unwrap();
        assert_eq!(tiles.revalidations(), 1);
    }
}
