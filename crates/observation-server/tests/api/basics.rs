use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use observation_core::bundle::{AreaBundle, BUNDLE_CONTENT_TYPE};
use observation_core::provenance::format_utc;

use crate::harness::{get_uri, json, offline, send};

#[tokio::test]
async fn healthz_and_security_headers() {
    let h = offline();
    let (status, headers, body) = get_uri(&h.app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"ok");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["referrer-policy"],
        "strict-origin-when-cross-origin"
    );
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("'wasm-unsafe-eval'") && csp.contains("frame-ancestors 'none'"));
    assert!(
        headers.contains_key("x-request-id"),
        "every response carries a request id"
    );

    // Error responses carry them too.
    let (status, headers, _) = get_uri(&h.app, "/api/area").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers["x-frame-options"], "DENY");
}

#[tokio::test]
async fn config_endpoint() {
    let h = offline();
    let (status, headers, body) = get_uri(&h.app, "/api/config").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let v = json(&body);
    assert_eq!(v["tile_url"], "/api/tiles/{z}/{x}/{y}.png");
    assert_eq!(v["attribution"], "© OpenStreetMap contributors");
    assert_eq!(v["min_bbox_side_m"], 20.0);
    assert_eq!(v["max_bbox_side_m"], 500.0);
    assert_eq!(v["demo_available"], true);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn bad_bboxes_are_400() {
    let h = offline();
    for uri in [
        "/api/area",
        "/api/area?bbox=",
        "/api/area?bbox=1,2,3",
        "/api/area?bbox=a,b,c,d",
        "/api/area?bbox=48,-122,47,-121",
        "/api/area?bbox=99,0,99.1,0.1",
        // Too large (about 11 km) and too small (about 1 m).
        "/api/area?bbox=47.0,-122.3,47.1,-122.2",
        "/api/area?bbox=47.0,-122.0,47.00001,-121.99999",
    ] {
        let (status, _, body) = get_uri(&h.app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(
            json(&body)["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn oversized_box_error_names_the_size_and_the_limit() {
    let h = offline();
    // About 11 km wide and tall.
    let (status, _, body) = get_uri(&h.app, "/api/area?bbox=47.0,-122.3,47.1,-122.2").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let msg = json(&body)["error"].as_str().unwrap().to_owned();
    assert!(msg.contains("wide") && msg.contains("tall"), "{msg}");
    assert!(msg.contains("20 to 500 m"), "{msg}");
}

#[tokio::test]
async fn demo_is_a_decodable_bundle() {
    let h = offline();
    let (status, headers, body) = get_uri(&h.app, "/api/demo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], BUNDLE_CONTENT_TYPE);
    let b = AreaBundle::decode(&body).unwrap();
    assert!(b.buildings.len() > 20);
    assert!(!b.roads.is_empty());
    assert_eq!(b.attribution, "© OpenStreetMap contributors");
    // The fixture's recorded data date, and a fingerprint of its features.
    assert_eq!(
        b.osm_base_unix.map(format_utc).as_deref(),
        Some("2026-10-31T00:00:00Z")
    );
    assert_ne!(b.osm_fingerprint, 0);
}

#[tokio::test]
async fn bundles_are_compressed_on_request() {
    let h = offline();
    let req = Request::builder()
        .uri("/api/demo")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_ENCODING], "gzip");
    assert_eq!(&body[..2], &[0x1f, 0x8b]);
}
