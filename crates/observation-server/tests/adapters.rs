//! The production adapters over real sockets, through the real
//! `AppState::new`: what fakes cannot show (the User-Agent on the wire, a
//! redirect not followed, a body cut off while streaming, the disk cache's
//! files). Everything else is tested on `TestApp` in `tests/api`.
#![allow(clippy::unwrap_used)] // integration-test helpers live outside `#[test]` fns

#[path = "api/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "api/mocks.rs"]
#[allow(dead_code)]
mod mocks;

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::routing::post;
use observation_core::bundle::{AreaBundle, BUNDLE_CONTENT_TYPE};

use crate::harness::{BBOX, area_uri, get_uri, harness};
use crate::mocks::{spawn, spawn_overpass};

#[tokio::test]
async fn area_fetches_once_then_serves_from_cache() {
    let (mock, url) = spawn_overpass().await;
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");

    // Mid-cell coordinates, so the snapped box does not depend on float noise at
    // grid lines. Nudged copies snap to the same cells and so share the cache entry.
    let fixture = observation_core::geo::BBox::parse(BBOX.trim()).unwrap();
    let (d, n) = (0.000_03, 0.000_001);
    let uri = |k: f64| {
        format!(
            "/api/area?bbox={:.6},{:.6},{:.6},{:.6}",
            fixture.south + d + k,
            fixture.west + d + k,
            fixture.north - d - k,
            fixture.east - d - k
        )
    };

    let (status, headers, body) = get_uri(&h.app, &uri(0.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], BUNDLE_CONTENT_TYPE);
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=86400");
    let b = AreaBundle::decode(&body).unwrap();
    assert!(b.buildings.len() > 20);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1);
    assert!(mock.user_agents.lock().unwrap()[0].starts_with("observation/"));
    let query = mock.queries.lock().unwrap()[0].clone();
    assert!(
        query.contains("out geom qt;") && query.contains("[\"building\"]"),
        "{query}"
    );

    // The bundle covers the snapped (slightly larger) box.
    let asked =
        observation_core::geo::BBox::parse(&uri(0.0).replace("/api/area?bbox=", "")).unwrap();
    assert!(b.bbox.south <= asked.south && b.bbox.north >= asked.north);
    assert!(b.bbox.west <= asked.west && b.bbox.east >= asked.east);

    for k in [0.0, n, -n] {
        let (status, _, again) = get_uri(&h.app, &uri(k)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again, body);
    }
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        1,
        "cache hits must not reach Overpass"
    );

    let files: Vec<_> = std::fs::read_dir(h.cache.path().join("bundles"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(files.len(), 1);
    assert!(files[0].file_name().to_str().unwrap().ends_with(".obsb"));
}

#[tokio::test]
async fn endless_overpass_bodies_are_cut_off() {
    // Same as the tile case, against the 32 MiB Overpass cap (no `Content-Length`).
    let router = Router::new().route(
        "/api/interpreter",
        post(|| async {
            Body::from_stream(futures_util::stream::repeat_with(|| {
                Ok::<_, std::io::Error>(Bytes::from(vec![b' '; 64 * 1024]))
            }))
        }),
    );
    let addr = spawn(router).await;
    let h = harness(
        &format!("http://{addr}/api/interpreter"),
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
    );
    let (status, _, _) =
        tokio::time::timeout(Duration::from_secs(10), get_uri(&h.app, &area_uri()))
            .await
            .expect("oversized body is rejected while streaming");
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!h.cache.path().join("bundles").exists());
}

#[tokio::test]
async fn overpass_redirects_are_not_followed() {
    let (inner, inner_url) = spawn_overpass().await;
    // 307 keeps the method and body, so a followed redirect would reach the interpreter.
    let router = Router::new().route(
        "/o",
        post(move || {
            let to = inner_url.clone();
            async move { (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, to)]) }
        }),
    );
    let addr = spawn(router).await;
    let h = harness(&format!("http://{addr}/o"), "http://127.0.0.1:9/{z}");
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(inner.hits.load(Ordering::SeqCst), 0);
}
