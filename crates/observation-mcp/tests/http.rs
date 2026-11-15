//! The HTTP transport end to end, in process: authentication, the
//! DNS-rebinding guards, the body limit, and MCP requests that reach the
//! tools.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use observation_mcp::http::config::{Auth, HttpConfig, Token};
use observation_mcp::http::{MAX_BODY_BYTES, router};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const LAPTOP: &str = "laptop-secret-laptop-secret-laptop-secret";
const CI: &str = "ci-secret-ci-secret-ci-secret-ci-secret-ci";

fn config(auth: Auth) -> HttpConfig {
    HttpConfig {
        listen: "127.0.0.1:0".parse().expect("address"),
        auth,
        allowed_hosts: vec!["localhost".into(), "mcp.example.com".into()],
        allowed_origins: vec!["https://app.example.com:443".into()],
    }
}

fn app() -> Router {
    router(
        &config(Auth::Tokens(vec![
            Token {
                name: "laptop".into(),
                secret: LAPTOP.into(),
            },
            Token {
                name: "ci".into(),
                secret: CI.into(),
            },
        ])),
        &CancellationToken::new(),
    )
}

fn post() -> axum::http::request::Builder {
    post_to("localhost:8090")
}

fn post_to(host: &str) -> axum::http::request::Builder {
    Request::post("/mcp")
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
}

fn rpc(id: u32, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn initialize() -> String {
    rpc(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}
        }),
    )
}

async fn send(app: Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app.oneshot(req).await.expect("infallible");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The JSON-RPC message in a response body, which is JSON or, if the server
/// chose to stream, a single SSE `data:` event.
fn message(body: &str) -> Value {
    if let Ok(v) = serde_json::from_str(body) {
        return v;
    }
    let data = body
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .find(|d| !d.is_empty())
        .unwrap_or_else(|| panic!("no message in {body:?}"));
    serde_json::from_str(data).expect("JSON-RPC message")
}

fn authed(token: &str, body: String) -> Request<Body> {
    post()
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body))
        .expect("request")
}

#[tokio::test]
async fn health_needs_no_token() {
    let req = Request::get("/healthz")
        .body(Body::empty())
        .expect("request");
    let (status, _, body) = send(app(), req).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
}

#[tokio::test]
async fn refuses_requests_without_a_valid_token() {
    let req = post().body(Body::from(initialize())).expect("request");
    let (status, headers, _) = send(app(), req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers[header::WWW_AUTHENTICATE],
        "Bearer realm=\"observation-mcp\""
    );

    let wrong = format!("{LAPTOP}x");
    for (scheme, token) in [
        ("Bearer", wrong.as_str()),
        ("Basic", LAPTOP),
        ("Bearer", ""),
    ] {
        let req = post()
            .header(header::AUTHORIZATION, format!("{scheme} {token}"))
            .body(Body::from(initialize()))
            .expect("request");
        let (status, _, _) = send(app(), req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{scheme} {token}");
    }

    let (status, headers, _) = send(app(), authed(&wrong, initialize())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers[header::WWW_AUTHENTICATE],
        "Bearer realm=\"observation-mcp\", error=\"invalid_token\""
    );
}

#[tokio::test]
async fn serves_mcp_to_each_token() {
    for token in [LAPTOP, CI] {
        let (status, _, body) = send(app(), authed(token, initialize())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let init = message(&body);
        assert_eq!(init["result"]["serverInfo"]["name"], "observation-mcp");
    }

    // Stateless: each request stands alone, with no session id to carry.
    let list = post()
        .header(header::AUTHORIZATION, format!("Bearer {LAPTOP}"))
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(rpc(2, "tools/list", json!({}))))
        .expect("request");
    let (status, headers, body) = send(app(), list).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!headers.contains_key("mcp-session-id"));
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    let tools = message(&body);
    assert!(
        tools["result"]["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "{tools}"
    );

    let call = post()
        .header(header::AUTHORIZATION, format!("Bearer {CI}"))
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(rpc(
            3,
            "tools/call",
            json!({"name": "load_demo_area", "arguments": {}}),
        )))
        .expect("request");
    let (status, _, body) = send(app(), call).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let result = &message(&body)["result"];
    assert_ne!(result["isError"], true, "{result}");
    assert!(
        result["structuredContent"]["building_count"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "{result}"
    );
}

#[tokio::test]
async fn refuses_unknown_hosts_and_origins() {
    let req = post_to("evil.example:8090")
        .header(header::AUTHORIZATION, format!("Bearer {LAPTOP}"))
        .body(Body::from(initialize()))
        .expect("request");
    let (status, _, _) = send(app(), req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let req = post_to("mcp.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {LAPTOP}"))
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from(initialize()))
        .expect("request");
    let (status, _, _) = send(app(), req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let req = post_to("mcp.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {LAPTOP}"))
        .header(header::ORIGIN, "https://app.example.com")
        .body(Body::from(initialize()))
        .expect("request");
    let (status, _, body) = send(app(), req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn refuses_oversized_bodies() {
    let padding = "x".repeat(MAX_BODY_BYTES);
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "ping", "params": {"pad": padding}
    })
    .to_string();
    let (status, _, _) = send(app(), authed(LAPTOP, body)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn insecure_mode_needs_no_token() {
    let app = router(&config(Auth::Disabled), &CancellationToken::new());
    let req = post().body(Body::from(initialize())).expect("request");
    let (status, _, body) = send(app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        message(&body)["result"]["serverInfo"]["name"],
        "observation-mcp"
    );
}

#[tokio::test]
async fn no_sessions_to_open_or_close() {
    for method in ["GET", "DELETE"] {
        let req = Request::builder()
            .method(method)
            .uri("/mcp")
            .header(header::HOST, "localhost")
            .header(header::ACCEPT, "text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {LAPTOP}"))
            .body(Body::empty())
            .expect("request");
        let (status, _, _) = send(app(), req).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }
}

/// Calls `tool` with `token`, through `app`; returns the result.
async fn call(app: &Router, token: &str, tool: &str, arguments: Value) -> Value {
    let req = post()
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(rpc(
            9,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        )))
        .expect("request");
    let (status, _, body) = send(app.clone(), req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    message(&body)["result"].clone()
}

/// A token's pairing with the app lasts across its stateless requests, and
/// no other token can use it.
#[tokio::test]
async fn each_token_has_its_own_app_connection() {
    use observation_agent_api::{PairingCreated, paths};
    use observation_server::testing::TestApp;
    use observation_upstream::testing::FakeHttp;
    use observation_upstream::{GuardedHttp, HttpClient, HttpRequest, Redirects};
    use std::net::SocketAddr;

    // The app's server with its agent bridge, on loopback.
    let server = TestApp::builder()
        .overpass(FakeHttp::new())
        .config(|c| {
            c.agent_bridge = true;
            c.dem_url = None;
        })
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().expect("address").port()
    );
    let router = server.router.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });

    // The browser opens a pairing and shows its code.
    let browser = GuardedHttp::builder(true, Redirects::None)
        .no_proxy()
        .build()
        .expect("client");
    let req = HttpRequest::post(format!("{base}{}", paths::PAIRINGS), 1 << 16)
        .json(br#"{"v":1}"#.to_vec());
    let resp = browser.send(req).await.expect("pairing");
    assert_eq!(resp.status, 201);
    let created: PairingCreated =
        serde_json::from_slice(resp.bytes().expect("body")).expect("pairing");

    let app = app();
    let connected = call(
        &app,
        LAPTOP,
        "connect_app",
        json!({"url": base, "code": created.code}),
    )
    .await;
    assert_ne!(connected["isError"], true, "{connected}");

    let laptop = call(&app, LAPTOP, "get_app_state", json!({})).await;
    assert_ne!(laptop["isError"], true, "{laptop}");
    let ci = call(&app, CI, "get_app_state", json!({})).await;
    assert_eq!(ci["isError"], true, "{ci}");
}

/// A preflight from `origin`, as a browser sends it before an MCP POST.
fn preflight(origin: &str) -> Request<Body> {
    Request::builder()
        .method("OPTIONS")
        .uri("/mcp")
        .header(header::HOST, "localhost:8090")
        .header(header::ORIGIN, origin)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization,content-type,mcp-protocol-version",
        )
        .body(Body::empty())
        .expect("request")
}

#[tokio::test]
async fn answers_cors_for_allowed_origins() {
    // The preflight needs no token.
    let (status, headers, _) = send(app(), preflight("https://app.example.com")).await;
    assert!(status.is_success(), "{status}");
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example.com"
    );
    let methods = headers[header::ACCESS_CONTROL_ALLOW_METHODS]
        .to_str()
        .expect("ascii");
    assert!(methods.contains("POST"), "{methods}");
    let allowed = headers[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()
        .expect("ascii")
        .to_ascii_lowercase();
    for h in ["authorization", "content-type", "mcp-protocol-version"] {
        assert!(allowed.contains(h), "{allowed}");
    }
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));

    // The request itself, and a refusal the page must be able to read.
    let req = post()
        .header(header::ORIGIN, "https://app.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {LAPTOP}"))
        .body(Body::from(initialize()))
        .expect("request");
    let (status, headers, body) = send(app(), req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example.com"
    );
    let req = post()
        .header(header::ORIGIN, "https://app.example.com")
        .body(Body::from(initialize()))
        .expect("request");
    let (status, headers, _) = send(app(), req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example.com"
    );
    let exposed = headers[header::ACCESS_CONTROL_EXPOSE_HEADERS]
        .to_str()
        .expect("ascii")
        .to_ascii_lowercase();
    assert!(exposed.contains("www-authenticate"), "{exposed}");
}

#[tokio::test]
async fn no_cors_for_other_origins() {
    for origin in ["https://evil.example", "http://app.example.com"] {
        let (_, headers, _) = send(app(), preflight(origin)).await;
        assert!(
            !headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "{origin}"
        );
    }
    // Without allowed origins there is no CORS at all.
    let app = router(&config_with_origins(&[]), &CancellationToken::new());
    let (_, headers, _) = send(app, preflight("https://app.example.com")).await;
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
}

fn config_with_origins(origins: &[&str]) -> HttpConfig {
    HttpConfig {
        allowed_origins: origins.iter().map(|&o| o.to_owned()).collect(),
        ..config(Auth::Disabled)
    }
}

/// A `subscriptions/listen` stream (MCP 2026-07-28) is answered with an SSE
/// response at once and stays open past the request timeout, which only
/// limits the wait for a request's first message.
#[tokio::test(start_paused = true)]
async fn listen_streams_outlive_the_request_timeout() {
    use observation_mcp::http::REQUEST_TIMEOUT;
    use std::time::Duration;

    let body = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "subscriptions/listen",
        "params": {
            "notifications": {"resourceSubscriptions": ["observation://app/state"]},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    })
    .to_string();
    let req = post()
        .header(header::AUTHORIZATION, format!("Bearer {LAPTOP}"))
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "subscriptions/listen")
        .body(Body::from(body))
        .expect("request");
    let res = tokio::time::timeout(Duration::from_secs(5), app().oneshot(req))
        .await
        .expect("answered at once")
        .expect("infallible");
    if res.status() != StatusCode::OK {
        let status = res.status();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        panic!("{status}: {}", String::from_utf8_lossy(&bytes));
    }
    assert!(
        res.headers()[header::CONTENT_TYPE]
            .to_str()
            .expect("ascii")
            .starts_with("text/event-stream")
    );
    let mut body = res.into_body();
    let first = body
        .frame()
        .await
        .expect("a first event")
        .expect("no error")
        .into_data()
        .expect("data");
    let first = String::from_utf8_lossy(&first);
    assert!(first.contains("data:"), "{first}");

    // Well past the request timeout the stream still delivers (keep-alive
    // comments, as nothing changes), rather than ending.
    tokio::time::advance(REQUEST_TIMEOUT + Duration::from_secs(5)).await;
    let later = body.frame().await;
    assert!(matches!(later, Some(Ok(_))), "the stream ended: {later:?}");
}

/// The legacy `resources/subscribe` would have no session to notify on, so
/// over HTTP it is refused with an error that says what to use instead.
#[tokio::test]
async fn legacy_subscribe_is_refused_over_http() {
    let req = post()
        .header(header::AUTHORIZATION, format!("Bearer {LAPTOP}"))
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(rpc(
            6,
            "resources/subscribe",
            json!({"uri": "observation://app/state"}),
        )))
        .expect("request");
    let (status, _, body) = send(app(), req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let error = &message(&body)["error"];
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|m| m.contains("subscriptions/listen")),
        "{body}"
    );
}

/// Each token has its own working scenes, as it has its own app connection.
#[tokio::test]
async fn each_token_has_its_own_working_scenes() {
    let app = app();
    let opened = call(&app, LAPTOP, "open_scene", json!({})).await;
    assert_eq!(opened["structuredContent"]["scene_id"], "s1", "{opened}");
    let laptop = call(&app, LAPTOP, "export_scene", json!({"scene_id": "s1"})).await;
    assert_ne!(laptop["isError"], true, "{laptop}");
    let ci = call(&app, CI, "export_scene", json!({"scene_id": "s1"})).await;
    assert_eq!(ci["isError"], true, "{ci}");
    let listed = call(&app, CI, "list_scenes", json!({})).await;
    assert_eq!(listed["structuredContent"]["scenes"], json!([]), "{listed}");
}
