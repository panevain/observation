use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::{StatusCode, header};
use observation_core::bundle::{AreaBundle, BUNDLE_CONTENT_TYPE};

use crate::harness::{BBOX, area_uri, get_uri, harness, harness_with, json, offline};
use crate::mocks::spawn_overpass;

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
async fn concurrent_area_requests_share_one_upstream_fetch() {
    let (mock, url) = spawn_overpass().await;
    mock.delay_ms.store(300, Ordering::SeqCst);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");

    let uri = area_uri();
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (app, uri) = (h.app.clone(), uri.clone());
            tokio::spawn(async move { get_uri(&app, &uri).await })
        })
        .collect();
    let mut bodies = Vec::new();
    for t in tasks {
        let (status, _, body) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        bodies.push(body);
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1, "single-flight failed");
}

#[tokio::test]
async fn upstream_failures_are_502_and_not_cached() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().push_back(500);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(json(&body)["error"].is_string());
    // Failure was not remembered: the next request fetches again and succeeds.
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);

    // Unreachable upstream is a 502 as well.
    let off = offline();
    let (status, _, _) = get_uri(&off.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn expired_bundle_is_served_when_upstream_fails() {
    let (mock, url) = spawn_overpass().await;
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.bundle_ttl = Duration::ZERO,
    );
    let (status, _, fresh) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    mock.script.lock().unwrap().push_back(500);
    let (status, _, stale) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stale, fresh);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
}
