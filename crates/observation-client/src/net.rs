//! Fetching bundles and tiles from the server.
//!
//! Requests run on the browser's event loop (`fetch`) and report back through
//! a channel that a system drains every frame.

use crate::state::{AreaLoaded, LoadArea, Loading, ToastAction, UiState};
use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_core::bundle::AreaBundle;
use observation_core::scene::AreaRef;
use observation_core::tiles::TileId;

#[derive(Debug, Clone)]
enum Request {
    Config,
    Area(LoadArea),
    Tile(TileId),
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
        let tx = self.tx.clone();
        spawn_fetch(url, move |res| {
            // The receiver lives as long as the app; a send error means shutdown.
            let _ = tx.send((req, res));
        });
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
            .add_systems(
                Update,
                (start_area_loads, pump_tiles, drain_replies).chain(),
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

fn drain_replies(
    net: Res<Net>,
    mut tiles: ResMut<Tiles>,
    mut images: ResMut<Assets<Image>>,
    mut loaded: MessageWriter<AreaLoaded>,
    mut loading: ResMut<Loading>,
    mut ui: ResMut<UiState>,
    mut config: ResMut<ServerConfig>,
) {
    for (req, res) in net.rx.try_iter() {
        match req {
            Request::Config => {
                let attribution = res.ok().and_then(|b| {
                    let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
                    v.get("attribution")?.as_str().map(str::to_owned)
                });
                if let Some(a) = attribution.filter(|a| !a.is_empty()) {
                    config.tile_attribution = a;
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
fn spawn_fetch(url: String, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let window = web_sys::window()
                .ok_or("the browser gave the app no window to make requests from")?;
            // A rejected fetch means no HTTP reply at all: offline, DNS, a dead
            // server, or a blocked connection.
            let resp: web_sys::Response = JsFuture::from(window.fetch_with_str(&url))
                .await
                .map_err(|e| {
                    format!(
                        "the server did not answer (you may be offline, or it may be down): {}",
                        js_cause(&e)
                    )
                })?
                .dyn_into()
                .map_err(|_| "the browser returned something other than a response".to_owned())?;
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
fn spawn_fetch(url: String, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    done(Err(format!(
        "networking is only available in the browser build ({url})"
    )));
}
