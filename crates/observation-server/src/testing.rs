//! Test doubles for the server's ports (stores, and the map data and terrain
//! sources of a bundle), the contract every store implementation must meet,
//! and [`TestApp`], the whole application over fakes. Built for this crate's
//! tests and, with the `testing` feature, for tests elsewhere.
//!
//! The memory stores never evict: size caps, estimates and eviction are
//! properties of the disk [`Cache`] and are tested on it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::body::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use observation_core::geo::BBox;
use observation_core::terrain::Terrain;
use observation_core::tiles::TileId;
use tokio::sync::Notify;
use tokio::time::Instant;

use axum::Router;
use observation_regions_api::RegionAgent;
use observation_regions_api::testing::FakeRegionAgent;
use observation_upstream::testing::FakeHttp;

use crate::cache::{Cache, CacheCaps};
use crate::clock::Clock;
pub use crate::clock::ManualClock;
use crate::config::Config;
use crate::error::ApiError;
use crate::overpass::OsmData;
use crate::ports::{
    BundleStore, CachedTile, MapDataSource, StoredBundle, TerrainSource, TileMeta, TileSet,
    TileStore,
};
use crate::{AppState, Ports, app};

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

/// A scripted [`MapDataSource`] ([`FakeMapData`]) or [`TerrainSource`]
/// ([`FakeTerrain`]). Calls take the queued replies in order; a reply can be
/// held behind a [`Gate`] until the test opens it. A call with no reply left
/// answers [`ApiError::Internal`] and is counted, so check
/// [`Self::assert_drained`] (a panic inside a spawned build would be lost).
/// Clones share the script.
#[derive(Debug)]
pub struct FakeSource<T>(Arc<SourceInner<T>>);

/// The fake Overpass of a bundle build.
pub type FakeMapData = FakeSource<OsmData>;
/// The fake elevation of a bundle build.
pub type FakeTerrain = FakeSource<Terrain>;

/// A queued reply and the gate holding it, if any.
type Scripted<T> = (Result<T, ApiError>, Option<Gate>);

#[derive(Debug)]
struct SourceInner<T> {
    replies: Mutex<VecDeque<Scripted<T>>>,
    /// The boxes asked for, with the budget start for map data.
    calls: Mutex<Vec<(BBox, Option<Instant>)>>,
    unscripted: AtomicUsize,
}

impl<T> Clone for FakeSource<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Default for FakeSource<T> {
    fn default() -> Self {
        Self(Arc::new(SourceInner {
            replies: Mutex::default(),
            calls: Mutex::default(),
            unscripted: AtomicUsize::new(0),
        }))
    }
}

impl<T: Send + 'static> FakeSource<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a reply given at once.
    pub fn reply(&self, reply: Result<T, ApiError>) {
        lock(&self.0.replies).push_back((reply, None));
    }

    /// Queues a reply held until the returned gate is opened.
    pub fn gated(&self, reply: Result<T, ApiError>) -> Gate {
        let gate = Gate::default();
        lock(&self.0.replies).push_back((reply, Some(gate.clone())));
        gate
    }

    /// How many calls were made, scripted or not.
    pub fn calls(&self) -> usize {
        lock(&self.0.calls).len()
    }

    /// The budget start each map data call was given, in call order.
    pub fn started(&self) -> Vec<Option<Instant>> {
        lock(&self.0.calls).iter().map(|c| c.1).collect()
    }

    /// Panics if a reply is left or a call found none.
    #[track_caller]
    pub fn assert_drained(&self) {
        assert_eq!(
            self.0.unscripted.load(Ordering::SeqCst),
            0,
            "unscripted calls"
        );
        assert!(lock(&self.0.replies).is_empty(), "replies left");
    }

    fn answer(
        &self,
        bbox: &BBox,
        started: Option<Instant>,
    ) -> BoxFuture<'static, Result<T, ApiError>> {
        lock(&self.0.calls).push((*bbox, started));
        let next = lock(&self.0.replies).pop_front();
        let Some((reply, gate)) = next else {
            self.0.unscripted.fetch_add(1, Ordering::SeqCst);
            return async { Err(ApiError::Internal("unscripted fake source call".into())) }.boxed();
        };
        async move {
            if let Some(gate) = gate {
                gate.0.entered.notify_one();
                gate.0.open.notified().await;
            }
            reply
        }
        .boxed()
    }
}

impl MapDataSource for FakeMapData {
    fn fetch_since<'a>(
        &'a self,
        bbox: &'a BBox,
        started: Instant,
    ) -> BoxFuture<'a, Result<OsmData, ApiError>> {
        self.answer(bbox, Some(started))
    }
}

impl TerrainSource for FakeTerrain {
    fn terrain<'a>(&'a self, bbox: &'a BBox) -> BoxFuture<'a, Result<Terrain, ApiError>> {
        self.answer(bbox, None)
    }
}

/// Holds one scripted reply of a [`FakeSource`] until opened.
#[derive(Debug, Clone, Default)]
pub struct Gate(Arc<GateInner>);

#[derive(Debug, Default)]
struct GateInner {
    entered: Notify,
    open: Notify,
}

impl Gate {
    /// Waits until the call holding this reply has started.
    pub async fn entered(&self) {
        self.0.entered.notified().await;
    }

    /// Lets the held reply through (now, or as soon as its call comes).
    pub fn open(&self) {
        self.0.open.notify_one();
    }
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

/// The whole application over fakes: [`AppState::from_ports`] with
/// [`FakeHttp`] for Overpass and for tiles, an optional [`FakeRegionAgent`],
/// memory stores and a [`ManualClock`], on the production configuration
/// unless [`TestAppBuilder::config`] changes it. The fakes time out as the
/// production clients do when a request sets no timeout (the Overpass
/// timeout, 20 s for tiles). Nothing is spawned: a test polls the agent
/// itself (`state.regions()`).
///
/// It leaves out what only the production adapters do: real sockets, TLS,
/// the address filter and proxy handling, the redirect policy, the
/// User-Agent, the client timeouts, the disk cache's eviction (the memory
/// stores never evict) and the background trim and poller. Those are tested
/// in `wiring`'s tests, `tests/` (on the real `AppState::new`) and
/// `observation-upstream/tests/guarded.rs`.
///
/// Dropping it checks that every fake was drained: no request went
/// unscripted and no reply was left unused.
#[derive(Debug)]
pub struct TestApp {
    pub state: AppState,
    pub router: Router,
    pub overpass: FakeHttp,
    pub tiles: FakeHttp,
    pub agent: Option<FakeRegionAgent>,
    /// The memory stores; `None` with [`TestAppBuilder::disk_cache`].
    pub bundle_store: Option<Arc<MemBundleStore>>,
    pub tile_store: Option<Arc<MemTileStore>>,
    pub clock: Arc<ManualClock>,
}

impl TestApp {
    pub fn builder() -> TestAppBuilder {
        TestAppBuilder::default()
    }

    /// Checks every fake now (it is also checked on drop).
    pub fn assert_drained(&self) {
        self.overpass.assert_drained();
        self.tiles.assert_drained();
        if let Some(agent) = &self.agent {
            agent.assert_drained();
        }
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.assert_drained();
        }
    }
}

/// A change to the configuration of a [`TestApp`].
type ConfigTweak = Box<dyn FnOnce(&mut Config)>;

/// Builds a [`TestApp`]; anything not set is a fresh fake, no agent, and a
/// clock at 2023-11-14T22:13:20Z (Unix 1 700 000 000).
#[derive(Default)]
pub struct TestAppBuilder {
    clock: Option<Arc<ManualClock>>,
    overpass: Option<FakeHttp>,
    tiles: Option<FakeHttp>,
    agent: Option<FakeRegionAgent>,
    disk_cache: Option<PathBuf>,
    tweaks: Vec<ConfigTweak>,
}

impl std::fmt::Debug for TestAppBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestAppBuilder")
            .field("clock", &self.clock)
            .field("overpass", &self.overpass)
            .field("tiles", &self.tiles)
            .field("agent", &self.agent)
            .field("disk_cache", &self.disk_cache)
            .field("tweaks", &self.tweaks.len())
            .finish()
    }
}

impl TestAppBuilder {
    pub fn clock(mut self, clock: Arc<ManualClock>) -> Self {
        self.clock = Some(clock);
        self
    }

    pub fn overpass(mut self, fake: FakeHttp) -> Self {
        self.overpass = Some(fake);
        self
    }

    /// Map and elevation tiles share it, as they share one client.
    pub fn tiles(mut self, fake: FakeHttp) -> Self {
        self.tiles = Some(fake);
        self
    }

    pub fn agent(mut self, fake: FakeRegionAgent) -> Self {
        self.agent = Some(fake);
        self
    }

    /// Stores bundles and tiles in the disk cache at `root` (with the
    /// configured caps and the test's clock) instead of in memory.
    pub fn disk_cache(mut self, root: impl Into<PathBuf>) -> Self {
        self.disk_cache = Some(root.into());
        self
    }

    /// Changes the production configuration; applied in order.
    pub fn config(mut self, tweak: impl FnOnce(&mut Config) + 'static) -> Self {
        self.tweaks.push(Box::new(tweak));
        self
    }

    /// Panics if the configuration does not validate, as `AppState::new`
    /// would refuse it.
    pub fn build(self) -> TestApp {
        let mut config = Config::from_lookup(|_| None).expect("the production configuration");
        for tweak in self.tweaks {
            tweak(&mut config);
        }
        config.validate().expect("a valid configuration");
        let clock = self
            .clock
            .unwrap_or_else(|| Arc::new(ManualClock::at_unix(1_700_000_000)));
        let overpass = self
            .overpass
            .unwrap_or_default()
            .with_timeout(config.overpass_timeout);
        let tiles = self
            .tiles
            .unwrap_or_default()
            .with_timeout(crate::wiring::TILE_TIMEOUT);
        let (bundle_store, tile_store, bundle_port, tile_port): (
            _,
            _,
            Arc<dyn BundleStore>,
            Arc<dyn TileStore>,
        ) = match &self.disk_cache {
            Some(root) => {
                let cache = Arc::new(Cache::open_with_clock(
                    root,
                    CacheCaps {
                        bundle_bytes: config.bundle_cache_cap_bytes,
                        tile_bytes: config.tile_cache_cap_bytes,
                        dem_bytes: config.dem_cache_cap_bytes,
                    },
                    clock.clone(),
                ));
                (None, None, cache.clone(), cache)
            }
            None => {
                let bundles = Arc::new(MemBundleStore::new(clock.clone()));
                let tiles = Arc::new(MemTileStore::new());
                (Some(bundles.clone()), Some(tiles.clone()), bundles, tiles)
            }
        };
        let state = AppState::from_ports(
            config,
            Ports {
                overpass_http: Arc::new(overpass.clone()),
                tile_http: Arc::new(tiles.clone()),
                agent: self
                    .agent
                    .clone()
                    .map(|a| Arc::new(a) as Arc<dyn RegionAgent>),
                bundle_store: bundle_port,
                tile_store: tile_port,
                clock: clock.clone(),
            },
        );
        TestApp {
            router: app(state.clone()),
            state,
            overpass,
            tiles,
            agent: self.agent,
            bundle_store,
            tile_store,
            clock,
        }
    }
}

#[cfg(test)]
mod test_app_tests;

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

    #[tokio::test(start_paused = true)]
    async fn a_fake_source_answers_in_order_and_holds_gated_replies() {
        let bbox = BBox::new(47.0, -122.0, 47.001, -121.999).unwrap();
        let map = FakeMapData::new();
        let gate = map.gated(Ok(OsmData {
            osm_base_unix: Some(1),
            ..OsmData::default()
        }));
        map.reply(Err(ApiError::Upstream("second".into())));
        let started = Instant::now();
        let first = tokio::spawn({
            let map = map.clone();
            async move { map.fetch_since(&bbox, started).await }
        });
        gate.entered().await;
        assert!(!first.is_finished(), "held until opened");
        let second = map.fetch_since(&bbox, started).await;
        assert!(matches!(second, Err(ApiError::Upstream(m)) if m == "second"));
        gate.open();
        assert_eq!(first.await.unwrap().unwrap().osm_base_unix, Some(1));
        assert_eq!(map.started(), vec![Some(started); 2]);
        map.assert_drained();

        let terrain = FakeTerrain::new();
        assert!(matches!(
            terrain.terrain(&bbox).await,
            Err(ApiError::Internal(_))
        ));
        assert_eq!(terrain.calls(), 1);
        assert_eq!(terrain.started(), vec![None]);
        let drained = std::panic::catch_unwind(|| terrain.assert_drained());
        assert!(drained.is_err(), "an unscripted call is reported");
    }
}
