use axum::http::{StatusCode, header};
use observation_server::config::Config;
use observation_upstream::testing::{FakeHttp, FakeReply};

pub(crate) use crate::common::{area_uri, get_uri, json, send};

/// The Overpass endpoint of the production configuration, which a
/// [`TestApp`](observation_server::testing::TestApp) keeps unless a test adds
/// others.
pub(crate) const OVERPASS: &str = observation_server::config::DEFAULT_OVERPASS_URL;

/// Overpass answering the demo fixture.
pub(crate) fn overpass_ok() -> FakeReply {
    FakeReply::ok(crate::common::FIXTURE)
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
    FakeReply::ok(crate::common::PNG)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ETAG, "\"tile-etag\"")
        .header(header::LAST_MODIFIED, "Mon, 01 Jan 2024 00:00:00 GMT")
        .header(header::CACHE_CONTROL, format!("max-age={max_age}"))
}
