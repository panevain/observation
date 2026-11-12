//! The server's side of the region agent on a scripted agent: following its
//! snapshot, local queries, the catalog cache, actions, and the API errors
//! agent failures become.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use observation_core::geo::BBox;
use observation_core::regions::{
    ActionKind, RegionAction, RegionView, RegionsSnapshot, SizesRequest,
};
use observation_regions_api::testing::{AgentCall, FakeRegionAgent};
use observation_regions_api::{AgentError, QueryResult};
use tokio::time::Instant;

use super::Regions;
use crate::error::ApiError;
use crate::overpass::build_query;

fn regions(manageable: bool) -> (Arc<Regions>, FakeRegionAgent) {
    let agent = FakeRegionAgent::new();
    (
        Arc::new(Regions::new(Arc::new(agent.clone()), manageable)),
        agent,
    )
}

fn bbox() -> BBox {
    BBox::new(47.6, -122.34, 47.601, -122.339).unwrap()
}

/// A ready region whose bounds hold [`bbox`].
fn region() -> RegionView {
    RegionView {
        id: "us/washington".into(),
        name: "Washington".into(),
        ready: true,
        bbox: BBox::new(45.5, -124.8, 49.0, -116.9).ok(),
        data_timestamp: None,
        imported_unix: Some(1_700_000_000),
        last_attempt_unix: None,
        auto_refresh: true,
        refresh_days: None,
        next_refresh_unix: None,
        disk_bytes: None,
        job: None,
        last_error: None,
        last_error_unix: None,
    }
}

fn snapshot(version: u64, regions: Vec<RegionView>) -> RegionsSnapshot {
    RegionsSnapshot {
        version,
        regions,
        ..RegionsSnapshot::default()
    }
}

fn snapshot_json(version: u64) -> axum::body::Bytes {
    serde_json::to_vec(&snapshot(version, vec![]))
        .unwrap()
        .into()
}

/// Puts `regions` online with `snap`.
async fn online(regions: &Regions, agent: &FakeRegionAgent, snap: RegionsSnapshot) {
    agent.snapshot_reply(Ok(snap));
    regions.poll_once().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_poll_asks_since_the_current_version_only_while_the_agent_is_online() {
    let (regions, agent) = regions(false);
    let mut rx = regions.subscribe();
    assert!(!regions.status().agent_online);

    agent.snapshot_reply(Ok(snapshot(3, vec![region()])));
    regions.poll_once().await.unwrap();
    let status = regions.status();
    assert!(status.agent_online);
    assert_eq!(status.snapshot.version, 3);
    assert!(rx.has_changed().unwrap());
    rx.mark_unchanged();

    // The same snapshot again changes nothing.
    agent.snapshot_reply(Ok(snapshot(3, vec![region()])));
    regions.poll_once().await.unwrap();
    assert!(!rx.has_changed().unwrap());

    agent.snapshot_reply(Err("connection refused".into()));
    assert_eq!(regions.poll_once().await.unwrap_err(), "connection refused");
    let status = regions.status();
    assert!(!status.agent_online);
    assert_eq!(status.snapshot.version, 3, "kept while offline");
    assert!(rx.has_changed().unwrap());

    // Back online, a restarted agent counting from 0 again replaces it.
    agent.snapshot_reply(Ok(snapshot(1, vec![])));
    regions.poll_once().await.unwrap();
    assert_eq!(regions.status().snapshot.version, 1);

    assert_eq!(
        agent.calls(),
        [None, Some(3), Some(3), None]
            .map(|since| AgentCall::Snapshot { since })
            .to_vec()
    );
    agent.assert_drained();
}

/// While the agent is unreachable the poller waits 1, 2, 4 ... 30 s between
/// tries; an answer resets the wait, and the next poll follows at once.
#[tokio::test(start_paused = true)]
async fn the_poller_backs_off_while_the_agent_is_unreachable() {
    let (regions, agent) = regions(false);
    for _ in 0..7 {
        agent.snapshot_reply(Err("connection refused".into()));
    }
    agent.snapshot_reply(Ok(snapshot(1, vec![])));
    agent.snapshot_reply(Err("connection reset".into()));
    agent.idle_when_drained();
    let started = Instant::now();
    let poller = tokio::spawn(Arc::clone(&regions).run_poller());
    tokio::time::sleep(Duration::from_secs(600)).await;
    poller.abort();
    let at: Vec<u64> = agent
        .call_times()
        .iter()
        .map(|t| t.duration_since(started).as_secs())
        .collect();
    assert_eq!(at, [0, 1, 3, 7, 15, 31, 61, 91, 91, 92]);
    let since: Vec<_> = agent
        .calls()
        .into_iter()
        .map(|c| match c {
            AgentCall::Snapshot { since } => since,
            other => panic!("{other:?}"),
        })
        .collect();
    let mut want = vec![None; 8];
    want.extend([Some(1), None]);
    assert_eq!(since, want);
    agent.assert_drained();
}

#[tokio::test(start_paused = true)]
async fn the_agent_is_asked_only_when_online_with_a_region_that_may_hold_the_box() {
    let (regions, agent) = regions(false);
    let b = bbox();
    let ask = || regions.fetch(&b, Duration::from_secs(15));
    assert_eq!(ask().await, Ok(None), "offline");
    online(&regions, &agent, snapshot(1, vec![])).await;
    assert_eq!(ask().await, Ok(None), "no region");
    let not_ready = RegionView {
        ready: false,
        ..region()
    };
    online(&regions, &agent, snapshot(2, vec![not_ready])).await;
    assert_eq!(ask().await, Ok(None), "not ready");
    let elsewhere = RegionView {
        bbox: BBox::new(0.0, 0.0, 1.0, 1.0).ok(),
        ..region()
    };
    online(&regions, &agent, snapshot(3, vec![elsewhere])).await;
    assert_eq!(ask().await, Ok(None), "elsewhere");
    assert!(
        !agent
            .calls()
            .iter()
            .any(|c| matches!(c, AgentCall::Query { .. }))
    );

    online(&regions, &agent, snapshot(4, vec![region()])).await;
    agent.query_reply(Ok(None));
    assert_eq!(ask().await, Ok(None), "the agent has no region for it");
    assert_eq!(
        agent.calls().last(),
        Some(&AgentCall::Query {
            bbox: bbox(),
            query: build_query(&bbox()),
            timeout: Duration::from_secs(15),
        })
    );
    agent.assert_drained();
}

#[tokio::test(start_paused = true)]
async fn local_results_are_parsed_and_dated() {
    let (regions, agent) = regions(false);
    online(&regions, &agent, snapshot(1, vec![region()])).await;
    let dated = r#"{"osm3s":{"timestamp_osm_base":"2024-05-01T12:00:00Z"},"elements":[]}"#;
    let undated = r#"{"osm3s":{"timestamp_osm_base":""},"elements":[]}"#;
    for (body, header, want) in [
        (dated, None, Some(1_714_564_800)),
        (dated, Some(1), Some(1_714_564_800)),
        (undated, Some(1_700_000_000), Some(1_700_000_000)),
        (undated, None, None),
    ] {
        agent.query_reply(Ok(Some(QueryResult {
            body: body.into(),
            osm_base_unix: header,
        })));
        let data = regions
            .fetch(&bbox(), Duration::from_secs(15))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(data.osm_base_unix, want, "{body} {header:?}");
    }
    agent.query_reply(Err("agent answered 500 Internal Server Error: boom".into()));
    assert_eq!(
        regions.fetch(&bbox(), Duration::from_secs(15)).await,
        Err("agent answered 500 Internal Server Error: boom".into())
    );
    agent.query_reply(Ok(Some(QueryResult {
        body: "<html>".into(),
        osm_base_unix: None,
    })));
    let e = regions
        .fetch(&bbox(), Duration::from_secs(15))
        .await
        .unwrap_err();
    assert!(!e.is_empty());
    agent.assert_drained();
}

/// Each agent failure becomes the API error it was before the port, with
/// the same status and text.
#[tokio::test(start_paused = true)]
async fn agent_failures_map_to_the_same_api_errors() {
    let unreachable = "the region agent is not reachable; try again shortly";
    let cases = [
        (
            AgentError::BadRequest("not a region id".into()),
            StatusCode::BAD_REQUEST,
            "not a region id",
        ),
        (
            AgentError::Conflict("monaco is importing".into()),
            StatusCode::CONFLICT,
            "monaco is importing",
        ),
        (
            AgentError::Unauthorized,
            StatusCode::BAD_GATEWAY,
            "the server is not allowed to use its region agent; check OBS_REGIONS_TOKEN",
        ),
        (
            AgentError::Unreachable,
            StatusCode::BAD_GATEWAY,
            unreachable,
        ),
        (
            AgentError::TooLarge,
            StatusCode::BAD_GATEWAY,
            "the region agent sent too much data",
        ),
        (
            AgentError::Other("disk full".into()),
            StatusCode::BAD_GATEWAY,
            "disk full",
        ),
    ];
    let sizes = SizesRequest {
        ids: vec!["monaco".into()],
    };
    let action = RegionAction {
        id: "monaco".into(),
        ..RegionAction::default()
    };
    for (error, status, text) in cases {
        let (regions, agent) = regions(true);
        agent.answer(Err(error.clone()));
        agent.answer(Err(error.clone()));
        agent.answer(Err(error.clone()));
        let results = [
            regions.catalog().await.unwrap_err(),
            regions.catalog_sizes(&sizes).await.unwrap_err(),
            regions.act(ActionKind::Import, &action).await.unwrap_err(),
        ];
        for e in results {
            assert_eq!(
                (e.status(), e.to_string().as_str()),
                (status, text),
                "{error:?}"
            );
        }
        agent.assert_drained();
    }
}

#[tokio::test(start_paused = true)]
async fn the_catalog_is_reused_for_an_hour_and_failures_are_not() {
    let (regions, agent) = regions(false);
    agent.answer(Err(AgentError::Unreachable));
    assert!(regions.catalog().await.is_err());
    agent.answer(Ok("[1]".into()));
    assert_eq!(regions.catalog().await.unwrap(), "[1]");
    tokio::time::advance(Duration::from_secs(3599)).await;
    assert_eq!(regions.catalog().await.unwrap(), "[1]", "cached");
    tokio::time::advance(Duration::from_secs(1)).await;
    agent.answer(Ok("[2]".into()));
    assert_eq!(regions.catalog().await.unwrap(), "[2]");
    assert_eq!(agent.calls(), vec![AgentCall::Catalog; 3]);
    // Sizes are not cached here (the agent caches them).
    let sizes = SizesRequest {
        ids: vec!["monaco".into()],
    };
    agent.answer(Ok("{}".into()));
    agent.answer(Ok("{}".into()));
    regions.catalog_sizes(&sizes).await.unwrap();
    regions.catalog_sizes(&sizes).await.unwrap();
    assert_eq!(
        agent.calls()[3..],
        [
            AgentCall::CatalogSizes(sizes.clone()),
            AgentCall::CatalogSizes(sizes)
        ]
    );
    agent.assert_drained();
}

#[tokio::test(start_paused = true)]
async fn actions_need_management_and_publish_only_newer_snapshots() {
    let action = RegionAction {
        id: "monaco".into(),
        ..RegionAction::default()
    };
    let (off, agent) = regions(false);
    assert!(matches!(
        off.act(ActionKind::Delete, &action).await,
        Err(ApiError::Forbidden(
            "region management is turned off on this server (set OBS_ADMIN_TOKEN)"
        ))
    ));
    assert!(agent.calls().is_empty(), "the agent is not asked");

    let (regions, agent) = regions(true);
    online(&regions, &agent, snapshot(5, vec![])).await;
    // An answer older than what the poller saw is dropped.
    agent.answer(Ok(snapshot_json(4)));
    let status = regions.act(ActionKind::Delete, &action).await.unwrap();
    assert_eq!(status.snapshot.version, 5);
    assert!(status.agent_online && status.manageable);
    agent.answer(Ok(snapshot_json(6)));
    let status = regions.act(ActionKind::Cancel, &action).await.unwrap();
    assert_eq!(status.snapshot.version, 6);
    assert_eq!(regions.status(), status);
    agent.answer(Ok("<html>".into()));
    let e = regions.act(ActionKind::Import, &action).await.unwrap_err();
    assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        matches!(&e, ApiError::Internal(m) if m.starts_with("agent snapshot unreadable: ")),
        "{e:?}"
    );
    assert_eq!(
        agent.calls()[1..],
        [
            AgentCall::Act(ActionKind::Delete, action.clone()),
            AgentCall::Act(ActionKind::Cancel, action.clone()),
            AgentCall::Act(ActionKind::Import, action),
        ]
    );
    agent.assert_drained();
}
