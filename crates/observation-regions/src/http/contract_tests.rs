//! The contract between the server's agent client and the agent: the
//! server's `HttpRegionAgent` talking to this crate's routes through
//! `RouterHttp`, with no socket. Each call's answer, and each refusal's
//! `AgentError`, is what the server will see from a real agent. HTTP
//! framing and the token on the wire are covered by socket tests elsewhere.

use std::sync::Arc;
use std::time::Duration;

use http::header::CONTENT_LENGTH;
use http::{Method, StatusCode};
use observation_core::geo::BBox;
use observation_core::regions::{ActionKind, CatalogEntry, RegionAction, SizesRequest};
use observation_regions_api::{AgentError, HttpRegionAgent, RegionAgent};
use observation_upstream::testing::{FakeHttp, FakeReply, RouterHttp};
use tokio::time::Instant;

use super::app;
use crate::agent::{Agent, Ports};
use crate::config::Config;
use crate::ports::ManualClock;
use crate::testing::{FakeDownload, FakeToolchain, GateRunner, Reply};
use crate::{poly, registry};

const INDEX_URL: &str = "https://download.geofabrik.de/index-v1-nogeom.json";
const INDEX: &str = r#"{"features":[{"properties":{"id":"monaco","parent":"europe","name":"Monaco",
  "urls":{"pbf":"https://download.geofabrik.de/europe/monaco-latest.osm.pbf"}}}]}"#;
const SQUARE: &str = "sq\n1\n 0 0\n 1 0\n 1 1\n 0 1\nEND\nEND\n";
pub(super) const TOKEN: &str = "s3cret";

/// An agent on fakes with the token, its catalog listing `monaco` (1234
/// bytes), a ready region `square` (the unit square, data from
/// 2026-10-31T19:43:02Z), queries that answer at once, and its clock at
/// Unix 1 700 000 000.
pub(super) async fn agent(data: &std::path::Path) -> Arc<Agent> {
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
        clock: Arc::new(ManualClock::at_unix(1_700_000_000)),
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
        ids: Vec::from_fn(41, |i| format!("r{i}")),
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
    assert_eq!(
        busy,
        AgentError::Conflict("Monaco is already being imported".into())
    );
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
    assert_eq!(
        server
            .act(ActionKind::Cancel, &action("square"))
            .await
            .unwrap_err(),
        AgentError::BadRequest("no import of that region is running".into())
    );

    let settings = RegionAction {
        id: "square".into(),
        auto_refresh: Some(false),
        refresh_days: Some(Some(30)),
    };
    let answer = server.act(ActionKind::Settings, &settings).await.unwrap();
    let snapshot: observation_core::regions::RegionsSnapshot =
        serde_json::from_slice(&answer).unwrap();
    assert_eq!(snapshot, agent.registry.snapshot());
    let square = snapshot.regions.iter().find(|r| r.id == "square").unwrap();
    assert_eq!(
        (square.auto_refresh, square.refresh_days),
        (false, Some(30))
    );
    let out_of_range = RegionAction {
        refresh_days: Some(Some(366)),
        ..settings.clone()
    };
    assert_eq!(
        server
            .act(ActionKind::Settings, &out_of_range)
            .await
            .unwrap_err(),
        AgentError::BadRequest(
            "expected `auto_refresh` (true or false), `refresh_days` (7 to 365), or both".into()
        )
    );
    let unknown = RegionAction {
        id: "nowhere".into(),
        ..settings
    };
    assert_eq!(
        server
            .act(ActionKind::Settings, &unknown)
            .await
            .unwrap_err(),
        AgentError::BadRequest("no such region".into())
    );
}

/// The long poll: `since` the current version waits 25 s for a change and
/// answers the unchanged snapshot; a change meanwhile answers at once.
#[tokio::test(start_paused = true)]
async fn the_long_poll_waits_for_a_change() {
    let data = tempfile::tempdir().unwrap();
    let agent = agent(data.path()).await;
    let server = client(&agent, Some(TOKEN));
    let version = server.snapshot(None).await.unwrap().version;

    let start = Instant::now();
    let unchanged = server.snapshot(Some(version)).await.unwrap();
    assert_eq!(start.elapsed(), Duration::from_secs(25));
    assert_eq!(unchanged.version, version);

    let start = Instant::now();
    let poll = tokio::spawn({
        let server = client(&agent, Some(TOKEN));
        async move { server.snapshot(Some(version)).await }
    });
    tokio::time::sleep(Duration::from_secs(3)).await;
    server
        .act(ActionKind::Import, &action("monaco"))
        .await
        .unwrap();
    let changed = poll.await.unwrap().unwrap();
    assert_eq!(start.elapsed(), Duration::from_secs(3));
    assert!(changed.version > version);
    assert!(changed.regions.iter().any(|r| r.id == "monaco"));
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

/// Every route of `app` but `/healthz`, as `mod.rs` writes its path.
fn guarded_routes() -> Vec<&'static str> {
    include_str!("mod.rs")
        .split(".route(")
        .skip(1)
        .filter_map(|route| route.split(',').next())
        .map(str::trim)
        .filter(|path| *path != "\"/healthz\"")
        .collect()
}

/// A wrong or missing token is refused on every route the router lists
/// (a route added there without a case here fails the test): the JSON
/// calls are `Unauthorized`, the snapshot and the query fail with a
/// description, and nothing changes.
#[tokio::test]
async fn a_wrong_token_is_refused_on_every_route() {
    let data = tempfile::tempdir().unwrap();
    let agent = agent(data.path()).await;
    let before = agent.registry.snapshot();
    let routes = guarded_routes();
    assert!(routes.len() >= 8, "the route list was not read: {routes:?}");
    for token in [Some("wrong"), None] {
        let server = client(&agent, token);
        for route in &routes {
            let kind = [
                ActionKind::Import,
                ActionKind::Cancel,
                ActionKind::Delete,
                ActionKind::Settings,
            ]
            .into_iter()
            .find(|kind| *route == format!("paths::action(ActionKind::{kind:?})"));
            match (*route, kind) {
                (_, Some(kind)) => {
                    let square = RegionAction {
                        id: "square".into(),
                        auto_refresh: Some(false),
                        refresh_days: None,
                    };
                    let id = match kind {
                        ActionKind::Import => action("monaco"),
                        ActionKind::Cancel | ActionKind::Delete | ActionKind::Settings => square,
                    };
                    assert_eq!(
                        server.act(kind, &id).await.unwrap_err(),
                        AgentError::Unauthorized,
                        "{route}"
                    );
                }
                ("paths::REGIONS", None) => {
                    for since in [None, Some(before.version)] {
                        assert_eq!(
                            server.snapshot(since).await.unwrap_err(),
                            "HTTP status client error (401 Unauthorized)"
                        );
                    }
                }
                ("paths::CATALOG", None) => {
                    assert_eq!(
                        server.catalog().await.unwrap_err(),
                        AgentError::Unauthorized
                    );
                }
                ("paths::CATALOG_SIZES", None) => {
                    let sizes = SizesRequest {
                        ids: vec!["monaco".into()],
                    };
                    assert_eq!(
                        server.catalog_sizes(&sizes).await.unwrap_err(),
                        AgentError::Unauthorized
                    );
                }
                ("paths::INTERPRETER", None) => {
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
                (other, None) => panic!("no token case for the route {other}"),
            }
        }
    }
    assert_eq!(agent.registry.snapshot(), before);
}
