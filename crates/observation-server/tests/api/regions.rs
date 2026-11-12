use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Method, Request, StatusCode, header};
use futures_util::StreamExt;
use observation_core::bundle::AreaBundle;
use observation_core::geo::BBox;
use observation_core::provenance::{format_utc, parse_utc};
use observation_core::regions::{
    ActionKind, RegionAction, RegionView, RegionsSnapshot, SizesRequest,
};
use observation_regions_api::testing::{AgentCall, FakeRegionAgent};
use observation_regions_api::{AgentError, QueryResult};
use observation_server::testing::TestApp;
use observation_upstream::testing::FakeHttp;
use tower::ServiceExt;

use crate::common::FIXTURE;
use crate::harness::{OVERPASS, area_uri, get_uri, hits, json, overpass_ok, send};

/// A [`TestApp`] following `agent`, on the production configuration without
/// elevation (as the socket harness had it); `admin` sets OBS_ADMIN_TOKEN.
fn region_app(overpass: FakeHttp, agent: FakeRegionAgent, admin: Option<&'static str>) -> TestApp {
    TestApp::builder()
        .overpass(overpass)
        .agent(agent)
        .config(|c| c.dem_url = None)
        .config(move |c| c.admin_token = admin.map(str::to_owned))
        .build()
}

/// Snapshot `version`, with `ready = (id, imported_unix)` a ready region
/// covering the demo street.
fn snapshot(version: u64, ready: Option<(&str, u64)>) -> RegionsSnapshot {
    RegionsSnapshot {
        version,
        regions: ready
            .into_iter()
            .map(|(id, imported_unix)| RegionView {
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
            })
            .collect(),
        ..RegionsSnapshot::default()
    }
}

fn snapshot_json(version: u64) -> Bytes {
    serde_json::to_vec(&snapshot(version, None)).unwrap().into()
}

/// Has the agent answer the next poll with `snap`, polls once, and checks
/// that the server now shows the agent online at that version.
async fn poll_to(app: &TestApp, snap: RegionsSnapshot) -> serde_json::Value {
    let version = snap.version;
    app.agent.as_ref().unwrap().snapshot_reply(Ok(snap));
    app.state.regions().unwrap().poll_once().await.unwrap();
    let (status, _, body) = get_uri(&app.router, "/api/regions").await;
    assert_eq!(status, StatusCode::OK);
    let v = json(&body);
    assert_eq!(v["agent_online"], true);
    assert_eq!(v["snapshot"]["version"], version);
    v
}

/// The agent's answer to a query: the demo fixture (with its data date
/// blanked when `undated`, as a database imported without one reports it)
/// and the `x-osm-base` header as the client parses it.
fn region_data(undated: bool, osm_base: Option<&str>) -> QueryResult {
    let body = if undated {
        FIXTURE.replacen(
            "\"timestamp_osm_base\":\"2026-10-31T00:00:00Z\"",
            "\"timestamp_osm_base\":\"\"",
            1,
        )
    } else {
        FIXTURE.to_owned()
    };
    QueryResult {
        body: body.into(),
        osm_base_unix: osm_base.and_then(parse_utc),
    }
}

fn queries(agent: &FakeRegionAgent) -> usize {
    agent
        .calls()
        .iter()
        .filter(|c| matches!(c, AgentCall::Query { .. }))
        .count()
}

fn acts(agent: &FakeRegionAgent) -> Vec<(ActionKind, RegionAction)> {
    agent
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            AgentCall::Act(kind, req) => Some((kind, req)),
            _ => None,
        })
        .collect()
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

fn events_request() -> Request<Body> {
    Request::builder()
        .uri("/api/regions/events")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn regions_are_off_without_an_agent() {
    let app = TestApp::builder().build();
    let (_, _, body) = get_uri(&app.router, "/api/config").await;
    assert_eq!(json(&body)["regions"], false);
    for uri in [
        "/api/regions",
        "/api/regions/events",
        "/api/regions/catalog",
    ] {
        let (status, _, _) = get_uri(&app.router, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test(start_paused = true)]
async fn region_bundles_carry_the_regions_data_date() {
    let agent = FakeRegionAgent::new();
    let app = region_app(FakeHttp::new(), agent.clone(), None);
    poll_to(&app, snapshot(1, Some(("wa", 1)))).await;
    // A database imported without a data date: the agent's header supplies it.
    agent.query_reply(Ok(Some(region_data(true, Some("2026-10-20T20:21:11Z")))));
    app.clock.advance(Duration::from_secs(60));
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    let b = AreaBundle::decode(&body).unwrap();
    assert_eq!(
        b.osm_base_unix.map(format_utc).as_deref(),
        Some("2026-10-20T20:21:11Z")
    );
    assert!(b.generated_unix > 1_700_000_000, "built now, not then");
    assert_eq!(b.generated_unix, 1_700_000_060);
}

#[tokio::test(start_paused = true)]
async fn an_overpass_data_date_beats_the_agents_header() {
    let agent = FakeRegionAgent::new();
    let app = region_app(FakeHttp::new(), agent.clone(), None);
    poll_to(&app, snapshot(1, Some(("wa", 1)))).await;
    agent.query_reply(Ok(Some(region_data(false, Some("2026-10-20T20:21:11Z")))));
    let (_, _, body) = get_uri(&app.router, &area_uri()).await;
    let b = AreaBundle::decode(&body).unwrap();
    assert_eq!(
        b.osm_base_unix.map(format_utc).as_deref(),
        Some("2026-10-31T00:00:00Z")
    );
}

#[tokio::test(start_paused = true)]
async fn an_undated_region_gives_an_undated_bundle() {
    let agent = FakeRegionAgent::new();
    let app = region_app(FakeHttp::new(), agent.clone(), None);
    poll_to(&app, snapshot(1, Some(("wa", 1)))).await;
    agent.query_reply(Ok(Some(region_data(true, Some("not a date")))));
    let (_, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(AreaBundle::decode(&body).unwrap().osm_base_unix, None);
}

#[tokio::test(start_paused = true)]
async fn uncovered_areas_and_agent_failures_fall_back_to_overpass() {
    let (overpass, agent) = (FakeHttp::new(), FakeRegionAgent::new());
    overpass.on(Method::POST, OVERPASS, overpass_ok());
    overpass.on(Method::POST, OVERPASS, overpass_ok());
    let app = region_app(overpass.clone(), agent.clone(), None);
    // No region yet: the agent is not even asked.
    poll_to(&app, snapshot(0, None)).await;
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queries(&agent), 0);
    assert_eq!(hits(&overpass, OVERPASS), 1);

    // A region whose bounds hold the area, but the agent fails: Overpass again.
    agent.query_reply(Err("agent answered 500 Internal Server Error".into()));
    poll_to(&app, snapshot(1, Some(("wa", u64::MAX / 4)))).await;
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queries(&agent), 1);
    assert_eq!(hits(&overpass, OVERPASS), 2);
}

#[tokio::test(start_paused = true)]
async fn bundles_cached_before_an_import_are_rebuilt_from_it() {
    let (overpass, agent) = (FakeHttp::new(), FakeRegionAgent::new());
    overpass.on(Method::POST, OVERPASS, overpass_ok());
    let app = region_app(overpass.clone(), agent.clone(), None);
    poll_to(&app, snapshot(0, None)).await;
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits(&overpass, OVERPASS), 1);

    // Imported after the bundle was cached (in the future, for the test's sake).
    let now = 1_700_000_000;
    agent.query_reply(Ok(Some(region_data(false, None))));
    poll_to(&app, snapshot(1, Some(("wa", now + 3600)))).await;
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queries(&agent), 1, "rebuilt locally");
    assert_eq!(hits(&overpass, OVERPASS), 1);
    // The second poll waited for a version newer than the first one's.
    let polls: Vec<_> = agent
        .calls()
        .into_iter()
        .filter(|c| matches!(c, AgentCall::Snapshot { .. }))
        .collect();
    assert_eq!(
        polls,
        [
            AgentCall::Snapshot { since: None },
            AgentCall::Snapshot { since: Some(0) }
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn region_actions_need_the_admin_token() {
    // Without OBS_ADMIN_TOKEN, management is off.
    let agent = FakeRegionAgent::new();
    let h = region_app(FakeHttp::new(), agent.clone(), None);
    let (status, _, body) = send(
        &h.router,
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
    let v = poll_to(&h, snapshot(0, None)).await;
    assert_eq!(v["manageable"], false);

    let agent2 = FakeRegionAgent::new();
    let h = region_app(FakeHttp::new(), agent2.clone(), Some("admin-secret"));
    let v = poll_to(&h, snapshot(0, None)).await;
    assert_eq!(v["manageable"], true);
    for token in [None, Some("wrong")] {
        let (status, _, _) = send(
            &h.router,
            post_json("/api/regions/import", token, r#"{"id":"monaco"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, _) = send(
        &h.router,
        post_json(
            "/api/regions/import",
            Some("admin-secret"),
            r#"{"id":"../etc"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(
        &h.router,
        post_json(
            "/api/regions/nuke",
            Some("admin-secret"),
            r#"{"id":"monaco"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    agent2.answer(Ok(snapshot_json(1)));
    let (status, headers, body) = send(
        &h.router,
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
    agent2.answer(Err(AgentError::Conflict(
        "busy is already being imported".into(),
    )));
    let (status, _, body) = send(
        &h.router,
        post_json(
            "/api/regions/import",
            Some("admin-secret"),
            r#"{"id":"busy"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(json(&body)["error"].as_str().unwrap().contains("already"));
    // Only the two well-formed, authorised imports reached the agent, which
    // refused the second.
    let import = |id: &str| {
        (
            ActionKind::Import,
            RegionAction {
                id: id.into(),
                ..RegionAction::default()
            },
        )
    };
    assert_eq!(acts(&agent2), vec![import("monaco"), import("busy")]);
    assert!(acts(&agent).is_empty());

    agent2.answer(Ok(Bytes::from_static(
        br#"[{"id": "monaco", "name": "Monaco", "parent": "europe"}]"#,
    )));
    let (status, _, body) = get_uri(&h.router, "/api/regions/catalog").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)[0]["id"], "monaco");
}

#[tokio::test(start_paused = true)]
async fn catalog_sizes_are_forwarded_without_a_token() {
    let agent = FakeRegionAgent::new();
    let h = region_app(FakeHttp::new(), agent.clone(), Some("admin-secret"));
    agent.answer(Ok(Bytes::from_static(
        br#"{"monaco": 1234, "us/california": 1234}"#,
    )));
    let (status, headers, body) = send(
        &h.router,
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
    assert_eq!(
        agent.calls(),
        [AgentCall::CatalogSizes(SizesRequest {
            ids: vec!["monaco".into(), "us/california".into()]
        })]
    );
    let too_many = serde_json::json!({ "ids": vec!["a"; 41] }).to_string();
    for bad in [
        r#"{"ids":["../x"]}"#,
        r#"{"id":"monaco"}"#,
        too_many.as_str(),
    ] {
        let (status, _, _) = send(
            &h.router,
            post_json("/api/regions/catalog/sizes", None, bad),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test(start_paused = true)]
async fn region_settings_are_forwarded_with_the_admin_token() {
    let agent = FakeRegionAgent::new();
    let h = region_app(FakeHttp::new(), agent.clone(), Some("admin-secret"));
    poll_to(&h, snapshot(0, None)).await;
    let (status, _, _) = send(
        &h.router,
        post_json(
            "/api/regions/settings",
            None,
            r#"{"id":"monaco","auto_refresh":false}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, body) = send(
        &h.router,
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
    for version in 1..=4 {
        agent.answer(Ok(snapshot_json(version)));
    }
    let (status, _, _) = send(
        &h.router,
        post_json(
            "/api/regions/settings",
            Some("admin-secret"),
            r#"{"id":"monaco","auto_refresh":false}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.router,
        post_json(
            "/api/regions/settings",
            Some("admin-secret"),
            r#"{"id":"monaco","refresh_days":30}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.router,
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
        &h.router,
        post_json(
            "/api/regions/delete",
            Some("admin-secret"),
            r#"{"id":"monaco","auto_refresh":true,"refresh_days":30}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let acts = acts(&agent);
    assert_eq!(
        acts.iter()
            .map(|(kind, req)| (kind.path().to_owned(), req.id.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("settings".to_owned(), "monaco".to_owned()),
            ("settings".to_owned(), "monaco".to_owned()),
            ("settings".to_owned(), "monaco".to_owned()),
            ("delete".to_owned(), "monaco".to_owned()),
        ]
    );
    // The bodies the agent client sends: it serializes the action as is.
    assert_eq!(
        acts.iter()
            .map(|(_, req)| serde_json::to_value(req).unwrap())
            .collect::<Vec<_>>(),
        vec![
            serde_json::json!({"id": "monaco", "auto_refresh": false}),
            serde_json::json!({"id": "monaco", "refresh_days": 30}),
            serde_json::json!({"id": "monaco", "refresh_days": null}),
            serde_json::json!({"id": "monaco"}),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn region_events_stream_every_change() {
    let agent = FakeRegionAgent::new();
    let h = region_app(FakeHttp::new(), agent.clone(), None);
    poll_to(&h, snapshot(0, None)).await;
    let resp = h.router.clone().oneshot(events_request()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut body = resp.into_body().into_data_stream();
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

    poll_to(&h, snapshot(1, Some(("wa", 1)))).await;
    let mut later = String::new();
    while !later.contains("\"wa\"") {
        later.push_str(&next().await);
    }
    // And the change after it: the agent going offline.
    agent.snapshot_reply(Err("agent unreachable".into()));
    assert!(h.state.regions().unwrap().poll_once().await.is_err());
    let mut offline = String::new();
    while !offline.contains("\"agent_online\":false") {
        offline.push_str(&next().await);
    }
}

/// At most 256 event streams are open at once; one more is refused with a
/// 503 until a stream closes.
#[tokio::test(start_paused = true)]
async fn region_events_are_limited_to_256_streams() {
    assert_eq!(observation_server::MAX_EVENT_STREAMS, 256);
    let h = region_app(FakeHttp::new(), FakeRegionAgent::new(), None);
    let mut open = Vec::new();
    for _ in 0..256 {
        let resp = h.router.clone().oneshot(events_request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        open.push(resp);
    }
    // The status first: a stream would never end.
    let refused = h.router.clone().oneshot(events_request()).await.unwrap();
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    let (status, headers, body) = send(&h.router, events_request()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers[header::RETRY_AFTER], "30");
    assert_eq!(
        json(&body),
        serde_json::json!({"error": "the map provider is busy; try again in about 30 s"})
    );
    drop(open.pop());
    let resp = h.router.clone().oneshot(events_request()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A shutdown ends the open event streams, and a stream asked for after it
/// ends at once.
#[tokio::test(start_paused = true)]
async fn region_events_end_when_the_server_shuts_down() {
    let h = region_app(FakeHttp::new(), FakeRegionAgent::new(), None);
    let resp = h.router.clone().oneshot(events_request()).await.unwrap();
    let mut body = resp.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("event: regions"));

    // A stream already waiting for the next change ends.
    let waiting = tokio::spawn(async move { body.next().await.is_none() });
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    assert!(!waiting.is_finished());
    h.state.begin_shutdown();
    let ended = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(ended, "the stream ended");

    // Read with a cap and a deadline: a stream that did not end would yield
    // keep-alives forever under paused time.
    let late = h.router.clone().oneshot(events_request()).await.unwrap();
    assert_eq!(late.status(), StatusCode::OK);
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        axum::body::to_bytes(late.into_body(), 64 * 1024),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(body.is_empty(), "ended before its first event");
}
