use std::time::Duration;

use axum::http::{Method, StatusCode};
use observation_core::bundle::{AreaBundle, BUNDLE_MAGIC, BUNDLE_VERSION};
use observation_server::testing::TestApp;
use observation_upstream::TransportErrorKind;
use observation_upstream::testing::{FakeHttp, FakeReply};

use crate::harness::{
    OVERPASS, area_uri, flat_app, get_uri, hits, json, overpass_ok, overpass_status,
};

#[tokio::test(start_paused = true)]
async fn concurrent_area_requests_share_one_upstream_fetch() {
    let overpass = FakeHttp::new();
    overpass.on(
        Method::POST,
        OVERPASS,
        overpass_ok().delay(Duration::from_millis(300)),
    );
    let app = flat_app(overpass.clone(), |_| {});

    let uri = area_uri();
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (router, uri) = (app.router.clone(), uri.clone());
            tokio::spawn(async move { get_uri(&router, &uri).await })
        })
        .collect();
    let mut bodies = Vec::new();
    for t in tasks {
        let (status, _, body) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        bodies.push(body);
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(hits(&overpass, OVERPASS), 1, "single-flight failed");
}

#[tokio::test(start_paused = true)]
async fn upstream_failures_are_502_and_not_cached() {
    let overpass = FakeHttp::new();
    overpass.on(Method::POST, OVERPASS, overpass_status(500));
    overpass.on(Method::POST, OVERPASS, overpass_ok());
    let app = flat_app(overpass.clone(), |_| {});
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(json(&body)["error"].is_string());
    // Failure was not remembered: the next request fetches again and succeeds.
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits(&overpass, OVERPASS), 2);

    // Unreachable upstream is a 502 as well (tried twice, as any failure).
    let unreachable = FakeHttp::new();
    for _ in 0..2 {
        unreachable.on(
            Method::POST,
            OVERPASS,
            FakeReply::error(TransportErrorKind::Connect),
        );
    }
    let off = flat_app(unreachable, |_| {});
    let (status, _, _) = get_uri(&off.router, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

/// A file in the bundle cache that is not a bundle of the current version
/// (a foreign file, an older format) is never served, though fresh by its
/// mtime: not as a hit, and not as an expired copy when the upstream fails.
#[tokio::test(start_paused = true)]
async fn cached_files_that_are_not_current_bundles_are_never_served() {
    let mut old_version = BUNDLE_MAGIC.to_vec();
    old_version.extend_from_slice(&(BUNDLE_VERSION - 1).to_le_bytes());
    old_version.extend_from_slice(&[0; 32]);
    for bad in [b"<html>".to_vec(), old_version] {
        let cache = tempfile::tempdir().unwrap();
        let overpass = FakeHttp::new();
        overpass.on(Method::POST, OVERPASS, overpass_ok());
        overpass.on(Method::POST, OVERPASS, overpass_status(500));
        overpass.on(Method::POST, OVERPASS, overpass_ok());
        let app = TestApp::builder()
            .overpass(overpass.clone())
            .disk_cache(cache.path())
            .config(|c| c.dem_url = None)
            .build();
        let (status, _, _) = get_uri(&app.router, &area_uri()).await;
        assert_eq!(status, StatusCode::OK);
        let [file] = std::fs::read_dir(cache.path().join("bundles"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        std::fs::write(&file, &bad).unwrap();

        // The upstream fails: its error, not the file.
        let (status, _, body) = get_uri(&app.router, &area_uri()).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            json(&body)["error"],
            "the map data provider answered with an error (HTTP 500)"
        );
        assert_eq!(hits(&overpass, OVERPASS), 2);
        assert_eq!(std::fs::read(&file).unwrap(), bad, "left until rebuilt");

        // The upstream works: rebuilt, served and stored over the file.
        let (status, _, body) = get_uri(&app.router, &area_uri()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(AreaBundle::decode(&body).is_ok());
        assert_eq!(hits(&overpass, OVERPASS), 3);
        assert_eq!(std::fs::read(&file).unwrap(), body);
    }
}

#[tokio::test(start_paused = true)]
async fn expired_bundle_is_served_when_upstream_fails() {
    let overpass = FakeHttp::new();
    overpass.on(Method::POST, OVERPASS, overpass_ok());
    overpass.on(Method::POST, OVERPASS, overpass_status(500));
    let app = flat_app(overpass.clone(), |c| c.bundle_ttl = Duration::ZERO);
    let (status, _, fresh) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, stale) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stale, fresh);
    assert_eq!(hits(&overpass, OVERPASS), 2);
}
