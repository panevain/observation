//! Fetching bundles and tiles from the server, and following its local regions.
//!
//! Requests run on the browser's event loop (`fetch`, `EventSource`) and report
//! back through a channel that a system drains every frame.

use crate::platform::{HttpRequest, Platform, unix_secs};
use crate::regions::{CatalogState, RegionRequest, Regions, announce};
use crate::state::{AreaLoaded, LoadArea, Loading, ToastAction, Toasts};
use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_client_model::net::{self, TileQueue, TileState};
use observation_core::bundle::{AreaBundle, BUNDLE_VERSION};
use observation_core::regions::{ActionKind, RegionAction, SizesRequest};
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
    RegionsSizes(Vec<String>),
}

/// Settings published by the server at `/api/config`, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct ServerConfig(net::ServerConfig);

type Reply = (Request, Result<Vec<u8>, String>);

#[derive(Resource)]
pub struct Net {
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
    /// The ports requests go through, and the clock and session storage the
    /// replies need.
    platform: Platform,
}

impl FromWorld for Net {
    fn from_world(world: &mut World) -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            rx,
            platform: Platform::of(world).clone(),
        }
    }
}

impl Net {
    fn get(&self, url: String, req: Request) {
        self.send(HttpRequest::get(url), req);
    }

    fn send(&self, http: HttpRequest, req: Request) {
        let tx = self.tx.clone();
        self.platform.http.fetch(
            http,
            Box::new(move |res| {
                // The receiver lives as long as the app; a send error means shutdown.
                let _ = tx.send((req, res));
            }),
        );
    }

    /// Follows `/api/regions/events`; every status (and every drop of the
    /// stream, which the browser reconnects) arrives as a `RegionsStatus` reply.
    fn follow_regions(&self) {
        let tx = self.tx.clone();
        self.platform.http.subscribe(
            "/api/regions/events",
            "regions",
            Box::new(move |res| {
                let _ = tx.send((Request::RegionsStatus, res));
            }),
        );
    }
}

/// URL for an area request. The bundle format version is part of it (the
/// server ignores it) so that the browser, which may cache bundles for a day,
/// never hands a new client a body in an older format.
pub fn area_url(area: &AreaRef) -> String {
    match area {
        AreaRef::Demo => format!("/api/demo?v={BUNDLE_VERSION}"),
        AreaRef::Osm { bbox } => format!(
            "/api/area?bbox={:.6},{:.6},{:.6},{:.6}&v={BUNDLE_VERSION}",
            bbox.south, bbox.west, bbox.north, bbox.east
        ),
    }
}

/// Map tiles shared by the 3D ground and the 2D map picker, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Tiles(TileQueue<Handle<Image>>);

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
        loading.start((**req).clone(), time.elapsed_secs_f64());
        net.get(area_url(&req.area), Request::Area(req.clone()));
    }
}

fn pump_tiles(net: Res<Net>, mut tiles: ResMut<Tiles>, config: Res<ServerConfig>) {
    // Tiles are cached by id, not URL: wait for the server's template.
    if !config.answered() {
        return;
    }
    while let Some(t) = tiles.next_request() {
        net.get(t.url(&config.tile_template), Request::Tile(t));
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
            RegionRequest::Action(action, id) => send_region_action(
                &net,
                &token,
                action,
                RegionAction {
                    id,
                    ..RegionAction::default()
                },
            ),
            RegionRequest::Sizes(ids) => {
                let body = SizesRequest { ids };
                net.send(
                    HttpRequest {
                        url: "/api/regions/catalog/sizes".to_owned(),
                        method: "POST",
                        body: serde_json::to_string(&body).ok(),
                        bearer: None,
                    },
                    Request::RegionsSizes(body.ids),
                );
            }
            RegionRequest::Settings(settings) => {
                send_region_action(&net, &token, ActionKind::Settings, settings);
            }
        }
    }
}

fn send_region_action(net: &Net, token: &str, action: ActionKind, body: RegionAction) {
    let id = body.id.clone();
    net.send(
        HttpRequest {
            url: format!("/api/regions/{}", action.path()),
            method: "POST",
            body: serde_json::to_string(&body).ok(),
            bearer: Some(token.to_owned()),
        },
        Request::RegionAction(action, id),
    );
}

fn drain_replies(
    net: Res<Net>,
    mut tiles: ResMut<Tiles>,
    mut images: ResMut<Assets<Image>>,
    mut loaded: MessageWriter<AreaLoaded>,
    mut loading: ResMut<Loading>,
    mut toasts: ResMut<Toasts>,
    mut config: ResMut<ServerConfig>,
    mut regions: ResMut<Regions>,
) {
    for (req, res) in net.rx.try_iter() {
        match req {
            Request::Config => {
                let has_regions = config.answer(res.ok().as_deref());
                if has_regions && !regions.enabled {
                    regions.enabled = true;
                    regions.restore_token(&*net.platform.session);
                    net.follow_regions();
                }
            }
            Request::RegionsStatus => match res {
                Ok(bytes) => {
                    regions.live = true;
                    match serde_json::from_slice(&bytes) {
                        Ok(status) => announce(regions.update(status), &mut toasts),
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
            Request::RegionsSizes(asked) => {
                let answer = res.ok().and_then(|b| serde_json::from_slice(&b).ok());
                regions.take_sizes(&asked, answer, unix_secs(&*net.platform.clock));
            }
            Request::RegionAction(action, id) => {
                if regions.busy.as_deref() == Some(id.as_str()) {
                    regions.busy = None;
                }
                match res.and_then(|b| {
                    serde_json::from_slice(&b).map_err(|e| format!("unreadable reply: {e}"))
                }) {
                    Ok(status) => {
                        announce(regions.update(status), &mut toasts);
                        if action == ActionKind::Import {
                            regions.store_token(&*net.platform.session);
                        }
                    }
                    Err(e) => toasts.error(format!("Could not {} {id}: {e}", action.verb())),
                }
            }
            Request::Area(request) => {
                if !loading.finish(request.area) {
                    continue; // superseded or cancelled
                }
                let result = res.and_then(|bytes| {
                    AreaBundle::decode(&bytes)
                        .map_err(|e| format!("{e}; reload the page and try again"))
                });
                if let Err(e) = &result {
                    toasts.error_with(
                        format!("Could not load area: {e}"),
                        ToastAction::Retry((*request).clone()),
                    );
                }
                loaded.write(AreaLoaded { request, result });
            }
            Request::Tile(t) => {
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
                    .map_or(TileState::Failed(unix_secs(&*net.platform.clock)), |img| {
                        TileState::Ready(images.add(img))
                    });
                tiles.finish(t, state);
            }
        }
    }
    tiles.evict(&*net.platform.clock);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{KeyValueStore, PlatformPlugin};
    use observation_client_model::testing::{
        FakeClock, FakeColorScheme, FakeFiles, FakeHttp, MemoryStore,
    };
    use observation_client_model::toasts::Toast;
    use observation_core::bundle::build_bundle;
    use observation_core::geo::BBox;
    use observation_core::terrain::Terrain;
    use std::sync::Arc;

    /// The net systems over a held-reply fake server, past startup (the
    /// config request is pending).
    struct Harness {
        app: App,
        http: Arc<FakeHttp>,
        session: Arc<MemoryStore>,
    }

    impl Harness {
        fn new() -> Self {
            let http = Arc::new(FakeHttp::default());
            let session = Arc::new(MemoryStore::default());
            let platform = Platform {
                http: http.clone(),
                local: Arc::new(MemoryStore::default()),
                session: session.clone(),
                files: Arc::new(FakeFiles::default()),
                clock: Arc::new(FakeClock::at(1_000_000.0)),
                scheme: Arc::new(FakeColorScheme::new(false)),
            };
            let mut app = App::new();
            app.insert_resource(platform)
                .add_plugins((MinimalPlugins, AssetPlugin::default()))
                .init_asset::<Image>()
                .add_plugins((PlatformPlugin, crate::ResourcesPlugin, NetPlugin));
            app.update();
            assert_eq!(http.pending_urls(), ["/api/config"]);
            Self { app, http, session }
        }

        fn load(&mut self, req: LoadArea) {
            self.app
                .world_mut()
                .resource_mut::<Messages<LoadArea>>()
                .write(req);
        }

        /// Every `AreaLoaded` written since the last call.
        fn loaded(&mut self) -> Vec<AreaLoaded> {
            self.app
                .world_mut()
                .resource_mut::<Messages<AreaLoaded>>()
                .drain()
                .collect()
        }

        fn toasts(&self) -> Vec<Toast> {
            self.app
                .world()
                .resource::<Toasts>()
                .iter()
                .cloned()
                .collect()
        }

        fn wanted(&self) -> Option<AreaRef> {
            self.app.world().resource::<Loading>().area()
        }
    }

    fn osm() -> AreaRef {
        AreaRef::Osm {
            bbox: BBox::new(51.0, -0.1, 51.001, -0.099).expect("valid box"),
        }
    }

    fn bundle() -> AreaBundle {
        let bbox = BBox::new(51.0, -0.1, 51.001, -0.099).expect("valid box");
        build_bundle(bbox, &[], &[], &[], Terrain::default(), "Test", 0, None)
    }

    #[test]
    fn an_area_reply_becomes_area_loaded_and_a_bad_one_offers_a_retry() {
        let mut h = Harness::new();
        h.load(LoadArea::demo());
        h.app.update();
        let demo = area_url(&AreaRef::Demo);
        assert_eq!(h.http.pending_urls(), ["/api/config", demo.as_str()]);
        assert_eq!(h.wanted(), Some(AreaRef::Demo));

        assert!(h.http.respond(&demo, Ok(bundle().encode())));
        h.app.update();
        let loaded = h.loaded();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].request.area, AreaRef::Demo);
        assert_eq!(loaded[0].result, Ok(bundle()));
        assert_eq!(h.wanted(), None, "no longer loading");
        assert!(h.toasts().is_empty());

        h.load(LoadArea::demo());
        h.app.update();
        assert!(h.http.respond(&demo, Ok(b"<html>sign in</html>".to_vec())));
        h.app.update();
        let loaded = h.loaded();
        let why = "the reply is not an area bundle (a proxy or sign-in page may have \
                   answered); reload the page and try again";
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].result, Err(why.to_owned()));
        let toasts = h.toasts();
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].text, format!("Could not load area: {why}"));
        assert_eq!(
            toasts[0].action,
            Some(ToastAction::Retry((*LoadArea::demo()).clone()))
        );
        assert_eq!(toasts[0].ttl, f32::INFINITY);
    }

    #[test]
    fn only_the_latest_area_request_is_fetched_and_loaded() {
        let mut h = Harness::new();
        h.load(LoadArea::demo());
        h.load(LoadArea::new(osm(), false));
        h.app.update();
        let (demo, osm_url) = (area_url(&AreaRef::Demo), area_url(&osm()));
        assert_eq!(
            h.http.pending_urls(),
            ["/api/config", osm_url.as_str()],
            "one frame's requests: the last one"
        );

        h.load(LoadArea::demo());
        h.app.update();
        assert_eq!(h.wanted(), Some(AreaRef::Demo));
        assert!(h.http.respond(&osm_url, Err("offline".into())));
        h.app.update();
        assert!(h.loaded().is_empty(), "superseded");
        assert!(h.toasts().is_empty(), "a superseded failure says nothing");
        assert_eq!(h.wanted(), Some(AreaRef::Demo));

        assert!(h.http.respond(&demo, Ok(bundle().encode())));
        h.app.update();
        let loaded = h.loaded();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].request.area, AreaRef::Demo);
    }

    #[test]
    fn a_cancelled_area_request_ignores_its_reply() {
        let mut h = Harness::new();
        h.load(LoadArea::demo());
        h.app.update();
        h.app.world_mut().resource_mut::<Loading>().cancel();
        assert!(
            h.http
                .respond(&area_url(&AreaRef::Demo), Err("offline".into()))
        );
        h.app.update();
        assert!(h.loaded().is_empty());
        assert!(h.toasts().is_empty());
    }

    #[test]
    fn tiles_wait_for_the_server_config_then_go_the_next_frame() {
        let mut h = Harness::new();
        let t = TileId::new(17, 65_000, 43_000).expect("valid tile");
        let clock = FakeClock::at(1_000_000.0);
        assert_eq!(
            h.app.world_mut().resource_mut::<Tiles>().get(t, &clock),
            None
        );
        h.app.update();
        h.app.update();
        assert_eq!(h.http.pending_urls(), ["/api/config"], "held");

        assert!(h.http.respond("/api/config", Ok(b"{}".to_vec())));
        h.app.update();
        assert!(
            h.http.pending_urls().is_empty(),
            "the config arrives after pump_tiles"
        );
        h.app.update();
        let url = t.url(&h.app.world().resource::<ServerConfig>().tile_template);
        assert_eq!(h.http.pending_urls(), std::slice::from_ref(&url));

        assert!(h.http.respond(&url, Err("offline".into())));
        h.app.update();
        let tiles = h.app.world().resource::<Tiles>();
        assert_eq!(tiles.ready(t), None, "a failed tile is not drawn");
    }

    #[test]
    fn region_statuses_follow_the_config_and_toast_finished_imports() {
        let mut h = Harness::new();
        h.session
            .set("observation.admin-token", "secret")
            .expect("stored");
        assert!(
            h.http
                .respond("/api/config", Ok(br#"{"regions": true}"#.to_vec()))
        );
        h.app.update();
        assert_eq!(
            h.http.subscriptions(),
            [("/api/regions/events".to_owned(), "regions".to_owned())]
        );
        assert_eq!(h.app.world().resource::<Regions>().token, "secret");

        let status = |importing: bool, error: Option<&str>| {
            serde_json::to_vec(&serde_json::json!({
                "agent_online": true, "manageable": true,
                "snapshot": {"version": 1, "max_download_bytes": 0, "refresh_days": 0,
                    "regions": [{
                        "id": "monaco", "name": "Monaco", "ready": false, "bbox": null,
                        "data_timestamp": null, "imported_unix": null, "disk_bytes": null,
                        "job": importing.then(|| serde_json::json!(
                            {"phase": "importing", "done": 0, "total": null, "since_unix": 0}
                        )),
                        "last_error": error,
                    }]},
            }))
            .expect("serialisable")
        };
        assert_eq!(
            h.http.emit("/api/regions/events", &Ok(status(true, None))),
            1
        );
        h.app.update();
        assert!(h.app.world().resource::<Regions>().live);
        assert!(h.toasts().is_empty());

        h.http
            .emit("/api/regions/events", &Ok(status(false, Some("disk full"))));
        h.app.update();
        let toasts = h.toasts();
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].text, "Importing Monaco failed: disk full");
        assert_eq!(toasts[0].ttl, f32::INFINITY);

        h.http
            .emit("/api/regions/events", &Err("stream dropped".into()));
        h.app.update();
        assert!(!h.app.world().resource::<Regions>().live);
    }
}
