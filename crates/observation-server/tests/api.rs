//! End-to-end tests of the HTTP API against local mock upstreams (no network).
#![allow(clippy::unwrap_used)] // integration-test helpers live outside `#[test]` fns

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, Request, Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use observation_core::bundle::{AreaBundle, BUNDLE_CONTENT_TYPE};
use observation_server::config::Config;
use observation_server::{AppState, app};
use tower::ServiceExt;

const FIXTURE: &str = include_str!("../fixtures/demo-overpass.json");
const BBOX: &str = include_str!("../fixtures/demo-bbox.txt");

/// A valid 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

// ----- mock upstreams ---------------------------------------------------------

#[derive(Default)]
struct MockOverpass {
    hits: AtomicUsize,
    delay_ms: AtomicU64,
    /// Statuses to answer with (front first) before serving the fixture.
    script: Mutex<VecDeque<u16>>,
    user_agents: Mutex<Vec<String>>,
    queries: Mutex<Vec<String>>,
}

async fn overpass_handler(
    State(m): State<Arc<MockOverpass>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    m.hits.fetch_add(1, Ordering::SeqCst);
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    m.user_agents.lock().unwrap().push(ua);
    m.queries
        .lock()
        .unwrap()
        .push(form.get("data").cloned().unwrap_or_default());
    tokio::time::sleep(Duration::from_millis(m.delay_ms.load(Ordering::SeqCst))).await;
    let scripted = m.script.lock().unwrap().pop_front();
    match scripted {
        Some(code) => (StatusCode::from_u16(code).unwrap(), "upstream says no").into_response(),
        None => ([(header::CONTENT_TYPE, "application/json")], FIXTURE).into_response(),
    }
}

async fn spawn(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

async fn spawn_overpass() -> (Arc<MockOverpass>, String) {
    let mock = Arc::new(MockOverpass::default());
    let router = Router::new()
        .route("/api/interpreter", post(overpass_handler))
        .with_state(Arc::clone(&mock));
    let addr = spawn(router).await;
    (mock, format!("http://{addr}/api/interpreter"))
}

struct MockTiles {
    hits: AtomicUsize,
    fail: AtomicBool,
    max_age: AtomicU64,
    user_agents: Mutex<Vec<String>>,
    referers: Mutex<Vec<String>>,
    conditional: Mutex<Vec<Option<String>>>,
}

async fn tile_handler(
    State(m): State<Arc<MockTiles>>,
    Path((_z, _x, _y)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response<Body> {
    m.hits.fetch_add(1, Ordering::SeqCst);
    let get = |h: &str| {
        headers
            .get(h)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    m.user_agents
        .lock()
        .unwrap()
        .push(get("user-agent").unwrap_or_default());
    if let Some(r) = get("referer") {
        m.referers.lock().unwrap().push(r);
    }
    let inm = get("if-none-match");
    m.conditional.lock().unwrap().push(inm.clone());
    if m.fail.load(Ordering::SeqCst) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response();
    }
    let cc = format!("max-age={}", m.max_age.load(Ordering::SeqCst));
    if inm.as_deref() == Some("\"tile-etag\"") {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, "\"tile-etag\"")
            .header(header::CACHE_CONTROL, cc)
            .body(Body::empty())
            .unwrap();
    }
    Response::builder()
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ETAG, "\"tile-etag\"")
        .header(header::LAST_MODIFIED, "Mon, 01 Jan 2024 00:00:00 GMT")
        .header(header::CACHE_CONTROL, cc)
        .body(Body::from(PNG))
        .unwrap()
}

async fn spawn_tiles(max_age: u64) -> (Arc<MockTiles>, String) {
    let mock = Arc::new(MockTiles {
        hits: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
        max_age: AtomicU64::new(max_age),
        user_agents: Mutex::default(),
        referers: Mutex::default(),
        conditional: Mutex::default(),
    });
    let router = Router::new()
        .route("/{z}/{x}/{y}", get(tile_handler))
        .with_state(Arc::clone(&mock));
    let addr = spawn(router).await;
    (mock, format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"))
}

// ----- harness --------------------------------------------------------------------

struct Harness {
    app: Router,
    cache: tempfile::TempDir,
    _static: tempfile::TempDir,
}

fn harness_with(
    overpass_url: &str,
    tile_url: &str,
    static_dir: tempfile::TempDir,
    tweak: impl FnOnce(&mut Config),
) -> Harness {
    let cache = tempfile::tempdir().unwrap();
    let mut config = Config::for_tests(cache.path(), static_dir.path(), overpass_url, tile_url);
    tweak(&mut config);
    let app = app(AppState::new(config).unwrap());
    Harness {
        app,
        cache,
        _static: static_dir,
    }
}

fn harness(overpass_url: &str, tile_url: &str) -> Harness {
    harness_with(overpass_url, tile_url, tempfile::tempdir().unwrap(), |_| {})
}

/// A harness whose upstreams are unreachable (for routes that never call them).
fn offline() -> Harness {
    harness(
        "http://127.0.0.1:9/overpass",
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
    )
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Bytes) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    (
        parts.status,
        parts.headers,
        axum::body::to_bytes(body, usize::MAX).await.unwrap(),
    )
}

async fn get_uri(app: &Router, uri: &str) -> (StatusCode, HeaderMap, Bytes) {
    send(
        app,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

fn json(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

fn area_uri() -> String {
    format!("/api/area?bbox={}", BBOX.trim())
}

// ----- basic routes -----------------------------------------------------------------

#[tokio::test]
async fn healthz_and_security_headers() {
    let h = offline();
    let (status, headers, body) = get_uri(&h.app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"ok");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["referrer-policy"],
        "strict-origin-when-cross-origin"
    );
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("'wasm-unsafe-eval'") && csp.contains("frame-ancestors 'none'"));

    // Error responses carry them too.
    let (status, headers, _) = get_uri(&h.app, "/api/area").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers["x-frame-options"], "DENY");
}

#[tokio::test]
async fn config_endpoint() {
    let h = offline();
    let (status, headers, body) = get_uri(&h.app, "/api/config").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let v = json(&body);
    assert_eq!(v["tile_url"], "/api/tiles/{z}/{x}/{y}.png");
    assert_eq!(v["attribution"], "© OpenStreetMap contributors");
    assert_eq!(v["max_bbox_side_m"], 500.0);
    assert_eq!(v["demo_available"], true);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn bad_bboxes_are_400() {
    let h = offline();
    for uri in [
        "/api/area",
        "/api/area?bbox=",
        "/api/area?bbox=1,2,3",
        "/api/area?bbox=a,b,c,d",
        "/api/area?bbox=48,-122,47,-121",
        "/api/area?bbox=99,0,99.1,0.1",
        // Too large (about 11 km) and too small (about 1 m).
        "/api/area?bbox=47.0,-122.3,47.1,-122.2",
        "/api/area?bbox=47.0,-122.0,47.00001,-121.99999",
    ] {
        let (status, _, body) = get_uri(&h.app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(
            json(&body)["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn demo_is_a_decodable_bundle() {
    let h = offline();
    let (status, headers, body) = get_uri(&h.app, "/api/demo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], BUNDLE_CONTENT_TYPE);
    let b = AreaBundle::decode(&body).unwrap();
    assert!(b.buildings.len() > 20);
    assert!(!b.roads.is_empty());
    assert_eq!(b.attribution, "© OpenStreetMap contributors");
}

#[tokio::test]
async fn bundles_are_compressed_on_request() {
    let h = offline();
    let req = Request::builder()
        .uri("/api/demo")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_ENCODING], "gzip");
    assert_eq!(&body[..2], &[0x1f, 0x8b]);
}

// ----- /api/area ---------------------------------------------------------------------

#[tokio::test]
async fn area_fetches_once_then_serves_from_cache() {
    let (mock, url) = spawn_overpass().await;
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");

    // Mid-cell coordinates, so the snapped box does not depend on float noise at
    // grid lines. Nudged copies snap to the same cells and so share the cache entry.
    let fixture = observation_core::geo::BBox::parse(BBOX.trim()).unwrap();
    let (d, n) = (0.000_03, 0.000_001);
    let uri = |k: f64| {
        format!(
            "/api/area?bbox={:.6},{:.6},{:.6},{:.6}",
            fixture.south + d + k,
            fixture.west + d + k,
            fixture.north - d - k,
            fixture.east - d - k
        )
    };

    let (status, headers, body) = get_uri(&h.app, &uri(0.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], BUNDLE_CONTENT_TYPE);
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=86400");
    let b = AreaBundle::decode(&body).unwrap();
    assert!(b.buildings.len() > 20);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1);
    assert!(mock.user_agents.lock().unwrap()[0].starts_with("observation/"));
    let query = mock.queries.lock().unwrap()[0].clone();
    assert!(
        query.contains("out geom qt;") && query.contains("[\"building\"]"),
        "{query}"
    );

    // The bundle covers the snapped (slightly larger) box.
    let asked =
        observation_core::geo::BBox::parse(&uri(0.0).replace("/api/area?bbox=", "")).unwrap();
    assert!(b.bbox.south <= asked.south && b.bbox.north >= asked.north);
    assert!(b.bbox.west <= asked.west && b.bbox.east >= asked.east);

    for k in [0.0, n, -n] {
        let (status, _, again) = get_uri(&h.app, &uri(k)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again, body);
    }
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        1,
        "cache hits must not reach Overpass"
    );

    let files: Vec<_> = std::fs::read_dir(h.cache.path().join("bundles"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(files.len(), 1);
    assert!(files[0].file_name().to_str().unwrap().ends_with(".obsb"));
}

#[tokio::test]
async fn concurrent_area_requests_share_one_upstream_fetch() {
    let (mock, url) = spawn_overpass().await;
    mock.delay_ms.store(300, Ordering::SeqCst);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");

    let uri = area_uri();
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (app, uri) = (h.app.clone(), uri.clone());
            tokio::spawn(async move { get_uri(&app, &uri).await })
        })
        .collect();
    let mut bodies = Vec::new();
    for t in tasks {
        let (status, _, body) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        bodies.push(body);
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1, "single-flight failed");
}

#[tokio::test]
async fn overpass_rate_limit_gives_503_then_fails_fast() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().extend([429, 429]);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");

    let (status, headers, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers[header::RETRY_AFTER], "30");
    assert!(json(&body)["error"].is_string());
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        2,
        "one inline retry, no more"
    );

    // During the cool-down another (different) area does not touch Overpass.
    let (status, headers, _) =
        get_uri(&h.app, "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let secs: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=30).contains(&secs));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn overpass_gateway_timeout_is_retried_once() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().push_back(504);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn upstream_failures_are_502_and_not_cached() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().push_back(500);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(json(&body)["error"].is_string());
    // Failure was not remembered: the next request fetches again and succeeds.
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);

    // Unreachable upstream is a 502 as well.
    let off = offline();
    let (status, _, _) = get_uri(&off.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn expired_bundle_is_served_when_upstream_fails() {
    let (mock, url) = spawn_overpass().await;
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.bundle_ttl = Duration::ZERO,
    );
    let (status, _, fresh) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    mock.script.lock().unwrap().push_back(500);
    let (status, _, stale) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stale, fresh);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
}

// ----- /api/tiles ---------------------------------------------------------------------

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

// ----- static files ---------------------------------------------------------------------

#[tokio::test]
async fn static_files_spa_fallback_and_cache_headers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>Observation</title>",
    )
    .unwrap();
    std::fs::write(dir.path().join("app-abc123_bg.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(dir.path().join("app-abc123.js"), "export {};").unwrap();
    let h = harness_with(
        "http://127.0.0.1:9/o",
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        dir,
        |_| {},
    );

    for uri in ["/", "/index.html", "/some/client/route"] {
        let (status, headers, body) = get_uri(&h.app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(
            String::from_utf8_lossy(&body).contains("<title>Observation</title>"),
            "{uri}"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache", "{uri}");
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html"),
            "{uri}"
        );
        assert_eq!(headers["x-content-type-options"], "nosniff");
    }

    let (status, headers, body) = get_uri(&h.app, "/app-abc123_bg.wasm").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/wasm");
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body.as_ref(), b"\0asm\x01\0\0\0");

    let (status, headers, _) = get_uri(&h.app, "/app-abc123.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
}

#[tokio::test]
async fn unknown_api_paths_are_json_404_not_the_spa() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<title>Observation</title>").unwrap();
    let h = harness_with(
        "http://127.0.0.1:9/o",
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        dir,
        |_| {},
    );
    let (status, _, body) = get_uri(&h.app, "/api/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(json(&body)["error"].is_string());
}
