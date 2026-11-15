//! The view tools against the real server's agent bridge, served on
//! loopback in process (`observation-server`'s `TestApp`), with the
//! browser's side played over the same socket: its event stream read over
//! a plain TCP connection.

use std::net::SocketAddr;
use std::time::Duration;

use super::*;
use crate::app::ConnectArgs;
use observation_agent_api::{PairingCreated, paths};
use observation_server::testing::TestApp;
use observation_upstream::testing::{FakeHttp, FakeReply};
use observation_upstream::{GuardedHttp, HttpClient, HttpRequest, Redirects};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const GEOCODER: &str = "https://nominatim.example/search";

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

fn bridge_app(geocoder: Option<FakeHttp>) -> TestApp {
    let on = geocoder.is_some();
    let mut b = TestApp::builder();
    if let Some(g) = geocoder {
        b = b.geocoder(g);
    }
    b.config(move |c| {
        c.agent_bridge = true;
        c.dem_url = None;
        if on {
            c.geocoder.url = Some(GEOCODER.into());
        }
    })
    .build()
}

/// The browser's side: a pairing, and its event stream once opened.
struct Browser {
    base: String,
    token: String,
    code: String,
    events: Option<TcpStream>,
    seen: String,
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
            base: base.to_owned(),
            token: created.browser_token,
            code: created.code,
            events: None,
            seen: String::new(),
        }
    }

    /// Opens the event stream, as the app does, and waits for its first event.
    async fn listen(&mut self) {
        let addr = self.base.trim_start_matches("http://");
        let mut sock = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET {}?{}={} HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n\r\n",
            paths::EVENTS,
            paths::TOKEN_PARAM,
            self.token
        );
        sock.write_all(req.as_bytes()).await.unwrap();
        self.events = Some(sock);
        self.until("\"type\":\"status\"").await;
    }

    /// Reads the event stream until what was read since the last call holds
    /// `needle`; returns it.
    async fn until(&mut self, needle: &str) -> String {
        let sock = self.events.as_mut().expect("listening");
        let mut buf = [0u8; 4096];
        while !self.seen.contains(needle) {
            let n = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf))
                .await
                .expect("an event within 10 s")
                .unwrap();
            assert!(n > 0, "the stream ended: {}", self.seen);
            self.seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        std::mem::take(&mut self.seen)
    }

    async fn share(&self, state: Value) {
        let http = GuardedHttp::builder(true, Redirects::None)
            .no_proxy()
            .build()
            .unwrap();
        let body = json!({"v": 1, "state": state}).to_string();
        let req = HttpRequest::new(
            http::Method::PUT,
            format!("{}{}", self.base, paths::STATE),
            1 << 16,
        )
        .bearer(&self.token)
        .unwrap()
        .json(body.into_bytes());
        assert_eq!(http.send(req).await.unwrap().status, 200);
    }
}

async fn connect(mcp: &ObservationMcp, base: &str, code: &str) {
    mcp.connect_app(Parameters(ConnectArgs {
        url: base.to_owned(),
        code: code.to_owned(),
    }))
    .await
    .map(drop)
    .unwrap();
}

/// A demo scene with two example cameras, a target and an area, and the
/// cameras' ids.
fn scene() -> (Value, Vec<u32>) {
    let mut file = edit::new_demo_scene().unwrap();
    let cat = crate::catalog::catalog().unwrap();
    let ids = edit::add_example_cameras(&mut file, cat, 2, &[]).unwrap();
    let mut scene = edit::finish(&file).unwrap().1;
    scene["targets"] = json!([{"id": 1, "name": "Front door",
        "at": {"kind": "ground", "points": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]}}]);
    scene["areas"] = json!([{"id": 2, "name": "Our garden",
        "points": [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]}]);
    (scene, ids)
}

#[tokio::test(flavor = "multi_thread")]
async fn view_commands_need_a_connection() {
    let mcp = ObservationMcp::new();
    let e = mcp
        .focus_view(Parameters(FocusArgs {
            at: Some([0.0, 0.0]),
            camera: None,
            distance_m: None,
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("connect_app"), "{e}");
    let e = mcp
        .search_places(Parameters(SearchArgs {
            query: "High Street".into(),
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("connect_app"), "{e}");
    let e = mcp
        .request_load_area(Parameters(LoadAreaArgs {
            bbox: None,
            center: Some([51.5, -0.12]),
            radius_m: None,
            reason: "r".into(),
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("connect_app"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_view_tools_reach_the_app_as_commands() {
    let app = bridge_app(None);
    let base = serve(&app).await;
    let mut browser = Browser::open(&base).await;
    let mcp = ObservationMcp::new();
    connect(&mcp, &base, &browser.code).await;

    // The app is not listening yet: the agent is told to try again.
    let e = mcp
        .focus_view(Parameters(FocusArgs {
            at: Some([1.0, 2.0]),
            camera: None,
            distance_m: None,
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("not listening"), "{e}");

    browser.listen().await;
    let (scene, ids) = scene();
    browser
        .share(json!({"scene": scene, "area": {"kind": "demo"}}))
        .await;

    // Focus on a camera of the shared scene; the distance is kept in range.
    let Json(done) = mcp
        .focus_view(Parameters(FocusArgs {
            at: None,
            camera: Some(ids[0]),
            distance_m: Some(3.0),
        }))
        .await
        .unwrap();
    assert_eq!(done.command_id, 1, "the refused one took no id");
    assert!(done.summary.contains("from 8 m away"), "{}", done.summary);
    assert!(done.summary.contains("kept to 8 m"), "{}", done.summary);
    let seen = browser.until("\"type\":\"command\"").await;
    assert!(seen.contains(r#""kind":"focus_view""#), "{seen}");
    assert!(seen.contains(r#""distance_m":8.0"#), "{seen}");

    // Selecting checks the ids against the shared scene.
    let e = mcp
        .select_cameras(Parameters(SelectArgs { ids: vec![9999] }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("no camera 9999"), "{e}");
    assert!(e.contains(&ids[0].to_string()), "{e}");
    let Json(done) = mcp
        .select_cameras(Parameters(SelectArgs {
            ids: vec![ids[1], ids[0]],
        }))
        .await
        .unwrap();
    assert!(
        done.summary.contains("only the first is selected"),
        "{}",
        done.summary
    );
    let seen = browser.until("select_cameras").await;
    assert!(
        seen.contains(&format!(r#""ids":[{},{}]"#, ids[1], ids[0])),
        "{seen}"
    );

    // A target by name, with a note; a wrong one lists what there is.
    let Json(done) = mcp
        .highlight(Parameters(HighlightArgs {
            camera: None,
            target: Some(IdOrName::Name("front door".into())),
            area: None,
            point: None,
            clear: false,
            note: Some("no camera sees it".into()),
        }))
        .await
        .unwrap();
    assert!(
        done.summary.contains("target 1 ‘Front door’"),
        "{}",
        done.summary
    );
    let seen = browser.until("highlight").await;
    assert!(
        seen.contains(r#""what":{"kind":"target","item":"front door"},"note":"no camera sees it""#),
        "{seen}"
    );
    let e = mcp
        .highlight(Parameters(HighlightArgs {
            camera: None,
            target: None,
            area: Some(IdOrName::Name("their drive".into())),
            point: None,
            clear: false,
            note: None,
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("2 ‘Our garden’"), "{e}");
    // Exactly one thing is marked.
    let e = mcp
        .highlight(Parameters(HighlightArgs {
            camera: Some(ids[0]),
            target: None,
            area: None,
            point: Some([0.0, 0.0]),
            clear: false,
            note: None,
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("exactly one"), "{e}");

    // View settings, and a refusal when nothing is given.
    let Json(done) = mcp
        .set_view_options(Parameters(ViewOptionsArgs {
            night: Some(true),
            detail_level: Some(Level::Recognise),
            overlay: None,
            cones: Some(false),
            walls: None,
        }))
        .await
        .unwrap();
    assert!(done.summary.contains("can undo"), "{}", done.summary);
    let seen = browser.until("set_view_options").await;
    assert!(
        seen.contains(r#""night":true,"detail_level":"recognise","cones":false"#),
        "{seen}"
    );
    let e = mcp
        .set_view_options(Parameters(ViewOptionsArgs {
            night: None,
            detail_level: None,
            overlay: None,
            cones: None,
            walls: None,
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("at least one"), "{e}");

    // A request to load an area: a town-sized bbox becomes the 500 m
    // around its centre, and the user decides.
    let Json(done) = mcp
        .request_load_area(Parameters(LoadAreaArgs {
            bbox: Some([51.0, -1.0, 51.1, -0.9]),
            center: None,
            radius_m: None,
            reason: "  to see\nyour street ".into(),
        }))
        .await
        .unwrap();
    assert!(done.summary.contains("unless they click Load"));
    assert!(done.summary.contains("500 m square"), "{}", done.summary);
    let b = done.bbox.unwrap();
    assert!((b[0] + b[2]) / 2.0 - 51.05 < 1e-9);
    let seen = browser.until("request_load_area").await;
    assert!(seen.contains(r#""reason":"to see your street""#), "{seen}");
    // A reason is needed, and is checked before anything is sent.
    let e = mcp
        .request_load_area(Parameters(LoadAreaArgs {
            bbox: None,
            center: Some([51.5, -0.12]),
            radius_m: Some(100.0),
            reason: "  ".into(),
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("say why"), "{e}");

    // Commands never change the app's state.
    let Json(state) = mcp.get_app_state().await.unwrap();
    assert_eq!(state.revision, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn places_are_found_through_the_apps_server() {
    let geocoder = FakeHttp::from_fn(|_| {
        FakeReply::ok(
            r#"[{"place_id": 1, "display_name": "High Street, London", "lat": "51.5007",
                 "lon": "-0.1246", "boundingbox": ["51.50", "51.51", "-0.13", "-0.12"]}]"#,
        )
        .header(http::header::CONTENT_TYPE, "application/json")
    });
    let app = bridge_app(Some(geocoder.clone()));
    let base = serve(&app).await;
    let browser = Browser::open(&base).await;
    let mcp = ObservationMcp::new();
    connect(&mcp, &base, &browser.code).await;
    let Json(found) = mcp
        .search_places(Parameters(SearchArgs {
            query: " High Street, London ".into(),
        }))
        .await
        .unwrap();
    assert_eq!(found.places.len(), 1);
    let place = &found.places[0];
    assert_eq!(place.name, "High Street, London");
    assert_eq!(place.center, [51.5007, -0.1246]);
    assert_eq!(place.bbox, Some([51.50, -0.13, 51.51, -0.12]));
    assert!(!found.credit.is_empty());
    assert!(
        found.summary.contains("request_load_area"),
        "{}",
        found.summary
    );
    // The query went out as one parameter, encoded.
    let sent = geocoder.requests();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].url.as_str().contains("q=high+street%2C+london"),
        "{}",
        sent[0].url
    );

    // A server without the search says so in words the agent can act on.
    let plain = bridge_app(None);
    let base = serve(&plain).await;
    let browser = Browser::open(&base).await;
    connect(&mcp, &base, &browser.code).await;
    let e = mcp
        .search_places(Parameters(SearchArgs {
            query: "High Street".into(),
        }))
        .await
        .map(drop)
        .unwrap_err();
    assert!(e.contains("not turned on"), "{e}");
    assert!(e.contains("bbox"), "{e}");
}

#[test]
fn the_view_tools_are_listed() {
    let names: Vec<String> = ObservationMcp::all_tools()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for n in [
        "focus_view",
        "select_cameras",
        "highlight",
        "set_view_options",
        "request_load_area",
        "search_places",
    ] {
        assert!(names.iter().any(|x| x == n), "{n} in {names:?}");
    }
}
