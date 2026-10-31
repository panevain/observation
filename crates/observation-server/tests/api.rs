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
    /// 200 bodies to answer with (front first), after `script`.
    bodies: Mutex<VecDeque<&'static str>>,
    /// Answer every request with this status, overriding the above.
    always: AtomicU64,
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
    let always = m.always.load(Ordering::SeqCst);
    let scripted = m.script.lock().unwrap().pop_front();
    let body = m.bodies.lock().unwrap().pop_front().unwrap_or(FIXTURE);
    match scripted.or((always != 0).then_some(always as u16)) {
        Some(code) => (StatusCode::from_u16(code).unwrap(), "upstream says no").into_response(),
        None => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
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
    assert!(
        headers.contains_key("x-request-id"),
        "every response carries a request id"
    );

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
async fn oversized_box_error_names_the_size_and_the_limit() {
    let h = offline();
    // About 11 km wide and tall.
    let (status, _, body) = get_uri(&h.app, "/api/area?bbox=47.0,-122.3,47.1,-122.2").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let msg = json(&body)["error"].as_str().unwrap().to_owned();
    assert!(msg.contains("wide") && msg.contains("tall"), "{msg}");
    assert!(msg.contains("20 to 500 m"), "{msg}");
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
    let msg = json(&body)["error"].as_str().unwrap().to_owned();
    assert!(msg.contains("busy") && msg.contains("30 s"), "{msg}");
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
async fn overpass_bad_gateway_is_retried() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().extend([502]);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);

    // Two failures in a row are a 502, and no cool-down starts.
    mock.script.lock().unwrap().extend([503, 502]);
    let other = "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970";
    let (status, _, body) = get_uri(&h.app, other).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(json(&body)["error"].is_string());
    let (status, _, _) = get_uri(&h.app, other).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn overpass_fails_over_to_the_next_endpoint() {
    let (down, down_url) = spawn_overpass().await;
    down.script.lock().unwrap().extend([502, 502, 502]);
    let (up, up_url) = spawn_overpass().await;
    let h = harness_with(
        &down_url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.overpass_urls.push(up_url),
    );
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(down.hits.load(Ordering::SeqCst), 1, "each endpoint once");
    assert_eq!(up.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn overpass_timeouts_are_504() {
    let (mock, url) = spawn_overpass().await;
    mock.delay_ms.store(1_000, Ordering::SeqCst);
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.overpass_timeout = Duration::from_millis(200),
    );
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert!(json(&body)["error"].as_str().unwrap().contains("too long"));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2, "retried once");
}

#[tokio::test]
async fn overpass_remarks_retry_only_timeouts() {
    let (mock, url) = spawn_overpass().await;
    let timed_out =
        r#"{"elements":[],"remark":"runtime error: Query timed out in \"query\" at line 3"}"#;
    mock.bodies.lock().unwrap().extend([timed_out, timed_out]);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2, "a timeout is retried");

    // Running out of memory would happen again: no retry, and not a "timeout".
    let oom = r#"{"elements":[],"remark":"runtime error: Query run out of memory using about 2048 MB of RAM."}"#;
    mock.bodies.lock().unwrap().push_back(oom);
    let (status, _, body) =
        get_uri(&h.app, "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!json(&body)["error"].as_str().unwrap().contains("too long"));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_busy_interpreter_rests_while_others_serve() {
    let (busy, busy_url) = spawn_overpass().await;
    busy.always.store(429, Ordering::SeqCst);
    let (up, up_url) = spawn_overpass().await;
    let h = harness_with(
        &busy_url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.overpass_urls.push(up_url),
    );
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    // The next miss skips the interpreter that said 429.
    let (status, _, _) =
        get_uri(&h.app, "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(busy.hits.load(Ordering::SeqCst), 1);
    assert_eq!(up.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn busy_then_failed_is_502_and_the_busy_one_still_rests() {
    let (busy, busy_url) = spawn_overpass().await;
    busy.always.store(429, Ordering::SeqCst);
    let (down, down_url) = spawn_overpass().await;
    down.script.lock().unwrap().push_back(502);
    let h = harness_with(
        &busy_url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.overpass_urls.push(down_url),
    );
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(busy.hits.load(Ordering::SeqCst), 1);
    assert_eq!(down.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_stop_before_the_request_times_out() {
    // Four slow interpreters would need 4 × 600 ms; the request allows 1 s.
    let mut urls = Vec::new();
    for _ in 0..4 {
        let (mock, url) = spawn_overpass().await;
        mock.delay_ms.store(2_000, Ordering::SeqCst);
        urls.push(url);
    }
    let h = harness_with(
        &urls[0],
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| {
            c.overpass_urls = urls.clone();
            c.overpass_timeout = Duration::from_millis(600);
            c.request_timeout = Duration::from_secs(1);
        },
    );
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "not the 408 of the request timeout"
    );
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
async fn endless_overpass_bodies_are_cut_off() {
    // Same as the tile case, against the 32 MiB Overpass cap (no `Content-Length`).
    let router = Router::new().route(
        "/api/interpreter",
        post(|| async {
            Body::from_stream(futures_util::stream::repeat_with(|| {
                Ok::<_, std::io::Error>(Bytes::from(vec![b' '; 64 * 1024]))
            }))
        }),
    );
    let addr = spawn(router).await;
    let h = harness(
        &format!("http://{addr}/api/interpreter"),
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
    );
    let (status, _, _) =
        tokio::time::timeout(Duration::from_secs(10), get_uri(&h.app, &area_uri()))
            .await
            .expect("oversized body is rejected while streaming");
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!h.cache.path().join("bundles").exists());
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
async fn overpass_redirects_are_not_followed() {
    let (inner, inner_url) = spawn_overpass().await;
    // 307 keeps the method and body, so a followed redirect would reach the interpreter.
    let router = Router::new().route(
        "/o",
        post(move || {
            let to = inner_url.clone();
            async move { (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, to)]) }
        }),
    );
    let addr = spawn(router).await;
    let h = harness(&format!("http://{addr}/o"), "http://127.0.0.1:9/{z}");
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(inner.hits.load(Ordering::SeqCst), 0);
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
    std::fs::write(
        dir.path().join("app-50916e44830ffe5e_bg.wasm"),
        b"\0asm\x01\0\0\0",
    )
    .unwrap();
    std::fs::write(dir.path().join("app-50916e44830ffe5e.js"), "export {};").unwrap();
    std::fs::write(
        dir.path().join("coverage-worker_loader.js"),
        "import init from './coverage-worker.js';await init();",
    )
    .unwrap();
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

    let (status, headers, body) = get_uri(&h.app, "/app-50916e44830ffe5e_bg.wasm").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/wasm");
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body.as_ref(), b"\0asm\x01\0\0\0");

    let (status, headers, _) = get_uri(&h.app, "/app-50916e44830ffe5e.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );

    // Files without trunk's content hash, such as the web worker's, are revalidated.
    let (status, headers, _) = get_uri(&h.app, "/coverage-worker_loader.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
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

// ----- local regions ------------------------------------------------------------------

use observation_core::geo::BBox;
use observation_core::regions::{RegionView, RegionsSnapshot};

#[derive(Default)]
struct MockAgent {
    snapshot: Mutex<RegionsSnapshot>,
    /// Status for `/interpreter` (0 = answer with the fixture).
    interpreter_status: AtomicU64,
    interpreter_hits: AtomicUsize,
    bboxes: Mutex<Vec<String>>,
    auth: Mutex<Vec<String>>,
    actions: Mutex<Vec<(String, String)>>,
}

impl MockAgent {
    fn record_auth(&self, headers: &HeaderMap) {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        self.auth.lock().unwrap().push(auth);
    }

    /// Makes `id` a ready region covering the demo street, imported at `imported_unix`.
    fn set_ready(&self, id: &str, imported_unix: u64) {
        let mut s = self.snapshot.lock().unwrap();
        s.version += 1;
        s.regions = vec![RegionView {
            id: id.into(),
            name: id.into(),
            ready: true,
            bbox: BBox::new(47.0, -123.0, 48.0, -122.0).ok(),
            data_timestamp: None,
            imported_unix: Some(imported_unix),
            disk_bytes: Some(1),
            job: None,
            last_error: None,
        }];
    }
}

async fn agent_regions(
    State(m): State<Arc<MockAgent>>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, u64>>,
) -> axum::Json<RegionsSnapshot> {
    m.record_auth(&headers);
    if let Some(since) = q.get("since") {
        for _ in 0..100 {
            if m.snapshot.lock().unwrap().version != *since {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    axum::Json(m.snapshot.lock().unwrap().clone())
}

async fn agent_interpreter(
    State(m): State<Arc<MockAgent>>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Response<Body> {
    m.record_auth(&headers);
    m.interpreter_hits.fetch_add(1, Ordering::SeqCst);
    m.bboxes
        .lock()
        .unwrap()
        .push(q.get("bbox").cloned().unwrap_or_default());
    match m.interpreter_status.load(Ordering::SeqCst) {
        0 => ([(header::CONTENT_TYPE, "application/json")], FIXTURE).into_response(),
        code => (
            StatusCode::from_u16(code as u16).unwrap(),
            axum::Json(serde_json::json!({"error": "agent says no"})),
        )
            .into_response(),
    }
}

async fn agent_action(
    State(m): State<Arc<MockAgent>>,
    Path(action): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    m.record_auth(&headers);
    let id = json(&body)["id"].as_str().unwrap_or_default().to_owned();
    if id == "busy" {
        return (
            StatusCode::CONFLICT,
            axum::Json(serde_json::json!({"error": "busy is already being imported"})),
        )
            .into_response();
    }
    m.actions.lock().unwrap().push((action, id));
    let snap = {
        let mut s = m.snapshot.lock().unwrap();
        s.version += 1;
        s.clone()
    };
    axum::Json(snap).into_response()
}

async fn spawn_agent() -> (Arc<MockAgent>, String) {
    let mock = Arc::new(MockAgent::default());
    let router = Router::new()
        .route("/regions", get(agent_regions))
        .route("/regions/{action}", post(agent_action))
        .route("/interpreter", post(agent_interpreter))
        .route(
            "/catalog",
            get(|| async {
                axum::Json(
                    serde_json::json!([{"id": "monaco", "name": "Monaco", "parent": "europe"}]),
                )
            }),
        )
        .with_state(Arc::clone(&mock));
    let addr = spawn(router).await;
    (mock, format!("http://{addr}"))
}

/// A harness with a mock Overpass and a mock region agent; `admin` sets OBS_ADMIN_TOKEN.
async fn region_harness(admin: Option<&str>) -> (Harness, Arc<MockOverpass>, Arc<MockAgent>) {
    let (overpass, overpass_url) = spawn_overpass().await;
    let (agent, agent_url) = spawn_agent().await;
    let admin = admin.map(str::to_owned);
    let h = harness_with(
        &overpass_url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        move |c| {
            c.regions_url = Some(agent_url);
            c.regions_token = Some("agent-secret".into());
            c.admin_token = admin;
        },
    );
    (h, overpass, agent)
}

/// Waits until the server's copy of the agent snapshot has `version`.
async fn wait_for_version(app: &Router, version: u64) -> serde_json::Value {
    for _ in 0..200 {
        let (status, _, body) = get_uri(app, "/api/regions").await;
        assert_eq!(status, StatusCode::OK);
        let v = json(&body);
        if v["agent_online"] == true && v["snapshot"]["version"] == version {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the server never saw snapshot version {version}");
}

fn post_json(uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_owned())).unwrap()
}

#[tokio::test]
async fn regions_are_off_without_an_agent() {
    let h = offline();
    let (_, _, body) = get_uri(&h.app, "/api/config").await;
    assert_eq!(json(&body)["regions"], false);
    for uri in [
        "/api/regions",
        "/api/regions/events",
        "/api/regions/catalog",
    ] {
        let (status, _, _) = get_uri(&h.app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn areas_inside_a_ready_region_come_from_the_agent() {
    let (h, overpass, agent) = region_harness(None).await;
    agent.set_ready("wa", 1);
    wait_for_version(&h.app, 1).await;
    let (_, _, body) = get_uri(&h.app, "/api/config").await;
    assert_eq!(json(&body)["regions"], true);

    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(agent.interpreter_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        overpass.hits.load(Ordering::SeqCst),
        0,
        "public Overpass untouched"
    );
    assert!(agent.bboxes.lock().unwrap()[0].contains(','));
    assert!(
        agent
            .auth
            .lock()
            .unwrap()
            .iter()
            .all(|a| a == "Bearer agent-secret"),
        "every agent request carries OBS_REGIONS_TOKEN"
    );
}

#[tokio::test]
async fn uncovered_areas_and_agent_failures_fall_back_to_overpass() {
    let (h, overpass, agent) = region_harness(None).await;
    // No region yet: the agent is not even asked.
    wait_for_version(&h.app, 0).await;
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent.interpreter_hits.load(Ordering::SeqCst), 0);
    assert_eq!(overpass.hits.load(Ordering::SeqCst), 1);

    // A region whose bounds hold the area, but the agent fails: Overpass again.
    agent.interpreter_status.store(500, Ordering::SeqCst);
    agent.set_ready("wa", u64::MAX / 4);
    wait_for_version(&h.app, 1).await;
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent.interpreter_hits.load(Ordering::SeqCst), 1);
    assert_eq!(overpass.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn bundles_cached_before_an_import_are_rebuilt_from_it() {
    let (h, overpass, agent) = region_harness(None).await;
    wait_for_version(&h.app, 0).await;
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overpass.hits.load(Ordering::SeqCst), 1);

    // Imported after the bundle was cached (in the future, for the test's sake).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    agent.set_ready("wa", now + 3600);
    wait_for_version(&h.app, 1).await;
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        agent.interpreter_hits.load(Ordering::SeqCst),
        1,
        "rebuilt locally"
    );
    assert_eq!(overpass.hits.load(Ordering::SeqCst), 1);
    let _ = &h.cache;
}

#[tokio::test]
async fn region_actions_need_the_admin_token() {
    // Without OBS_ADMIN_TOKEN, management is off.
    let (h, _, agent) = region_harness(None).await;
    let (status, _, body) = send(
        &h.app,
        post_json("/api/regions/import", Some("x"), r#"{"id":"monaco"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        json(&body)["error"]
            .as_str()
            .unwrap()
            .contains("OBS_ADMIN_TOKEN")
    );
    let v = wait_for_version(&h.app, 0).await;
    assert_eq!(v["manageable"], false);

    let (h, _, agent2) = region_harness(Some("admin-secret")).await;
    let v = wait_for_version(&h.app, 0).await;
    assert_eq!(v["manageable"], true);
    for token in [None, Some("wrong")] {
        let (status, _, _) = send(
            &h.app,
            post_json("/api/regions/import", token, r#"{"id":"monaco"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/api/regions/import",
            Some("admin-secret"),
            r#"{"id":"../etc"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/api/regions/nuke",
            Some("admin-secret"),
            r#"{"id":"monaco"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, headers, body) = send(
        &h.app,
        post_json(
            "/api/regions/import",
            Some("admin-secret"),
            r#"{"id":"monaco"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(json(&body)["snapshot"]["version"], 1);
    let (status, _, body) = send(
        &h.app,
        post_json(
            "/api/regions/import",
            Some("admin-secret"),
            r#"{"id":"busy"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(json(&body)["error"].as_str().unwrap().contains("already"));
    assert_eq!(
        *agent2.actions.lock().unwrap(),
        vec![("import".to_owned(), "monaco".to_owned())]
    );
    assert!(agent.actions.lock().unwrap().is_empty());

    let (status, _, body) = get_uri(&h.app, "/api/regions/catalog").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)[0]["id"], "monaco");
}

#[tokio::test]
async fn region_events_stream_every_change() {
    let (h, _, agent) = region_harness(None).await;
    wait_for_version(&h.app, 0).await;
    let resp = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/regions/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut body = resp.into_body().into_data_stream();
    use futures_util::StreamExt;
    let mut seen = String::new();
    let mut next = async || {
        let chunk = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        String::from_utf8(chunk.to_vec()).unwrap()
    };
    seen.push_str(&next().await);
    assert!(seen.contains("event: regions"), "{seen}");
    assert!(seen.contains("\"agent_online\":true"), "{seen}");

    agent.set_ready("wa", 1);
    let mut later = String::new();
    while !later.contains("\"wa\"") {
        later.push_str(&next().await);
    }
}
