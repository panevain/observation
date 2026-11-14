//! `/api/geocode`: off by default, Nominatim's usage policy when on (an
//! identifying `User-Agent`, one request a second, cached answers), per-client
//! limits and the mapping of upstream failures.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use observation_server::config::Config;
use observation_server::geocoder::{CACHE_TTL, COOLDOWN, MIN_INTERVAL};
use observation_server::ratelimit::Rate;
use observation_server::testing::TestApp;
use observation_upstream::TransportErrorKind;
use observation_upstream::testing::{FakeHttp, FakeReply};
use tokio::time::Instant;

use crate::harness::{get_uri, json, send};

const ENDPOINT: &str = "https://nominatim.example/search";

/// The upstream URL for the normalised query `q` (already form-encoded).
fn upstream(q: &str) -> String {
    format!("{ENDPOINT}?q={q}&format=jsonv2&limit=5")
}

/// A Nominatim answer with one place.
fn answer(name: &str) -> FakeReply {
    FakeReply::ok(format!(
        r#"[{{"place_id": 1, "display_name": "{name}", "lat": "51.5007", "lon": "-0.1246",
             "boundingbox": ["51.50", "51.51", "-0.13", "-0.12"], "licence": "ODbL"}}]"#
    ))
    .header(header::CONTENT_TYPE, "application/json")
}

/// An app with the search on, over `fake`, and `tweak`.
fn app(fake: FakeHttp, tweak: impl FnOnce(&mut Config) + 'static) -> TestApp {
    TestApp::builder()
        .geocoder(fake)
        .config(|c| c.geocoder.url = Some(ENDPOINT.into()))
        .config(tweak)
        .build()
}

/// A search for `q` (form-encoded) from `peer`.
fn search(q: &str, peer: &str) -> Request<Body> {
    let mut req = Request::builder()
        .uri(format!("/api/geocode?q={q}"))
        .body(Body::empty())
        .unwrap();
    let peer = SocketAddr::new(peer.parse().unwrap(), 40000);
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    req
}

const A: &str = "192.0.2.1";
const B: &str = "192.0.2.2";

#[tokio::test(start_paused = true)]
async fn the_search_is_off_by_default() {
    let app = TestApp::builder().build();
    let (_, _, body) = get_uri(&app.router, "/api/config").await;
    assert_eq!(json(&body)["geocoder"], serde_json::Value::Null);
    let (status, _, body) = send(&app.router, search("london", A)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        json(&body)["error"],
        "place search is not turned on on this server"
    );
    assert!(app.geocoder.requests().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_config_announces_the_search_and_its_credit() {
    let app = app(FakeHttp::new(), |c| {
        c.geocoder.attribution = "Search: somebody".into();
    });
    let (_, _, body) = get_uri(&app.router, "/api/config").await;
    assert_eq!(
        json(&body)["geocoder"],
        serde_json::json!({"attribution": "Search: somebody", "max_query_chars": 200})
    );
}

#[tokio::test(start_paused = true)]
async fn answers_are_passed_on_with_credit_and_cached() {
    let fake = FakeHttp::new();
    fake.on(
        Method::GET,
        &upstream("high+street%2C+london"),
        answer("High Street"),
    );
    fake.on(
        Method::GET,
        &upstream("high+street%2C+london"),
        answer("High Street"),
    );
    let app = app(fake.clone(), |_| {});

    let (status, headers, body) = send(&app.router, search("High%20Street,%20London", A)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        json(&body),
        serde_json::json!({
            "results": [{"label": "High Street", "lat": 51.5007, "lon": -0.1246,
                         "bbox": [51.50, -0.13, 51.51, -0.12]}],
            "attribution": observation_server::config::DEFAULT_GEOCODER_ATTRIBUTION,
        })
    );

    // The same search in other case and spacing, from anyone, is answered
    // from the cache.
    tokio::time::advance(CACHE_TTL - Duration::from_secs(1)).await;
    for (q, peer) in [
        ("high%20%20street,%20london", B),
        ("%20HIGH%20STREET,%20LONDON", A),
    ] {
        let (status, _, again) = send(&app.router, search(q, peer)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again, body);
    }
    assert_eq!(fake.requests().len(), 1);

    // Until it expires.
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        send(&app.router, search("high%20street,%20london", A))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(fake.requests().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn requests_identify_the_app_and_the_operator_only() {
    let fake = FakeHttp::new();
    fake.on(
        Method::GET,
        &format!("{}&email=ops%40example.org", upstream("kirkland")),
        answer("Kirkland"),
    );
    let app = app(fake.clone(), |c| {
        c.geocoder.contact = Some("ops@example.org".into());
    });
    let mut req = search("kirkland", A);
    req.headers_mut().insert(
        header::REFERER,
        "https://visitor.example/page".parse().unwrap(),
    );
    req.headers_mut()
        .insert(header::USER_AGENT, "VisitorBrowser/1.0".parse().unwrap());
    req.headers_mut()
        .insert(header::ACCEPT_LANGUAGE, "de".parse().unwrap());
    assert_eq!(send(&app.router, req).await.0, StatusCode::OK);

    let sent = fake.requests();
    assert_eq!(sent.len(), 1);
    let headers = sent[0].headers();
    assert_eq!(
        headers[header::USER_AGENT],
        format!(
            "observation/{} (+https://github.com/panevain/observation; ops@example.org)",
            env!("CARGO_PKG_VERSION")
        )
    );
    assert_eq!(headers[header::ACCEPT], "application/json");
    // Nothing of the visitor's goes upstream.
    let names: Vec<_> = headers.keys().map(|k| k.as_str().to_owned()).collect();
    assert_eq!(names, ["user-agent", "accept"]);
}

#[tokio::test(start_paused = true)]
async fn queries_are_checked_before_anything_goes_upstream() {
    let app = app(FakeHttp::new(), |_| {});
    let long = "a".repeat(201);
    for uri in [
        "/api/geocode".to_owned(),
        "/api/geocode?q=".to_owned(),
        "/api/geocode?q=%20x%20".to_owned(),
        "/api/geocode?q=a%00b".to_owned(),
        format!("/api/geocode?q={long}"),
    ] {
        let (status, _, body) = get_uri(&app.router, &uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(json(&body)["error"].as_str().is_some(), "{uri}");
    }
    assert!(app.geocoder.requests().is_empty());
    // 200 characters is fine.
    app.geocoder.on(
        Method::GET,
        &upstream(&"a".repeat(200)),
        FakeReply::ok("[]"),
    );
    let (status, _, body) =
        get_uri(&app.router, &format!("/api/geocode?q={}", "a".repeat(200))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["results"], serde_json::json!([]));
}

#[tokio::test(start_paused = true)]
async fn the_server_sends_at_most_one_request_a_second() {
    let starts = Arc::new(Mutex::new(Vec::new()));
    let fake = FakeHttp::from_fn({
        let starts = Arc::clone(&starts);
        move |_| {
            starts.lock().unwrap().push(Instant::now());
            FakeReply::ok("[]").delay(Duration::from_millis(200))
        }
    });
    let app = app(fake, |_| {});
    // Five different searches from five clients at once.
    let searches = (0..5).map(|n| {
        let router = app.router.clone();
        async move {
            send(
                &router,
                search(&format!("place{n}"), &format!("192.0.2.{}", n + 10)),
            )
            .await
            .0
        }
    });
    let statuses = futures_util::future::join_all(searches).await;
    assert!(
        statuses.iter().all(|s| *s == StatusCode::OK),
        "{statuses:?}"
    );
    let starts = starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 5);
    for pair in starts.windows(2) {
        assert!(pair[1] - pair[0] >= MIN_INTERVAL, "{:?}", pair[1] - pair[0]);
    }
}

#[tokio::test(start_paused = true)]
async fn searches_are_limited_per_client_but_cached_ones_are_not() {
    let fake = FakeHttp::from_fn(|_| FakeReply::ok("[]"));
    let app = app(fake.clone(), |c| {
        c.geocoder.rate = Rate {
            per_minute: 1,
            burst: 2,
        };
    });
    for q in ["one", "two"] {
        assert_eq!(send(&app.router, search(q, A)).await.0, StatusCode::OK);
    }
    let (status, headers, body) = send(&app.router, search("three", A)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let secs: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&secs), "{secs}");
    assert!(
        json(&body)["error"]
            .as_str()
            .unwrap()
            .starts_with("too many place searches from your address"),
    );
    assert_eq!(fake.requests().len(), 2, "refused before the geocoder");
    // A cached search is free; another client has its own allowance.
    assert_eq!(send(&app.router, search("one", A)).await.0, StatusCode::OK);
    assert_eq!(
        send(&app.router, search("three", B)).await.0,
        StatusCode::OK
    );
    assert_eq!(fake.requests().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn upstream_failures_are_mapped_and_not_cached() {
    let fake = FakeHttp::new();
    let url = upstream("somewhere");
    for reply in [
        FakeReply::status(StatusCode::INTERNAL_SERVER_ERROR).body("oops"),
        FakeReply::status(StatusCode::FOUND).header(header::LOCATION, "https://elsewhere.example/"),
        FakeReply::error(TransportErrorKind::Connect),
        FakeReply::hang(),
        FakeReply::ok("<html>not json</html>"),
        FakeReply::ok(vec![b' '; 300 * 1024]),
        answer("Somewhere"),
    ] {
        fake.on(Method::GET, &url, reply);
    }
    let app = app(fake.clone(), |c| {
        c.geocoder.rate = Rate {
            per_minute: 0,
            burst: 1,
        };
    });
    let expected = [
        (
            StatusCode::BAD_GATEWAY,
            "the place search answered with an error (HTTP 500)",
        ),
        (
            StatusCode::BAD_GATEWAY,
            "the place search answered with an error (HTTP 302)",
        ),
        (StatusCode::BAD_GATEWAY, "could not reach the place search"),
        (
            StatusCode::GATEWAY_TIMEOUT,
            "the place search did not answer in time",
        ),
        (
            StatusCode::BAD_GATEWAY,
            "the place search sent an answer that could not be read",
        ),
        (
            StatusCode::BAD_GATEWAY,
            "the place search sent an answer that could not be read",
        ),
    ];
    for (status, message) in expected {
        let (got, _, body) = send(&app.router, search("somewhere", A)).await;
        assert_eq!(
            (got, json(&body)["error"].as_str().unwrap()),
            (status, message)
        );
    }
    // Each failure went upstream again; the answer finally comes.
    let (status, _, body) = send(&app.router, search("somewhere", A)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["results"][0]["label"], "Somewhere");
    assert_eq!(fake.requests().len(), 7);
}

#[tokio::test(start_paused = true)]
async fn a_limiting_geocoder_pauses_every_search() {
    let fake = FakeHttp::new();
    fake.on(
        Method::GET,
        &upstream("first"),
        FakeReply::status(StatusCode::TOO_MANY_REQUESTS),
    );
    fake.on(Method::GET, &upstream("second"), answer("Second"));
    let app = app(fake.clone(), |_| {});
    let (status, headers, body) = send(&app.router, search("first", A)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers[header::RETRY_AFTER], COOLDOWN.as_secs().to_string());
    assert_eq!(
        json(&body)["error"],
        format!(
            "the place search is busy; try again in about {} s",
            COOLDOWN.as_secs()
        )
    );
    // Other searches fail at once during the cooldown, without a request.
    let (status, headers, _) = send(&app.router, search("second", B)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(headers.contains_key(header::RETRY_AFTER));
    assert_eq!(fake.requests().len(), 1);
    tokio::time::advance(COOLDOWN).await;
    assert_eq!(
        send(&app.router, search("second", B)).await.0,
        StatusCode::OK
    );
    assert_eq!(fake.requests().len(), 2);
}
