//! The Overpass client's retry, failover, cool-down, budget and error
//! mapping, on a fake upstream under paused time.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use observation_core::geo::BBox;
use observation_upstream::testing::{FakeHttp, FakeReply};
use observation_upstream::{RequestBody, TransportErrorKind};
use tokio::time::Instant;

use super::{MAX_RESPONSE_BYTES, OverpassClient, build_query};
use crate::config::Config;
use crate::error::ApiError;

const FIXTURE: &str = include_str!("../../fixtures/demo-overpass.json");
const URLS: [&str; 4] = [
    "http://a.example/api/interpreter",
    "http://b.example/api/interpreter",
    "http://c.example/api/interpreter",
    "http://d.example/api/interpreter",
];

/// The production timings: 2 s retry pause, 30 s cool-down, 35 s attempts,
/// an 80 s request (so a 72 s fetch budget).
fn setup(urls: &[&str]) -> (OverpassClient, FakeHttp) {
    let mut config = Config::for_tests("cache", "dist", URLS[0], "http://t/");
    config.overpass_urls = urls.iter().map(|u| (*u).to_owned()).collect();
    config.overpass_retry_wait = Duration::from_secs(2);
    config.overpass_cooldown = Duration::from_secs(30);
    config.overpass_timeout = Duration::from_secs(35);
    config.request_timeout = Duration::from_secs(80);
    let fake = FakeHttp::new();
    (OverpassClient::new(Arc::new(fake.clone()), &config), fake)
}

fn bbox() -> BBox {
    BBox::new(47.7075, -122.208, 47.7098, -122.204).unwrap()
}

fn on(fake: &FakeHttp, url: &str, reply: FakeReply) {
    fake.on(Method::POST, url, reply);
}

fn ok() -> FakeReply {
    FakeReply::ok(FIXTURE)
}

fn status(code: u16) -> FakeReply {
    FakeReply::status(StatusCode::from_u16(code).unwrap())
}

fn urls_asked(fake: &FakeHttp) -> Vec<String> {
    fake.requests().into_iter().map(|r| r.url).collect()
}

fn message(e: &ApiError) -> String {
    match e {
        ApiError::Upstream(m) | ApiError::UpstreamTimeout(m) => m.clone(),
        other => format!("{other:?}"),
    }
}

const UNREACHABLE: &str = "could not reach the map data provider; try again shortly";
const TOO_SLOW: &str =
    "the map data provider took too long to answer; try again shortly, or draw a smaller area";

#[tokio::test(start_paused = true)]
async fn requests_are_capped_form_posts_with_the_attempt_timeout() {
    let (client, fake) = setup(&URLS[..1]);
    on(&fake, URLS[0], ok());
    let data = client.fetch(&bbox()).await.unwrap();
    assert!(!data.buildings.is_empty());
    let [req] = fake.requests().try_into().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.url, URLS[0]);
    assert_eq!(
        req.body,
        RequestBody::Form(vec![("data".into(), build_query(&bbox()))])
    );
    assert_eq!(
        req.wire_parts().0[header::CONTENT_TYPE],
        "application/x-www-form-urlencoded"
    );
    assert_eq!(req.timeout, Some(Duration::from_secs(35)));
    assert_eq!(req.max_bytes, 32 * 1024 * 1024);
    assert_eq!(req.error_max_bytes, 0, "error bodies are never read");
}

/// A transport failure of each kind, twice on a lone interpreter (one retry
/// after the 2 s pause): only a timeout after connecting is "too slow".
#[tokio::test(start_paused = true)]
async fn transport_failures_are_classified_by_kind() {
    for (kind, want) in [
        (TransportErrorKind::Timeout, TOO_SLOW),
        (TransportErrorKind::ConnectTimeout, UNREACHABLE),
        (TransportErrorKind::Connect, UNREACHABLE),
        (TransportErrorKind::Body, UNREACHABLE),
        (TransportErrorKind::Other, UNREACHABLE),
    ] {
        let (client, fake) = setup(&URLS[..1]);
        on(&fake, URLS[0], FakeReply::error(kind));
        on(&fake, URLS[0], FakeReply::error(kind));
        let started = Instant::now();
        let e = client.fetch(&bbox()).await.unwrap_err();
        assert_eq!(message(&e), want, "{kind:?}");
        let status = if kind == TransportErrorKind::Timeout {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::BAD_GATEWAY
        };
        assert_eq!(e.status(), status, "{kind:?}");
        assert_eq!(fake.requests().len(), 2, "{kind:?}");
        assert_eq!(started.elapsed(), Duration::from_secs(2), "{kind:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn failures_move_on_to_the_next_interpreter_in_order() {
    let (client, fake) = setup(&URLS);
    on(&fake, URLS[0], status(502));
    on(
        &fake,
        URLS[1],
        FakeReply::error(TransportErrorKind::Connect),
    );
    on(&fake, URLS[2], ok());
    let started = Instant::now();
    assert!(client.fetch(&bbox()).await.is_ok());
    assert_eq!(urls_asked(&fake), URLS[..3]);
    assert_eq!(started.elapsed(), Duration::from_secs(4), "two 2 s pauses");
    fake.assert_drained();

    // Each of several interpreters is tried once.
    let (client, fake) = setup(&URLS);
    for url in URLS {
        on(&fake, url, status(503));
    }
    let e = client.fetch(&bbox()).await.unwrap_err();
    assert_eq!(message(&e), UNREACHABLE);
    assert_eq!(urls_asked(&fake), URLS);
}

/// 429 and 504 mean "busy": the interpreter rests for 30 s unless this fetch
/// retries it; `Retry-After` is not read. 502 and 503 are plain failures.
#[tokio::test(start_paused = true)]
async fn busy_interpreters_rest_and_retry_after_is_ignored() {
    for busy in [
        status(429),
        status(429).header(header::RETRY_AFTER, "120"),
        status(504),
    ] {
        let (client, fake) = setup(&URLS[..1]);
        on(&fake, URLS[0], busy.clone());
        on(&fake, URLS[0], busy);
        let e = client.fetch(&bbox()).await.unwrap_err();
        assert!(
            matches!(
                e,
                ApiError::RateLimited {
                    retry_after_secs: 30
                }
            ),
            "{e:?}"
        );
        assert_eq!(e.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(fake.requests().len(), 2);
        // Resting: refused at once, without a request, until the 30 s are up.
        tokio::time::advance(Duration::from_secs(29)).await;
        let started = Instant::now();
        assert!(matches!(
            client.fetch(&bbox()).await,
            Err(ApiError::RateLimited {
                retry_after_secs: 1
            })
        ));
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(fake.requests().len(), 2);
        tokio::time::advance(Duration::from_secs(1)).await;
        on(&fake, URLS[0], ok());
        assert!(client.fetch(&bbox()).await.is_ok());
    }
    let (client, fake) = setup(&URLS[..1]);
    on(
        &fake,
        URLS[0],
        status(503).header(header::RETRY_AFTER, "120"),
    );
    on(&fake, URLS[0], status(503));
    let e = client.fetch(&bbox()).await.unwrap_err();
    assert_eq!(message(&e), UNREACHABLE);
    assert!(client.check_cooldown().is_ok(), "503 starts no cool-down");
}

#[tokio::test(start_paused = true)]
async fn a_busy_interpreter_rests_while_the_others_serve() {
    let (client, fake) = setup(&URLS[..2]);
    on(&fake, URLS[0], status(429));
    on(&fake, URLS[1], ok());
    assert!(client.fetch(&bbox()).await.is_ok());
    on(&fake, URLS[1], ok());
    assert!(client.fetch(&bbox()).await.is_ok());
    assert_eq!(urls_asked(&fake), [URLS[0], URLS[1], URLS[1]]);
    // Busy then failed: the failure is reported, the busy one still rests.
    tokio::time::advance(Duration::from_secs(30)).await;
    on(&fake, URLS[0], status(504));
    on(&fake, URLS[1], status(502));
    let e = client.fetch(&bbox()).await.unwrap_err();
    assert_eq!(message(&e), UNREACHABLE);
    assert!(client.cooling(0).is_some() && client.cooling(1).is_none());
}

/// Each attempt gets at most 35 s and the whole fetch 72 s, the time spent
/// before Overpass included.
#[tokio::test(start_paused = true)]
async fn attempts_and_the_whole_fetch_are_timed() {
    let (client, fake) = setup(&URLS[..1]);
    on(&fake, URLS[0], FakeReply::hang());
    on(&fake, URLS[0], FakeReply::hang());
    let started = Instant::now();
    let e = client.fetch(&bbox()).await.unwrap_err();
    assert_eq!(message(&e), TOO_SLOW);
    let timeouts: Vec<_> = fake.requests().iter().map(|r| r.timeout).collect();
    // 35 s, the pause, then what is left of the 72 s.
    assert_eq!(
        timeouts,
        [Some(Duration::from_secs(35)), Some(Duration::from_secs(35))]
    );
    assert_eq!(started.elapsed(), Duration::from_secs(72));

    // 65 s already spent elsewhere: one shortened attempt, and no retry once
    // less than a tenth of an attempt (3.5 s) is left.
    let (client, fake) = setup(&URLS[..1]);
    on(&fake, URLS[0], FakeReply::hang());
    let begun = Instant::now();
    tokio::time::advance(Duration::from_secs(65)).await;
    let e = client.fetch_since(&bbox(), begun).await.unwrap_err();
    assert_eq!(message(&e), TOO_SLOW);
    let [req] = fake.requests().try_into().unwrap();
    assert_eq!(req.timeout, Some(Duration::from_secs(7)));
    assert_eq!(begun.elapsed(), Duration::from_secs(74));
}

#[tokio::test(start_paused = true)]
async fn bodies_and_statuses_that_are_not_retried() {
    let fatal = |reply: FakeReply| async move {
        let (client, fake) = setup(&URLS[..2]);
        on(&fake, URLS[0], reply);
        let e = client.fetch(&bbox()).await.unwrap_err();
        assert_eq!(fake.requests().len(), 1, "not retried");
        assert_eq!(e.status(), StatusCode::BAD_GATEWAY);
        message(&e)
    };
    assert_eq!(
        fatal(FakeReply::ok(vec![b' '; MAX_RESPONSE_BYTES + 1])).await,
        "this area has more map data than the server accepts; draw a smaller area"
    );
    assert_eq!(
        fatal(status(418).body("I'm a teapot")).await,
        "the map data provider answered with an error (HTTP 418)"
    );
    assert_eq!(
        fatal(FakeReply::ok(
            r#"{"elements":[],"remark":"runtime error: out of memory"}"#
        ))
        .await,
        "the map data provider could not process this area; try a smaller area"
    );
    assert_eq!(
        fatal(FakeReply::ok("<html>")).await,
        "the map data provider sent a reply the server could not read; try again shortly"
    );
}

#[tokio::test(start_paused = true)]
async fn timed_out_remarks_and_redirects_are_retried() {
    let (client, fake) = setup(&URLS[..1]);
    on(
        &fake,
        URLS[0],
        FakeReply::ok(r#"{"elements":[],"remark":"runtime error: Query timed out in \"query\""}"#),
    );
    on(&fake, URLS[0], ok());
    assert!(client.fetch(&bbox()).await.is_ok());
    assert_eq!(fake.requests().len(), 2);

    let (client, fake) = setup(&URLS[..1]);
    let redirect = || status(307).header(header::LOCATION, "http://elsewhere.example/");
    on(&fake, URLS[0], redirect());
    on(&fake, URLS[0], redirect());
    let e = client.fetch(&bbox()).await.unwrap_err();
    assert_eq!(message(&e), UNREACHABLE);
    assert_eq!(urls_asked(&fake), [URLS[0], URLS[0]], "not followed");
}

/// One fetch in flight and three waiting; a fifth is refused at once with a
/// 10 s retry hint (HTTP 503 with `Retry-After: 10`).
#[tokio::test(start_paused = true)]
async fn a_full_queue_refuses_at_once() {
    let (client, fake) = setup(&URLS[..1]);
    for _ in 0..4 {
        on(&fake, URLS[0], ok().delay(Duration::from_secs(10)));
    }
    let b = bbox();
    let fetches = (0..5).map(|_| client.fetch(&b));
    let results = futures_util::future::join_all(fetches).await;
    let refused: Vec<_> = results
        .iter()
        .filter_map(|r| match r {
            Err(ApiError::RateLimited { retry_after_secs }) => Some(*retry_after_secs),
            _ => None,
        })
        .collect();
    assert_eq!(refused, [10]);
    assert!(matches!(results[4], Err(ApiError::RateLimited { .. })));
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 4);
    assert_eq!(fake.requests().len(), 4);
}

/// Counts `observation_overpass_fetches_total` by its labels (the
/// workspace has no debugging recorder outside the client's dependency tree).
#[derive(Debug, Default)]
struct FetchCounts(std::sync::Mutex<std::collections::BTreeMap<String, Arc<CountFn>>>);

#[derive(Debug, Default)]
struct CountFn(std::sync::atomic::AtomicU64);

impl metrics::CounterFn for CountFn {
    fn increment(&self, value: u64) {
        self.0.fetch_add(value, std::sync::atomic::Ordering::SeqCst);
    }
    fn absolute(&self, value: u64) {
        self.0.store(value, std::sync::atomic::Ordering::SeqCst);
    }
}

impl metrics::Recorder for FetchCounts {
    fn describe_counter(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_gauge(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_histogram(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn register_counter(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Counter {
        if key.name() != crate::metrics::OVERPASS_FETCH {
            return metrics::Counter::noop();
        }
        let labels: Vec<String> = key
            .labels()
            .map(|l| format!("{}={}", l.key(), l.value()))
            .collect();
        let mut map = self.0.lock().unwrap();
        let counter = map.entry(labels.join(",")).or_default();
        metrics::Counter::from_arc(Arc::clone(counter))
    }
    fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
        metrics::Gauge::noop()
    }
    fn register_histogram(
        &self,
        _: &metrics::Key,
        _: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        metrics::Histogram::noop()
    }
}

#[tokio::test(start_paused = true)]
async fn fetch_outcomes_are_counted() {
    let recorder = FetchCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let (client, fake) = setup(&URLS[..1]);
    on(&fake, URLS[0], ok());
    on(&fake, URLS[0], status(418));
    on(&fake, URLS[0], status(429));
    on(&fake, URLS[0], status(429));
    assert!(client.fetch(&bbox()).await.is_ok());
    assert!(client.fetch(&bbox()).await.is_err());
    assert!(client.fetch(&bbox()).await.is_err());
    // Refused while resting: counted as rate limited too.
    assert!(client.fetch(&bbox()).await.is_err());
    let counts: Vec<(String, u64)> = recorder
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.0.load(std::sync::atomic::Ordering::SeqCst)))
        .collect();
    assert_eq!(
        counts,
        [
            ("outcome=error".to_owned(), 1),
            ("outcome=ok".to_owned(), 1),
            ("outcome=rate_limited".to_owned(), 2),
        ]
    );
}
