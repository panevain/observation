//! The connection to the app against the real server's agent bridge, served
//! on loopback in process (`observation-server`'s `TestApp`), with the
//! browser's side played over the same socket.

use std::net::SocketAddr;
use std::time::Duration;

use super::*;
use crate::app::{AppStateOut, ConnectArgs};
use observation_agent_api::{PairingCreated, StateAccepted, paths};
use observation_core::geo::BBox;
use observation_server::testing::TestApp;
use observation_upstream::testing::{FakeHttp, FakeReply};
use observation_upstream::{GuardedHttp, HttpClient, HttpRequest, Redirects};
use serde_json::json;

/// Serves `app` on a loopback port, with peer addresses; returns its base URL.
async fn serve(app: &TestApp) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = app.router.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://127.0.0.1:{port}")
}

fn bridge_app(overpass: FakeHttp) -> TestApp {
    TestApp::builder()
        .overpass(overpass)
        .config(|c| {
            c.agent_bridge = true;
            c.dem_url = None;
        })
        .build()
}

/// The browser's side of the bridge.
struct Browser {
    http: GuardedHttp,
    base: String,
    token: String,
    code: String,
}

impl Browser {
    async fn open(base: &str) -> Self {
        let http = GuardedHttp::builder(true, Redirects::None)
            .no_proxy()
            .build()
            .unwrap();
        let req = HttpRequest::post(format!("{base}{}", paths::PAIRINGS), 1 << 16)
            .json(br#"{"v":1}"#.to_vec());
        let resp = http.send(req).await.unwrap();
        assert_eq!(resp.status, 201);
        let created: PairingCreated = serde_json::from_slice(resp.bytes().unwrap()).unwrap();
        Self {
            http,
            base: base.to_owned(),
            token: created.browser_token,
            code: created.code,
        }
    }

    async fn share(&self, state: Value) -> u64 {
        let body = json!({"v": 1, "state": state}).to_string();
        let req = HttpRequest::new(
            http::Method::PUT,
            format!("{}{}", self.base, paths::STATE),
            1 << 16,
        )
        .bearer(&self.token)
        .unwrap()
        .json(body.into_bytes());
        let resp = self.http.send(req).await.unwrap();
        assert_eq!(resp.status, 200);
        serde_json::from_slice::<StateAccepted>(resp.bytes().unwrap())
            .unwrap()
            .revision
    }

    async fn disconnect(&self) {
        let req = HttpRequest::new(
            http::Method::DELETE,
            format!("{}{}", self.base, paths::PAIRING),
            1 << 16,
        )
        .bearer(&self.token)
        .unwrap();
        assert_eq!(self.http.send(req).await.unwrap().status, 204);
    }
}

async fn connect(mcp: &ObservationMcp, url: &str, code: &str) -> Result<AppStateOut, String> {
    mcp.connect_app(Parameters(ConnectArgs {
        url: url.to_owned(),
        code: code.to_owned(),
    }))
    .await
    .map(|Json(out)| out)
}

/// Waits for the link to report a change.
async fn next_change(rx: &mut tokio::sync::watch::Receiver<u64>) {
    tokio::time::timeout(Duration::from_secs(10), rx.changed())
        .await
        .expect("a change within 10 s")
        .unwrap();
}

fn demo_scene() -> Value {
    let s = edit::new_demo_scene().unwrap();
    edit::finish(&s).unwrap().1
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_reads_what_the_app_shares_and_hears_of_changes() {
    let app = bridge_app(FakeHttp::new());
    let base = serve(&app).await;
    let browser = Browser::open(&base).await;
    let mcp = ObservationMcp::new();

    // The address as a person might give it, and the code as they might type it.
    let url = base.replace("127.0.0.1", "localhost");
    let typed = format!("{}-{}", &browser.code[..4], &browser.code[4..]).to_lowercase();
    let out = connect(&mcp, &url, &typed).await.unwrap();
    assert!(out.connected);
    assert_eq!(out.server.as_deref(), Some(base.as_str()));
    assert_eq!((out.revision, out.scene.as_ref()), (0, None));

    let mut changes = mcp.link().subscribe();
    let revision = browser
        .share(json!({
            "scene": demo_scene(),
            "scene_name": "Home",
            "area": {"kind": "demo"},
            "selection": [1],
            "pinned": {"x": 4.0, "z": -6.0, "lat": 47.7, "lon": -122.2, "seq": 1},
        }))
        .await;
    assert_eq!(revision, 1);
    // The long poll brings it without being asked.
    next_change(&mut changes).await;
    assert_eq!(
        mcp.link().snapshot().view.map(|v| v.revision),
        Some(1),
        "the background poll has it"
    );

    let Json(state) = mcp.get_app_state().await.unwrap();
    assert_eq!(state.revision, 1);
    assert_eq!(state.scene_name.as_deref(), Some("Home"));
    assert_eq!(state.area, Some(json!({"kind": "demo"})));
    assert_eq!(state.selection, [1]);
    let pinned = state.pinned.unwrap();
    assert_eq!((pinned.x, pinned.z, pinned.seq), (4.0, -6.0, 1));
    // The shared scene is ready for the scene tools.
    let scene = state.scene.unwrap();
    assert!(SceneFile::from_json(&scene.to_string()).is_ok());
    assert_eq!(state.scene_problem, None);
    // The resource reads the same.
    let text = mcp.app_state_json().await;
    assert!(text.contains(r#""scene_name": "Home""#), "{text}");
    assert_eq!(STATE_URI, "observation://app/state");

    // A scene the app could not have sent is reported, not passed on.
    browser.share(json!({"scene": {"format": "nope"}})).await;
    next_change(&mut changes).await;
    let Json(state) = mcp.get_app_state().await.unwrap();
    assert_eq!(state.scene, None);
    assert!(state.scene_problem.unwrap().contains("scene"));

    let Json(gone) = mcp.disconnect_app().await;
    assert!(gone.was_connected);
    assert!(app.state.agent().unwrap().is_empty(), "the server ended it");
    let Json(after) = mcp.get_app_state().await.unwrap();
    assert!(!after.connected);
    assert_eq!(after.ended.as_deref(), Some("the agent disconnected"));
    let Json(again) = mcp.disconnect_app().await;
    assert!(!again.was_connected);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_hears_when_the_app_disconnects() {
    let app = bridge_app(FakeHttp::new());
    let base = serve(&app).await;
    let browser = Browser::open(&base).await;
    let mcp = ObservationMcp::new();
    connect(&mcp, &base, &browser.code).await.unwrap();
    let mut changes = mcp.link().subscribe();
    browser.disconnect().await;
    // The follower may still report the state it read after connecting, so
    // wait for the change that ends the connection rather than the first one.
    while mcp.link().is_connected() {
        next_change(&mut changes).await;
    }
    let Json(state) = mcp.get_app_state().await.unwrap();
    assert_eq!(state.ended.as_deref(), Some("the app disconnected"));
}

#[tokio::test(flavor = "multi_thread")]
async fn connecting_is_refused_with_reasons() {
    let mcp = ObservationMcp::new();
    // Without a connection there is nothing to read.
    assert!(
        mcp.get_app_state()
            .await
            .map(drop)
            .expect_err("an error")
            .contains("connect_app")
    );
    // Only this computer, over plain http, and before any request is made.
    for url in [
        "https://127.0.0.1:8080",
        "http://169.254.169.254",
        "http://observation.example",
        "http://127.0.0.1:8080/api/agent/claim",
    ] {
        let e = connect(&mcp, url, "ABCD-EFGH")
            .await
            .map(drop)
            .expect_err("an error");
        assert!(
            e.contains("this computer") || e.contains("path"),
            "{url}: {e}"
        );
    }

    let app = bridge_app(FakeHttp::new());
    let base = serve(&app).await;
    let _browser = Browser::open(&base).await;
    let e = connect(&mcp, &base, "0000-0000")
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("no pairing has that code"), "{e}");
    assert!(!mcp.link().is_connected());

    // A server without the bridge says so.
    let plain = TestApp::builder().build();
    let base = serve(&plain).await;
    let e = connect(&mcp, &base, "ABCD-EFGH")
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("OBS_AGENT_BRIDGE=on"), "{e}");

    // Nothing listening.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let e = connect(&mcp, &format!("http://127.0.0.1:{closed}"), "ABCD-EFGH")
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("could not reach"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn redirects_off_this_computer_are_not_followed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://169.254.169.254/latest\r\n\
                      Content-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    });
    let mcp = ObservationMcp::new();
    let e = connect(&mcp, &format!("http://127.0.0.1:{port}"), "ABCD-EFGH")
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("redirect"), "{e}");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

fn osm_scene(bbox: BBox) -> Value {
    let origin = edit::new_demo_scene().unwrap().origin;
    edit::finish(&SceneFile::new(AreaRef::Osm { bbox }, origin))
        .unwrap()
        .1
}

#[tokio::test(flavor = "multi_thread")]
async fn coverage_runs_on_the_users_real_area_while_connected() {
    let bbox = BBox::parse(observation_mapdata::demo::BBOX_TXT.trim()).unwrap();
    let scene = osm_scene(bbox);
    let mcp = ObservationMcp::new();

    // Offline, a real area is refused as before.
    let offline = mcp
        .compute_coverage(Parameters(SceneArg {
            scene: scene.clone(),
        }))
        .await
        .map(drop)
        .expect_err("an error");
    assert!(offline.contains("connect_app"), "{offline}");

    // The server builds the area once, from the demo street's OSM data.
    let overpass = FakeHttp::new();
    overpass.on(
        http::Method::POST,
        observation_server::config::DEFAULT_OVERPASS_URL,
        FakeReply::ok(observation_mapdata::demo::OVERPASS_JSON),
    );
    let app = bridge_app(overpass);
    let base = serve(&app).await;
    let browser = Browser::open(&base).await;
    connect(&mcp, &base, &browser.code).await.unwrap();

    let args = |scene: &Value| json!({ "scene": scene }).as_object().cloned();
    let placed = mcp
        .in_tool_scope(args(&scene).as_ref(), async {
            mcp.add_example_cameras(Parameters(ExampleArgs {
                scene: scene.clone(),
                count: 3,
                model_ids: Vec::new(),
            }))
        })
        .await
        .unwrap()
        .0
        .scene;
    let Json(out) = mcp
        .in_tool_scope(
            args(&placed).as_ref(),
            mcp.compute_coverage(Parameters(SceneArg {
                scene: placed.clone(),
            })),
        )
        .await
        .unwrap();
    assert_eq!(out.cameras_counted.len(), 3);
    assert!(out.levels[0].street_m2 > 1000.0, "{:?}", out.levels[0]);
    assert!(out.levels[0].street_seen_m2 > 0.0);
    let Json(address) = mcp
        .in_tool_scope(args(&placed).as_ref(), async {
            mcp.address_at_point(Parameters(PointQueryArgs {
                scene: placed.clone(),
                x: 0.0,
                z: 0.0,
            }))
        })
        .await
        .unwrap();
    let _ = address;
    // Fetched once, then held: the fake has no second reply to give.
    assert!(mcp.link().areas().contains(&bbox));

    // Disconnecting forgets it: offline is offline again.
    mcp.disconnect_app().await;
    let again = mcp
        .in_tool_scope(
            args(&placed).as_ref(),
            mcp.compute_coverage(Parameters(SceneArg { scene: placed })),
        )
        .await
        .map(drop)
        .expect_err("an error");
    assert!(again.contains("connect_app"), "{again}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_area_fetch_is_reported_to_the_tool() {
    let app = bridge_app(FakeHttp::new());
    let base = serve(&app).await;
    let browser = Browser::open(&base).await;
    let mcp = ObservationMcp::new();
    connect(&mcp, &base, &browser.code).await.unwrap();
    // A box the server refuses (far below its smallest side), so nothing goes upstream.
    let bbox = BBox::new(47.0, -122.0, 47.00001, -121.99999).unwrap();
    let scene = osm_scene(bbox);
    let args = json!({ "scene": scene }).as_object().cloned();
    let e = mcp
        .in_tool_scope(
            args.as_ref(),
            mcp.compute_coverage(Parameters(SceneArg { scene })),
        )
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("could not send the area"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn places_and_queries_work_on_the_users_real_area() {
    use crate::places::{AddTargetArgs, AreaEdit, LabelAreaArgs, LabelArg, PlaceArg};
    use crate::queries::{CameraSplitArgs, FindGapsArgs, TargetVerdictsArgs, WhoSeesArgs};

    let bbox = BBox::parse(observation_mapdata::demo::BBOX_TXT.trim()).unwrap();
    let scene = osm_scene(bbox);
    let mcp = ObservationMcp::new();
    let area = AreaEdit {
        points: Some(vec![[-6.0, 6.0], [7.0, 6.0], [7.0, 14.0], [-6.0, 14.0]]),
        label: Some(LabelArg::Own),
        ..AreaEdit::default()
    };
    // Offline, a real area is refused.
    let offline = mcp
        .label_area(Parameters(LabelAreaArgs {
            scene: scene.clone(),
            area: area.clone(),
        }))
        .map(drop)
        .expect_err("an error");
    assert!(offline.contains("connect_app"), "{offline}");

    let overpass = FakeHttp::new();
    overpass.on(
        http::Method::POST,
        observation_server::config::DEFAULT_OVERPASS_URL,
        FakeReply::ok(observation_mapdata::demo::OVERPASS_JSON),
    );
    let app = bridge_app(overpass);
    let base = serve(&app).await;
    let browser = Browser::open(&base).await;
    connect(&mcp, &base, &browser.code).await.unwrap();
    let args = |scene: &Value| json!({ "scene": scene }).as_object().cloned();

    let placed = mcp
        .in_tool_scope(args(&scene).as_ref(), async {
            mcp.add_example_cameras(Parameters(ExampleArgs {
                scene: scene.clone(),
                count: 3,
                model_ids: Vec::new(),
            }))
        })
        .await
        .unwrap()
        .0
        .scene;
    let labelled = mcp
        .in_tool_scope(args(&placed).as_ref(), async {
            mcp.label_area(Parameters(LabelAreaArgs {
                scene: placed.clone(),
                area,
            }))
        })
        .await
        .unwrap()
        .0
        .scene;
    let marked = mcp
        .in_tool_scope(args(&labelled).as_ref(), async {
            mcp.add_target(Parameters(AddTargetArgs {
                scene: labelled.clone(),
                name: "Front door".into(),
                at: PlaceArg::Wall {
                    x: 0.0,
                    z: 7.0,
                    height_m: None,
                    normal: None,
                    facing_camera: None,
                    facing_street: false,
                },
            }))
        })
        .await
        .unwrap()
        .0;
    assert!(marked.wall.and_then(|w| w.building).is_some());
    let scene = marked.scene;

    let Json(verdicts) = mcp
        .in_tool_scope(
            args(&scene).as_ref(),
            mcp.target_verdicts(Parameters(TargetVerdictsArgs {
                scene: scene.clone(),
                target_id: None,
            })),
        )
        .await
        .unwrap();
    assert_eq!(verdicts.targets[0].cameras.len(), 3);
    assert!(verdicts.sources.osm_data_as_of.is_some());
    let Json(split) = mcp
        .in_tool_scope(
            args(&scene).as_ref(),
            mcp.camera_split(Parameters(CameraSplitArgs {
                scene: scene.clone(),
                camera_id: None,
                dori: None,
                lighting: None,
            })),
        )
        .await
        .unwrap();
    assert_eq!(split.cameras.len(), 3);
    assert_eq!(split.cameras[0].areas.len(), 1);
    let Json(point) = mcp
        .in_tool_scope(
            args(&scene).as_ref(),
            mcp.who_sees_point(Parameters(WhoSeesArgs {
                scene: scene.clone(),
                x: 0.0,
                z: 10.0,
                height_m: None,
            })),
        )
        .await
        .unwrap();
    assert_eq!(point.cameras.len(), 3);
    let Json(gaps) = mcp
        .in_tool_scope(
            args(&scene).as_ref(),
            mcp.find_gaps(Parameters(FindGapsArgs {
                scene: scene.clone(),
                dori: None,
                lighting: None,
                max_gaps: None,
                min_area_m2: None,
            })),
        )
        .await
        .unwrap();
    assert!(gaps.street_m2 > 1000.0 && !gaps.gaps.is_empty());
    assert!(mcp.link().areas().contains(&bbox));
}

impl Browser {
    /// Posts the user's decision on proposal `id`.
    async fn decide(&self, id: u64, decision: &str, reason: Option<&str>) {
        let body = json!({"v": 1, "id": id, "decision": decision, "reason": reason}).to_string();
        let req = HttpRequest::post(format!("{}{}", self.base, paths::DECISION), 1 << 16)
            .bearer(&self.token)
            .unwrap()
            .json(body.into_bytes());
        assert_eq!(self.http.send(req).await.unwrap().status, 204);
    }
}

async fn propose(
    mcp: &ObservationMcp,
    scene: &Value,
    summary: &str,
) -> Result<crate::propose::Proposed, String> {
    mcp.propose_changes(Parameters(crate::propose::ProposeArgs {
        scene: scene.clone(),
        summary: summary.to_owned(),
    }))
    .await
    .map(|Json(out)| out)
}

async fn proposal(mcp: &ObservationMcp, id: u64) -> crate::propose::ProposalStatusOut {
    mcp.get_proposal(Parameters(crate::propose::ProposalIdArgs { id }))
        .await
        .map(|Json(out)| out)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_proposes_and_hears_the_users_decision() {
    let app = bridge_app(FakeHttp::new());
    let base = serve(&app).await;
    let browser = Browser::open(&base).await;
    let mcp = ObservationMcp::new();

    // Without a connection there is nobody to propose to.
    let e = propose(&mcp, &demo_scene(), "x")
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("connect_app"), "{e}");

    connect(&mcp, &base, &browser.code).await.unwrap();
    let Json(placed) = mcp
        .add_example_cameras(Parameters(ExampleArgs {
            scene: demo_scene(),
            count: 2,
            model_ids: Vec::new(),
        }))
        .unwrap();
    let shared = placed.scene;
    let mut changes = mcp.link().subscribe();
    browser
        .share(json!({"scene": shared, "area": {"kind": "demo"}}))
        .await;
    next_change(&mut changes).await;

    // The app's own scene changes nothing.
    let e = propose(&mcp, &shared, "Nothing")
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("nothing to propose"), "{e}");
    // Another area cannot be proposed.
    let elsewhere = osm_scene(BBox::new(47.0, -122.0, 47.001, -121.999).unwrap());
    let e = propose(&mcp, &elsewhere, "Move")
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("another area"), "{e}");

    // Turn the first camera and remove the second.
    let ids = placed.camera_ids.unwrap();
    let Json(aimed) = mcp
        .aim_camera(Parameters(AimArgs {
            scene: shared.clone(),
            id: ids[0],
            yaw_deg: Some(90.0),
            pitch_deg: None,
            roll_deg: None,
        }))
        .unwrap();
    let Json(edited) = mcp
        .remove_camera(Parameters(CameraArgs {
            scene: aimed.scene,
            id: ids[1],
        }))
        .unwrap();
    let out = propose(&mcp, &edited.scene, "  Face the first camera east\n")
        .await
        .unwrap();
    assert_eq!((out.id, out.status.as_str()), (1, "pending"));
    assert_eq!(out.summary, "Face the first camera east");
    assert_eq!(out.changes.len(), 2, "{:?}", out.changes);
    assert!(out.changes[0].contains("aim at 90°"), "{:?}", out.changes);
    assert!(
        out.changes[1].starts_with("Remove Camera"),
        "{:?}",
        out.changes
    );
    assert!(out.note.contains("Nothing has changed yet"));
    assert_eq!(proposal(&mcp, 1).await.status, "pending");
    let Json(state) = mcp.get_app_state().await.unwrap();
    assert_eq!(state.proposals.len(), 1);
    // The server holds it for the browser, with the base values.
    let pending = app.state.agent().unwrap().len();
    assert_eq!(pending, 1);

    // The user rejects it; the follower hears without being asked.
    let mut changes = mcp.link().subscribe();
    browser.decide(1, "rejected", Some("keep the second")).await;
    next_change(&mut changes).await;
    let rejected = proposal(&mcp, 1).await;
    assert_eq!(rejected.status, "rejected");
    assert_eq!(rejected.reason.as_deref(), Some("keep the second"));
    assert!(rejected.meaning.contains("nothing changed"));

    // A newer proposal supersedes a pending one; a pending one can be withdrawn.
    assert_eq!(propose(&mcp, &edited.scene, "Again").await.unwrap().id, 2);
    assert_eq!(
        propose(&mcp, &edited.scene, "Once more").await.unwrap().id,
        3
    );
    assert_eq!(proposal(&mcp, 2).await.status, "superseded");
    let Json(withdrawn) = mcp
        .withdraw_proposal(Parameters(crate::propose::ProposalIdArgs { id: 3 }))
        .await
        .unwrap();
    assert_eq!(withdrawn.status, "withdrawn");
    // Withdrawing a decided one is refused with its status.
    let e = mcp
        .withdraw_proposal(Parameters(crate::propose::ProposalIdArgs { id: 1 }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("rejected"), "{e}");
    let e = mcp
        .get_proposal(Parameters(crate::propose::ProposalIdArgs { id: 9 }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("no proposal 9"), "{e}");

    // Accepted, as the app reports it.
    assert_eq!(propose(&mcp, &edited.scene, "Last").await.unwrap().id, 4);
    browser.decide(4, "accepted", None).await;
    let mut accepted = proposal(&mcp, 4).await;
    for _ in 0..50 {
        if accepted.status == "accepted" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        accepted = proposal(&mcp, 4).await;
    }
    assert_eq!(accepted.status, "accepted");
    assert!(accepted.meaning.contains("one undo step"));
}
