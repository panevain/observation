//! The agent client's wire shapes and error mapping, on a fake transport.

use std::sync::Arc;
use std::time::Duration;

use http::header::{AUTHORIZATION, CONTENT_TYPE, HeaderName};
use http::{Method, StatusCode};
use observation_core::geo::BBox;
use observation_upstream::testing::{FakeHttp, FakeReply};
use observation_upstream::{HttpRequest, RequestBody, TransportErrorKind};

use super::{CALL_TIMEOUT, HttpRegionAgent};
use crate::{
    ActionKind, AgentError, QueryResult, RegionAction, RegionAgent, RegionsSnapshot, SizesRequest,
};

const BASE: &str = "http://agent.example:8090";
/// The caps, as literals so that changing one fails a test.
const MIB_4: usize = 4 * 1024 * 1024;
const MIB_32: usize = 32 * 1024 * 1024;
const KIB_4: usize = 4096;

fn agent(token: Option<&str>) -> (HttpRegionAgent, FakeHttp) {
    let fake = FakeHttp::new();
    let agent = HttpRegionAgent::new(
        Arc::new(fake.clone()),
        BASE.to_owned(),
        token.map(str::to_owned),
    );
    (agent, fake)
}

fn url(path: &str) -> String {
    format!("{BASE}{path}")
}

fn bbox() -> BBox {
    BBox::new(47.6, -122.34, 47.601, -122.339).unwrap()
}

fn status(code: u16) -> FakeReply {
    FakeReply::status(StatusCode::from_u16(code).unwrap())
}

fn only_request(fake: &FakeHttp) -> HttpRequest {
    let [req] = fake.requests().try_into().unwrap();
    req
}

fn header(req: &HttpRequest, name: HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_owned())
}

#[tokio::test(start_paused = true)]
async fn snapshots_are_long_polled_with_the_token() {
    let (agent, fake) = agent(Some("s3cret"));
    let snap = RegionsSnapshot {
        version: 3,
        ..RegionsSnapshot::default()
    };
    let json = serde_json::to_vec(&snap).unwrap();
    fake.on(Method::GET, &url("/regions"), FakeReply::ok(json.clone()));
    fake.on(Method::GET, &url("/regions?since=3"), FakeReply::ok(json));
    assert_eq!(agent.snapshot(None).await.unwrap(), snap);
    assert_eq!(agent.snapshot(Some(3)).await.unwrap(), snap);
    for req in fake.requests() {
        assert_eq!(
            header(&req, AUTHORIZATION).as_deref(),
            Some("Bearer s3cret")
        );
        assert!(req.headers()[AUTHORIZATION].is_sensitive());
        assert_eq!(req.body, RequestBody::None);
        assert_eq!(req.timeout, Some(Duration::from_secs(40)));
        assert_eq!(req.max_bytes, MIB_4);
        assert!(
            req.skip_error_bodies,
            "an error status's body is never read"
        );
    }
    fake.assert_drained();
}

/// Each failed snapshot gives the text the poller logs, worded as before
/// (reqwest's own texts for statuses and transport errors).
#[tokio::test(start_paused = true)]
async fn snapshot_failures_are_described() {
    let bad_json = serde_json::from_slice::<RegionsSnapshot>(b"<html>")
        .unwrap_err()
        .to_string();
    // At the cap the body is read (and is no snapshot); one byte over, not.
    let blank = serde_json::from_slice::<RegionsSnapshot>(&vec![b' '; MIB_4])
        .unwrap_err()
        .to_string();
    for (reply, want) in [
        (
            status(401),
            "HTTP status client error (401 Unauthorized)".to_owned(),
        ),
        (
            status(404),
            "HTTP status client error (404 Not Found)".to_owned(),
        ),
        (
            status(503),
            "HTTP status server error (503 Service Unavailable)".to_owned(),
        ),
        // The reply's own phrase, as reqwest printed it.
        (
            status(503).reason("Back Soon"),
            "HTTP status server error (503 Back Soon)".to_owned(),
        ),
        // A 3xx is read under the cap, like a 2xx.
        (
            status(302).body(vec![b' '; MIB_4 + 1]),
            "the snapshot is too large".to_owned(),
        ),
        (status(302).body("<html>"), bad_json.clone()),
        (
            FakeReply::ok(vec![b' '; MIB_4 + 1]),
            "the snapshot is too large".to_owned(),
        ),
        (FakeReply::ok(vec![b' '; MIB_4]), blank),
        (FakeReply::ok("<html>"), bad_json),
    ] {
        let (agent, fake) = agent(None);
        fake.on(Method::GET, &url("/regions"), reply);
        assert_eq!(agent.snapshot(None).await.unwrap_err(), want);
    }
    let (agent, fake) = agent(None);
    fake.on(
        Method::GET,
        &url("/regions"),
        FakeReply::error(TransportErrorKind::Connect),
    );
    // The transport's text as it is (GuardedHttp's is reqwest's, without
    // the URL).
    assert_eq!(
        agent.snapshot(None).await.unwrap_err(),
        "FakeHttp: scripted Connect"
    );
    fake.assert_drained();
}

#[tokio::test(start_paused = true)]
async fn queries_post_the_form_to_the_interpreter_with_the_box() {
    let (agent, fake) = agent(Some("s3cret"));
    let reply = FakeReply::ok("{\"elements\":[]}").header(
        HeaderName::from_static("x-osm-base"),
        "2024-05-01T12:00:00Z",
    );
    let at = url("/interpreter?bbox=47.6000000,-122.3400000,47.6010000,-122.3390000");
    fake.on(Method::POST, &at, reply);
    let got = agent
        .query(&bbox(), "[out:json];", Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(
        got,
        Some(QueryResult {
            body: "{\"elements\":[]}".into(),
            osm_base_unix: Some(1_714_564_800),
        })
    );
    let req = only_request(&fake);
    assert_eq!(req.url, at);
    assert_eq!(
        req.body,
        RequestBody::Form(vec![("data".into(), "[out:json];".into())])
    );
    assert_eq!(
        req.wire_parts().0[CONTENT_TYPE],
        "application/x-www-form-urlencoded"
    );
    assert_eq!(
        header(&req, AUTHORIZATION).as_deref(),
        Some("Bearer s3cret")
    );
    assert_eq!(req.timeout, Some(Duration::from_secs(15)), "the caller's");
    assert_eq!(req.max_bytes, MIB_32);
    assert_eq!(req.error_max_bytes, KIB_4);
    assert_eq!(
        req.body_skipped_for,
        Some(StatusCode::NOT_FOUND),
        "a 404's body is never read"
    );
    fake.assert_drained();
}

#[tokio::test(start_paused = true)]
async fn query_outcomes_are_mapped() {
    let long = "x".repeat(KIB_4 + 1);
    let at_cap = "x".repeat(KIB_4);
    let at_cap_text = format!("agent answered 503 Service Unavailable: {at_cap}");
    let at = url("/interpreter?bbox=47.6000000,-122.3400000,47.6010000,-122.3390000");
    let cases: [(FakeReply, Result<Option<QueryResult>, &str>); 13] = [
        (status(404), Ok(None)),
        // Never read: neither its content nor its failure counts.
        (status(404).body(r#"{"error":"no region"}"#), Ok(None)),
        (
            status(404)
                .body("x")
                .body_fails(TransportErrorKind::BodyTimeout),
            Ok(None),
        ),
        (
            FakeReply::ok("{}"),
            Ok(Some(QueryResult {
                body: "{}".into(),
                osm_base_unix: None,
            })),
        ),
        (
            FakeReply::ok("{}").header(HeaderName::from_static("x-osm-base"), "not a date"),
            Ok(Some(QueryResult {
                body: "{}".into(),
                osm_base_unix: None,
            })),
        ),
        (
            status(500).body("boom"),
            Err("agent answered 500 Internal Server Error: boom"),
        ),
        (
            status(502).body(long),
            Err("agent answered 502 Bad Gateway: "),
        ),
        (status(503).body(at_cap), Err(&at_cap_text)),
        (
            FakeReply::ok(vec![b' '; MIB_32 + 1]),
            Err("the result is too large"),
        ),
        (
            FakeReply::ok(vec![b' '; MIB_32]),
            Ok(Some(QueryResult {
                body: vec![b' '; MIB_32].into(),
                osm_base_unix: None,
            })),
        ),
        // A body that breaks off: an error status is logged without it,
        // a success gives the transport's text.
        (
            status(500)
                .body("boom")
                .body_fails(TransportErrorKind::Body),
            Err("agent answered 500 Internal Server Error: "),
        ),
        (
            FakeReply::ok("{").body_fails(TransportErrorKind::Body),
            Err("FakeHttp: scripted Body in the body"),
        ),
        (
            FakeReply::error(TransportErrorKind::Connect),
            Err("FakeHttp: scripted Connect"),
        ),
    ];
    for (i, (reply, want)) in cases.into_iter().enumerate() {
        let (agent, fake) = agent(None);
        fake.on(Method::POST, &at, reply);
        let got = agent.query(&bbox(), "q", Duration::from_secs(15)).await;
        assert_eq!(got, want.map_err(str::to_owned), "case {i}");
        fake.assert_drained();
    }
}

#[tokio::test(start_paused = true)]
async fn catalog_requests_and_actions_send_json_with_the_token() {
    let (agent, fake) = agent(Some("s3cret"));
    fake.on(Method::GET, &url("/catalog"), FakeReply::ok("[]"));
    assert_eq!(agent.catalog().await.unwrap(), "[]");
    let sizes = SizesRequest {
        ids: vec!["europe/monaco".into()],
    };
    fake.on(Method::POST, &url("/catalog/sizes"), FakeReply::ok("{}"));
    assert_eq!(agent.catalog_sizes(&sizes).await.unwrap(), "{}");
    let action = RegionAction {
        id: "europe/monaco".into(),
        auto_refresh: Some(false),
        refresh_days: Some(None),
    };
    let kinds = [
        (ActionKind::Import, "/regions/import"),
        (ActionKind::Cancel, "/regions/cancel"),
        (ActionKind::Delete, "/regions/delete"),
        (ActionKind::Settings, "/regions/settings"),
    ];
    for (kind, path) in kinds {
        fake.on(Method::POST, &url(path), FakeReply::ok("{\"version\":1}"));
        assert_eq!(agent.act(kind, &action).await.unwrap(), "{\"version\":1}");
    }
    let requests = fake.requests();
    assert_eq!(requests.len(), 2 + kinds.len());
    assert_eq!(requests[0].body, RequestBody::None);
    assert_eq!(
        requests[1].body,
        RequestBody::Json(r#"{"ids":["europe/monaco"]}"#.into())
    );
    for req in &requests[2..] {
        assert_eq!(
            req.body,
            RequestBody::Json(
                r#"{"id":"europe/monaco","auto_refresh":false,"refresh_days":null}"#.into()
            )
        );
    }
    for req in &requests {
        assert_eq!(header(req, AUTHORIZATION).as_deref(), Some("Bearer s3cret"));
        assert_eq!(req.timeout, Some(CALL_TIMEOUT));
        assert_eq!(req.timeout, Some(Duration::from_secs(45)));
        assert_eq!((req.max_bytes, req.error_max_bytes), (MIB_4, MIB_4));
    }
    for req in &requests[1..] {
        assert_eq!(req.wire_parts().0[CONTENT_TYPE], "application/json");
    }
    fake.assert_drained();
}

/// How an answer to a catalog request or an action maps to [`AgentError`].
#[tokio::test(start_paused = true)]
async fn answers_map_to_agent_errors() {
    let too_large = || vec![b' '; MIB_4 + 1];
    let at_cap = " ".repeat(MIB_4);
    let cases: [(FakeReply, Result<&str, AgentError>); 14] = [
        (FakeReply::ok(at_cap.clone()), Ok(&at_cap)),
        (
            status(500).body(at_cap.clone()),
            Err(AgentError::Other(
                "the region agent answered HTTP 500".into(),
            )),
        ),
        (FakeReply::ok("[1]"), Ok("[1]")),
        (
            FakeReply::status(StatusCode::CREATED).body("[2]"),
            Ok("[2]"),
        ),
        (
            status(400).body(r#"{"error":"not a region id"}"#),
            Err(AgentError::BadRequest("not a region id".into())),
        ),
        (
            status(404),
            Err(AgentError::BadRequest(
                "the region agent answered HTTP 404".into(),
            )),
        ),
        (
            status(409).body(r#"{"error":"monaco is importing"}"#),
            Err(AgentError::Conflict("monaco is importing".into())),
        ),
        (
            status(401).body(r#"{"error":"wrong token"}"#),
            Err(AgentError::Unauthorized),
        ),
        (
            status(500).body(r#"{"error":"disk full"}"#),
            Err(AgentError::Other("disk full".into())),
        ),
        (
            status(503).body(r#"{"error":7}"#),
            Err(AgentError::Other(
                "the region agent answered HTTP 503".into(),
            )),
        ),
        (
            status(502).body("<html>"),
            Err(AgentError::Other(
                "the region agent answered HTTP 502".into(),
            )),
        ),
        (FakeReply::ok(too_large()), Err(AgentError::TooLarge)),
        (status(500).body(too_large()), Err(AgentError::TooLarge)),
        (
            FakeReply::error(TransportErrorKind::Timeout),
            Err(AgentError::Unreachable),
        ),
    ];
    for (i, (reply, want)) in cases.into_iter().enumerate() {
        let (agent, fake) = agent(None);
        fake.on(Method::GET, &url("/catalog"), reply);
        let got = agent.catalog().await;
        assert_eq!(
            got.as_ref().map(|b| std::str::from_utf8(b).unwrap()),
            want.as_ref().map(|s| *s),
            "case {i}"
        );
        assert_eq!(
            header(&only_request(&fake), AUTHORIZATION),
            None,
            "case {i}"
        );
        fake.assert_drained();
    }
}

/// Neither the token nor credentials in the base URL show in `Debug`.
#[test]
fn debug_output_hides_the_token() {
    let agent = HttpRegionAgent::new(
        Arc::new(FakeHttp::new()),
        "http://user:pw@agent.example:8090".to_owned(),
        Some("s3cret".to_owned()),
    );
    let shown = format!("{agent:?}");
    assert!(!shown.contains("s3cret"), "{shown}");
    assert!(!shown.contains("pw"), "{shown}");
    assert!(shown.contains("agent.example:8090"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
}

/// A token that cannot be a header value fails every request before it is
/// sent, as reqwest's builder did.
#[tokio::test(start_paused = true)]
async fn an_unusable_token_fails_without_a_request() {
    let (agent, fake) = agent(Some("bad\ntoken"));
    assert_eq!(agent.snapshot(None).await.unwrap_err(), "builder error");
    assert_eq!(
        agent
            .query(&bbox(), "q", Duration::from_secs(1))
            .await
            .unwrap_err(),
        "builder error"
    );
    assert_eq!(agent.catalog().await, Err(AgentError::Unreachable));
    assert!(fake.requests().is_empty());
}
