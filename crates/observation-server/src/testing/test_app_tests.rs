//! [`TestApp`]: the application built over fakes with nothing spawned.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use observation_core::bundle::AreaBundle;
use observation_core::regions::RegionsSnapshot;
use observation_regions_api::testing::{AgentCall, FakeRegionAgent};
use tower::ServiceExt;

use super::{ManualClock, TestApp};

async fn get(app: &TestApp, uri: &str) -> (StatusCode, Bytes) {
    let resp = app
        .router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body)
}

fn json(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

/// The demo bundle is dated by the ports' clock, and serving it asks no
/// upstream.
#[tokio::test]
async fn the_demo_is_dated_by_the_injected_clock() {
    let app = TestApp::builder()
        .clock(Arc::new(ManualClock::at_unix(1_234_567_890)))
        .build();
    let (status, body) = get(&app, "/api/demo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        AreaBundle::decode(&body).unwrap().generated_unix,
        1_234_567_890
    );
    assert!(app.overpass.requests().is_empty());
    assert!(app.tiles.requests().is_empty());
}

/// Without an agent the region routes are off, as without
/// `OBS_REGIONS_URL`.
#[tokio::test]
async fn without_an_agent_regions_are_not_enabled() {
    let app = TestApp::builder().build();
    assert!(app.state.regions().is_none());
    let (status, body) = get(&app, "/api/regions").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        json(&body),
        serde_json::json!({"error": "local regions are not enabled on this server"})
    );
}

/// With an agent, nothing polls it until the test does: the state is built
/// offline, and a poll publishes the agent's snapshot. The configuration
/// tweak (an admin token) makes the regions manageable.
#[tokio::test(start_paused = true)]
async fn the_agent_is_followed_only_when_polled() {
    let agent = FakeRegionAgent::new();
    let app = TestApp::builder()
        .agent(agent.clone())
        .config(|c| c.admin_token = Some("admin".into()))
        .build();
    let (status, body) = get(&app, "/api/regions").await;
    assert_eq!(status, StatusCode::OK);
    let status_json = json(&body);
    assert_eq!(status_json["agent_online"], false);
    assert_eq!(status_json["manageable"], true);
    tokio::task::yield_now().await;
    assert!(agent.calls().is_empty(), "nothing was spawned");

    agent.snapshot_reply(Ok(RegionsSnapshot {
        version: 7,
        ..RegionsSnapshot::default()
    }));
    app.state.regions().unwrap().poll_once().await.unwrap();
    let (_, body) = get(&app, "/api/regions").await;
    let status_json = json(&body);
    assert_eq!(status_json["agent_online"], true);
    assert_eq!(status_json["snapshot"]["version"], 7);
    assert_eq!(agent.calls(), [AgentCall::Snapshot { since: None }]);
}
