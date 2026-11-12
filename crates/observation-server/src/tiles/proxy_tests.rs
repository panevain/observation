//! The map tile proxy on a fake upstream, a memory store and a manual clock.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderValue, Method, StatusCode, header};
use observation_core::tiles::TileId;
use observation_upstream::testing::{FakeHttp, FakeReply};
use observation_upstream::{HttpRequest, TransportErrorKind};

use super::{PNG_MAGIC, TileProxy};
use crate::clock::{Clock, ManualClock};
use crate::config::Config;
use crate::error::ApiError;
use crate::ports::{TileMeta, TileSet, TileStore};
use crate::ratelimit::{ClientKey, Rate};
use crate::testing::{MemTileStore, MetricCounts};

const T: u64 = 1_700_000_000;
const WEEK: u64 = 7 * 86_400;
const TEMPLATE: &str = "https://{s}.tile.example/{z}/{x}/{y}.png";

struct Rig {
    proxy: TileProxy,
    fake: FakeHttp,
    store: Arc<MemTileStore>,
    clock: Arc<ManualClock>,
}

/// The production configuration (7-day floor, default per-client limit)
/// with a tile template on a fake host.
fn rig_with(tweak: impl FnOnce(&mut Config)) -> Rig {
    let mut config = Config::from_lookup(|_| None).unwrap();
    config.tile_url = TEMPLATE.into();
    tweak(&mut config);
    let fake = FakeHttp::new();
    let store = Arc::new(MemTileStore::new());
    let clock = Arc::new(ManualClock::at_unix(T));
    let proxy = TileProxy::new(
        Arc::new(fake.clone()),
        &config,
        store.clone(),
        clock.clone(),
    );
    Rig {
        proxy,
        fake,
        store,
        clock,
    }
}

fn rig() -> Rig {
    rig_with(|_| {})
}

fn id() -> TileId {
    TileId::new(12, 654, 1583).unwrap()
}

const URL: &str = "https://a.tile.example/12/654/1583.png";

fn png(fill: u8) -> Vec<u8> {
    let mut v = PNG_MAGIC.to_vec();
    v.extend_from_slice(&[fill; 8]);
    v
}

fn upstream_png(fill: u8) -> FakeReply {
    FakeReply::ok(png(fill))
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ETAG, "\"v2\"")
        .header(header::LAST_MODIFIED, "Mon, 01 Jan 2024 00:00:00 GMT")
        .header(header::CACHE_CONTROL, "max-age=60")
}

/// An expired copy in the store, with validators.
async fn expired_copy(store: &MemTileStore) {
    let meta = TileMeta {
        etag: Some("\"v1\"".into()),
        last_modified: Some("Sun, 31 Dec 2023 00:00:00 GMT".into()),
        expires_unix: T,
    };
    assert!(store.put(TileSet::Map, id(), &png(1), &meta).await.unwrap());
}

fn counts(recorder: &MetricCounts) -> Vec<(String, u64)> {
    recorder.counters(crate::metrics::TILE_CACHE)
}

fn header_of(req: &HttpRequest, name: header::HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_owned())
}

#[tokio::test]
async fn a_miss_is_fetched_with_the_policy_headers_then_served_from_the_store() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    r.fake.on(Method::GET, URL, upstream_png(2));
    let referer = HeaderValue::from_static("https://app.example/");
    let tile = r.proxy.get(id(), Some(&referer), None).await.unwrap();
    assert_eq!((tile.bytes.as_ref(), tile.stale), (&png(2)[..], false));
    let [req] = r.fake.requests().try_into().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.url, URL, "built from the template and the tile id only");
    assert_eq!(
        header_of(&req, header::REFERER).as_deref(),
        Some("https://app.example/")
    );
    assert_eq!(header_of(&req, header::IF_NONE_MATCH), None);
    assert_eq!(header_of(&req, header::IF_MODIFIED_SINCE), None);
    assert_eq!(req.timeout, None, "the client's 20 s covers it");
    assert_eq!((req.max_bytes, req.error_max_bytes), (2 * 1024 * 1024, 0));
    assert_eq!(req.body_only_for, Some(StatusCode::OK));
    // Stored with the validators and the 7-day floor over max-age=60.
    let stored = r.store.get(TileSet::Map, id()).await.unwrap();
    assert_eq!(
        stored.meta,
        TileMeta {
            etag: Some("\"v2\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            expires_unix: T + WEEK,
        }
    );
    // Fresh until exactly `expires_unix`: served without asking upstream.
    r.clock.advance(Duration::from_secs(WEEK - 1));
    let tile = r.proxy.get(id(), None, None).await.unwrap();
    assert!(!tile.stale);
    assert_eq!(r.fake.requests().len(), 1);
    assert_eq!(
        counts(&recorder),
        [("result=hit".to_owned(), 1), ("result=miss".to_owned(), 1)]
    );
    assert_eq!(r.store.revalidations(), 0);
}

#[tokio::test]
async fn an_expired_copy_is_revalidated_and_a_304_keeps_it() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    expired_copy(&r.store).await;
    r.fake.on(
        Method::GET,
        URL,
        FakeReply::status(StatusCode::NOT_MODIFIED)
            .header(header::CACHE_CONTROL, format!("max-age={}", 30 * 86_400)),
    );
    let tile = r.proxy.get(id(), None, None).await.unwrap();
    assert_eq!((tile.bytes.as_ref(), tile.stale), (&png(1)[..], false));
    let [req] = r.fake.requests().try_into().unwrap();
    assert_eq!(
        header_of(&req, header::IF_NONE_MATCH).as_deref(),
        Some("\"v1\"")
    );
    assert_eq!(
        header_of(&req, header::IF_MODIFIED_SINCE).as_deref(),
        Some("Sun, 31 Dec 2023 00:00:00 GMT")
    );
    assert_eq!(
        header_of(&req, header::REFERER),
        None,
        "only when the client sent one"
    );
    // The store is told (it touches the image) and keeps the old validators
    // the 304 did not repeat, with the new expiry.
    assert_eq!(r.store.revalidations(), 1);
    let stored = r.store.get(TileSet::Map, id()).await.unwrap();
    assert_eq!(stored.bytes.as_ref(), png(1));
    assert_eq!(stored.meta.etag.as_deref(), Some("\"v1\""));
    assert_eq!(stored.meta.expires_unix, T + 30 * 86_400);
    assert_eq!(counts(&recorder), [("result=revalidated".to_owned(), 1)]);
}

#[tokio::test]
async fn an_expired_copy_replaced_by_a_new_body_is_refreshed() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig();
    expired_copy(&r.store).await;
    r.fake.on(Method::GET, URL, upstream_png(3));
    let tile = r.proxy.get(id(), None, None).await.unwrap();
    assert_eq!((tile.bytes.as_ref(), tile.stale), (&png(3)[..], false));
    let stored = r.store.get(TileSet::Map, id()).await.unwrap();
    assert_eq!(stored.bytes.as_ref(), png(3));
    assert_eq!(stored.meta.etag.as_deref(), Some("\"v2\""));
    assert_eq!(counts(&recorder), [("result=refreshed".to_owned(), 1)]);
}

/// A failed upstream serves the expired copy (stale, so the route sends
/// `max-age=300`), or fails without one.
#[tokio::test]
async fn failures_serve_the_expired_copy_or_fail() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    for failure in [
        FakeReply::status(StatusCode::INTERNAL_SERVER_ERROR).body("boom"),
        FakeReply::status(StatusCode::TOO_MANY_REQUESTS),
        FakeReply::error(TransportErrorKind::Timeout),
        FakeReply::ok("<html>not a png</html>"),
    ] {
        let r = rig();
        expired_copy(&r.store).await;
        r.fake.on(Method::GET, URL, failure);
        let tile = r.proxy.get(id(), None, None).await.unwrap();
        assert_eq!((tile.bytes.as_ref(), tile.stale), (&png(1)[..], true));
        assert_eq!(
            r.store
                .get(TileSet::Map, id())
                .await
                .unwrap()
                .meta
                .expires_unix,
            T
        );
    }
    assert_eq!(counts(&recorder), [("result=stale_served".to_owned(), 4)]);

    let errors = [
        (
            FakeReply::status(StatusCode::INTERNAL_SERVER_ERROR),
            StatusCode::BAD_GATEWAY,
            "the tile server answered with an error (HTTP 500)",
        ),
        (
            FakeReply::status(StatusCode::SERVICE_UNAVAILABLE),
            StatusCode::SERVICE_UNAVAILABLE,
            "RateLimited { retry_after_secs: 30 }",
        ),
        (
            FakeReply::status(StatusCode::NOT_FOUND),
            StatusCode::NOT_FOUND,
            "the tile server has no tile here",
        ),
        (
            FakeReply::error(TransportErrorKind::Connect),
            StatusCode::BAD_GATEWAY,
            "could not reach the tile server; map tiles may be missing",
        ),
        (
            FakeReply::error(TransportErrorKind::Body),
            StatusCode::BAD_GATEWAY,
            "the download from the tile server was interrupted",
        ),
        // A timeout while the body arrives is an interrupted download; one
        // before the response is an unreachable server.
        (
            FakeReply::error(TransportErrorKind::BodyTimeout),
            StatusCode::BAD_GATEWAY,
            "the download from the tile server was interrupted",
        ),
        (
            FakeReply::error(TransportErrorKind::Timeout),
            StatusCode::BAD_GATEWAY,
            "could not reach the tile server; map tiles may be missing",
        ),
        (
            FakeReply::ok("<html>"),
            StatusCode::BAD_GATEWAY,
            "the tile server returned something that is not a PNG",
        ),
        (
            FakeReply::ok(vec![0x89; 2 * 1024 * 1024 + 1]),
            StatusCode::BAD_GATEWAY,
            "the tile server returned a tile that is too large",
        ),
        (
            FakeReply::status(StatusCode::NOT_MODIFIED),
            StatusCode::BAD_GATEWAY,
            "the tile server sent an unexpected 'not modified' reply",
        ),
        (
            FakeReply::status(StatusCode::FOUND).header(header::LOCATION, "https://elsewhere/"),
            StatusCode::BAD_GATEWAY,
            "the tile server answered with an error (HTTP 302)",
        ),
    ];
    for (reply, status, message) in errors {
        let r = rig();
        r.fake.on(Method::GET, URL, reply);
        let e = r.proxy.get(id(), None, None).await.unwrap_err();
        assert_eq!(e.status(), status, "{message}");
        let shown = match &e {
            ApiError::Upstream(m) => m.clone(),
            ApiError::NotFound(m) => (*m).to_owned(),
            other => format!("{other:?}"),
        };
        assert_eq!(shown, message);
        assert!(
            r.store.get(TileSet::Map, id()).await.is_none(),
            "{message}: not stored"
        );
    }
    let errors_counted = counts(&recorder);
    // Every row but the 304 without a copy, which counts nothing (as before).
    assert_eq!(errors_counted[0], ("result=error".to_owned(), 10));
}

/// A client over its limit is not sent upstream: it gets the expired copy
/// when there is one, else a 429 with `Retry-After`.
#[tokio::test]
async fn a_client_over_its_limit_gets_the_expired_copy_or_a_429() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig_with(|c| {
        c.tile_rate = Rate {
            per_minute: 60,
            burst: 1,
        }
    });
    let client = Some(ClientKey::from_ip("192.0.2.1".parse().unwrap()));
    let other = TileId::new(12, 654, 1584).unwrap();
    r.fake.on(
        Method::GET,
        "https://a.tile.example/12/654/1584.png",
        upstream_png(4),
    );
    // The one token goes on a miss.
    assert!(r.proxy.get(other, None, client).await.is_ok());
    // A fresh copy costs nothing.
    assert!(r.proxy.get(other, None, client).await.is_ok());
    // Over the limit: the expired copy, without a request.
    expired_copy(&r.store).await;
    let tile = r.proxy.get(id(), None, client).await.unwrap();
    assert!(tile.stale);
    assert_eq!(r.fake.requests().len(), 1);
    // No copy: 429.
    let missing = TileId::new(12, 654, 1585).unwrap();
    let e = r.proxy.get(missing, None, client).await.unwrap_err();
    assert!(
        matches!(
            e,
            ApiError::TooManyRequests {
                retry_after_secs: 1
            }
        ),
        "{e:?}"
    );
    assert_eq!(e.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        counts(&recorder),
        [
            ("result=error".to_owned(), 1),
            ("result=hit".to_owned(), 1),
            ("result=miss".to_owned(), 1),
            ("result=stale_served".to_owned(), 1),
        ]
    );
    assert_eq!(
        recorder.counters(crate::metrics::CLIENT_LIMITED),
        [("route=tiles".to_owned(), 2)]
    );
}

/// Exactly four fetches run at once: with five tiles of 10 s each, four
/// requests are in flight until one finishes, and only then the fifth.
#[tokio::test(start_paused = true)]
async fn at_most_four_fetches_go_upstream_at_once() {
    let r = rig();
    let ids: Vec<TileId> = (0..5).map(|y| TileId::new(12, 654, y).unwrap()).collect();
    for id in &ids {
        let url = format!("https://a.tile.example/12/654/{}.png", id.y);
        r.fake.on(
            Method::GET,
            &url,
            upstream_png(5).delay(Duration::from_secs(10)),
        );
    }
    let started = tokio::time::Instant::now();
    let fetches = futures_util::future::join_all(ids.iter().map(|&id| r.proxy.get(id, None, None)));
    let watch = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(r.fake.requests().len(), 4, "four at once");
        tokio::time::sleep(Duration::from_millis(4_900)).await;
        assert_eq!(r.fake.requests().len(), 4, "the fifth waits");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(r.fake.requests().len(), 5, "then starts");
    };
    let (tiles, ()) = tokio::join!(fetches, watch);
    assert!(tiles.iter().all(Result::is_ok));
    assert_eq!(started.elapsed(), Duration::from_secs(20));
    r.fake.assert_drained();
}

/// Only a 200's body is read: another 2xx is the error it always was, at
/// once, even when its body would break off or stall.
#[tokio::test(start_paused = true)]
async fn the_body_of_a_2xx_other_than_200_is_not_read() {
    for (status, failure) in [
        (StatusCode::PARTIAL_CONTENT, TransportErrorKind::BodyTimeout),
        (StatusCode::PARTIAL_CONTENT, TransportErrorKind::Body),
        (StatusCode::NO_CONTENT, TransportErrorKind::BodyTimeout),
        (StatusCode::NO_CONTENT, TransportErrorKind::Body),
    ] {
        let r = rig();
        r.fake.on(
            Method::GET,
            URL,
            FakeReply::status(status).body(png(1)).body_fails(failure),
        );
        let started = tokio::time::Instant::now();
        let e = r.proxy.get(id(), None, None).await.unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "the tile server answered with an error (HTTP {})",
                status.as_u16()
            ),
            "{failure:?}"
        );
        assert_eq!(started.elapsed(), Duration::ZERO);
        let [req] = r.fake.requests().try_into().unwrap();
        assert_eq!(req.body_only_for, Some(StatusCode::OK));
    }
}

/// The cap is 2 MiB: a tile of exactly that size is accepted.
#[tokio::test]
async fn a_tile_of_exactly_two_mib_is_accepted() {
    let r = rig();
    let mut big = PNG_MAGIC.to_vec();
    big.resize(2 * 1024 * 1024, 7);
    r.fake.on(Method::GET, URL, FakeReply::ok(big.clone()));
    let tile = r.proxy.get(id(), None, None).await.unwrap();
    assert_eq!(tile.bytes.len(), 2 * 1024 * 1024);
}

/// The store failing to write does not fail the request.
#[tokio::test]
async fn a_store_that_cannot_write_still_serves_the_tile() {
    let r = rig();
    r.store.fail_writes(true);
    r.fake.on(Method::GET, URL, upstream_png(6));
    let tile = r.proxy.get(id(), None, None).await.unwrap();
    assert_eq!(tile.bytes.as_ref(), png(6));
    assert!(r.store.get(TileSet::Map, id()).await.is_none());
}

/// A cached tile goes upstream from exactly `expires_unix` on.
#[tokio::test]
async fn cached_tiles_expire_at_expires_unix() {
    let r = rig();
    let meta = TileMeta {
        expires_unix: T + 100,
        ..TileMeta::default()
    };
    r.store
        .put(TileSet::Map, id(), &png(7), &meta)
        .await
        .unwrap();
    let stale_at = |unix: u64| {
        r.clock
            .set(std::time::UNIX_EPOCH + Duration::from_secs(unix));
        r.fake.on(
            Method::GET,
            URL,
            FakeReply::error(TransportErrorKind::Connect),
        );
        let proxy = &r.proxy;
        async move { proxy.get(id(), None, None).await.unwrap().stale }
    };
    assert!(!stale_at(T + 99).await);
    assert!(stale_at(T + 100).await, "expired at exactly expires_unix");
    assert!(stale_at(T + 101).await);
    assert_eq!(r.fake.requests().len(), 2);
    assert_eq!(unix(&r.clock), T + 101);
}

fn unix(clock: &ManualClock) -> u64 {
    crate::clock::unix_secs(clock.now())
}
