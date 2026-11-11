use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use observation_core::bundle::AreaBundle;
use observation_core::geo::BBox;
use observation_core::provenance::format_utc;
use observation_core::regions::{RegionView, RegionsSnapshot};
use tower::ServiceExt;

use crate::harness::{Harness, area_uri, get_uri, harness_with, json, offline, send};
use crate::mocks::{FIXTURE, MockOverpass, spawn, spawn_overpass};

#[derive(Default)]
struct MockAgent {
    snapshot: Mutex<RegionsSnapshot>,
    /// Status for `/interpreter` (0 = answer with the fixture).
    interpreter_status: AtomicU64,
    interpreter_hits: AtomicUsize,
    /// The `x-osm-base` header on `/interpreter` answers, if any.
    osm_base: Mutex<Option<String>>,
    /// Answer with the fixture's `timestamp_osm_base` blanked, as a database
    /// imported without a data date reports it.
    undated: AtomicBool,
    bboxes: Mutex<Vec<String>>,
    auth: Mutex<Vec<String>>,
    actions: Mutex<Vec<(String, String)>>,
    /// Bodies of the action requests.
    bodies: Mutex<Vec<serde_json::Value>>,
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
) -> Response<Body> {
    m.record_auth(&headers);
    m.interpreter_hits.fetch_add(1, Ordering::SeqCst);
    m.bboxes
        .lock()
        .unwrap()
        .push(q.get("bbox").cloned().unwrap_or_default());
    match m.interpreter_status.load(Ordering::SeqCst) {
        0 => {
            let body = if m.undated.load(Ordering::SeqCst) {
                FIXTURE.replacen(
                    "\"timestamp_osm_base\":\"2026-10-31T00:00:00Z\"",
                    "\"timestamp_osm_base\":\"\"",
                    1,
                )
            } else {
                FIXTURE.to_owned()
            };
            let mut resp = ([(header::CONTENT_TYPE, "application/json")], body).into_response();
            if let Some(v) = m.osm_base.lock().unwrap().clone() {
                resp.headers_mut().insert(
                    observation_core::regions::OSM_BASE_HEADER,
                    v.parse().unwrap(),
                );
            }
            resp
        }
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
    m.bodies.lock().unwrap().push(json(&body));
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
        .route(
            "/catalog/sizes",
            post(|body: Bytes| async move {
                let ids = json(&body)["ids"].clone();
                let sizes: serde_json::Map<String, serde_json::Value> = ids
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|id| Some((id.as_str()?.to_owned(), 1234.into())))
                    .collect();
                axum::Json(sizes)
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
async fn region_bundles_carry_the_regions_data_date() {
    let (h, _, agent) = region_harness(None).await;
    agent.set_ready("wa", 1);
    wait_for_version(&h.app, 1).await;
    // A database imported without a data date: the agent's header supplies it.
    agent.undated.store(true, Ordering::SeqCst);
    *agent.osm_base.lock().unwrap() = Some("2026-10-20T20:21:11Z".into());
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    let b = AreaBundle::decode(&body).unwrap();
    assert_eq!(
        b.osm_base_unix.map(format_utc).as_deref(),
        Some("2026-10-20T20:21:11Z")
    );
    assert!(b.generated_unix > 1_700_000_000, "built now, not then");
}

#[tokio::test]
async fn an_overpass_data_date_beats_the_agents_header() {
    let (h, _, agent) = region_harness(None).await;
    agent.set_ready("wa", 1);
    wait_for_version(&h.app, 1).await;
    *agent.osm_base.lock().unwrap() = Some("2026-10-20T20:21:11Z".into());
    let (_, _, body) = get_uri(&h.app, &area_uri()).await;
    let b = AreaBundle::decode(&body).unwrap();
    assert_eq!(
        b.osm_base_unix.map(format_utc).as_deref(),
        Some("2026-10-31T00:00:00Z")
    );
}

#[tokio::test]
async fn an_undated_region_gives_an_undated_bundle() {
    let (h, _, agent) = region_harness(None).await;
    agent.set_ready("wa", 1);
    wait_for_version(&h.app, 1).await;
    agent.undated.store(true, Ordering::SeqCst);
    *agent.osm_base.lock().unwrap() = Some("not a date".into());
    let (_, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(AreaBundle::decode(&body).unwrap().osm_base_unix, None);
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
async fn catalog_sizes_are_forwarded_without_a_token() {
    let (h, _, _) = region_harness(Some("admin-secret")).await;
    let (status, headers, body) = send(
        &h.app,
        post_json(
            "/api/regions/catalog/sizes",
            None,
            r#"{"ids":["monaco","us/california"]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        json(&body),
        serde_json::json!({"monaco": 1234, "us/california": 1234})
    );
    let too_many = serde_json::json!({ "ids": vec!["a"; 41] }).to_string();
    for bad in [
        r#"{"ids":["../x"]}"#,
        r#"{"id":"monaco"}"#,
        too_many.as_str(),
    ] {
        let (status, _, _) = send(&h.app, post_json("/api/regions/catalog/sizes", None, bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn region_settings_are_forwarded_with_the_admin_token() {
    let (h, _, agent) = region_harness(Some("admin-secret")).await;
    wait_for_version(&h.app, 0).await;
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/api/regions/settings",
            None,
            r#"{"id":"monaco","auto_refresh":false}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, body) = send(
        &h.app,
        post_json(
            "/api/regions/settings",
            Some("admin-secret"),
            r#"{"id":"monaco"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json(&body)["error"]
            .as_str()
            .unwrap()
            .contains("auto_refresh")
    );
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/api/regions/settings",
            Some("admin-secret"),
            r#"{"id":"monaco","auto_refresh":false}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/api/regions/settings",
            Some("admin-secret"),
            r#"{"id":"monaco","refresh_days":30}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/api/regions/settings",
            Some("admin-secret"),
            r#"{"id":"monaco","refresh_days":null}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "null clears the interval");
    // Other actions forward the id alone.
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/api/regions/delete",
            Some("admin-secret"),
            r#"{"id":"monaco","auto_refresh":true,"refresh_days":30}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        *agent.actions.lock().unwrap(),
        vec![
            ("settings".to_owned(), "monaco".to_owned()),
            ("settings".to_owned(), "monaco".to_owned()),
            ("settings".to_owned(), "monaco".to_owned()),
            ("delete".to_owned(), "monaco".to_owned()),
        ]
    );
    assert_eq!(
        *agent.bodies.lock().unwrap(),
        vec![
            serde_json::json!({"id": "monaco", "auto_refresh": false}),
            serde_json::json!({"id": "monaco", "refresh_days": 30}),
            serde_json::json!({"id": "monaco", "refresh_days": null}),
            serde_json::json!({"id": "monaco"}),
        ]
    );
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
