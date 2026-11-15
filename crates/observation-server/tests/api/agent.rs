//! The agent bridge (`/api/agent/...`) through the whole router: off by
//! default, loopback only, the pairing lifecycle over HTTP, the event
//! stream, the long poll and the body limits.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use observation_agent_api::{
    COMMAND_BURST, MAX_COMMAND_BYTES, MAX_FAILED_CLAIMS, MAX_STATE_BYTES, PairingCreated,
    StateAccepted, StateView, paths,
};
use observation_server::testing::TestApp;
use serde_json::{Value, json};

use crate::harness::{json, open, send};

const HOME: &str = "127.0.0.1";

fn bridge_app() -> TestApp {
    TestApp::builder().config(|c| c.agent_bridge = true).build()
}

/// A request to the bridge from `peer`, addressed to `host`.
fn request(
    method: Method,
    uri: &str,
    peer: Option<&str>,
    host: &str,
    bearer: Option<&str>,
    body: Option<String>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, host);
    if let Some(t) = bearer {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = b.body(body.map_or_else(Body::empty, Body::from)).unwrap();
    if let Some(peer) = peer {
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(SocketAddr::new(
                peer.parse().unwrap(),
                40000,
            )));
    }
    req
}

/// A request from this computer to it.
fn local(method: Method, uri: &str, bearer: Option<&str>, body: Option<Value>) -> Request<Body> {
    request(
        method,
        uri,
        Some(HOME),
        "127.0.0.1:8080",
        bearer,
        body.map(|b| b.to_string()),
    )
}

async fn create(router: &Router) -> PairingCreated {
    let (status, headers, body) = send(
        router,
        local(Method::POST, paths::PAIRINGS, None, Some(json!({"v": 1}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    serde_json::from_slice(&body).unwrap()
}

async fn claim(router: &Router, code: &str) -> (StatusCode, Value) {
    let (status, _, body) = send(
        router,
        local(
            Method::POST,
            paths::CLAIM,
            None,
            Some(json!({"v": 1, "code": code, "agent": "test agent"})),
        ),
    )
    .await;
    (status, json(&body))
}

async fn put_state(router: &Router, token: &str, state: Value) -> (StatusCode, Value) {
    let (status, _, body) = send(
        router,
        local(
            Method::PUT,
            paths::STATE,
            Some(token),
            Some(json!({"v": 1, "state": state})),
        ),
    )
    .await;
    (status, json(&body))
}

async fn get_state(router: &Router, token: &str, since: Option<u64>) -> (StatusCode, Value) {
    let uri = match since {
        Some(s) => format!("{}?since={s}", paths::STATE),
        None => paths::STATE.to_owned(),
    };
    let (status, _, body) = send(router, local(Method::GET, &uri, Some(token), None)).await;
    (status, json(&body))
}

fn events_uri(token: &str) -> String {
    format!("{}?token={token}", paths::EVENTS)
}

#[tokio::test(start_paused = true)]
async fn the_bridge_is_off_by_default() {
    let app = TestApp::builder().build();
    let (status, _, body) = send(
        &app.router,
        local(Method::POST, paths::PAIRINGS, None, Some(json!({"v": 1}))),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&body)["error"], "no such API address on this server");
    let (_, _, body) = send(&app.router, local(Method::GET, "/api/config", None, None)).await;
    assert_eq!(json(&body)["agent_bridge"], false);
}

#[tokio::test(start_paused = true)]
async fn only_this_computer_reaches_the_bridge() {
    let app = bridge_app();
    let pairing = |peer: Option<&str>, host: &str| {
        request(
            Method::POST,
            paths::PAIRINGS,
            peer,
            host,
            None,
            Some(r#"{"v":1}"#.into()),
        )
    };
    // Another computer, or no peer address at all: as if there were no bridge.
    for peer in [Some("192.168.1.20"), Some("::ffff:10.0.0.2"), None] {
        let (status, _, body) = send(&app.router, pairing(peer, "127.0.0.1:8080")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{peer:?}");
        assert_eq!(json(&body)["error"], "no such API address on this server");
    }
    // A forwarded address is never believed.
    let mut forwarded = pairing(Some("192.168.1.20"), "127.0.0.1:8080");
    forwarded
        .headers_mut()
        .insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
    assert_eq!(send(&app.router, forwarded).await.0, StatusCode::NOT_FOUND);
    // This computer, but addressed by another name (DNS rebinding).
    let (status, _, _) = send(&app.router, pairing(Some(HOME), "evil.example:8080")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // IPv6 loopback and `localhost` are this computer.
    for (peer, host) in [
        ("::1", "[::1]:8080"),
        ("::ffff:127.0.0.1", "localhost:8080"),
    ] {
        let (status, _, _) = send(&app.router, pairing(Some(peer), host)).await;
        assert_eq!(status, StatusCode::CREATED, "{peer} {host}");
    }
    // `/api/config` offers the bridge to this computer only.
    let config =
        |peer: &str, host: &str| request(Method::GET, "/api/config", Some(peer), host, None, None);
    let offered = |body: &[u8]| json(body)["agent_bridge"].as_bool().unwrap();
    let (_, _, body) = send(&app.router, config(HOME, "localhost:8080")).await;
    assert!(offered(&body));
    let (_, _, body) = send(&app.router, config("192.168.1.20", "192.168.1.2:8080")).await;
    assert!(!offered(&body));
    let (_, _, body) = send(&app.router, config(HOME, "my-machine:8080")).await;
    assert!(!offered(&body));
}

#[tokio::test(start_paused = true)]
async fn a_pairing_from_code_to_state_to_disconnect() {
    let app = bridge_app();
    let created = create(&app.router).await;
    assert_eq!(created.code.len(), 8);
    let (status, _, mut stream) = open(
        &app.router,
        local(Method::GET, &events_uri(&created.browser_token), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first = stream.until("\n\n").await;
    assert!(first.contains("event: agent"), "{first}");
    assert!(
        first.contains(r#"data: {"v":1,"type":"status","phase":"waiting"}"#),
        "{first}"
    );

    // The code as a person might type it.
    let typed = format!(
        "{}-{}",
        &created.code[..4],
        created.code[4..].to_lowercase()
    );
    let (status, claimed) = claim(&app.router, &typed).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let agent = claimed["agent_token"].as_str().unwrap().to_owned();
    assert_eq!(claimed["long_poll_s"], 25);
    let seen = stream.until("claimed").await;
    assert!(
        seen.contains(r#"{"v":1,"type":"status","phase":"claimed","agent":"test agent"}"#),
        "{seen}"
    );

    // Nothing posted yet.
    let (status, view) = get_state(&app.router, &agent, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view, json!({"v": 1, "revision": 0}));

    let scene = json!({"format": "observation-scene", "version": 2});
    let (status, accepted) = put_state(
        &app.router,
        &created.browser_token,
        json!({"scene": scene, "area": {"kind": "demo"}, "selection": [2],
               "pinned": {"x": 1.0, "z": -3.5, "seq": 1}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    let accepted: StateAccepted = serde_json::from_value(accepted).unwrap();
    assert_eq!(accepted.revision, 1);

    let (_, view) = get_state(&app.router, &agent, Some(0)).await;
    let view: StateView = serde_json::from_value(view).unwrap();
    assert_eq!(view.revision, 1);
    let state = view.state.unwrap();
    assert_eq!(state.scene, scene);
    assert_eq!(state.selection, [2]);
    assert_eq!(state.pinned.map(|p| (p.x, p.z)), Some((1.0, -3.5)));

    // A long poll waits for the next post.
    let poll = {
        let (router, agent) = (app.router.clone(), agent.clone());
        tokio::spawn(async move { get_state(&router, &agent, Some(1)).await })
    };
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!poll.is_finished());
    put_state(&app.router, &created.browser_token, json!({"scene": {}})).await;
    let (_, view) = poll.await.unwrap();
    assert_eq!(view["revision"], 2);

    // With nothing new it answers when its time is up, with the same revision.
    let started = tokio::time::Instant::now();
    let (status, view) = get_state(&app.router, &agent, Some(2)).await;
    assert_eq!(
        (status, view["revision"].as_u64()),
        (StatusCode::OK, Some(2))
    );
    assert_eq!(started.elapsed(), Duration::from_secs(25));

    // The agent disconnects: the browser hears why, and the stream closes.
    let (status, _, _) = send(
        &app.router,
        local(Method::DELETE, paths::PAIRING, Some(&agent), None),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let rest = String::from_utf8(stream.rest().await).unwrap();
    assert!(
        rest.contains(r#"data: {"v":1,"type":"ended","reason":"agent"}"#),
        "{rest}"
    );
    // Both tokens are dead.
    assert_eq!(
        get_state(&app.router, &agent, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        put_state(&app.router, &created.browser_token, json!({"scene": {}}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test(start_paused = true)]
async fn the_browser_ends_it_and_a_waiting_poll_hears() {
    let app = bridge_app();
    let created = create(&app.router).await;
    let (_, _, _stream) = open(
        &app.router,
        local(Method::GET, &events_uri(&created.browser_token), None, None),
    )
    .await;
    let (_, claimed) = claim(&app.router, &created.code).await;
    let agent = claimed["agent_token"].as_str().unwrap().to_owned();
    let poll = {
        let (router, agent) = (app.router.clone(), agent.clone());
        tokio::spawn(async move { get_state(&router, &agent, Some(0)).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, _, _) = send(
        &app.router,
        local(
            Method::DELETE,
            paths::PAIRING,
            Some(&created.browser_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, view) = poll.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["ended"], "browser");
}

#[tokio::test(start_paused = true)]
async fn wrong_codes_burn_the_code_and_the_browser_is_told() {
    let app = bridge_app();
    let created = create(&app.router).await;
    let (_, _, mut stream) = open(
        &app.router,
        local(Method::GET, &events_uri(&created.browser_token), None, None),
    )
    .await;
    stream.until("waiting").await;
    for attempt in 0..MAX_FAILED_CLAIMS {
        let wrong = if created.code == "00000000" {
            "11111111"
        } else {
            "00000000"
        };
        // Some attempts are not codes at all; they count the same.
        let code = if attempt % 2 == 0 { wrong } else { "nonsense!" };
        let (status, body) = claim(&app.router, code).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }
    let rest = String::from_utf8(stream.rest().await).unwrap();
    assert!(rest.contains(r#""reason":"code_burnt""#), "{rest}");
    assert_eq!(
        claim(&app.router, &created.code).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test(start_paused = true)]
async fn requests_are_checked_before_they_are_read() {
    let app = bridge_app();
    let created = create(&app.router).await;
    let token = created.browser_token.as_str();
    // Not JSON by its content type (as a page on another site would send).
    let mut plain = local(Method::POST, paths::PAIRINGS, None, Some(json!({"v": 1})));
    plain
        .headers_mut()
        .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    assert_eq!(send(&app.router, plain).await.0, StatusCode::BAD_REQUEST);
    // Another protocol version.
    let (status, _, body) = send(
        &app.router,
        local(Method::POST, paths::PAIRINGS, None, Some(json!({"v": 2}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json(&body)["error"]
            .as_str()
            .unwrap()
            .contains("version 1, not 2")
    );
    // A scene that is not an object.
    assert_eq!(
        put_state(&app.router, token, json!({"scene": "x"})).await.0,
        StatusCode::BAD_REQUEST
    );
    // Missing, wrong and other side's tokens.
    for bearer in [None, Some("wrong")] {
        let req = local(
            Method::PUT,
            paths::STATE,
            bearer,
            Some(json!({"v": 1, "state": {"scene": {}}})),
        );
        assert_eq!(send(&app.router, req).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        get_state(&app.router, token, None).await.0,
        StatusCode::UNAUTHORIZED,
        "the browser's token does not read the state"
    );
    let (status, _, _) = send(
        &app.router,
        local(Method::GET, &events_uri("wrong"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(start_paused = true)]
async fn the_bridge_takes_a_large_scene_and_the_rest_keeps_its_limit() {
    let app = bridge_app();
    let created = create(&app.router).await;
    let token = created.browser_token.as_str();
    // About 100 KiB: what 256 cameras take, far over the server's 16 KiB.
    let cameras: Vec<Value> = (0..256)
        .map(|i| json!({"id": i, "label": "x".repeat(300)}))
        .collect();
    let big = json!({"scene": {"cameras": cameras}});
    assert!(big.to_string().len() > 64 * 1024);
    let (status, body) = put_state(&app.router, token, big).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Over the bridge's own limit.
    let huge = json!({"scene": {"pad": "x".repeat(MAX_STATE_BYTES + 8192)}});
    let req = local(
        Method::PUT,
        paths::STATE,
        Some(token),
        Some(json!({"v": 1, "state": huge})),
    );
    assert_eq!(
        send(&app.router, req).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // Everywhere else the limit is still 16 KiB.
    let sizes = Request::builder()
        .method(Method::POST)
        .uri("/api/regions/catalog/sizes")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(vec![b' '; 20 * 1024]))
        .unwrap();
    assert_eq!(
        send(&app.router, sizes).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test(start_paused = true)]
async fn shutting_down_ends_pairings_and_their_streams() {
    let app = bridge_app();
    let created = create(&app.router).await;
    let (_, _, mut stream) = open(
        &app.router,
        local(Method::GET, &events_uri(&created.browser_token), None, None),
    )
    .await;
    stream.until("waiting").await;
    app.state.begin_shutdown();
    let rest = String::from_utf8(stream.rest().await).unwrap();
    assert!(rest.contains(r#""reason":"shutdown""#), "{rest}");
    assert!(app.state.agent().unwrap().is_empty());
}

/// A claimed pairing: `(browser token, agent token)`.
async fn paired(router: &Router) -> (String, String) {
    let created = create(router).await;
    let (_, claimed) = claim(router, &created.code).await;
    let agent = claimed["agent_token"].as_str().unwrap().to_owned();
    (created.browser_token, agent)
}

fn proposal_body(summary: &str, new: u32) -> Value {
    json!({
        "v": 1,
        "summary": summary,
        "base_revision": 0,
        "changes": {"changes": [{"item": "limit_figures_to", "new": new}]}
    })
}

async fn propose(router: &Router, token: &str, body: Value) -> (StatusCode, Value) {
    let (status, _, body) = send(
        router,
        local(Method::POST, paths::PROPOSAL, Some(token), Some(body)),
    )
    .await;
    (status, json(&body))
}

async fn decide(router: &Router, token: &str, body: Value) -> StatusCode {
    send(
        router,
        local(Method::POST, paths::DECISION, Some(token), Some(body)),
    )
    .await
    .0
}

fn withdraw_req(agent: &str, id: Option<u64>) -> Request<Body> {
    let uri = match id {
        Some(id) => format!("{}?id={id}", paths::PROPOSAL),
        None => paths::PROPOSAL.to_owned(),
    };
    local(Method::DELETE, &uri, Some(agent), None)
}

#[tokio::test(start_paused = true)]
async fn a_proposal_reaches_the_browser_and_its_decision_the_agent() {
    let app = bridge_app();
    let (browser, agent) = paired(&app.router).await;
    let (_, _, mut stream) = open(
        &app.router,
        local(Method::GET, &events_uri(&browser), None, None),
    )
    .await;
    stream.until("claimed").await;

    // A long poll is waiting when the proposal arrives.
    let poll = {
        let (router, agent) = (app.router.clone(), agent.clone());
        tokio::spawn(async move { get_state(&router, &agent, Some(0)).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, submitted) = propose(
        &app.router,
        &agent,
        proposal_body("  Limit\nthe figures ", 1),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{submitted}");
    assert_eq!(submitted, json!({"v": 1, "id": 1}));
    let seen = stream.until("\"type\":\"proposal\"").await;
    assert!(
        seen.contains(
            r#""proposal":{"id":1,"summary":"Limit the figures","base_revision":0,"changes":{"changes":[{"item":"limit_figures_to","new":1}]}}"#
        ),
        "{seen}"
    );
    let (_, view) = poll.await.unwrap();
    assert_eq!(view["revision"], 1);
    assert_eq!(
        view["proposals"],
        json!([{"id": 1, "summary": "Limit the figures", "status": "pending"}])
    );

    // A stream opened while it is pending gets it at once.
    let (_, _, mut late) = open(
        &app.router,
        local(Method::GET, &events_uri(&browser), None, None),
    )
    .await;
    let replay = late.until("\"type\":\"proposal\"").await;
    assert!(replay.contains(r#""id":1"#), "{replay}");

    // The user rejects it, with a reason.
    let status = decide(
        &app.router,
        &browser,
        json!({"v": 1, "id": 1, "decision": "rejected", "reason": "  not\non the hedge "}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, view) = get_state(&app.router, &agent, Some(1)).await;
    assert_eq!(
        view["proposals"],
        json!([{"id": 1, "summary": "Limit the figures", "status": "rejected",
                "reason": "not on the hedge"}])
    );
    // Decided once only.
    let again = json!({"v": 1, "id": 1, "decision": "accepted"});
    assert_eq!(
        decide(&app.router, &browser, again).await,
        StatusCode::CONFLICT
    );

    // A withdrawn proposal is announced to the browser.
    let (_, submitted) = propose(&app.router, &agent, proposal_body("second", 2)).await;
    assert_eq!(submitted["id"], 2);
    stream.until(r#""id":2"#).await;
    let (status, _, _) = send(&app.router, withdraw_req(&agent, Some(2))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let seen = stream.until("proposal_withdrawn").await;
    assert!(
        seen.contains(r#"{"v":1,"type":"proposal_withdrawn","id":2}"#),
        "{seen}"
    );
    // Withdrawing a decided proposal is refused, withdrawing twice is not,
    // and an unknown one is not found.
    let status = |id| {
        let (router, agent) = (app.router.clone(), agent.clone());
        async move { send(&router, withdraw_req(&agent, Some(id))).await.0 }
    };
    assert_eq!(status(1).await, StatusCode::CONFLICT);
    assert_eq!(status(2).await, StatusCode::NO_CONTENT);
    assert_eq!(status(9).await, StatusCode::NOT_FOUND);
}

#[tokio::test(start_paused = true)]
async fn proposals_and_decisions_are_checked() {
    let app = bridge_app();
    let (browser, agent) = paired(&app.router).await;
    // Only the agent proposes, only the browser decides.
    assert_eq!(
        propose(&app.router, &browser, proposal_body("x", 1))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, submitted) = propose(&app.router, &agent, proposal_body("x", 1)).await;
    assert_eq!(submitted["id"], 1);
    let decision = json!({"v": 1, "id": 1, "decision": "accepted"});
    assert_eq!(
        decide(&app.router, &agent, decision.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    // No summary, an empty change set, malformed ones.
    let (status, body) = propose(&app.router, &agent, proposal_body(" \n ", 1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("summary"),
        "{body}"
    );
    for changes in [
        json!({"changes": []}),
        json!({"changes": [{"item": "limit_figures_to", "base": 1, "new": 1}]}),
        json!({"changes": [{"item": "camera", "id": 3}]}),
        json!({"changes": [{"item": "unknown"}]}),
    ] {
        let body = json!({"v": 1, "summary": "s", "changes": changes});
        let (status, answer) = propose(&app.router, &agent, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    }
    // A decision for no such proposal, and a withdrawal without an id.
    let unknown = json!({"v": 1, "id": 9, "decision": "accepted"});
    assert_eq!(
        decide(&app.router, &browser, unknown).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&app.router, withdraw_req(&agent, None)).await.0,
        StatusCode::BAD_REQUEST
    );
    // From another computer the routes are not there; addressed by another
    // name they are refused.
    let away = request(
        Method::POST,
        paths::PROPOSAL,
        Some("192.168.1.20"),
        "127.0.0.1:8080",
        Some(&agent),
        Some(proposal_body("x", 1).to_string()),
    );
    assert_eq!(send(&app.router, away).await.0, StatusCode::NOT_FOUND);
    let renamed = request(
        Method::POST,
        paths::DECISION,
        Some(HOME),
        "evil.example",
        Some(&browser),
        Some(decision.to_string()),
    );
    assert_eq!(send(&app.router, renamed).await.0, StatusCode::FORBIDDEN);
    // The bridge's own size limit applies.
    let huge = json!({"v": 1, "summary": "x".repeat(MAX_STATE_BYTES + 8192),
                      "changes": {"changes": []}});
    let req = local(Method::POST, paths::PROPOSAL, Some(&agent), Some(huge));
    assert_eq!(
        send(&app.router, req).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

async fn command(router: &Router, token: &str, command: Value) -> (StatusCode, Value) {
    let (status, headers, body) = send(
        router,
        local(
            Method::POST,
            paths::COMMAND,
            Some(token),
            Some(json!({"v": 1, "command": command})),
        ),
    )
    .await;
    if status == StatusCode::ACCEPTED {
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    }
    (status, json(&body))
}

#[tokio::test(start_paused = true)]
async fn a_view_command_reaches_the_browser_as_an_event() {
    let app = bridge_app();
    let (browser, agent) = paired(&app.router).await;
    // With no stream open nothing would hear it.
    let focus = json!({"kind": "focus_view", "at": [12.5, -4.0], "distance_m": 40.0});
    let (status, body) = command(&app.router, &agent, focus.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("not listening"),
        "{body}"
    );

    let (_, _, mut stream) = open(
        &app.router,
        local(Method::GET, &events_uri(&browser), None, None),
    )
    .await;
    stream.until("claimed").await;
    let (status, sent) = command(&app.router, &agent, focus).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{sent}");
    assert_eq!(sent, json!({"v": 1, "id": 1}));
    let seen = stream.until("\"type\":\"command\"").await;
    assert!(
        seen.contains(
            r#"{"v":1,"type":"command","id":1,"command":{"kind":"focus_view","at":[12.5,-4.0],"distance_m":40.0}}"#
        ),
        "{seen}"
    );
    // The note is tidied on the way.
    let highlight = json!({"kind": "highlight", "what": {"kind": "camera", "id": 3},
                           "note": "  blind\nspot "});
    let (status, sent) = command(&app.router, &agent, highlight).await;
    assert_eq!(
        (status, sent["id"].as_u64()),
        (StatusCode::ACCEPTED, Some(2))
    );
    let seen = stream.until(r#""id":2"#).await;
    assert!(seen.contains(r#""note":"blind spot""#), "{seen}");
    // Commands are not state: the agent's revision has not moved.
    let (_, view) = get_state(&app.router, &agent, None).await;
    assert_eq!(view["revision"], 0);
    // A stream opened now does not get them again.
    let (_, _, mut late) = open(
        &app.router,
        local(Method::GET, &events_uri(&browser), None, None),
    )
    .await;
    let first = late.until("\n\n").await;
    assert!(!first.contains("command"), "{first}");
}

#[tokio::test(start_paused = true)]
async fn view_commands_are_checked_and_limited() {
    let app = bridge_app();
    let (browser, agent) = paired(&app.router).await;
    let (_, _, mut stream) = open(
        &app.router,
        local(Method::GET, &events_uri(&browser), None, None),
    )
    .await;
    stream.until("claimed").await;
    let select = json!({"kind": "select_cameras", "ids": [1]});
    // Only the agent sends them.
    assert_eq!(
        command(&app.router, &browser, select.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Unknown kinds, numbers out of range, an area the app cannot load, and
    // a request to load one without a reason.
    for bad in [
        json!({"kind": "fly_through"}),
        json!({"kind": "focus_view", "at": [99999.0, 0.0]}),
        json!({"kind": "focus_view", "at": [0.0, 0.0], "distance_m": 1.0}),
        json!({"kind": "set_view_options"}),
        json!({"kind": "set_view_options", "detail_level": "custom"}),
        json!({"kind": "highlight", "what": {"kind": "tree"}}),
        json!({"kind": "request_load_area", "reason": "r",
               "bbox": {"south": 51.0, "west": -1.0, "north": 52.0, "east": 0.0}}),
        json!({"kind": "request_load_area", "reason": " ",
               "bbox": {"south": 51.5, "west": -0.13, "north": 51.502, "east": -0.127}}),
    ] {
        let (status, body) = command(&app.router, &agent, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} {body}");
    }
    // The command's own size limit.
    let huge = json!({"v": 1, "command": {"kind": "highlight", "what": {"kind": "clear"},
                      "note": "x".repeat(MAX_COMMAND_BYTES)}});
    let req = local(Method::POST, paths::COMMAND, Some(&agent), Some(huge));
    assert_eq!(
        send(&app.router, req).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // From another computer the route is not there.
    let away = request(
        Method::POST,
        paths::COMMAND,
        Some("192.168.1.20"),
        "127.0.0.1:8080",
        Some(&agent),
        Some(json!({"v": 1, "command": select}).to_string()),
    );
    assert_eq!(send(&app.router, away).await.0, StatusCode::NOT_FOUND);
    // A burst of COMMAND_BURST, then 429 with when to try again.
    for _ in 0..COMMAND_BURST {
        assert_eq!(
            command(&app.router, &agent, select.clone()).await.0,
            StatusCode::ACCEPTED
        );
    }
    let (status, headers, _) = send(
        &app.router,
        local(
            Method::POST,
            paths::COMMAND,
            Some(&agent),
            Some(json!({"v": 1, "command": select})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers[header::RETRY_AFTER], "1");
}
