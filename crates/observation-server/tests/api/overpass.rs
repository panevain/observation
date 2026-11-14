use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use observation_core::bundle::AreaBundle;
use observation_upstream::testing::FakeHttp;

use crate::harness::{
    OVERPASS, area_uri, flat_app, get_uri, hits, json, overpass_ok, overpass_status,
};

/// Another area than [`area_uri`], so that it misses the bundle cache.
const OTHER: &str = "/api/area?bbox=47.7000,-122.2000,47.7020,-122.1970";
/// A second Overpass endpoint.
const SECOND: &str = "https://overpass.example/api/interpreter";

/// `replies` for [`OVERPASS`], in order.
fn overpass(
    replies: impl IntoIterator<Item = observation_upstream::testing::FakeReply>,
) -> FakeHttp {
    let fake = FakeHttp::new();
    for reply in replies {
        fake.on(Method::POST, OVERPASS, reply);
    }
    fake
}

#[tokio::test(start_paused = true)]
async fn overpass_rate_limit_gives_503_then_fails_fast() {
    let fake = overpass([overpass_status(429), overpass_status(429)]);
    let app = flat_app(fake.clone(), |_| {});

    let (status, headers, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers[header::RETRY_AFTER], "30");
    let msg = json(&body)["error"].as_str().unwrap().to_owned();
    assert!(msg.contains("busy") && msg.contains("30 s"), "{msg}");
    assert_eq!(hits(&fake, OVERPASS), 2, "one inline retry, no more");

    // During the cool-down another (different) area does not touch Overpass.
    let (status, headers, _) = get_uri(&app.router, OTHER).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let secs: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=30).contains(&secs));
    assert_eq!(hits(&fake, OVERPASS), 2);
}

#[tokio::test(start_paused = true)]
async fn overpass_gateway_timeout_is_retried_once() {
    let fake = overpass([overpass_status(504), overpass_ok()]);
    let app = flat_app(fake.clone(), |_| {});
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(hits(&fake, OVERPASS), 2);
}

#[tokio::test(start_paused = true)]
async fn overpass_bad_gateway_is_retried() {
    let fake = overpass([
        overpass_status(502),
        overpass_ok(),
        overpass_status(503),
        overpass_status(502),
        overpass_ok(),
    ]);
    let app = flat_app(fake.clone(), |_| {});
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(hits(&fake, OVERPASS), 2);

    // Two failures in a row are a 502, and no cool-down starts.
    let (status, _, body) = get_uri(&app.router, OTHER).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(json(&body)["error"].is_string());
    let (status, _, _) = get_uri(&app.router, OTHER).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn overpass_fails_over_to_the_next_endpoint() {
    let fake = overpass([overpass_status(502)]);
    fake.on(Method::POST, SECOND, overpass_ok());
    let app = flat_app(fake.clone(), |c| c.overpass_urls.push(SECOND.into()));
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(AreaBundle::decode(&body).is_ok());
    assert_eq!(hits(&fake, OVERPASS), 1, "each endpoint once");
    assert_eq!(hits(&fake, SECOND), 1);
}

#[tokio::test(start_paused = true)]
async fn overpass_timeouts_are_504() {
    let slow = || overpass_ok().delay(Duration::from_millis(1_000));
    let fake = overpass([slow(), slow()]);
    let app = flat_app(fake.clone(), |c| {
        c.overpass_timeout = Duration::from_millis(200);
    });
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert!(json(&body)["error"].as_str().unwrap().contains("too long"));
    assert_eq!(hits(&fake, OVERPASS), 2, "retried once");
}

#[tokio::test(start_paused = true)]
async fn overpass_remarks_retry_only_timeouts() {
    let timed_out =
        r#"{"elements":[],"remark":"runtime error: Query timed out in \"query\" at line 3"}"#;
    // Running out of memory would happen again: no retry, and not a "timeout".
    let oom = r#"{"elements":[],"remark":"runtime error: Query run out of memory using about 2048 MB of RAM."}"#;
    let reply = |body: &'static str| overpass_ok().body(body);
    let fake = overpass([reply(timed_out), reply(timed_out), reply(oom)]);
    let app = flat_app(fake.clone(), |_| {});
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(hits(&fake, OVERPASS), 2, "a timeout is retried");

    let (status, _, body) = get_uri(&app.router, OTHER).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!json(&body)["error"].as_str().unwrap().contains("too long"));
    assert_eq!(hits(&fake, OVERPASS), 3);
}

#[tokio::test(start_paused = true)]
async fn a_busy_interpreter_rests_while_others_serve() {
    let fake = overpass([overpass_status(429)]);
    fake.on(Method::POST, SECOND, overpass_ok());
    fake.on(Method::POST, SECOND, overpass_ok());
    let app = flat_app(fake.clone(), |c| c.overpass_urls.push(SECOND.into()));
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    // The next miss skips the interpreter that said 429.
    let (status, _, _) = get_uri(&app.router, OTHER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits(&fake, OVERPASS), 1);
    assert_eq!(hits(&fake, SECOND), 2);
}

#[tokio::test(start_paused = true)]
async fn busy_then_failed_is_502_and_the_busy_one_still_rests() {
    let fake = overpass([overpass_status(429)]);
    fake.on(Method::POST, SECOND, overpass_status(502));
    fake.on(Method::POST, SECOND, overpass_ok());
    let app = flat_app(fake.clone(), |c| c.overpass_urls.push(SECOND.into()));
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits(&fake, OVERPASS), 1);
    assert_eq!(hits(&fake, SECOND), 2);
}

#[tokio::test(start_paused = true)]
async fn retries_stop_before_the_request_times_out() {
    // Four slow interpreters would need 4 × 600 ms; the request allows 1 s.
    let urls: Vec<String> = std::iter::once(OVERPASS.to_owned())
        .chain((1..4).map(|i| format!("https://overpass{i}.example/api/interpreter")))
        .collect();
    // Two fit in the budget (90% of 1 s): 600 ms, a 50 ms pause, 250 ms.
    // The others are never asked.
    let fake = FakeHttp::new();
    for url in &urls[..2] {
        fake.on(
            Method::POST,
            url,
            overpass_ok().delay(Duration::from_millis(2_000)),
        );
    }
    let app = flat_app(fake.clone(), {
        let urls = urls.clone();
        move |c| {
            c.overpass_urls = urls;
            c.overpass_timeout = Duration::from_millis(600);
            c.request_timeout = Duration::from_secs(1);
            // As the socket version had it. The pause between attempts is not
            // bounded by the budget, so a 2 s pause (the default) would end in
            // the request's 408 with these short timeouts.
            c.overpass_retry_wait = Duration::from_millis(50);
        }
    });
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "not the 408 of the request timeout"
    );
    assert_eq!(
        fake.requests().iter().map(|r| &r.url).collect::<Vec<_>>(),
        urls[..2].iter().collect::<Vec<_>>()
    );
}
