//! The production adapters over real sockets, through the real
//! `AppState::new`: what fakes cannot show, for Overpass, for tiles and for
//! the region agent (the User-Agent and the agent's token on the wire, the
//! agent long-polled by the spawned poller, a redirect not followed, a body
//! cut off while streaming, the disk cache's files). Everything else is
//! tested on `TestApp` in `tests/api`.
#![allow(clippy::unwrap_used)] // integration-test helpers live outside `#[test]` fns

#[path = "common/mod.rs"]
mod common;
#[path = "adapters/harness.rs"]
mod harness;
#[path = "adapters/mocks.rs"]
mod mocks;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::{get, post};
use observation_core::bundle::{AreaBundle, BUNDLE_CONTENT_TYPE};

use crate::common::{BBOX, PNG, area_uri, get_uri, json, send};
use crate::harness::{Harness, harness, harness_with};
use crate::mocks::{MockAgent, MockOverpass, spawn, spawn_agent, spawn_overpass, spawn_tiles};

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

#[tokio::test]
async fn tiles_are_proxied_once_and_cached() {
    let (mock, url) = spawn_tiles(3600).await;
    let h = harness("http://127.0.0.1:9/o", &url);
    let req = Request::builder()
        .uri("/api/tiles/17/20/30.png")
        .header(header::REFERER, "https://example.test/app")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=604800");
    assert_eq!(body.as_ref(), PNG);
    assert!(mock.user_agents.lock().unwrap()[0].contains("observation/"));
    assert_eq!(
        mock.referers.lock().unwrap().as_slice(),
        ["https://example.test/app"]
    );

    let (status, _, body) = get_uri(&h.app, "/api/tiles/17/20/30.png").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), PNG);
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        1,
        "second request must be served from disk"
    );

    let root = h.cache.path().join("tiles/17/20");
    assert!(root.join("30.png").is_file() && root.join("30.json").is_file());
    let meta = json(&std::fs::read(root.join("30.json")).unwrap());
    assert_eq!(meta["etag"], "\"tile-etag\"");
}

#[tokio::test]
async fn tile_redirects_are_not_followed() {
    let (target, template) = spawn_tiles(3600).await;
    let target_url = template
        .replace("{z}", "1")
        .replace("{x}", "0")
        .replace("{y}", "0");
    let router = Router::new().route(
        "/{z}/{x}/{y}",
        get(move || {
            let to = target_url.clone();
            async move { (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, to)]) }
        }),
    );
    let addr = spawn(router).await;
    let h = harness(
        "http://127.0.0.1:9/o",
        &format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"),
    );
    let (status, _, _) = get_uri(&h.app, "/api/tiles/1/0/0.png").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(target.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn endless_tile_bodies_are_cut_off() {
    // A chunked body that never ends: buffering it whole would only stop at the
    // client timeout (and after eating memory); the streaming cap stops at 2 MiB.
    let router = Router::new().route(
        "/{z}/{x}/{y}",
        get(|| async {
            let first = futures_util::stream::once(async { Ok(Bytes::from_static(PNG)) });
            let rest = futures_util::stream::repeat_with(|| {
                Ok::<_, std::io::Error>(Bytes::from(vec![0u8; 64 * 1024]))
            });
            Body::from_stream(futures_util::StreamExt::chain(first, rest))
        }),
    );
    let addr = spawn(router).await;
    let h = harness(
        "http://127.0.0.1:9/o",
        &format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"),
    );
    let (status, _, _) = tokio::time::timeout(
        Duration::from_secs(10),
        get_uri(&h.app, "/api/tiles/1/0/0.png"),
    )
    .await
    .expect("oversized body is rejected while streaming");
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!h.cache.path().join("tiles").exists());
}

/// A harness with a mock Overpass and a mock region agent; `admin` sets OBS_ADMIN_TOKEN.
async fn region_harness(admin: Option<&str>) -> (Harness, Arc<MockOverpass>, Arc<MockAgent>) {
    let (overpass, overpass_url) = spawn_overpass().await;
    let (agent, agent_url) = spawn_agent().await;
    let admin = admin.map(str::to_owned);
    let h = harness_with(
        &overpass_url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        move |c| {
            c.regions_url = Some(agent_url);
            c.regions_token = Some("agent-secret".into());
            c.admin_token = admin;
        },
    );
    (h, overpass, agent)
}

/// Waits until the server's copy of the agent snapshot has `version`.
async fn wait_for_version(app: &Router, version: u64) -> serde_json::Value {
    for _ in 0..200 {
        let (status, _, body) = get_uri(app, "/api/regions").await;
        assert_eq!(status, StatusCode::OK);
        let v = json(&body);
        if v["agent_online"] == true && v["snapshot"]["version"] == version {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the server never saw snapshot version {version}");
}

#[tokio::test]
async fn areas_inside_a_ready_region_come_from_the_agent() {
    let (h, overpass, agent) = region_harness(None).await;
    agent.set_ready("wa", 1);
    wait_for_version(&h.app, 1).await;
    let (_, _, body) = get_uri(&h.app, "/api/config").await;
    assert_eq!(json(&body)["regions"], true);

    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(agent.interpreter_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        overpass.hits.load(Ordering::SeqCst),
        0,
        "public Overpass untouched"
    );
    assert!(agent.bboxes.lock().unwrap()[0].contains(','));
    assert!(
        agent
            .auth
            .lock()
            .unwrap()
            .iter()
            .all(|a| a == "Bearer agent-secret"),
        "every agent request carries OBS_REGIONS_TOKEN"
    );
}
