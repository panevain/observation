use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::get;

use crate::harness::{get_uri, harness, json, send};
use crate::mocks::{PNG, spawn, spawn_tiles};

#[tokio::test]
async fn tiles_are_proxied_once_and_cached() {
    let (mock, url) = spawn_tiles(3600).await;
    let h = harness("http://127.0.0.1:9/o", &url);
    let req = Request::builder()
        .uri("/api/tiles/17/20/30.png")
        .header(header::REFERER, "https://example.test/app")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=604800");
    assert_eq!(body.as_ref(), PNG);
    assert!(mock.user_agents.lock().unwrap()[0].contains("observation/"));
    assert_eq!(
        mock.referers.lock().unwrap().as_slice(),
        ["https://example.test/app"]
    );

    let (status, _, body) = get_uri(&h.app, "/api/tiles/17/20/30.png").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), PNG);
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        1,
        "second request must be served from disk"
    );

    let root = h.cache.path().join("tiles/17/20");
    assert!(root.join("30.png").is_file() && root.join("30.json").is_file());
    let meta = json(&std::fs::read(root.join("30.json")).unwrap());
    assert_eq!(meta["etag"], "\"tile-etag\"");
}

#[tokio::test]
async fn invalid_tiles_are_404_without_upstream_calls() {
    let (mock, url) = spawn_tiles(3600).await;
    let h = harness("http://127.0.0.1:9/o", &url);
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
        let (status, _, body) = get_uri(&h.app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(json(&body)["error"].is_string(), "{uri}");
    }
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn expired_tiles_are_revalidated_and_stale_served_on_failure() {
    let (mock, url) = spawn_tiles(0).await;
    let h = harness("http://127.0.0.1:9/o", &url);

    let (status, _, body) = get_uri(&h.app, "/api/tiles/5/3/4.png").await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, PNG));
    // max-age=0 and no minimum TTL in tests: the next request revalidates.
    let (status, _, body) = get_uri(&h.app, "/api/tiles/5/3/4.png").await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, PNG));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        mock.conditional.lock().unwrap().as_slice(),
        [None, Some("\"tile-etag\"".to_owned())]
    );

    // Upstream down: the stale copy is served instead of an error.
    mock.fail.store(true, Ordering::SeqCst);
    let (status, _, body) = get_uri(&h.app, "/api/tiles/5/3/4.png").await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, PNG));
    // ...but a tile we never had is a 502.
    let (status, _, _) = get_uri(&h.app, "/api/tiles/5/3/5.png").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn tile_redirects_are_not_followed() {
    let (target, template) = spawn_tiles(3600).await;
    let target_url = template
        .replace("{z}", "1")
        .replace("{x}", "0")
        .replace("{y}", "0");
    let router = Router::new().route(
        "/{z}/{x}/{y}",
        get(move || {
            let to = target_url.clone();
            async move { (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, to)]) }
        }),
    );
    let addr = spawn(router).await;
    let h = harness(
        "http://127.0.0.1:9/o",
        &format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"),
    );
    let (status, _, _) = get_uri(&h.app, "/api/tiles/1/0/0.png").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(target.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn upstream_tiles_must_be_png() {
    let router = Router::new().route(
        "/{z}/{x}/{y}",
        get(|| async {
            (
                [(header::CONTENT_TYPE, "text/html")],
                "<html>blocked</html>",
            )
        }),
    );
    let addr = spawn(router).await;
    let h = harness(
        "http://127.0.0.1:9/o",
        &format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"),
    );
    let (status, _, _) = get_uri(&h.app, "/api/tiles/1/0/0.png").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!h.cache.path().join("tiles").exists());
}

#[tokio::test]
async fn endless_tile_bodies_are_cut_off() {
    // A chunked body that never ends: buffering it whole would only stop at the
    // client timeout (and after eating memory); the streaming cap stops at 2 MiB.
    let router = Router::new().route(
        "/{z}/{x}/{y}",
        get(|| async {
            let first = futures_util::stream::once(async { Ok(Bytes::from_static(PNG)) });
            let rest = futures_util::stream::repeat_with(|| {
                Ok::<_, std::io::Error>(Bytes::from(vec![0u8; 64 * 1024]))
            });
            Body::from_stream(futures_util::StreamExt::chain(first, rest))
        }),
    );
    let addr = spawn(router).await;
    let h = harness(
        "http://127.0.0.1:9/o",
        &format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"),
    );
    let (status, _, _) = tokio::time::timeout(
        Duration::from_secs(10),
        get_uri(&h.app, "/api/tiles/1/0/0.png"),
    )
    .await
    .expect("oversized body is rejected while streaming");
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!h.cache.path().join("tiles").exists());
}
