//! Fetching bundles and tiles from the server.
//!
//! Requests run on the browser's event loop (`fetch`) and report back through
//! a channel that a system drains every frame.

use crate::state::{AreaLoaded, LoadArea, Loading, UiState};
use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::platform::collections::HashMap;
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
}

/// Concurrent tile requests (the server limits upstream fetches too).
const MAX_IN_FLIGHT: usize = 6;
/// Soft cap on decoded tiles kept in memory.
const MAX_TILES: usize = 400;

impl Tiles {
    /// Returns the tile if loaded; queues it otherwise.
    pub fn get(&mut self, t: TileId) -> Option<Handle<Image>> {
        match self.tiles.get(&t) {
            Some(TileState::Ready(h)) => Some(h.clone()),
            Some(_) => None,
            None => {
                self.tiles.insert(t, TileState::Pending);
                self.queue.push(t);
                None
            }
        }
    }

    /// Drops queued requests that are no longer wanted (e.g. after panning the map).
    pub fn retain_queue(&mut self, keep: impl Fn(&TileId) -> bool) {
        let tiles = &mut self.tiles;
        self.queue.retain(|t| {
            let k = keep(t);
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
) {
    // Only the most recent request matters.
    if let Some(req) = reqs.read().last() {
        loading.area = Some(req.area);
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
                } else {
                    continue; // superseded
                }
                let result =
                    res.and_then(|bytes| AreaBundle::decode(&bytes).map_err(|e| e.to_string()));
                if let Err(e) = &result {
                    ui.error(format!("Could not load area: {e}"));
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

#[cfg(target_arch = "wasm32")]
fn spawn_fetch(url: String, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let window = web_sys::window().ok_or("no window")?;
            let resp: web_sys::Response = JsFuture::from(window.fetch_with_str(&url))
                .await
                .map_err(|_| "network error".to_owned())?
                .dyn_into()
                .map_err(|_| "bad response".to_owned())?;
            if !resp.ok() {
                let body = JsFuture::from(resp.text().map_err(|_| "bad body")?)
                    .await
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                let msg = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| format!("HTTP {}", resp.status()));
                return Err(msg);
            }
            let buf = JsFuture::from(resp.array_buffer().map_err(|_| "bad body")?)
                .await
                .map_err(|_| "read failed".to_owned())?;
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
