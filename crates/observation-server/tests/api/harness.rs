use std::time::Duration;

use axum::Router;
use axum::body::{Body, BodyDataStream, Bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use futures_util::StreamExt;
use observation_server::config::Config;
use observation_upstream::testing::{FakeHttp, FakeReply};
use tower::ServiceExt;

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

/// Longest [`Stream`] waits for one piece.
pub(crate) const PIECE_DEADLINE: Duration = Duration::from_secs(5);

/// Most a [`Stream`] reads in all. An event stream under paused time yields
/// keep-alives forever, so every read is bounded by this and the deadline.
pub(crate) const STREAM_CAP: usize = 64 * 1024;

/// Sends `req` and returns the head with the body unread: check the status
/// before reading, since a stream never ends.
pub(crate) async fn open(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Stream) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let stream = Stream {
        body: body.into_data_stream(),
        read: 0,
    };
    (parts.status, parts.headers, stream)
}

/// A response body read piece by piece, each read bounded by
/// [`PIECE_DEADLINE`] and all of them together by [`STREAM_CAP`]; a read
/// past either bound fails the test.
#[derive(Debug)]
pub(crate) struct Stream {
    body: BodyDataStream,
    read: usize,
}

impl Stream {
    /// The next piece, or `None` at the end.
    pub(crate) async fn next(&mut self) -> Option<Bytes> {
        let piece = tokio::time::timeout(PIECE_DEADLINE, self.body.next())
            .await
            .unwrap_or_else(|_| panic!("no piece within {PIECE_DEADLINE:?}"))?
            .unwrap();
        self.read += piece.len();
        assert!(
            self.read <= STREAM_CAP,
            "more than {STREAM_CAP} bytes read without finding what was expected"
        );
        Some(piece)
    }

    /// The next piece as text; the stream must not have ended.
    pub(crate) async fn next_text(&mut self) -> String {
        let piece = self.next().await.expect("the stream ended");
        String::from_utf8(piece.to_vec()).unwrap()
    }

    /// Reads until the text read by this call contains `needle`.
    pub(crate) async fn until(&mut self, needle: &str) -> String {
        let mut seen = String::new();
        while !seen.contains(needle) {
            seen.push_str(&self.next_text().await);
        }
        seen
    }

    /// Reads to the end.
    pub(crate) async fn rest(mut self) -> Vec<u8> {
        let mut all = Vec::new();
        while let Some(piece) = self.next().await {
            all.extend_from_slice(&piece);
        }
        all
    }
}
