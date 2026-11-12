//! The bundle service on fake sources, a memory store and a manual clock:
//! single-flight, the per-client charge and its refunds, expired bundles
//! when an upstream fails, and the bundle cache metric.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use axum::body::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use observation_core::bundle::{AreaBundle, build_bundle};
use observation_core::geo::BBox;
use observation_core::regions::{RegionView, RegionsSnapshot};
use observation_core::terrain::Terrain;
use observation_regions_api::QueryResult;
use observation_regions_api::testing::{AgentCall, FakeRegionAgent};
use tokio::sync::Notify;
use tokio::time::Instant;

use super::BundleService;
use crate::config::Config;
use crate::error::ApiError;
use crate::metrics::{BUNDLE_CACHE, REGION_QUERY};
use crate::overpass::OsmData;
use crate::ports::{BundleStore, StoredBundle};
use crate::ratelimit::{ClientKey, RateLimiter};
use crate::regions::Regions;
use crate::testing::{FakeMapData, FakeTerrain, ManualClock, MemBundleStore, MetricCounts};

const T: u64 = 1_700_000_000;

struct Rig {
    service: Arc<BundleService>,
    map: FakeMapData,
    terrain: FakeTerrain,
    store: Arc<MemBundleStore>,
    clock: Arc<ManualClock>,
    config: Config,
}

/// The production configuration (TTL, attribution, per-client limit) over
/// `store`, whose bundles live in `mem`.
fn rig_on(
    store: Arc<dyn BundleStore>,
    mem: Arc<MemBundleStore>,
    regions: Option<Arc<Regions>>,
) -> Rig {
    let config = Config::from_lookup(|_| None).unwrap();
    let map = FakeMapData::new();
    let terrain = FakeTerrain::new();
    let clock = Arc::new(ManualClock::at_unix(T));
    let service = Arc::new(BundleService::new(
        store,
        Arc::new(map.clone()),
        regions,
        Arc::new(terrain.clone()),
        config.attribution.clone(),
        RateLimiter::new("area", config.area_rate),
        clock.clone(),
        config.bundle_ttl,
    ));
    Rig {
        service,
        map,
        terrain,
        store: mem,
        clock,
        config,
    }
}

/// Every test ends with both sources drained: no call went unscripted and
/// no reply was left unused.
impl Drop for Rig {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.map.assert_drained();
            self.terrain.assert_drained();
        }
    }
}

/// [`rig_on`] a memory store, without a region agent.
fn rig() -> Rig {
    let store = mem_store();
    rig_on(store.clone(), store, None)
}

fn mem_store() -> Arc<MemBundleStore> {
    Arc::new(MemBundleStore::new(Arc::new(ManualClock::at_unix(T))))
}

fn bbox() -> BBox {
    BBox::new(47.6, -122.34, 47.601, -122.339).unwrap()
}

fn client(n: u8) -> ClientKey {
    ClientKey::from_ip([192, 0, 2, n].into())
}

fn data() -> OsmData {
    OsmData {
        osm_base_unix: Some(1_699_999_000),
        ..OsmData::default()
    }
}

/// A valid bundle that differs from any the fakes lead to.
fn other_bundle() -> Bytes {
    Bytes::from(build_bundle(bbox(), &[], &[], &[], Terrain::default(), "older", 1, None).encode())
}

/// Stores `bytes` under the test box as written `age` before now.
fn seed(store: &MemBundleStore, bytes: &Bytes, age: Duration) {
    store.insert(
        &bbox().cache_key(),
        StoredBundle {
            bytes: bytes.clone(),
            written: Some(UNIX_EPOCH + Duration::from_secs(T) - age),
        },
    );
}

async fn stored(store: &MemBundleStore) -> Option<Bytes> {
    store.get(&bbox().cache_key()).await.map(|s| s.bytes)
}

/// How many more builds `client` may start (spends them).
fn tokens_left(service: &BundleService, client: ClientKey) -> usize {
    (0..100)
        .take_while(|_| service.limiter.check(client).is_ok())
        .count()
}

fn bundle_counts(recorder: &MetricCounts) -> Vec<(String, u64)> {
    recorder.counters(BUNDLE_CACHE)
}

fn result(label: &str, n: u64) -> (String, u64) {
    (format!("result={label}"), n)
}

fn spawn_get(
    service: &Arc<BundleService>,
    n: u8,
) -> tokio::task::JoinHandle<Result<Bytes, ApiError>> {
    let service = Arc::clone(service);
    tokio::spawn(async move { service.get(bbox(), client(n)).await })
}

/// Lets spawned requests run up to their first real wait.
async fn run_others() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

/// Runs the scheduler until no build is in flight (bounded).
async fn settle(service: &BundleService) {
    for _ in 0..100 {
        if service.in_flight.lock().unwrap().is_empty() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the build did not finish");
}

#[tokio::test(start_paused = true)]
async fn a_build_asks_both_sources_once_then_is_stored_and_served_as_a_hit() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    r.terrain.reply(Ok(Terrain::default()));
    r.map.reply(Ok(data()));
    let bytes = r.service.get(bbox(), client(1)).await.unwrap();
    let bundle = AreaBundle::decode(&bytes).unwrap();
    assert_eq!(bundle.generated_unix, T, "stamped by the clock");
    assert_eq!(bundle.osm_base_unix, Some(1_699_999_000));
    assert_eq!(bundle.attribution, r.config.attribution);
    assert_eq!(
        stored(&r.store).await,
        Some(bytes.clone()),
        "under the cache key"
    );
    assert_eq!(bundle_counts(&recorder), vec![result("built", 1)]);
    // The second request is answered from the store, for free.
    assert_eq!(r.service.get(bbox(), client(1)).await.unwrap(), bytes);
    assert_eq!((r.terrain.calls(), r.map.calls()), (1, 1));
    assert_eq!(
        bundle_counts(&recorder),
        vec![result("built", 1), result("hit", 1)]
    );
    r.map.assert_drained();
    r.terrain.assert_drained();
    assert_eq!(
        tokens_left(&r.service, client(1)),
        9,
        "only the build was charged"
    );
}

/// One budget for both sources: the map data source is given the time the
/// build started, before the terrain was fetched.
#[tokio::test(start_paused = true)]
async fn the_map_data_budget_starts_before_the_terrain() {
    let r = rig();
    let gate = r.terrain.gated(Ok(Terrain::default()));
    r.map.reply(Ok(data()));
    let started = Instant::now();
    let request = spawn_get(&r.service, 1);
    gate.entered().await;
    tokio::time::advance(Duration::from_secs(3)).await;
    gate.open();
    request.await.unwrap().unwrap();
    assert_eq!(r.map.started(), vec![Some(started)]);
}

#[tokio::test(start_paused = true)]
async fn concurrent_requests_share_one_build_and_only_the_first_is_charged() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    r.terrain.reply(Ok(Terrain::default()));
    let gate = r.map.gated(Ok(data()));
    let first = spawn_get(&r.service, 1);
    gate.entered().await;
    let joiners = [spawn_get(&r.service, 2), spawn_get(&r.service, 3)];
    run_others().await;
    gate.open();
    let bytes = first.await.unwrap().unwrap();
    for j in joiners {
        assert_eq!(j.await.unwrap().unwrap(), bytes);
    }
    assert_eq!((r.terrain.calls(), r.map.calls()), (1, 1));
    assert_eq!(bundle_counts(&recorder), vec![result("built", 1)]);
    assert!(r.service.in_flight.lock().unwrap().is_empty(), "cleaned up");
    assert_eq!(tokens_left(&r.service, client(1)), 9);
    assert_eq!(tokens_left(&r.service, client(2)), 10, "joining is free");
    assert_eq!(tokens_left(&r.service, client(3)), 10, "joining is free");
}

#[tokio::test(start_paused = true)]
async fn a_build_finishes_and_is_stored_when_every_client_disconnects() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    r.terrain.reply(Ok(Terrain::default()));
    let gate = r.map.gated(Ok(data()));
    let request = spawn_get(&r.service, 1);
    gate.entered().await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    gate.open();
    settle(&r.service).await;
    let bytes = stored(&r.store).await.expect("the flight filled the store");
    assert_eq!(bundle_counts(&recorder), vec![result("built", 1)]);
    // The next request is a hit: no second build.
    assert_eq!(r.service.get(bbox(), client(2)).await.unwrap(), bytes);
    assert_eq!((r.terrain.calls(), r.map.calls()), (1, 1));
    assert_eq!(
        bundle_counts(&recorder),
        vec![result("built", 1), result("hit", 1)]
    );
}

/// A store that misses until released, and holds its second read (the
/// build's own re-check) until then; every read after that is real.
#[derive(Debug)]
struct RecheckStore {
    inner: Arc<MemBundleStore>,
    reads: AtomicUsize,
    released: AtomicBool,
    entered: Notify,
    open: Notify,
}

impl BundleStore for RecheckStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<StoredBundle>> {
        let n = self.reads.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 1 {
                self.entered.notify_one();
                self.open.notified().await;
                self.released.store(true, Ordering::SeqCst);
            }
            if self.released.load(Ordering::SeqCst) {
                self.inner.get(key).await
            } else {
                None
            }
        }
        .boxed()
    }

    fn put<'a>(&'a self, key: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, io::Result<bool>> {
        self.inner.put(key, bytes)
    }
}

/// A bundle stored (by an earlier flight) after the requests missed is
/// found by the build's own check: one hit for the whole flight, its
/// starter refunded, and no source asked.
#[tokio::test(start_paused = true)]
async fn a_bundle_found_by_the_builds_own_check_is_one_hit_and_refunded() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let mem = mem_store();
    let store = Arc::new(RecheckStore {
        inner: mem.clone(),
        reads: AtomicUsize::new(0),
        released: AtomicBool::new(false),
        entered: Notify::new(),
        open: Notify::new(),
    });
    let r = rig_on(store.clone(), mem.clone(), None);
    let fresh = other_bundle();
    seed(&mem, &fresh, Duration::ZERO);
    let first = spawn_get(&r.service, 1);
    store.entered.notified().await;
    let joiners = [spawn_get(&r.service, 2), spawn_get(&r.service, 3)];
    run_others().await;
    store.open.notify_one();
    assert_eq!(first.await.unwrap().unwrap(), fresh);
    for j in joiners {
        assert_eq!(j.await.unwrap().unwrap(), fresh);
    }
    assert_eq!(
        store.reads.load(Ordering::SeqCst),
        4,
        "three requests, one re-check"
    );
    assert_eq!((r.terrain.calls(), r.map.calls()), (0, 0));
    assert_eq!(bundle_counts(&recorder), vec![result("hit", 1)]);
    for n in 1..=3 {
        assert_eq!(tokens_left(&r.service, client(n)), 10, "client {n}");
    }
}

/// What a build that asked the sources costs its starter, and what it
/// counts: of these, only public Overpass refusing as busy is given back.
#[tokio::test(start_paused = true)]
async fn the_charge_is_refunded_only_when_overpass_refused_as_busy() {
    let busy = || ApiError::RateLimited {
        retry_after_secs: 30,
    };
    let upstream = || ApiError::Upstream("map data failed".into());
    let slow = || ApiError::UpstreamTimeout("too slow".into());
    let no_terrain = || ApiError::Upstream("elevation failed".into());
    type Row = (
        Result<Terrain, ApiError>,
        Option<Result<OsmData, ApiError>>,
        usize,
        &'static str,
    );
    // (terrain, map data or `None` when not asked, tokens left, metric)
    let rows: [Row; 5] = [
        (Ok(Terrain::default()), Some(Ok(data())), 9, "built"),
        (Ok(Terrain::default()), Some(Err(busy())), 10, "error"),
        (Ok(Terrain::default()), Some(Err(upstream())), 9, "error"),
        (Ok(Terrain::default()), Some(Err(slow())), 9, "error"),
        (Err(no_terrain()), None, 9, "error"),
    ];
    for (i, (terrain, map, left, label)) in rows.into_iter().enumerate() {
        let recorder = MetricCounts::default();
        let _guard = metrics::set_default_local_recorder(&recorder);
        let r = rig();
        let want = match (&terrain, &map) {
            (Err(e), _) | (Ok(_), Some(Err(e))) => Some(e.to_string()),
            _ => None,
        };
        let asked_map = map.is_some();
        r.terrain.reply(terrain);
        if let Some(m) = map {
            r.map.reply(m);
        }
        let got = r.service.get(bbox(), client(1)).await;
        assert_eq!(got.as_ref().err().map(ToString::to_string), want, "row {i}");
        assert_eq!(r.map.calls(), usize::from(asked_map), "row {i}");
        assert_eq!(stored(&r.store).await.is_some(), got.is_ok(), "row {i}");
        assert_eq!(bundle_counts(&recorder), vec![result(label, 1)], "row {i}");
        assert!(recorder.counters(REGION_QUERY).is_empty(), "row {i}");
        assert_eq!(tokens_left(&r.service, client(1)), left, "row {i}");
        r.map.assert_drained();
        r.terrain.assert_drained();
    }
}

/// With an agent configured but not seen online, no region holds the area:
/// the agent is not asked, the query counts as `not_covered`, and the
/// build uses Overpass and stays charged.
#[tokio::test(start_paused = true)]
async fn an_offline_agent_counts_not_covered_and_overpass_is_charged() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    // Never contacted: nothing polls it here, so it stays offline.
    let agent = FakeRegionAgent::new();
    let regions = Arc::new(Regions::new(Arc::new(agent.clone()), false));
    let store = mem_store();
    let r = rig_on(store.clone(), store, Some(regions));
    r.terrain.reply(Ok(Terrain::default()));
    r.map.reply(Ok(data()));
    r.service.get(bbox(), client(1)).await.unwrap();
    assert_eq!(
        recorder.counters(REGION_QUERY),
        vec![("outcome=not_covered".to_owned(), 1)]
    );
    assert_eq!(bundle_counts(&recorder), vec![result("built", 1)]);
    assert_eq!(tokens_left(&r.service, client(1)), 9);
    assert!(agent.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_expired_bundle_is_served_when_a_source_fails() {
    for terrain_fails in [false, true] {
        let recorder = MetricCounts::default();
        let _guard = metrics::set_default_local_recorder(&recorder);
        let r = rig();
        let expired = other_bundle();
        seed(
            &r.store,
            &expired,
            r.config.bundle_ttl + Duration::from_secs(1),
        );
        if terrain_fails {
            r.terrain
                .reply(Err(ApiError::Upstream("elevation failed".into())));
        } else {
            r.terrain.reply(Ok(Terrain::default()));
            r.map
                .reply(Err(ApiError::Upstream("map data failed".into())));
        }
        let got = r.service.get(bbox(), client(1)).await.unwrap();
        let case = format!("terrain fails: {terrain_fails}");
        assert_eq!(got, expired, "{case}");
        assert_eq!(
            bundle_counts(&recorder),
            vec![result("stale_served", 1)],
            "{case}"
        );
        assert_eq!(stored(&r.store).await, Some(expired), "{case}");
        assert_eq!(tokens_left(&r.service, client(1)), 9, "{case}");
        r.map.assert_drained();
        r.terrain.assert_drained();
    }
}

/// A client over its limit gets an expired bundle if there is one, counted
/// nowhere, and the 429 otherwise; no source is asked either way.
#[tokio::test(start_paused = true)]
async fn a_client_over_its_limit_gets_an_expired_bundle_or_429() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    assert_eq!(tokens_left(&r.service, client(1)), 10, "all spent");
    let got = r.service.get(bbox(), client(1)).await;
    assert!(
        matches!(
            got,
            Err(ApiError::TooManyRequests {
                retry_after_secs: 6
            })
        ),
        "{got:?}"
    );
    let expired = other_bundle();
    seed(
        &r.store,
        &expired,
        r.config.bundle_ttl + Duration::from_secs(1),
    );
    assert_eq!(r.service.get(bbox(), client(1)).await.unwrap(), expired);
    assert!(bundle_counts(&recorder).is_empty());
    assert_eq!((r.terrain.calls(), r.map.calls()), (0, 0));
}

/// A bundle the store declines or fails to keep is still served (a store
/// error is only logged) and counted as built.
#[tokio::test(start_paused = true)]
async fn a_bundle_the_store_does_not_keep_is_still_served() {
    for fail in [false, true] {
        let recorder = MetricCounts::default();
        let _guard = metrics::set_default_local_recorder(&recorder);
        let r = rig();
        r.store.fail_writes(fail);
        r.store.refuse_writes(!fail);
        r.terrain.reply(Ok(Terrain::default()));
        r.map.reply(Ok(data()));
        let bytes = r.service.get(bbox(), client(1)).await.unwrap();
        assert!(AreaBundle::decode(&bytes).is_ok(), "fail: {fail}");
        assert_eq!(stored(&r.store).await, None, "fail: {fail}");
        assert_eq!(
            bundle_counts(&recorder),
            vec![result("built", 1)],
            "fail: {fail}"
        );
    }
}

/// Freshness follows the production TTL against the store's written time.
#[tokio::test(start_paused = true)]
async fn a_stored_bundle_is_rebuilt_once_older_than_the_ttl() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    let old = other_bundle();
    seed(&r.store, &old, r.config.bundle_ttl - Duration::from_secs(1));
    assert_eq!(r.service.get(bbox(), client(1)).await.unwrap(), old);
    assert_eq!(bundle_counts(&recorder), vec![result("hit", 1)]);
    assert_eq!((r.terrain.calls(), r.map.calls()), (0, 0));
    seed(&r.store, &old, r.config.bundle_ttl + Duration::from_secs(1));
    r.terrain.reply(Ok(Terrain::default()));
    r.map.reply(Ok(data()));
    let rebuilt = r.service.get(bbox(), client(1)).await.unwrap();
    assert_ne!(rebuilt, old);
    assert_eq!(stored(&r.store).await, Some(rebuilt));
    assert_eq!(
        bundle_counts(&recorder),
        vec![result("built", 1), result("hit", 1)]
    );
}

/// A failed build leaves no flight behind: the next request builds anew.
#[tokio::test(start_paused = true)]
async fn a_failed_build_is_removed_from_the_flights() {
    let r = rig();
    r.terrain.reply(Ok(Terrain::default()));
    r.map
        .reply(Err(ApiError::Upstream("map data failed".into())));
    assert!(r.service.get(bbox(), client(1)).await.is_err());
    assert!(r.service.in_flight.lock().unwrap().is_empty());
    r.terrain.reply(Ok(Terrain::default()));
    r.map.reply(Ok(data()));
    assert!(r.service.get(bbox(), client(1)).await.is_ok());
    assert_eq!((r.terrain.calls(), r.map.calls()), (2, 2));
}

/// Freshness is judged at the injected clock's time, not the wall clock's.
#[tokio::test(start_paused = true)]
async fn freshness_is_judged_by_the_injected_clock() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    let old = other_bundle();
    seed(&r.store, &old, Duration::ZERO);
    r.clock
        .advance(r.config.bundle_ttl - Duration::from_secs(1));
    assert_eq!(r.service.get(bbox(), client(1)).await.unwrap(), old);
    assert_eq!(bundle_counts(&recorder), vec![result("hit", 1)]);
    r.clock.advance(Duration::from_secs(1));
    r.terrain.reply(Ok(Terrain::default()));
    r.map.reply(Ok(data()));
    let rebuilt = r.service.get(bbox(), client(1)).await.unwrap();
    assert_ne!(rebuilt, old, "expired exactly at the TTL");
    assert_eq!(
        bundle_counts(&recorder),
        vec![result("built", 1), result("hit", 1)]
    );
    r.map.assert_drained();
    r.terrain.assert_drained();
}

/// A stored file that is not a bundle of the current version is never
/// served, though freshly written: the upstream's error is returned and
/// counted as `error` (not `stale_served`), and a working upstream
/// rebuilds and replaces it.
#[tokio::test(start_paused = true)]
async fn a_stored_file_that_is_not_a_current_bundle_is_never_served() {
    let mut old_version = observation_core::bundle::BUNDLE_MAGIC.to_vec();
    old_version.extend_from_slice(&(observation_core::bundle::BUNDLE_VERSION - 1).to_le_bytes());
    old_version.extend_from_slice(&[0; 32]);
    for bad in [Bytes::from_static(b"<html>"), Bytes::from(old_version)] {
        let recorder = MetricCounts::default();
        let _guard = metrics::set_default_local_recorder(&recorder);
        let r = rig();
        seed(&r.store, &bad, Duration::ZERO);
        r.terrain.reply(Ok(Terrain::default()));
        r.map
            .reply(Err(ApiError::Upstream("map data failed".into())));
        let got = r.service.get(bbox(), client(1)).await;
        assert!(
            matches!(&got, Err(ApiError::Upstream(m)) if m == "map data failed"),
            "{got:?}"
        );
        assert_eq!(bundle_counts(&recorder), vec![result("error", 1)]);
        r.terrain.reply(Ok(Terrain::default()));
        r.map.reply(Ok(data()));
        let rebuilt = r.service.get(bbox(), client(1)).await.unwrap();
        assert!(AreaBundle::decode(&rebuilt).is_ok());
        assert_eq!(stored(&r.store).await, Some(rebuilt));
        assert_eq!(
            bundle_counts(&recorder),
            vec![result("built", 1), result("error", 1)]
        );
        r.map.assert_drained();
        r.terrain.assert_drained();
    }
}

/// A rig whose agent is online with a ready region holding the test box,
/// imported at `imported_unix`.
async fn rig_with_region(imported_unix: u64) -> (Rig, FakeRegionAgent) {
    let (regions, agent) = regions_with_region(imported_unix).await;
    let store = mem_store();
    (rig_on(store.clone(), store, Some(regions)), agent)
}

/// An agent online with a ready region holding the test box, imported at
/// `imported_unix`, and the server's view of it.
async fn regions_with_region(imported_unix: u64) -> (Arc<Regions>, FakeRegionAgent) {
    let agent = FakeRegionAgent::new();
    let regions = Arc::new(Regions::new(Arc::new(agent.clone()), false));
    let region = RegionView {
        id: "us/washington".into(),
        name: "Washington".into(),
        ready: true,
        bbox: BBox::new(45.5, -124.8, 49.0, -116.9).ok(),
        data_timestamp: None,
        imported_unix: Some(imported_unix),
        last_attempt_unix: None,
        auto_refresh: true,
        refresh_days: None,
        next_refresh_unix: None,
        disk_bytes: None,
        job: None,
        last_error: None,
        last_error_unix: None,
    };
    agent.snapshot_reply(Ok(RegionsSnapshot {
        version: 1,
        regions: vec![region],
        ..RegionsSnapshot::default()
    }));
    regions.poll_once().await.unwrap();
    (regions, agent)
}

/// Overpass JSON as a region answers it, dated 2023-11-14T22:00:00Z.
fn local_result() -> QueryResult {
    QueryResult {
        body: r#"{"osm3s":{"timestamp_osm_base":"2023-11-14T22:00:00Z"},"elements":[]}"#.into(),
        osm_base_unix: None,
    }
}

/// A local region's data builds the bundle without Overpass, so the
/// starter gets the charge back.
#[tokio::test(start_paused = true)]
async fn a_local_regions_data_is_used_and_the_charge_refunded() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let (r, agent) = rig_with_region(T - 86_400).await;
    r.terrain.reply(Ok(Terrain::default()));
    agent.query_reply(Ok(Some(local_result())));
    let bytes = r.service.get(bbox(), client(1)).await.unwrap();
    let bundle = AreaBundle::decode(&bytes).unwrap();
    assert_eq!(bundle.osm_base_unix, Some(1_699_999_200));
    assert!(matches!(
        agent.calls().last(),
        Some(AgentCall::Query { timeout, .. }) if *timeout == Duration::from_secs(15)
    ));
    assert_eq!(r.map.calls(), 0);
    assert_eq!(
        recorder.counters(REGION_QUERY),
        vec![("outcome=ok".to_owned(), 1)]
    );
    assert_eq!(bundle_counts(&recorder), vec![result("built", 1)]);
    assert_eq!(tokens_left(&r.service, client(1)), 10);
    agent.assert_drained();
}

/// A local region that fails or has nothing for the box leaves the build
/// to Overpass, charged; the terrain comes first, so a terrain failure
/// asks neither.
#[tokio::test(start_paused = true)]
async fn a_local_region_that_fails_or_has_nothing_leaves_overpass_charged() {
    for (local, label) in [
        (
            Err("agent answered 500 Internal Server Error: ".to_owned()),
            "error",
        ),
        (Ok(None), "not_covered"),
    ] {
        let recorder = MetricCounts::default();
        let _guard = metrics::set_default_local_recorder(&recorder);
        let (r, agent) = rig_with_region(T - 86_400).await;
        r.terrain.reply(Ok(Terrain::default()));
        agent.query_reply(local);
        r.map.reply(Ok(data()));
        r.service.get(bbox(), client(1)).await.unwrap();
        assert_eq!(r.map.calls(), 1, "{label}");
        assert_eq!(
            recorder.counters(REGION_QUERY),
            vec![(format!("outcome={label}"), 1)]
        );
        assert_eq!(bundle_counts(&recorder), vec![result("built", 1)]);
        assert_eq!(tokens_left(&r.service, client(1)), 9, "{label}");
        agent.assert_drained();
        r.map.assert_drained();
    }

    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let (r, agent) = rig_with_region(T - 86_400).await;
    r.terrain
        .reply(Err(ApiError::Upstream("elevation failed".into())));
    assert!(r.service.get(bbox(), client(1)).await.is_err());
    assert_eq!(agent.calls().len(), 1, "only the snapshot");
    assert_eq!(r.map.calls(), 0);
    assert!(recorder.counters(REGION_QUERY).is_empty());
    assert_eq!(tokens_left(&r.service, client(1)), 9);
    agent.assert_drained();
    r.map.assert_drained();
    r.terrain.assert_drained();
}

/// An agent that went offline is not asked, though its last snapshot holds
/// a region for the box: the bundle comes from Overpass, charged, and the
/// query counts as `not_covered`.
#[tokio::test(start_paused = true)]
async fn an_agent_gone_offline_is_not_asked_though_its_region_holds_the_box() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let (regions, agent) = regions_with_region(T - 86_400).await;
    agent.snapshot_reply(Err("down".into()));
    assert!(regions.poll_once().await.is_err());
    assert!(!regions.status().agent_online);
    assert_eq!(regions.status().snapshot.regions.len(), 1, "kept");
    let store = mem_store();
    let r = rig_on(store.clone(), store, Some(regions));
    r.terrain.reply(Ok(Terrain::default()));
    r.map.reply(Ok(data()));
    let bytes = r.service.get(bbox(), client(1)).await.unwrap();
    assert!(AreaBundle::decode(&bytes).is_ok());
    assert_eq!(r.map.calls(), 1);
    assert_eq!(
        recorder.counters(REGION_QUERY),
        vec![("outcome=not_covered".to_owned(), 1)]
    );
    assert_eq!(bundle_counts(&recorder), vec![result("built", 1)]);
    assert_eq!(tokens_left(&r.service, client(1)), 9);
    assert_eq!(agent.calls().len(), 2, "the two snapshots, no query");
    agent.assert_drained();
}

/// A bundle stored before the region holding its box was imported is
/// rebuilt (from the region); one stored since is fresh.
#[tokio::test(start_paused = true)]
async fn bundles_older_than_the_regions_import_are_rebuilt() {
    let (r, agent) = rig_with_region(T - 60).await;
    let old = other_bundle();
    seed(&r.store, &old, Duration::from_secs(60));
    assert_eq!(
        r.service.get(bbox(), client(1)).await.unwrap(),
        old,
        "stored at the import: fresh"
    );
    seed(&r.store, &old, Duration::from_secs(61));
    r.terrain.reply(Ok(Terrain::default()));
    agent.query_reply(Ok(Some(local_result())));
    let rebuilt = r.service.get(bbox(), client(1)).await.unwrap();
    assert_ne!(rebuilt, old);
    assert_eq!(stored(&r.store).await, Some(rebuilt));
    agent.assert_drained();
}
