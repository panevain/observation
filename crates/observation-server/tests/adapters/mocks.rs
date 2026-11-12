use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, Response, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use observation_core::geo::BBox;
use observation_core::regions::{RegionView, RegionsSnapshot};

use crate::common::{FIXTURE, PNG};

#[derive(Default)]
pub(crate) struct MockOverpass {
    pub(crate) hits: AtomicUsize,
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
    ([(header::CONTENT_TYPE, "application/json")], FIXTURE)
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
    max_age: AtomicU64,
    pub(crate) user_agents: Mutex<Vec<String>>,
    pub(crate) referers: Mutex<Vec<String>>,
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
    let cc = format!("max-age={}", m.max_age.load(Ordering::SeqCst));
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
        max_age: AtomicU64::new(max_age),
        user_agents: Mutex::default(),
        referers: Mutex::default(),
    });
    let router = Router::new()
        .route("/{z}/{x}/{y}", get(tile_handler))
        .with_state(Arc::clone(&mock));
    let addr = spawn(router).await;
    (mock, format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"))
}

/// A region agent: the snapshot (long-polled with `?since=`) and the
/// interpreter, which answers the demo fixture. It records what the server
/// sent.
#[derive(Default)]
pub(crate) struct MockAgent {
    snapshot: Mutex<RegionsSnapshot>,
    pub(crate) interpreter_hits: AtomicUsize,
    pub(crate) bboxes: Mutex<Vec<String>>,
    pub(crate) auth: Mutex<Vec<String>>,
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
    pub(crate) fn set_ready(&self, id: &str, imported_unix: u64) {
        let mut s = self.snapshot.lock().unwrap();
        s.version += 1;
        s.regions = vec![RegionView {
            id: id.into(),
            name: id.into(),
            ready: true,
            bbox: BBox::new(47.0, -123.0, 48.0, -122.0).ok(),
            data_timestamp: None,
            imported_unix: Some(imported_unix),
            last_attempt_unix: Some(imported_unix),
            auto_refresh: true,
            refresh_days: None,
            next_refresh_unix: None,
            disk_bytes: Some(1),
            job: None,
            last_error: None,
            last_error_unix: None,
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
) -> impl IntoResponse {
    m.record_auth(&headers);
    m.interpreter_hits.fetch_add(1, Ordering::SeqCst);
    m.bboxes
        .lock()
        .unwrap()
        .push(q.get("bbox").cloned().unwrap_or_default());
    ([(header::CONTENT_TYPE, "application/json")], FIXTURE)
}

pub(crate) async fn spawn_agent() -> (Arc<MockAgent>, String) {
    let mock = Arc::new(MockAgent::default());
    let router = Router::new()
        .route("/regions", get(agent_regions))
        .route("/interpreter", post(agent_interpreter))
        .with_state(Arc::clone(&mock));
    let addr = spawn(router).await;
    (mock, format!("http://{addr}"))
}
