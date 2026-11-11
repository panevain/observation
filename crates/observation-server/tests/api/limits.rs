use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use observation_server::ratelimit::{Cidr, Rate};

use crate::harness::{area_uri, harness_with, json, send};
use crate::mocks::{PNG, spawn_overpass, spawn_tiles};

/// A GET for `uri` arriving from `peer`, optionally through a proxy that sent `forwarded`.
fn from(uri: &str, peer: &str, forwarded: Option<&str>) -> Request<Body> {
    let mut req = Request::builder().uri(uri);
    if let Some(f) = forwarded {
        req = req.header("x-forwarded-for", f);
    }
    let mut req = req.body(Body::empty()).unwrap();
    let peer = SocketAddr::new(peer.parse().unwrap(), 40000);
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    req
}

/// The `n`th of a row of distinct, non-overlapping valid boxes.
fn nth_area(n: usize) -> String {
    let south = 47.6 + 0.01 * n as f64;
    format!(
        "/api/area?bbox={south:.4},-122.2,{:.4},-122.197",
        south + 0.002
    )
}

fn rate(per_minute: u32, burst: u32) -> Rate {
    Rate { per_minute, burst }
}

#[tokio::test]
async fn new_areas_are_limited_per_client_but_cached_ones_are_not() {
    let (mock, url) = spawn_overpass().await;
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/t",
        tempfile::tempdir().unwrap(),
        |c| {
            c.area_rate = rate(1, 2);
        },
    );
    let a = "192.0.2.1";
    for n in 0..2 {
        assert_eq!(
            send(&h.app, from(&nth_area(n), a, None)).await.0,
            StatusCode::OK
        );
    }
    let (status, headers, body) = send(&h.app, from(&nth_area(2), a, None)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let secs: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&secs), "{secs}");
    let msg = json(&body)["error"].as_str().unwrap().to_owned();
    assert!(msg.contains("too many") && msg.contains(" s"), "{msg}");
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        2,
        "refused before Overpass"
    );

    // Cached areas stay free for the limited client.
    assert_eq!(
        send(&h.app, from(&nth_area(0), a, None)).await.0,
        StatusCode::OK
    );
    // Another client has its own allowance, and the same /64 shares one.
    assert_eq!(
        send(&h.app, from(&nth_area(2), "192.0.2.2", None)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.app, from(&nth_area(3), "2001:db8::1", None))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.app, from(&nth_area(4), "2001:db8::2", None))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.app, from(&nth_area(5), "2001:db8::3", None))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(mock.hits.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn joining_a_build_in_progress_is_free() {
    let (mock, url) = spawn_overpass().await;
    mock.delay_ms.store(300, Ordering::SeqCst);
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/t",
        tempfile::tempdir().unwrap(),
        |c| {
            c.area_rate = rate(1, 1);
        },
    );
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let app = h.app.clone();
            tokio::spawn(async move { send(&app, from(&area_uri(), "192.0.2.1", None)).await.0 })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_limited_client_gets_an_expired_area_rather_than_a_refusal() {
    let (mock, url) = spawn_overpass().await;
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/t",
        tempfile::tempdir().unwrap(),
        |c| {
            c.area_rate = rate(1, 1);
            c.bundle_ttl = Duration::ZERO;
        },
    );
    let a = "192.0.2.1";
    let (status, _, first) = send(&h.app, from(&nth_area(0), a, None)).await;
    assert_eq!(status, StatusCode::OK);
    // Expired at once, and no token left to rebuild it: the old copy is served.
    let (status, _, again) = send(&h.app, from(&nth_area(0), a, None)).await;
    assert_eq!((status, &again), (StatusCode::OK, &first));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1);
    // With nothing on disk the refusal stands.
    assert_eq!(
        send(&h.app, from(&nth_area(1), a, None)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn builds_refused_as_busy_cost_no_token() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().extend([429, 429]);
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/t",
        tempfile::tempdir().unwrap(),
        |c| {
            c.area_rate = rate(1, 1);
        },
    );
    // The first goes upstream and is refused; the rest fail fast in the
    // cooldown. All are 503s, not the client's own 429.
    for n in 0..4 {
        assert_eq!(
            send(&h.app, from(&nth_area(n), "192.0.2.1", None)).await.0,
            StatusCode::SERVICE_UNAVAILABLE,
            "{n}"
        );
    }
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn tile_fetches_are_limited_per_client_and_expired_copies_still_served() {
    let (mock, url) = spawn_tiles(0).await;
    let h = harness_with(
        "http://127.0.0.1:9/o",
        &url,
        tempfile::tempdir().unwrap(),
        |c| {
            c.tile_rate = rate(1, 2);
        },
    );
    let a = "192.0.2.1";
    for uri in ["/api/tiles/5/3/4.png", "/api/tiles/5/3/5.png"] {
        assert_eq!(send(&h.app, from(uri, a, None)).await.0, StatusCode::OK);
    }
    let (status, headers, _) = send(&h.app, from("/api/tiles/5/3/6.png", a, None)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
    // max-age=0: this tile is expired, so it would be revalidated, but the
    // limited client gets the copy on disk rather than a refusal.
    let (status, headers, body) = send(&h.app, from("/api/tiles/5/3/4.png", a, None)).await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, PNG));
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=300",
        "an expired copy is not kept for a week"
    );
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        send(&h.app, from("/api/tiles/5/3/6.png", "192.0.2.2", None))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn forwarded_clients_are_told_apart_only_behind_a_trusted_proxy() {
    let (_mock, url) = spawn_tiles(3600).await;
    let h = harness_with(
        "http://127.0.0.1:9/o",
        &url,
        tempfile::tempdir().unwrap(),
        |c| {
            c.tile_rate = rate(1, 1);
            c.trusted_proxies = vec![Cidr::parse("10.0.0.0/8").unwrap()];
        },
    );
    let tile = |n: u32| format!("/api/tiles/10/{n}/0.png");
    // Through the proxy, each forwarded client has its own bucket...
    assert_eq!(
        send(&h.app, from(&tile(0), "10.0.0.2", Some("192.0.2.1")))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.app, from(&tile(1), "10.0.0.2", Some("192.0.2.2")))
            .await
            .0,
        StatusCode::OK
    );
    // ...and a client cannot dodge its own by prepending an address.
    assert_eq!(
        send(
            &h.app,
            from(&tile(2), "10.0.0.2", Some("203.0.113.5, 192.0.2.1"))
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Without trust the header is ignored: both share the peer's bucket.
    assert_eq!(
        send(&h.app, from(&tile(3), "198.51.100.1", Some("192.0.2.3")))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.app, from(&tile(4), "198.51.100.1", Some("192.0.2.4")))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}
