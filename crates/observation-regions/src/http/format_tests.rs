//! Byte-for-byte goldens of what the agent sends the server: the snapshot,
//! the catalog, the sizes and the error bodies. The server parses these
//! (`observation_regions_api`), so their bytes are a wire format.

use observation_upstream::testing::RouterHttp;
use observation_upstream::{HttpClient, HttpRequest};

use super::app;
use super::contract_tests::{TOKEN, agent};

const BASE: &str = "http://agent.internal:8090";

/// Sends `req` to an agent on fakes; the status and the body as text.
async fn exchange(req: HttpRequest) -> (u16, String) {
    let data = tempfile::tempdir().unwrap();
    let http = RouterHttp::new(app(agent(data.path()).await));
    let resp = http.send(req).await.unwrap();
    let body = String::from_utf8(resp.bytes().unwrap().to_vec()).unwrap();
    (resp.status.as_u16(), body)
}

fn get(path: &str) -> HttpRequest {
    HttpRequest::get(format!("{BASE}{path}"), 1 << 20)
        .bearer(TOKEN)
        .unwrap()
}

fn post(path: &str, json: &'static str) -> HttpRequest {
    HttpRequest::post(format!("{BASE}{path}"), 1 << 20)
        .bearer(TOKEN)
        .unwrap()
        .json(json)
}

#[tokio::test]
async fn the_snapshot_is_these_bytes() {
    assert_eq!(
        exchange(get("/regions")).await,
        (200, GOLDEN_SNAPSHOT.to_owned())
    );
}

/// The ready region `square` on the agent's production settings.
const GOLDEN_SNAPSHOT: &str = concat!(
    r#"{"version":2,"regions":[{"id":"square","name":"Square","ready":true,"#,
    r#""bbox":{"south":0.0,"west":0.0,"north":1.0,"east":1.0},"#,
    r#""data_timestamp":"2026-10-31T19:43:02Z","imported_unix":1700000000,"last_attempt_unix":1700000000,"#,
    r#""auto_refresh":true,"refresh_days":null,"next_refresh_unix":1700604800,"disk_bytes":0,"#,
    r#""job":null,"last_error":null,"last_error_unix":null}],"#,
    r#""max_download_bytes":2147483648,"refresh_days":7,"min_refresh_days":7,"max_refresh_days":365}"#
);

#[tokio::test]
async fn catalog_and_sizes_are_these_bytes() {
    let catalog = r#"[{"id":"monaco","name":"Monaco","parent":"europe"}]"#;
    assert_eq!(exchange(get("/catalog")).await, (200, catalog.to_owned()));
    let sizes = post("/catalog/sizes", r#"{"ids":["monaco","nowhere"]}"#);
    assert_eq!(
        exchange(sizes).await,
        (200, r#"{"monaco":1234}"#.to_owned())
    );
}

#[tokio::test]
async fn error_bodies_are_these_bytes() {
    let cases = [
        (
            post("/regions/delete", r#"{"id":"nowhere"}"#),
            404,
            r#"{"error":"no such region"}"#,
        ),
        (
            HttpRequest::get(format!("{BASE}/regions"), 1 << 20)
                .bearer("wrong")
                .unwrap(),
            401,
            r#"{"error":"missing or wrong agent token"}"#,
        ),
        (
            HttpRequest::post(format!("{BASE}/interpreter?bbox=5,5,6,6"), 1 << 20)
                .bearer(TOKEN)
                .unwrap()
                .form([("data", "q")]),
            404,
            r#"{"error":"no imported region holds this area"}"#,
        ),
        (
            post("/catalog/sizes", r#"{"ids":["Bad Id"]}"#),
            400,
            r#"{"error":"expected {\"ids\": [...]} with at most 40 region ids"}"#,
        ),
    ];
    for (req, status, body) in cases {
        let label = req.url.clone();
        assert_eq!(exchange(req).await, (status, body.to_owned()), "{label}");
    }
}
