use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::routing::post;
use observation_core::bundle::AreaBundle;

use crate::harness::{area_uri, get_uri, harness, harness_with, json};
use crate::mocks::{spawn, spawn_overpass};

#[tokio::test]
async fn overpass_rate_limit_gives_503_then_fails_fast() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().extend([429, 429]);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");

    let (status, headers, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers[header::RETRY_AFTER], "30");
    let msg = json(&body)["error"].as_str().unwrap().to_owned();
    assert!(msg.contains("busy") && msg.contains("30 s"), "{msg}");
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        2,
        "one inline retry, no more"
    );

    // During the cool-down another (different) area does not touch Overpass.
    let (status, headers, _) =
        get_uri(&h.app, "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let secs: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=30).contains(&secs));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn overpass_gateway_timeout_is_retried_once() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().push_back(504);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn overpass_bad_gateway_is_retried() {
    let (mock, url) = spawn_overpass().await;
    mock.script.lock().unwrap().extend([502]);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);

    // Two failures in a row are a 502, and no cool-down starts.
    mock.script.lock().unwrap().extend([503, 502]);
    let other = "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970";
    let (status, _, body) = get_uri(&h.app, other).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(json(&body)["error"].is_string());
    let (status, _, _) = get_uri(&h.app, other).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn overpass_fails_over_to_the_next_endpoint() {
    let (down, down_url) = spawn_overpass().await;
    down.script.lock().unwrap().extend([502, 502, 502]);
    let (up, up_url) = spawn_overpass().await;
    let h = harness_with(
        &down_url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.overpass_urls.push(up_url),
    );
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(down.hits.load(Ordering::SeqCst), 1, "each endpoint once");
    assert_eq!(up.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn overpass_timeouts_are_504() {
    let (mock, url) = spawn_overpass().await;
    mock.delay_ms.store(1_000, Ordering::SeqCst);
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.overpass_timeout = Duration::from_millis(200),
    );
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert!(json(&body)["error"].as_str().unwrap().contains("too long"));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2, "retried once");
}

#[tokio::test]
async fn overpass_remarks_retry_only_timeouts() {
    let (mock, url) = spawn_overpass().await;
    let timed_out =
        r#"{"elements":[],"remark":"runtime error: Query timed out in \"query\" at line 3"}"#;
    mock.bodies.lock().unwrap().extend([timed_out, timed_out]);
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2, "a timeout is retried");

    // Running out of memory would happen again: no retry, and not a "timeout".
    let oom = r#"{"elements":[],"remark":"runtime error: Query run out of memory using about 2048 MB of RAM."}"#;
    mock.bodies.lock().unwrap().push_back(oom);
    let (status, _, body) =
        get_uri(&h.app, "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!json(&body)["error"].as_str().unwrap().contains("too long"));
    assert_eq!(mock.hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_busy_interpreter_rests_while_others_serve() {
    let (busy, busy_url) = spawn_overpass().await;
    busy.always.store(429, Ordering::SeqCst);
    let (up, up_url) = spawn_overpass().await;
    let h = harness_with(
        &busy_url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.overpass_urls.push(up_url),
    );
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    // The next miss skips the interpreter that said 429.
    let (status, _, _) =
        get_uri(&h.app, "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(busy.hits.load(Ordering::SeqCst), 1);
    assert_eq!(up.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn busy_then_failed_is_502_and_the_busy_one_still_rests() {
    let (busy, busy_url) = spawn_overpass().await;
    busy.always.store(429, Ordering::SeqCst);
    let (down, down_url) = spawn_overpass().await;
    down.script.lock().unwrap().push_back(502);
    let h = harness_with(
        &busy_url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.overpass_urls.push(down_url),
    );
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(busy.hits.load(Ordering::SeqCst), 1);
    assert_eq!(down.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_stop_before_the_request_times_out() {
    // Four slow interpreters would need 4 × 600 ms; the request allows 1 s.
    let mut urls = Vec::new();
    for _ in 0..4 {
        let (mock, url) = spawn_overpass().await;
        mock.delay_ms.store(2_000, Ordering::SeqCst);
        urls.push(url);
    }
    let h = harness_with(
        &urls[0],
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| {
            c.overpass_urls = urls.clone();
            c.overpass_timeout = Duration::from_millis(600);
            c.request_timeout = Duration::from_secs(1);
        },
    );
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "not the 408 of the request timeout"
    );
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
