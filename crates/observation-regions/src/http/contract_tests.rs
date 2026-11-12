//! The contract between the server's agent client and the agent: the
//! server's `HttpRegionAgent` talking to this crate's routes through
//! `RouterHttp`, with no socket. Each call's answer, and each refusal's
//! `AgentError`, is what the server will see from a real agent.

use std::sync::Arc;
use std::time::Duration;

use http::header::CONTENT_LENGTH;
use http::{Method, StatusCode};
use observation_core::geo::BBox;
use observation_core::regions::{ActionKind, CatalogEntry, RegionAction, SizesRequest};
use observation_regions_api::{AgentError, HttpRegionAgent, RegionAgent};
use observation_upstream::testing::{FakeHttp, FakeReply, RouterHttp};

use super::app;
use crate::agent::{Agent, Ports};
use crate::config::Config;
use crate::ports::SystemClock;
use crate::testing::{FakeDownload, FakeToolchain, GateRunner, Reply};
use crate::{poly, registry};

const INDEX_URL: &str = "https://download.geofabrik.de/index-v1-nogeom.json";
const INDEX: &str = r#"{"features":[{"properties":{"id":"monaco","parent":"europe","name":"Monaco",
  "urls":{"pbf":"https://download.geofabrik.de/europe/monaco-latest.osm.pbf"}}}]}"#;
const SQUARE: &str = "sq\n1\n 0 0\n 1 0\n 1 1\n 0 1\nEND\nEND\n";
const TOKEN: &str = "s3cret";

/// An agent on fakes with the token, its catalog listing `monaco` (1234
/// bytes), a ready region `square` (the unit square, data from
/// 2026-10-31T19:43:02Z), and queries that answer at once.
async fn agent(data: &std::path::Path) -> Arc<Agent> {
    let http = FakeHttp::new();
    http.on(Method::GET, INDEX_URL, FakeReply::ok(INDEX));
    http.on(
        Method::HEAD,
        "https://download.geofabrik.de/europe/monaco-latest.osm.pbf",
        FakeReply::status(StatusCode::OK).header(CONTENT_LENGTH, "1234"),
    );
    let runner = Arc::new(GateRunner::new());
    runner.release(100);
    let config = Config::from_lookup(|k| match k {
        "OBS_REGIONS_DATA_DIR" => Some(data.display().to_string()),
        "OBS_REGIONS_TOKEN" => Some(TOKEN.into()),
        _ => None,
    })
    .unwrap();
    let ports = Ports {
        http: Arc::new(http),
        download: Arc::new(FakeDownload::new(Reply::Hang)),
        toolchain: Arc::new(FakeToolchain::default()),
        query_runner: runner,
        clock: Arc::new(SystemClock),
    };
    let agent = Agent::from_parts(&config, ports).unwrap();
    let reg = &agent.registry;
    reg.enqueue("square", "Square").unwrap();
    let (id, job) = reg.next_job().await;
    assert!(reg.started(
        &id,
        job,
        tokio::spawn(std::future::pending::<()>()).abort_handle()
    ));
    let db_dir = data.join("db/square-1");
    std::fs::create_dir_all(&db_dir).unwrap();
    let boundary = poly::Boundary::parse(SQUARE).unwrap();
    reg.finish(
        &id,
        job,
        Some(Ok(registry::Imported {
            bbox: boundary.bbox(),
            db: Arc::new(registry::RegionDb::new(db_dir, boundary)),
            db_dir: "square-1".into(),
            data_timestamp: Some("2026-10-31T19:43:02Z".into()),
            disk_bytes: 0,
        })),
    );
    agent
}

/// The server's client for `agent`, sending `token`.
fn client(agent: &Arc<Agent>, token: Option<&str>) -> HttpRegionAgent {
    HttpRegionAgent::new(
        Arc::new(RouterHttp::new(app(Arc::clone(agent)))),
        "http://agent.internal:8090".into(),
        token.map(str::to_owned),
    )
}

fn action(id: &str) -> RegionAction {
    RegionAction {
        id: id.into(),
        auto_refresh: None,
        refresh_days: None,
    }
}

#[tokio::test]
async fn snapshots_catalog_and_sizes_arrive_as_the_agent_sent_them() {
    let data = tempfile::tempdir().unwrap();
    let agent = agent(data.path()).await;
    let server = client(&agent, Some(TOKEN));

    let snapshot = server.snapshot(None).await.unwrap();
    assert_eq!(snapshot, agent.registry.snapshot());
    assert_eq!(snapshot.regions[0].id, "square");

    let catalog: Vec<CatalogEntry> =
        serde_json::from_slice(&server.catalog().await.unwrap()).unwrap();
    assert_eq!(
        catalog,
        [CatalogEntry {
            id: "monaco".into(),
            name: "Monaco".into(),
            parent: Some("europe".into()),
        }]
    );
    let sizes = SizesRequest {
        ids: vec!["monaco".into(), "nowhere".into()],
    };
    let sizes: serde_json::Value =
        serde_json::from_slice(&server.catalog_sizes(&sizes).await.unwrap()).unwrap();
    assert_eq!(sizes, serde_json::json!({"monaco": 1234}));
    let too_many = SizesRequest {
        ids: (0..41).map(|i| format!("r{i}")).collect(),
    };
    assert_eq!(
        server.catalog_sizes(&too_many).await.unwrap_err(),
        AgentError::BadRequest("expected {\"ids\": [...]} with at most 40 region ids".into())
    );
}

/// An action answers with the agent's new snapshot; its refusals map to
/// the `AgentError` the server turns into its own answer.
#[tokio::test]
async fn actions_answer_snapshots_and_refusals_map_to_agent_errors() {
    let data = tempfile::tempdir().unwrap();
    let agent = agent(data.path()).await;
    let server = client(&agent, Some(TOKEN));

    let answer = server
        .act(ActionKind::Import, &action("monaco"))
        .await
        .unwrap();
    let snapshot: observation_core::regions::RegionsSnapshot =
        serde_json::from_slice(&answer).unwrap();
    assert_eq!(snapshot, agent.registry.snapshot());
    assert!(
        snapshot
            .regions
            .iter()
            .any(|r| r.id == "monaco" && r.job.is_some())
    );

    let busy = server
        .act(ActionKind::Import, &action("monaco"))
        .await
        .unwrap_err();
    assert!(matches!(busy, AgentError::Conflict(_)), "{busy:?}");
    assert_eq!(
        server
            .act(ActionKind::Import, &action("nowhere"))
            .await
            .unwrap_err(),
        AgentError::BadRequest("no extract called `nowhere`".into())
    );
    assert_eq!(
        server
            .act(ActionKind::Delete, &action("nowhere"))
            .await
            .unwrap_err(),
        AgentError::BadRequest("no such region".into())
    );
    server
        .act(ActionKind::Cancel, &action("monaco"))
        .await
        .unwrap();
    assert!(
        agent
            .registry
            .snapshot()
            .regions
            .iter()
            .all(|r| r.id != "monaco")
    );
}

/// A query inside a ready region answers its output with the region's data
/// date; one outside every region is `None`.
#[tokio::test]
async fn queries_answer_inside_a_region_and_none_outside() {
    let data = tempfile::tempdir().unwrap();
    let agent = agent(data.path()).await;
    let server = client(&agent, Some(TOKEN));
    let timeout = Duration::from_secs(30);

    let inside = BBox::new(0.2, 0.2, 0.3, 0.3).unwrap();
    let result = server.query(&inside, "q", timeout).await.unwrap().unwrap();
    assert_eq!(result.body, "{\"elements\":[]}");
    assert_eq!(
        result.osm_base_unix,
        observation_core::provenance::parse_utc("2026-10-31T19:43:02Z")
    );
    let outside = BBox::new(5.0, 5.0, 6.0, 6.0).unwrap();
    assert_eq!(server.query(&outside, "q", timeout).await.unwrap(), None);
}

/// A wrong or missing token: the JSON calls are `Unauthorized`, the
/// snapshot and the query fail with a description.
#[tokio::test]
async fn a_wrong_token_is_refused_on_every_route() {
    let data = tempfile::tempdir().unwrap();
    let agent = agent(data.path()).await;
    for token in [Some("wrong"), None] {
        let server = client(&agent, token);
        assert_eq!(
            server.catalog().await.unwrap_err(),
            AgentError::Unauthorized
        );
        assert_eq!(
            server
                .act(ActionKind::Import, &action("monaco"))
                .await
                .unwrap_err(),
            AgentError::Unauthorized
        );
        assert_eq!(
            server.snapshot(None).await.unwrap_err(),
            "HTTP status client error (401 Unauthorized)"
        );
        let inside = BBox::new(0.2, 0.2, 0.3, 0.3).unwrap();
        let err = server
            .query(&inside, "q", Duration::from_secs(30))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "agent answered 401 Unauthorized: {\"error\":\"missing or wrong agent token\"}"
        );
    }
    assert!(
        agent
            .registry
            .snapshot()
            .regions
            .iter()
            .all(|r| r.id != "monaco")
    );
}
