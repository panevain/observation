//! Fetching bundles and tiles from the server, and following its local regions.
//!
//! Requests run on the browser's event loop (`fetch`, `EventSource`) and report
//! back through a channel that a system drains every frame.

use crate::regions::{CatalogState, RegionRequest, Regions};
use crate::state::{AreaLoaded, LoadArea, Loading, ToastAction, UiState};
use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_core::bundle::AreaBundle;
use observation_core::regions::ActionKind;
use observation_core::scene::AreaRef;
use observation_core::tiles::TileId;

#[derive(Debug, Clone)]
enum Request {
    Config,
    Area(LoadArea),
    Tile(TileId),
    /// A status from the region event stream (an error: the stream dropped).
    RegionsStatus,
    RegionsCatalog,
    RegionAction(ActionKind, String),
}

/// Settings published by the server at `/api/config`.
#[derive(Debug, Resource)]
pub struct ServerConfig {
    /// Attribution required by the configured tile provider.
    pub tile_attribution: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            tile_attribution: "© OpenStreetMap contributors".to_owned(),
        }
    }
}

type Reply = (Request, Result<Vec<u8>, String>);

#[derive(Resource)]
pub struct Net {
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
}

impl Default for Net {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx }
    }
}

impl Net {
    fn get(&self, url: String, req: Request) {
        self.send(HttpRequest::get(url), req);
    }

    fn send(&self, http: HttpRequest, req: Request) {
        let tx = self.tx.clone();
        spawn_fetch(http, move |res| {
            // The receiver lives as long as the app; a send error means shutdown.
            let _ = tx.send((req, res));
        });
    }

    /// Follows `/api/regions/events`; every status (and every drop of the
    /// stream, which the browser reconnects) arrives as a `RegionsStatus` reply.
    fn follow_regions(&self) {
        let tx = self.tx.clone();
        spawn_event_source("/api/regions/events", "regions", move |res| {
            let _ = tx.send((Request::RegionsStatus, res));
        });
    }
}

/// One HTTP request.
#[derive(Debug, Clone)]
struct HttpRequest {
    url: String,
    method: &'static str,
    /// JSON body.
    body: Option<String>,
    bearer: Option<String>,
}

impl HttpRequest {
    fn get(url: String) -> Self {
        Self {
            url,
            method: "GET",
            body: None,
            bearer: None,
        }
    }
}

/// URL for an area request.
pub fn area_url(area: &AreaRef) -> String {
    match area {
        AreaRef::Demo => "/api/demo".to_owned(),
        AreaRef::Osm { bbox } => format!(
            "/api/area?bbox={:.6},{:.6},{:.6},{:.6}",
            bbox.south, bbox.west, bbox.north, bbox.east
        ),
    }
}

pub fn tile_url(t: TileId) -> String {
    format!("/api/tiles/{}/{}/{}.png", t.z, t.x, t.y)
}

/// State of one map tile.
#[derive(Debug, Clone)]
pub enum TileState {
    Pending,
    Ready(Handle<Image>),
    Failed,
}

/// Map tiles shared by the 3D ground and the 2D map picker.
#[derive(Debug, Default, Resource)]
pub struct Tiles {
    tiles: HashMap<TileId, TileState>,
    in_flight: usize,
    queue: Vec<TileId>,
    /// Tiles the 3D ground needs; the map picker never cancels these.
    pinned: HashSet<TileId>,
}

/// Concurrent tile requests (the server limits upstream fetches too).
const MAX_IN_FLIGHT: usize = 6;
/// Soft cap on decoded tiles kept in memory.
const MAX_TILES: usize = 400;

impl Tiles {
    /// Returns the tile if loaded; queues it otherwise.
    pub fn get(&mut self, t: TileId) -> Option<Handle<Image>> {
        if !self.tiles.contains_key(&t) {
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
    pub fn ready(&self, t: TileId) -> Option<Handle<Image>> {
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
}

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Net>()
            .init_resource::<Tiles>()
            .init_resource::<Loading>()
            .init_resource::<ServerConfig>()
            .add_systems(Startup, |net: Res<Net>| {
                net.get("/api/config".to_owned(), Request::Config)
            })
            .add_message::<LoadArea>()
            .add_message::<AreaLoaded>()
            .init_resource::<Regions>()
            .add_systems(
                Update,
                (
                    start_area_loads,
                    pump_tiles,
                    send_region_requests,
                    drain_replies,
                )
                    .chain(),
            );
    }
}

fn start_area_loads(
    mut reqs: MessageReader<LoadArea>,
    net: Res<Net>,
    mut loading: ResMut<Loading>,
    time: Res<Time<Real>>,
) {
    // Only the most recent request matters.
    if let Some(req) = reqs.read().last() {
        loading.area = Some(req.area);
        loading.request = Some(req.clone());
        loading.started = time.elapsed_secs_f64();
        net.get(area_url(&req.area), Request::Area(req.clone()));
    }
}

fn pump_tiles(net: Res<Net>, mut tiles: ResMut<Tiles>) {
    while tiles.in_flight < MAX_IN_FLIGHT {
        let Some(t) = tiles.queue.pop() else { break };
        tiles.in_flight += 1;
        net.get(tile_url(t), Request::Tile(t));
    }
}

fn send_region_requests(net: Res<Net>, mut regions: ResMut<Regions>) {
    if regions.outbox.is_empty() {
        return;
    }
    let token = regions.token.trim().to_owned();
    for r in std::mem::take(&mut regions.outbox) {
        match r {
            RegionRequest::Catalog => {
                regions.catalog = CatalogState::Loading;
                net.get("/api/regions/catalog".to_owned(), Request::RegionsCatalog);
            }
            RegionRequest::Action(action, id) => {
                let body = serde_json::json!({ "id": id }).to_string();
                net.send(
                    HttpRequest {
                        url: format!("/api/regions/{}", action.path()),
                        method: "POST",
                        body: Some(body),
                        bearer: Some(token.clone()),
                    },
                    Request::RegionAction(action, id),
                );
            }
        }
    }
}

fn drain_replies(
    net: Res<Net>,
    mut tiles: ResMut<Tiles>,
    mut images: ResMut<Assets<Image>>,
    mut loaded: MessageWriter<AreaLoaded>,
    mut loading: ResMut<Loading>,
    mut ui: ResMut<UiState>,
    mut config: ResMut<ServerConfig>,
    mut regions: ResMut<Regions>,
) {
    for (req, res) in net.rx.try_iter() {
        match req {
            Request::Config => {
                let v: Option<serde_json::Value> =
                    res.ok().and_then(|b| serde_json::from_slice(&b).ok());
                let attribution = v
                    .as_ref()
                    .and_then(|v| v.get("attribution")?.as_str().map(str::to_owned));
                if let Some(a) = attribution.filter(|a| !a.is_empty()) {
                    config.tile_attribution = a;
                }
                let has_regions = v
                    .as_ref()
                    .and_then(|v| v.get("regions")?.as_bool())
                    .unwrap_or(false);
                if has_regions && !regions.enabled {
                    regions.enabled = true;
                    regions.restore_token();
                    net.follow_regions();
                }
            }
            Request::RegionsStatus => match res {
                Ok(bytes) => {
                    regions.live = true;
                    match serde_json::from_slice(&bytes) {
                        Ok(status) => regions.update(status, &mut ui),
                        Err(e) => warn!("unreadable region status: {e}"),
                    }
                }
                Err(_) => regions.live = false,
            },
            Request::RegionsCatalog => {
                regions.catalog = match res.and_then(|b| {
                    serde_json::from_slice(&b)
                        .map_err(|e| format!("the extract list could not be read: {e}"))
                }) {
                    Ok(list) => CatalogState::Loaded(list),
                    Err(e) => CatalogState::Failed(e),
                };
            }
            Request::RegionAction(action, id) => {
                if regions.busy.as_deref() == Some(id.as_str()) {
                    regions.busy = None;
                }
                match res.and_then(|b| {
                    serde_json::from_slice(&b).map_err(|e| format!("unreadable reply: {e}"))
                }) {
                    Ok(status) => {
                        regions.update(status, &mut ui);
                        if action == ActionKind::Import {
                            regions.store_token();
                        }
                    }
                    Err(e) => ui.error(format!("Could not {} {id}: {e}", action.path())),
                }
            }
            Request::Area(request) => {
                if loading.area == Some(request.area) {
                    loading.area = None;
                    loading.request = None;
                } else {
                    continue; // superseded or cancelled
                }
                let result = res.and_then(|bytes| {
                    AreaBundle::decode(&bytes)
                        .map_err(|e| format!("{e}; reload the page and try again"))
                });
                if let Err(e) = &result {
                    ui.error_with(
                        format!("Could not load area: {e}"),
                        ToastAction::Retry(request.clone()),
                    );
                }
                loaded.write(AreaLoaded { request, result });
            }
            Request::Tile(t) => {
                tiles.in_flight = tiles.in_flight.saturating_sub(1);
                let state = res
                    .and_then(|bytes| {
                        Image::from_buffer(
                            &bytes,
                            ImageType::Extension("png"),
                            CompressedImageFormats::NONE,
                            true,
                            ImageSampler::linear(),
                            RenderAssetUsages::RENDER_WORLD,
                        )
                        .map_err(|e| e.to_string())
                    })
                    .map_or(TileState::Failed, |img| TileState::Ready(images.add(img)));
                tiles.tiles.insert(t, state);
            }
        }
    }
    if tiles.tiles.len() > MAX_TILES {
        // Drop failed entries so they can be retried; loaded tiles stay cached.
        tiles
            .tiles
            .retain(|_, s| matches!(s, TileState::Ready(_) | TileState::Pending));
    }
}

/// What a failed HTTP reply means to the user: the server's own `error` text
/// when it sent one, else a description of the status (proxies and the request
/// timeout answer without one).
#[cfg(target_arch = "wasm32")]
fn http_failure(status: u16, body: &str) -> String {
    let from_server = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
        .filter(|m| !m.is_empty());
    from_server.unwrap_or_else(|| match status {
        404 => "the server does not have this (HTTP 404); the app may be out of date, so reload the page".to_owned(),
        408 | 504 => format!("the server took too long to answer (HTTP {status}); try again shortly"),
        502 | 503 => format!("the server is unavailable right now (HTTP {status}); try again shortly"),
        500..=599 => format!("the server had a problem (HTTP {status}); try again shortly"),
        _ => format!("the server refused the request (HTTP {status})"),
    })
}

/// The message of a JavaScript exception (`TypeError: Failed to fetch`, a
/// `DOMException`, ...), or a placeholder when it carries none.
#[cfg(target_arch = "wasm32")]
pub(crate) fn js_cause(e: &wasm_bindgen::JsValue) -> String {
    use wasm_bindgen::JsCast;
    let msg = match e.dyn_ref::<js_sys::Error>() {
        Some(err) => String::from(err.message()),
        None => e.as_string().unwrap_or_default(),
    };
    if msg.is_empty() {
        "no details from the browser".to_owned()
    } else {
        msg
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn_fetch(req: HttpRequest, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let window = web_sys::window()
                .ok_or("the browser gave the app no window to make requests from")?;
            let init = web_sys::RequestInit::new();
            init.set_method(req.method);
            let headers = web_sys::Headers::new()
                .map_err(|e| format!("could not build the request: {}", js_cause(&e)))?;
            if let Some(body) = &req.body {
                let _ = headers.set("content-type", "application/json");
                init.set_body(&wasm_bindgen::JsValue::from_str(body));
            }
            if let Some(token) = req.bearer.as_deref().filter(|t| !t.is_empty()) {
                let _ = headers.set("authorization", &format!("Bearer {token}"));
            }
            init.set_headers(&headers);
            // A rejected fetch means no HTTP reply at all: offline, DNS, a dead
            // server, or a blocked connection.
            let resp: web_sys::Response =
                JsFuture::from(window.fetch_with_str_and_init(&req.url, &init))
                    .await
                    .map_err(|e| {
                        format!(
                            "the server did not answer (you may be offline, or it may be down): {}",
                            js_cause(&e)
                        )
                    })?
                    .dyn_into()
                    .map_err(|_| {
                        "the browser returned something other than a response".to_owned()
                    })?;
            if !resp.ok() {
                let body = match resp.text() {
                    Ok(text) => JsFuture::from(text)
                        .await
                        .ok()
                        .and_then(|v| v.as_string())
                        .unwrap_or_default(),
                    Err(_) => String::new(),
                };
                return Err(http_failure(resp.status(), &body));
            }
            let read_err = |e: wasm_bindgen::JsValue| {
                format!(
                    "the connection dropped while the reply was downloading: {}",
                    js_cause(&e)
                )
            };
            let buf = JsFuture::from(resp.array_buffer().map_err(read_err)?)
                .await
                .map_err(read_err)?;
            Ok(js_sys::Uint8Array::new(&buf).to_vec())
        }
        .await
        .map_err(|e: String| e);
        done(result);
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_fetch(req: HttpRequest, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    done(Err(format!(
        "networking is only available in the browser build ({})",
        req.url
    )));
}

/// Opens an `EventSource` and calls `on` with the data of every `event`
/// message, and with an error whenever the connection drops. The browser
/// retries a dropped stream by itself, but gives up for good after an error
/// answer (a 503 from the stream cap, a 502 from a proxy during a restart);
/// then a new one is opened after [`REOPEN_AFTER_MS`]. Lives as long as the page.
#[cfg(target_arch = "wasm32")]
fn spawn_event_source(url: &str, event: &str, on: impl Fn(Result<Vec<u8>, String>) + 'static) {
    open_event_source(url.to_owned(), event.to_owned(), std::rc::Rc::new(on));
}

/// Wait before replacing an `EventSource` the browser closed.
#[cfg(target_arch = "wasm32")]
const REOPEN_AFTER_MS: i32 = 5_000;

#[cfg(target_arch = "wasm32")]
fn open_event_source(url: String, event: String, on: std::rc::Rc<dyn Fn(Result<Vec<u8>, String>)>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let source = match web_sys::EventSource::new(&url) {
        Ok(s) => s,
        Err(e) => {
            on(Err(js_cause(&e)));
            return;
        }
    };
    let on_message = {
        let on = std::rc::Rc::clone(&on);
        Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            if let Some(data) = e.data().as_string() {
                on(Ok(data.into_bytes()));
            }
        })
    };
    let on_error = {
        let source = source.clone();
        let (url, event) = (url.clone(), event.clone());
        Closure::<dyn Fn(web_sys::Event)>::new(move |_: web_sys::Event| {
            on(Err("the live connection dropped".to_owned()));
            if source.ready_state() != web_sys::EventSource::CLOSED {
                return; // the browser is reconnecting
            }
            let (url, event, on) = (url.clone(), event.clone(), std::rc::Rc::clone(&on));
            let reopen = Closure::once_into_js(move || open_event_source(url, event, on));
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    reopen.unchecked_ref(),
                    REOPEN_AFTER_MS,
                );
            }
        })
    };
    let _ = source.add_event_listener_with_callback(&event, on_message.as_ref().unchecked_ref());
    source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    // The handlers live as long as their source: for the page, or until it is
    // closed and replaced, which is rare enough not to matter.
    on_message.forget();
    on_error.forget();
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_event_source(_: &str, _: &str, on: impl Fn(Result<Vec<u8>, String>) + 'static) {
    on(Err(
        "live updates are only available in the browser build".to_owned()
    ));
}
