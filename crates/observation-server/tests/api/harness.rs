use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use observation_server::config::Config;
use observation_server::{AppState, app};
use observation_upstream::testing::{FakeHttp, FakeReply};
use tower::ServiceExt;

pub(crate) const BBOX: &str = include_str!("../../fixtures/demo-bbox.txt");

pub(crate) struct Harness {
    pub(crate) app: Router,
    pub(crate) cache: tempfile::TempDir,
    _static: tempfile::TempDir,
}

pub(crate) fn harness_with(
    overpass_url: &str,
    tile_url: &str,
    static_dir: tempfile::TempDir,
    tweak: impl FnOnce(&mut Config),
) -> Harness {
    let cache = tempfile::tempdir().unwrap();
    let mut config = Config::for_tests(cache.path(), static_dir.path(), overpass_url, tile_url);
    tweak(&mut config);
    let app = app(AppState::new(config).unwrap());
    Harness {
        app,
        cache,
        _static: static_dir,
    }
}

pub(crate) fn harness(overpass_url: &str, tile_url: &str) -> Harness {
    harness_with(overpass_url, tile_url, tempfile::tempdir().unwrap(), |_| {})
}

/// A harness whose upstreams are unreachable (for routes that never call them).
pub(crate) fn offline() -> Harness {
    harness(
        "http://127.0.0.1:9/overpass",
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
    )
}

pub(crate) async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Bytes) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    (
        parts.status,
        parts.headers,
        axum::body::to_bytes(body, usize::MAX).await.unwrap(),
    )
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

/// The Overpass endpoint of the production configuration, which a
/// [`TestApp`](observation_server::testing::TestApp) keeps unless a test adds
/// others.
pub(crate) const OVERPASS: &str = observation_server::config::DEFAULT_OVERPASS_URL;

/// Overpass answering the demo fixture.
pub(crate) fn overpass_ok() -> FakeReply {
    FakeReply::ok(crate::mocks::FIXTURE)
}

/// Overpass answering `code`, with a body as an interpreter might send.
pub(crate) fn overpass_status(code: u16) -> FakeReply {
    FakeReply::status(StatusCode::from_u16(code).unwrap()).body("upstream says no")
}

/// How many requests `fake` received for `url`.
pub(crate) fn hits(fake: &FakeHttp, url: &str) -> usize {
    fake.requests().iter().filter(|r| r.url == url).count()
}

/// A [`TestApp`](observation_server::testing::TestApp) on the production
/// configuration without elevation (`OBS_DEM_URL=off`), so that areas need
/// no terrain tiles, and with `tweak`.
pub(crate) fn flat_app(
    overpass: FakeHttp,
    tweak: impl FnOnce(&mut Config) + 'static,
) -> observation_server::testing::TestApp {
    observation_server::testing::TestApp::builder()
        .overpass(overpass)
        .config(|c| c.dem_url = None)
        .config(tweak)
        .build()
}

/// The production tile server's URL for `z/x/y`.
pub(crate) fn tile_url(z: u32, x: u32, y: u32) -> String {
    format!("https://tile.openstreetmap.org/{z}/{x}/{y}.png")
}

/// A tile as the socket mock served it: a PNG with an ETag, a
/// Last-Modified date and `max-age`.
pub(crate) fn tile_png(max_age: u64) -> FakeReply {
    FakeReply::ok(crate::mocks::PNG)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ETAG, "\"tile-etag\"")
        .header(header::LAST_MODIFIED, "Mon, 01 Jan 2024 00:00:00 GMT")
        .header(header::CACHE_CONTROL, format!("max-age={max_age}"))
}
