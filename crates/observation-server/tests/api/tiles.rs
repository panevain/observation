use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use observation_server::testing::TestApp;
use observation_upstream::testing::{FakeHttp, FakeReply};

use crate::common::PNG;
use crate::harness::{get_uri, json, tile_png, tile_url};

#[tokio::test(start_paused = true)]
async fn invalid_tiles_are_404_without_upstream_calls() {
    let tiles = FakeHttp::new();
    let app = TestApp::builder().tiles(tiles.clone()).build();
    for uri in [
        "/api/tiles/20/0/0.png",
        "/api/tiles/2/4/0.png",
        "/api/tiles/2/0/4.png",
        "/api/tiles/2/0/0.jpg",
        "/api/tiles/2/0/0",
        "/api/tiles/x/0/0.png",
        "/api/tiles/2/-1/0.png",
        "/api/tiles/2/0/..png",
    ] {
        let (status, _, body) = get_uri(&app.router, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(json(&body)["error"].is_string(), "{uri}");
    }
    assert!(tiles.requests().is_empty());
}

#[tokio::test(start_paused = true)]
async fn expired_tiles_are_revalidated_and_stale_served_on_failure() {
    let tiles = FakeHttp::new();
    let at = tile_url(5, 3, 4);
    tiles.on(Method::GET, &at, tile_png(0));
    tiles.on(
        Method::GET,
        &at,
        FakeReply::status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, "\"tile-etag\"")
            .header(header::CACHE_CONTROL, "max-age=0"),
    );
    let boom = || FakeReply::status(StatusCode::INTERNAL_SERVER_ERROR).body("boom");
    tiles.on(Method::GET, &at, boom());
    tiles.on(Method::GET, &tile_url(5, 3, 5), boom());
    // No minimum TTL, as the socket harness had it, so max-age=0 counts.
    let app = TestApp::builder()
        .tiles(tiles.clone())
        .config(|c| c.tile_min_ttl = Duration::ZERO)
        .build();

    let (status, _, body) = get_uri(&app.router, "/api/tiles/5/3/4.png").await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, PNG));
    // max-age=0 and no minimum TTL: the next request revalidates.
    let (status, _, body) = get_uri(&app.router, "/api/tiles/5/3/4.png").await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, PNG));
    assert_eq!(tiles.requests().len(), 2);
    let conditional: Vec<_> = tiles
        .requests()
        .iter()
        .map(|r| {
            r.headers()
                .get(header::IF_NONE_MATCH)
                .map(|v| v.to_str().unwrap().to_owned())
        })
        .collect();
    assert_eq!(conditional, [None, Some("\"tile-etag\"".to_owned())]);

    // Upstream down: the stale copy is served instead of an error.
    let (status, _, body) = get_uri(&app.router, "/api/tiles/5/3/4.png").await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, PNG));
    // ...but a tile we never had is a 502.
    let (status, _, _) = get_uri(&app.router, "/api/tiles/5/3/5.png").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test(start_paused = true)]
async fn upstream_tiles_must_be_png() {
    let cache = tempfile::tempdir().unwrap();
    let tiles = FakeHttp::new();
    tiles.on(
        Method::GET,
        &tile_url(1, 0, 0),
        FakeReply::ok("<html>blocked</html>").header(header::CONTENT_TYPE, "text/html"),
    );
    let app = TestApp::builder()
        .tiles(tiles)
        .disk_cache(cache.path())
        .build();
    let (status, _, _) = get_uri(&app.router, "/api/tiles/1/0/0.png").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!cache.path().join("tiles").exists());
}
