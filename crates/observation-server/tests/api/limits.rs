use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use observation_server::ratelimit::{Cidr, Rate};
use observation_server::testing::TestApp;
use observation_upstream::testing::FakeHttp;

use crate::common::PNG;
use crate::harness::{
    OVERPASS, area_uri, flat_app, json, overpass_ok, overpass_status, send, tile_png, tile_url,
};

/// Overpass answering `replies` in order.
fn overpass(
    replies: impl IntoIterator<Item = observation_upstream::testing::FakeReply>,
) -> FakeHttp {
    let fake = FakeHttp::new();
    for reply in replies {
        fake.on(Method::POST, OVERPASS, reply);
    }
    fake
}

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

#[tokio::test(start_paused = true)]
async fn new_areas_are_limited_per_client_but_cached_ones_are_not() {
    let mock = overpass((0..5).map(|_| overpass_ok()));
    let h = flat_app(mock.clone(), |c| {
        c.area_rate = rate(1, 2);
    });
    let a = "192.0.2.1";
    for n in 0..2 {
        assert_eq!(
            send(&h.router, from(&nth_area(n), a, None)).await.0,
            StatusCode::OK
        );
    }
    let (status, headers, body) = send(&h.router, from(&nth_area(2), a, None)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let secs: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&secs), "{secs}");
    let msg = json(&body)["error"].as_str().unwrap().to_owned();
    assert!(msg.contains("too many") && msg.contains(" s"), "{msg}");
    assert_eq!(hits_of(&mock), 2, "refused before Overpass");

    // Cached areas stay free for the limited client.
    assert_eq!(
        send(&h.router, from(&nth_area(0), a, None)).await.0,
        StatusCode::OK
    );
    // Another client has its own allowance, and the same /64 shares one.
    assert_eq!(
        send(&h.router, from(&nth_area(2), "192.0.2.2", None))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.router, from(&nth_area(3), "2001:db8::1", None))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.router, from(&nth_area(4), "2001:db8::2", None))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.router, from(&nth_area(5), "2001:db8::3", None))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(hits_of(&mock), 5);
}

#[tokio::test(start_paused = true)]
async fn joining_a_build_in_progress_is_free() {
    let mock = overpass([overpass_ok().delay(Duration::from_millis(300))]);
    let h = flat_app(mock.clone(), |c| {
        c.area_rate = rate(1, 1);
    });
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let app = h.router.clone();
            tokio::spawn(async move { send(&app, from(&area_uri(), "192.0.2.1", None)).await.0 })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(hits_of(&mock), 1);
}

#[tokio::test(start_paused = true)]
async fn a_limited_client_gets_an_expired_area_rather_than_a_refusal() {
    let mock = overpass([overpass_ok()]);
    let h = flat_app(mock.clone(), |c| {
        c.area_rate = rate(1, 1);
        c.bundle_ttl = Duration::ZERO;
    });
    let a = "192.0.2.1";
    let (status, _, first) = send(&h.router, from(&nth_area(0), a, None)).await;
    assert_eq!(status, StatusCode::OK);
    // Expired at once, and no token left to rebuild it: the old copy is served.
    let (status, _, again) = send(&h.router, from(&nth_area(0), a, None)).await;
    assert_eq!((status, &again), (StatusCode::OK, &first));
    assert_eq!(hits_of(&mock), 1);
    // With nothing on disk the refusal stands.
    assert_eq!(
        send(&h.router, from(&nth_area(1), a, None)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test(start_paused = true)]
async fn builds_refused_as_busy_cost_no_token() {
    let mock = overpass([overpass_status(429), overpass_status(429)]);
    let h = flat_app(mock.clone(), |c| {
        c.area_rate = rate(1, 1);
    });
    // The first goes upstream and is refused; the rest fail fast in the
    // cooldown. All are 503s, not the client's own 429.
    for n in 0..4 {
        assert_eq!(
            send(&h.router, from(&nth_area(n), "192.0.2.1", None))
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE,
            "{n}"
        );
    }
    assert_eq!(hits_of(&mock), 2);
}

#[tokio::test(start_paused = true)]
async fn tile_fetches_are_limited_per_client_and_expired_copies_still_served() {
    let mock = FakeHttp::new();
    for y in [4, 5, 6] {
        mock.on(Method::GET, &tile_url(5, 3, y), tile_png(0));
    }
    // No minimum TTL, as the socket harness had it, so max-age=0 counts.
    let h = TestApp::builder()
        .tiles(mock.clone())
        .config(|c| {
            c.tile_rate = rate(1, 2);
            c.tile_min_ttl = Duration::ZERO;
        })
        .build();
    let a = "192.0.2.1";
    for uri in ["/api/tiles/5/3/4.png", "/api/tiles/5/3/5.png"] {
        assert_eq!(send(&h.router, from(uri, a, None)).await.0, StatusCode::OK);
    }
    let (status, headers, _) = send(&h.router, from("/api/tiles/5/3/6.png", a, None)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
    // max-age=0: this tile is expired, so it would be revalidated, but the
    // limited client gets the copy on disk rather than a refusal.
    let (status, headers, body) = send(&h.router, from("/api/tiles/5/3/4.png", a, None)).await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, PNG));
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=300",
        "an expired copy is not kept for a week"
    );
    assert_eq!(hits_of(&mock), 2);
    assert_eq!(
        send(&h.router, from("/api/tiles/5/3/6.png", "192.0.2.2", None))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test(start_paused = true)]
async fn forwarded_clients_are_told_apart_only_behind_a_trusted_proxy() {
    let mock = FakeHttp::new();
    for x in [0, 1, 3] {
        mock.on(Method::GET, &tile_url(10, x, 0), tile_png(3600));
    }
    let h = TestApp::builder()
        .tiles(mock)
        .config(|c| {
            c.tile_rate = rate(1, 1);
            c.trusted_proxies = vec![Cidr::parse("10.0.0.0/8").unwrap()];
        })
        .build();
    let tile = |n: u32| format!("/api/tiles/10/{n}/0.png");
    // Through the proxy, each forwarded client has its own bucket...
    assert_eq!(
        send(&h.router, from(&tile(0), "10.0.0.2", Some("192.0.2.1")))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.router, from(&tile(1), "10.0.0.2", Some("192.0.2.2")))
            .await
            .0,
        StatusCode::OK
    );
    // ...and a client cannot dodge its own by prepending an address.
    assert_eq!(
        send(
            &h.router,
            from(&tile(2), "10.0.0.2", Some("203.0.113.5, 192.0.2.1"))
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Without trust the header is ignored: both share the peer's bucket.
    assert_eq!(
        send(&h.router, from(&tile(3), "198.51.100.1", Some("192.0.2.3")))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&h.router, from(&tile(4), "198.51.100.1", Some("192.0.2.4")))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

/// Requests `fake` received, at any URL.
fn hits_of(fake: &FakeHttp) -> usize {
    fake.requests().len()
}
