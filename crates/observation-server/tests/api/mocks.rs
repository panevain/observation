use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};

pub(crate) const FIXTURE: &str = include_str!("../../fixtures/demo-overpass.json");

/// A valid 1x1 PNG.
pub(crate) const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

#[derive(Default)]
pub(crate) struct MockOverpass {
    pub(crate) hits: AtomicUsize,
    pub(crate) delay_ms: AtomicU64,
    /// Statuses to answer with (front first) before serving the fixture.
    pub(crate) script: Mutex<VecDeque<u16>>,
    /// 200 bodies to answer with (front first), after `script`.
    pub(crate) bodies: Mutex<VecDeque<&'static str>>,
    /// Answer every request with this status, overriding the above.
    pub(crate) always: AtomicU64,
    pub(crate) user_agents: Mutex<Vec<String>>,
    pub(crate) queries: Mutex<Vec<String>>,
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

pub(crate) async fn spawn(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

pub(crate) async fn spawn_overpass() -> (Arc<MockOverpass>, String) {
    let mock = Arc::new(MockOverpass::default());
    let router = Router::new()
        .route("/api/interpreter", post(overpass_handler))
        .with_state(Arc::clone(&mock));
    let addr = spawn(router).await;
    (mock, format!("http://{addr}/api/interpreter"))
}

pub(crate) struct MockTiles {
    pub(crate) hits: AtomicUsize,
    pub(crate) fail: AtomicBool,
    max_age: AtomicU64,
    pub(crate) user_agents: Mutex<Vec<String>>,
    pub(crate) referers: Mutex<Vec<String>>,
    pub(crate) conditional: Mutex<Vec<Option<String>>>,
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

pub(crate) async fn spawn_tiles(max_age: u64) -> (Arc<MockTiles>, String) {
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
