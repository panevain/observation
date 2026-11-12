//! What both test binaries use: the demo fixtures and request helpers.

use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;

pub(crate) const BBOX: &str = include_str!("../../fixtures/demo-bbox.txt");

pub(crate) const FIXTURE: &str = include_str!("../../fixtures/demo-overpass.json");

/// A valid 1x1 PNG.
pub(crate) const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// Largest body [`send`] reads.
pub(crate) const BODY_CAP: usize = 64 << 20;

/// Longest [`send`] waits for a whole body.
pub(crate) const BODY_DEADLINE: Duration = Duration::from_secs(30);

/// Sends `req` and reads the whole answer. The read is bounded, so that a
/// route that streams forever fails the test instead of running it out of
/// memory: more than [`BODY_CAP`], or no end within [`BODY_DEADLINE`], is a
/// test failure. Read streams with the `api` harness's `open` instead.
pub(crate) async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Bytes) {
    let uri = req.uri().clone();
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let body = tokio::time::timeout(BODY_DEADLINE, axum::body::to_bytes(body, BODY_CAP))
        .await
        .unwrap_or_else(|_| panic!("{uri}: the body did not end within {BODY_DEADLINE:?}"))
        .unwrap_or_else(|e| {
            panic!("{uri}: the body could not be read within {BODY_CAP} bytes: {e}")
        });
    (parts.status, parts.headers, body)
}

pub(crate) async fn get_uri(app: &Router, uri: &str) -> (StatusCode, HeaderMap, Bytes) {
    send(
        app,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

pub(crate) fn json(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

pub(crate) fn area_uri() -> String {
    format!("/api/area?bbox={}", BBOX.trim())
}
