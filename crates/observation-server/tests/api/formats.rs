//! Byte-for-byte goldens of the public API's JSON: `/api/config`,
//! `/api/regions`, and the error bodies whose texts are fixed. The other
//! tests check fields; these pin the bytes (field names, order and number
//! formats) that clients parse.

use axum::http::StatusCode;
use observation_core::geo::BBox;
use observation_core::regions::{JobPhase, JobView, RegionView, RegionsSnapshot};
use observation_regions_api::testing::FakeRegionAgent;
use observation_server::testing::TestApp;

use crate::common::{get_uri, send};

fn text(body: &[u8]) -> &str {
    std::str::from_utf8(body).unwrap()
}

#[tokio::test(start_paused = true)]
async fn the_config_body_is_these_bytes() {
    let app = TestApp::builder().build();
    let (status, _, body) = get_uri(&app.router, "/api/config").await;
    assert_eq!(status, StatusCode::OK);
    let want = format!(
        concat!(
            r#"{{"tile_url":"/api/tiles/{{z}}/{{x}}/{{y}}.png","attribution":"© OpenStreetMap contributors","#,
            r#""min_bbox_side_m":20.0,"max_bbox_side_m":500.0,"demo_available":true,"regions":false,"#,
            r#""version":"{}"}}"#
        ),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(text(&body), want);
}

/// One region with every field set, one importing.
fn snapshot() -> RegionsSnapshot {
    let ready = RegionView {
        id: "europe/monaco".into(),
        name: "Monaco".into(),
        ready: true,
        bbox: BBox::new(43.72, 7.40, 43.76, 7.44).ok(),
        data_timestamp: Some("2026-10-31T19:43:02Z".into()),
        imported_unix: Some(1_700_000_000),
        last_attempt_unix: Some(1_700_000_100),
        auto_refresh: false,
        refresh_days: Some(30),
        next_refresh_unix: Some(1_702_592_000),
        disk_bytes: Some(123_456),
        job: None,
        last_error: Some("the download stalled; try again later".into()),
        last_error_unix: Some(1_700_000_100),
    };
    let importing = RegionView {
        id: "andorra".into(),
        name: "Andorra".into(),
        ready: false,
        bbox: None,
        data_timestamp: None,
        imported_unix: None,
        last_attempt_unix: None,
        auto_refresh: true,
        refresh_days: None,
        next_refresh_unix: None,
        disk_bytes: None,
        job: Some(JobView {
            phase: JobPhase::Downloading,
            done: 1024,
            total: Some(4096),
            since_unix: 1_700_000_200,
        }),
        last_error: None,
        last_error_unix: None,
    };
    RegionsSnapshot {
        version: 7,
        regions: vec![ready, importing],
        max_download_bytes: 2048 << 20,
        refresh_days: 7,
        min_refresh_days: 1,
        max_refresh_days: 365,
    }
}

#[tokio::test(start_paused = true)]
async fn the_regions_body_is_these_bytes() {
    let agent = FakeRegionAgent::new();
    let app = TestApp::builder()
        .agent(agent)
        .config(|c| c.dem_url = None)
        .build();
    app.agent.as_ref().unwrap().snapshot_reply(Ok(snapshot()));
    app.state.regions().unwrap().poll_once().await.unwrap();
    let (status, _, body) = get_uri(&app.router, "/api/regions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text(&body), GOLDEN_REGIONS);
}

/// `agent_online` and `manageable`, then the agent's snapshot as it sent it.
const GOLDEN_REGIONS: &str = concat!(
    r#"{"agent_online":true,"manageable":false,"snapshot":{"version":7,"regions":["#,
    r#"{"id":"europe/monaco","name":"Monaco","ready":true,"#,
    r#""bbox":{"south":43.72,"west":7.4,"north":43.76,"east":7.44},"#,
    r#""data_timestamp":"2026-10-31T19:43:02Z","imported_unix":1700000000,"last_attempt_unix":1700000100,"#,
    r#""auto_refresh":false,"refresh_days":30,"next_refresh_unix":1702592000,"disk_bytes":123456,"#,
    r#""job":null,"last_error":"the download stalled; try again later","last_error_unix":1700000100},"#,
    r#"{"id":"andorra","name":"Andorra","ready":false,"bbox":null,"data_timestamp":null,"#,
    r#""imported_unix":null,"last_attempt_unix":null,"auto_refresh":true,"refresh_days":null,"#,
    r#""next_refresh_unix":null,"disk_bytes":null,"#,
    r#""job":{"phase":"downloading","done":1024,"total":4096,"since_unix":1700000200},"#,
    r#""last_error":null,"last_error_unix":null}],"#,
    r#""max_download_bytes":2147483648,"refresh_days":7,"min_refresh_days":1,"max_refresh_days":365}}"#
);

/// The error bodies whose texts are fixed in the server.
#[tokio::test(start_paused = true)]
async fn fixed_error_bodies_are_these_bytes() {
    let app = TestApp::builder()
        .agent(FakeRegionAgent::new())
        .config(|c| c.dem_url = None)
        .build();
    let cases = [
        (
            "GET",
            "/api/nope",
            StatusCode::NOT_FOUND,
            r#"{"error":"no such API address on this server"}"#,
        ),
        (
            "GET",
            "/api/tiles/99/0/0.png",
            StatusCode::NOT_FOUND,
            r#"{"error":"not a valid map tile address"}"#,
        ),
        (
            "POST",
            "/api/regions/frobnicate",
            StatusCode::NOT_FOUND,
            r#"{"error":"no such region action"}"#,
        ),
        (
            "POST",
            "/api/regions/import",
            StatusCode::FORBIDDEN,
            r#"{"error":"region management is turned off on this server (set OBS_ADMIN_TOKEN)"}"#,
        ),
    ];
    for (method, uri, status, want) in cases {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(axum::body::Body::from(r#"{"id":"monaco"}"#))
            .unwrap();
        let (got, _, body) = send(&app.router, req).await;
        assert_eq!((got, text(&body)), (status, want), "{uri}");
    }
    let admin = TestApp::builder()
        .agent(FakeRegionAgent::new())
        .config(|c| c.dem_url = None)
        .config(|c| c.admin_token = Some("s3cret".into()))
        .build();
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/regions/import")
        .header("authorization", "Bearer wrong")
        .body(axum::body::Body::from(r#"{"id":"monaco"}"#))
        .unwrap();
    let (got, _, body) = send(&admin.router, req).await;
    assert_eq!(
        (got, text(&body)),
        (
            StatusCode::UNAUTHORIZED,
            r#"{"error":"the admin token is missing or wrong"}"#
        )
    );
}
