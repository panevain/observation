//! Talking to the server: its published settings, the map tiles in flight
//! and cached, the area request that counts, and what a failed request says.
//!
//! The client sends the requests through the [`crate::platform::Http`] port
//! and feeds the replies back here.

use crate::document::LoadArea;
use crate::platform::{Clock, unix_secs};
use observation_core::geo::{MAX_BBOX_SIDE_M, MIN_BBOX_SIDE_M};
use observation_core::scene::AreaRef;
use observation_core::tiles::TileId;
use std::collections::{BTreeMap, BTreeSet};

/// Settings published by the server at `/api/config`. Until it answers, and
/// for any field it leaves out or gets wrong, this build's own values apply.
#[derive(Debug)]
pub struct ServerConfig {
    /// Attribution required by the configured tile provider.
    pub tile_attribution: String,
    /// Tile URL template with `{z}`, `{x}` and `{y}` placeholders.
    pub tile_template: String,
    /// Side limits of an area the server will load, metres (min < max).
    pub min_bbox_side_m: f64,
    pub max_bbox_side_m: f64,
    /// The server has answered (or failed to): tiles may be requested.
    answered: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            tile_attribution: "© OpenStreetMap contributors".to_owned(),
            tile_template: "/api/tiles/{z}/{x}/{y}.png".to_owned(),
            min_bbox_side_m: MIN_BBOX_SIDE_M,
            max_bbox_side_m: MAX_BBOX_SIDE_M,
            answered: false,
        }
    }
}

impl ServerConfig {
    /// Takes the server's `/api/config` reply, or `None` when the request
    /// failed: applies what is present and sane, and lets tiles be
    /// requested either way. Returns whether the server has local regions.
    pub fn answer(&mut self, body: Option<&[u8]>) -> bool {
        let v: Option<serde_json::Value> = body.and_then(|b| serde_json::from_slice(b).ok());
        if let Some(v) = &v {
            self.apply(v);
        }
        self.answered = true;
        v.as_ref()
            .and_then(|v| v.get("regions")?.as_bool())
            .unwrap_or(false)
    }

    /// Whether the server has answered (or failed to): tiles are cached by
    /// id, not URL, so none is requested before its template is known.
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// Takes the fields of a `/api/config` body that are present and sane.
    fn apply(&mut self, v: &serde_json::Value) {
        let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str);
        if let Some(a) = text("attribution").filter(|a| !a.is_empty()) {
            a.clone_into(&mut self.tile_attribution);
        }
        // A same-origin path with every placeholder. Browsers read `\` as `/`
        // and drop tabs and newlines, so `/\host` would leave the origin.
        if let Some(u) = text("tile_url").filter(|u| {
            u.starts_with('/')
                && !u.starts_with("//")
                && !u
                    .chars()
                    .any(|c| c == '\\' || c.is_whitespace() || c.is_control())
                && ["{z}", "{x}", "{y}"].iter().all(|p| u.contains(p))
        }) {
            u.clone_into(&mut self.tile_template);
        }
        // An older server publishes only the maximum.
        let side = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
        let min = side("min_bbox_side_m").unwrap_or(self.min_bbox_side_m);
        let max = side("max_bbox_side_m").unwrap_or(self.max_bbox_side_m);
        if min.is_finite() && max.is_finite() && 0.0 < min && min < max {
            self.min_bbox_side_m = min;
            self.max_bbox_side_m = max;
        }
    }
}

/// State of one map tile; `H` is the client's handle to the decoded image.
#[derive(Debug, Clone, PartialEq)]
pub enum TileState<H> {
    Pending,
    Ready(H),
    /// The request failed at this time (unix seconds); asked for again after
    /// [`TILE_RETRY_SECS`].
    Failed(u64),
}

/// Concurrent tile requests (the server limits upstream fetches too).
pub const MAX_IN_FLIGHT: usize = 6;
/// Soft cap on decoded tiles kept in memory.
pub const MAX_TILES: usize = 400;
/// Wait before asking again for a tile that failed (the server may be limiting
/// this client), so failed tiles are neither blank for good nor re-requested every frame.
pub const TILE_RETRY_SECS: u64 = 30;

/// Map tiles shared by the 3D ground and the 2D map picker: which are
/// loaded, failed or wanted, and how many requests are in flight.
#[derive(Debug)]
pub struct TileQueue<H> {
    tiles: BTreeMap<TileId, TileState<H>>,
    in_flight: usize,
    queue: Vec<TileId>,
    /// Tiles the 3D ground needs; the map picker never cancels these.
    pinned: BTreeSet<TileId>,
}

impl<H> Default for TileQueue<H> {
    fn default() -> Self {
        Self {
            tiles: BTreeMap::new(),
            in_flight: 0,
            queue: Vec::new(),
            pinned: BTreeSet::new(),
        }
    }
}

impl<H: Clone> TileQueue<H> {
    /// Returns the tile if loaded; queues it otherwise.
    pub fn get(&mut self, t: TileId, clock: &dyn Clock) -> Option<H> {
        let retry = match self.tiles.get(&t) {
            None => true,
            Some(TileState::Failed(at)) => unix_secs(clock) >= at + TILE_RETRY_SECS,
            Some(_) => false,
        };
        if retry {
            self.tiles.insert(t, TileState::Pending);
            self.queue.push(t);
        }
        self.ready(t)
    }

    /// Marks the tiles the 3D ground shows, replacing the previous set.
    pub fn pin(&mut self, tiles: impl IntoIterator<Item = TileId>) {
        self.pinned = tiles.into_iter().collect();
    }

    /// Returns the tile if loaded, without requesting it.
    pub fn ready(&self, t: TileId) -> Option<H> {
        match self.tiles.get(&t) {
            Some(TileState::Ready(h)) => Some(h.clone()),
            _ => None,
        }
    }

    /// Drops queued requests that are no longer wanted (e.g. after panning the
    /// map), except pinned ones.
    pub fn retain_queue(&mut self, keep: impl Fn(&TileId) -> bool) {
        let (tiles, pinned) = (&mut self.tiles, &self.pinned);
        self.queue.retain(|t| {
            let k = pinned.contains(t) || keep(t);
            if !k {
                tiles.remove(t);
            }
            k
        });
    }

    /// The next queued tile to request, newest first, while fewer than
    /// [`MAX_IN_FLIGHT`] are in flight; it counts as in flight from now.
    pub fn next_request(&mut self) -> Option<TileId> {
        if self.in_flight >= MAX_IN_FLIGHT {
            return None;
        }
        let t = self.queue.pop()?;
        self.in_flight += 1;
        Some(t)
    }

    /// A tile's reply arrived: [`TileState::Ready`] or [`TileState::Failed`].
    pub fn finish(&mut self, t: TileId, state: TileState<H>) {
        self.in_flight = self.in_flight.saturating_sub(1);
        self.tiles.insert(t, state);
    }

    /// Past [`MAX_TILES`], drops failed entries due for a retry; loaded
    /// tiles stay cached.
    pub fn evict(&mut self, clock: &dyn Clock) {
        if self.tiles.len() > MAX_TILES {
            let now = unix_secs(clock);
            self.tiles.retain(|_, s| match s {
                TileState::Failed(at) => now < *at + TILE_RETRY_SECS,
                TileState::Ready(_) | TileState::Pending => true,
            });
        }
    }
}

/// The area request that counts: only the latest one sent, until its reply
/// arrives or the user cancels it.
#[derive(Debug, Default)]
pub struct AreaRequests {
    area: Option<AreaRef>,
    /// The request in flight, for Retry.
    request: Option<LoadArea>,
    /// When it was sent, seconds since start-up.
    started: f64,
}

impl AreaRequests {
    /// The area being loaded.
    pub fn area(&self) -> Option<AreaRef> {
        self.area
    }

    /// The request in flight.
    pub fn request(&self) -> Option<&LoadArea> {
        self.request.as_ref()
    }

    /// When the request in flight was sent, seconds since start-up.
    pub fn started(&self) -> f64 {
        self.started
    }

    /// Sends `req` (`now` in seconds since start-up), superseding any request
    /// in flight.
    pub fn start(&mut self, req: LoadArea, now: f64) {
        self.area = Some(req.area);
        self.request = Some(req);
        self.started = now;
    }

    /// Stops waiting; the reply, when it comes, is ignored as superseded.
    pub fn cancel(&mut self) {
        self.area = None;
        self.request = None;
    }

    /// A reply for `area` arrived. Returns whether it is the one waited for,
    /// which then is no longer in flight; a superseded or cancelled one is
    /// to be ignored.
    pub fn finish(&mut self, area: AreaRef) -> bool {
        if self.area == Some(area) {
            self.cancel();
            true
        } else {
            false
        }
    }
}

/// What a failed HTTP reply means to the user: the server's own `error` text
/// when it sent one, else a description of the status (proxies and the request
/// timeout answer without one).
pub fn http_failure(status: u16, body: &str) -> String {
    let from_server = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
        .filter(|m| !m.is_empty());
    from_server.unwrap_or_else(|| match status {
        404 => "the server does not have this (HTTP 404); the app may be out of date, so reload the page".to_owned(),
        429 => "too many requests to the server (HTTP 429); wait a minute and try again".to_owned(),
        408 | 504 => format!("the server took too long to answer (HTTP {status}); try again shortly"),
        502 | 503 => format!("the server is unavailable right now (HTTP {status}); try again shortly"),
        500..=599 => format!("the server had a problem (HTTP {status}); try again shortly"),
        _ => format!("the server refused the request (HTTP {status})"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeClock;
    use observation_core::geo::BBox;
    use serde_json::json;

    fn applied(v: &serde_json::Value) -> ServerConfig {
        let mut c = ServerConfig::default();
        c.apply(v);
        c
    }

    #[test]
    fn takes_the_servers_limits_and_tile_url() {
        let c = applied(&json!({
            "attribution": "© Someone",
            "tile_url": "/tiles/{z}/{y}/{x}.png",
            "min_bbox_side_m": 50.0,
            "max_bbox_side_m": 1000.0,
        }));
        assert_eq!(c.tile_attribution, "© Someone");
        assert_eq!((c.min_bbox_side_m, c.max_bbox_side_m), (50.0, 1000.0));
        let t = TileId::new(18, 130_000, 87_000).unwrap();
        assert_eq!(t.url(&c.tile_template), "/tiles/18/87000/130000.png");
        assert_eq!(
            t.url(&ServerConfig::default().tile_template),
            "/api/tiles/18/130000/87000.png"
        );
    }

    #[test]
    fn an_older_server_without_a_minimum_keeps_ours() {
        let c = applied(&json!({ "max_bbox_side_m": 400.0 }));
        assert_eq!(
            (c.min_bbox_side_m, c.max_bbox_side_m),
            (MIN_BBOX_SIDE_M, 400.0)
        );
    }

    #[test]
    fn ignores_values_that_make_no_sense() {
        let d = ServerConfig::default();
        for v in [
            json!({ "min_bbox_side_m": 600.0, "max_bbox_side_m": 500.0 }),
            json!({ "min_bbox_side_m": 0.0 }),
            json!({ "max_bbox_side_m": -1.0 }),
            json!({ "max_bbox_side_m": 10.0 }),
            json!({ "min_bbox_side_m": 300.0, "max_bbox_side_m": 300.0 }),
            json!({ "max_bbox_side_m": "500" }),
            json!({ "tile_url": "https://elsewhere.example/{z}/{x}/{y}.png" }),
            json!({ "tile_url": "//elsewhere.example/{z}/{x}/{y}.png" }),
            json!({ "tile_url": "/\\elsewhere.example/{z}/{x}/{y}.png" }),
            json!({ "tile_url": "/\t/elsewhere.example/{z}/{x}/{y}.png" }),
            json!({ "tile_url": "/api/tiles/{z}/{x}.png" }),
            json!({ "attribution": "" }),
        ] {
            let c = applied(&v);
            assert_eq!(c.min_bbox_side_m, d.min_bbox_side_m, "{v}");
            assert_eq!(c.max_bbox_side_m, d.max_bbox_side_m, "{v}");
            assert_eq!(c.tile_template, d.tile_template, "{v}");
            assert_eq!(c.tile_attribution, d.tile_attribution, "{v}");
        }
    }

    #[test]
    fn the_config_reply_says_whether_the_server_has_regions() {
        let mut c = ServerConfig::default();
        assert!(!c.answered());
        assert!(c.answer(Some(br#"{"regions": true, "attribution": "X"}"#)));
        assert!(c.answered());
        assert_eq!(c.tile_attribution, "X");
        let mut c = ServerConfig::default();
        assert!(!c.answer(Some(br#"{"regions": "yes"}"#)));
        // Failed or unreadable: this build's values, and tiles may go.
        for body in [None, Some(&b"not json"[..])] {
            let mut c = ServerConfig::default();
            assert!(!c.answer(body));
            assert!(c.answered());
            assert_eq!(c.tile_template, ServerConfig::default().tile_template);
        }
    }

    fn tile(x: u32) -> TileId {
        TileId::new(18, 130_000 + x, 87_000).expect("valid tile")
    }

    #[test]
    fn tiles_are_queued_once_and_requested_newest_first_up_to_the_cap() {
        let clock = FakeClock::at(0.0);
        let mut q = TileQueue::<u8>::default();
        for x in 0..8 {
            assert_eq!(q.get(tile(x), &clock), None);
        }
        // Asking again does not queue it twice.
        q.get(tile(0), &clock);
        let sent: Vec<_> = std::iter::from_fn(|| q.next_request()).collect();
        assert_eq!(sent, (2..8).rev().map(tile).collect::<Vec<_>>());
        assert_eq!(sent.len(), MAX_IN_FLIGHT);
        // A reply frees a slot.
        q.finish(tile(7), TileState::Ready(7));
        assert_eq!(q.next_request(), Some(tile(1)));
        assert_eq!(q.next_request(), None);
        assert_eq!(q.ready(tile(7)), Some(7));
        assert_eq!(q.get(tile(7), &clock), Some(7));
    }

    #[test]
    fn a_failed_tile_is_asked_for_again_after_the_retry_wait() {
        let clock = FakeClock::at(1_000_000.0);
        let mut q = TileQueue::<u8>::default();
        q.get(tile(0), &clock);
        assert_eq!(q.next_request(), Some(tile(0)));
        q.finish(tile(0), TileState::Failed(unix_secs(&clock)));
        clock.advance((TILE_RETRY_SECS * 1000) as f64 - 1.0);
        q.get(tile(0), &clock);
        assert_eq!(q.next_request(), None, "not yet");
        clock.advance(1.0);
        q.get(tile(0), &clock);
        assert_eq!(q.next_request(), Some(tile(0)));
    }

    #[test]
    fn panning_drops_unwanted_queued_tiles_but_not_pinned_ones() {
        let clock = FakeClock::at(0.0);
        let mut q = TileQueue::<u8>::default();
        for x in 0..3 {
            q.get(tile(x), &clock);
        }
        q.pin([tile(0)]);
        q.retain_queue(|t| *t == tile(1));
        let sent: Vec<_> = std::iter::from_fn(|| q.next_request()).collect();
        assert_eq!(sent, [tile(1), tile(0)]);
        // The dropped tile is forgotten, so asking queues it again.
        q.get(tile(2), &clock);
        assert_eq!(q.next_request(), Some(tile(2)));
    }

    #[test]
    fn past_the_cap_only_failed_tiles_due_for_a_retry_are_dropped() {
        let clock = FakeClock::at(100_000_000.0);
        let now = unix_secs(&clock);
        let mut q = TileQueue::<u8>::default();
        let n = MAX_TILES as u32;
        for x in 0..n {
            q.finish(tile(x), TileState::Ready(0));
        }
        q.evict(&clock);
        assert_eq!(q.tiles.len(), MAX_TILES, "at the cap nothing goes");
        q.finish(tile(n), TileState::Failed(now - TILE_RETRY_SECS));
        q.finish(tile(n + 1), TileState::Failed(now - 1));
        q.finish(tile(n + 2), TileState::Pending);
        q.evict(&clock);
        assert_eq!(q.tiles.len(), MAX_TILES + 2);
        assert!(!q.tiles.contains_key(&tile(n)));
        assert_eq!(q.tiles.get(&tile(n + 1)), Some(&TileState::Failed(now - 1)));
    }

    #[test]
    fn only_the_latest_area_request_counts() {
        let osm = AreaRef::Osm {
            bbox: BBox::new(51.0, -0.1, 51.001, -0.099).expect("valid box"),
        };
        let mut a = AreaRequests::default();
        a.start(LoadArea::demo(), 1.5);
        a.start(
            LoadArea {
                area: osm,
                keep_cameras: true,
            },
            2.0,
        );
        assert_eq!((a.area(), a.started()), (Some(osm), 2.0));
        assert!(a.request().is_some_and(|r| r.keep_cameras));
        // The demo's reply is superseded; the latest one is taken once.
        assert!(!a.finish(AreaRef::Demo));
        assert!(a.finish(osm));
        assert_eq!((a.area(), a.request()), (None, None));
        assert!(!a.finish(osm));
    }

    #[test]
    fn a_cancelled_request_ignores_its_reply() {
        let mut a = AreaRequests::default();
        a.start(LoadArea::demo(), 3.0);
        a.cancel();
        assert_eq!(a.area(), None);
        assert!(!a.finish(AreaRef::Demo));
    }

    #[test]
    fn a_failure_shows_the_servers_message_or_explains_the_status() {
        assert_eq!(
            http_failure(400, r#"{"error": "the area is too large"}"#),
            "the area is too large"
        );
        let cases = [
            (
                404,
                "the server does not have this (HTTP 404); the app may be out of date, so reload the page",
            ),
            (
                429,
                "too many requests to the server (HTTP 429); wait a minute and try again",
            ),
            (
                408,
                "the server took too long to answer (HTTP 408); try again shortly",
            ),
            (
                504,
                "the server took too long to answer (HTTP 504); try again shortly",
            ),
            (
                502,
                "the server is unavailable right now (HTTP 502); try again shortly",
            ),
            (
                503,
                "the server is unavailable right now (HTTP 503); try again shortly",
            ),
            (
                500,
                "the server had a problem (HTTP 500); try again shortly",
            ),
            (
                599,
                "the server had a problem (HTTP 599); try again shortly",
            ),
            (403, "the server refused the request (HTTP 403)"),
        ];
        for (status, text) in cases {
            // No body, a body that is not JSON, or an empty `error`.
            for body in ["", "<html>Bad gateway</html>", r#"{"error": ""}"#] {
                assert_eq!(http_failure(status, body), text, "{status} {body}");
            }
        }
    }
}
