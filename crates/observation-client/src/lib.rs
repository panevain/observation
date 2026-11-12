//! Observation: visualise what home surveillance cameras on a street can see.
//!
//! The app is a Bevy scene (buildings, map tiles, cameras, coverage overlays)
//! framed by egui panels. Heavy geometry work happens on the server; coverage
//! is computed here with the shared `observation-core` logic.
//!
//! [`ClientPlugins`] is the whole app on top of Bevy's `DefaultPlugins`, mesh
//! picking and egui, which the `observation-client` binary adds.

mod addresses;
mod cameras;
mod coverage;
mod history;
mod icons;
mod input;
mod net;
mod persist;
mod platform;
mod regions;
mod state;
mod theme;
mod ui;
mod world;

use bevy::app::PluginGroupBuilder;
use bevy::prelude::*;
use observation_core::camera::Catalog;

/// Curated camera catalog: every `.ron` file under `assets/catalog/`, discovered by build.rs.
mod catalog {
    include!(concat!(env!("OUT_DIR"), "/catalog_files.rs"));
}

/// The app's plugins, in the order they are built: the platform's ports and
/// the shared resources first, then one plugin per area of the app.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClientPlugins;

impl PluginGroup for ClientPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(platform::PlatformPlugin)
            .add(ResourcesPlugin)
            .add(net::NetPlugin)
            .add(world::WorldPlugin)
            .add(cameras::CamerasPlugin)
            .add(addresses::AddressesPlugin)
            .add(coverage::CoveragePlugin)
            .add(input::InputPlugin)
            .add(persist::PersistPlugin)
            .add(history::HistoryPlugin)
            .add(ui::UiPlugin)
            .add(LoaderPlugin)
    }
}

/// Inserts the resources every other plugin shares: theme tokens, stored
/// preferences (read through the platform's ports), the camera catalog, the
/// document and the UI state.
struct ResourcesPlugin;

impl Plugin for ResourcesPlugin {
    fn build(&self, app: &mut App) {
        let catalog = match Catalog::from_ron_parts(catalog::CATALOG_FILES) {
            Ok(c) => c,
            Err(e) => {
                // The catalog is validated in CI; fall back to an empty list rather than crash.
                bevy::log::error!("camera catalog invalid: {e}");
                Catalog::default()
            }
        };
        let platform = platform::Platform::of(app.world());
        let tokens = theme::Tokens::from_environment(&*platform.scheme);
        let font = persist::load_font(&*platform.local);
        let address_jump = persist::load_address_jump(&*platform.local);
        app.insert_resource(tokens)
            .insert_resource(font)
            .insert_resource(address_jump)
            .insert_resource(state::Models::new(catalog))
            .init_resource::<state::Scene>()
            .init_resource::<state::DataChangeNotice>()
            .init_resource::<state::Editor>()
            .init_resource::<state::Panels>()
            .init_resource::<state::Hover>()
            .init_resource::<state::Toasts>()
            .init_resource::<state::UiLayout>();
    }
}

/// Hides the HTML loading screen at startup.
struct LoaderPlugin;

impl Plugin for LoaderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, platform::hide_loader);
    }
}

#[cfg(test)]
mod schedule_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{KeyValueStore, Platform, PlatformPlugin};
    use observation_client_model::testing::{
        FakeClock, FakeColorScheme, FakeFiles, FakeHttp, MemoryStore,
    };
    use std::sync::Arc;

    /// The startup half of the app over fake ports: what it reads from the
    /// page and what it asks the server for, with nothing rendered.
    #[test]
    fn startup_reads_preferences_and_the_server_config_through_the_ports() {
        let http = Arc::new(FakeHttp::default());
        let local = Arc::new(MemoryStore::default());
        local.set("observation.font", "iosevka").expect("stored");
        local
            .set("observation.address-jump", "off")
            .expect("stored");
        let platform = Platform {
            http: http.clone(),
            local,
            session: Arc::new(MemoryStore::default()),
            files: Arc::new(FakeFiles::default()),
            clock: Arc::new(FakeClock::at(0.0)),
            scheme: Arc::new(FakeColorScheme::new(false)),
        };
        let mut app = App::new();
        app.insert_resource(platform)
            .add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .add_plugins((PlatformPlugin, ResourcesPlugin, net::NetPlugin));

        let w = app.world();
        assert_eq!(*w.resource::<theme::UiFont>(), theme::UiFont::Iosevka);
        assert_eq!(
            *w.resource::<state::AddressJump>(),
            state::AddressJump(false)
        );
        assert!(
            !w.resource::<theme::Tokens>().dark,
            "light, as the scheme says"
        );

        app.update();
        assert_eq!(http.pending_urls(), ["/api/config"]);
        assert!(http.respond(
            "/api/config",
            Ok(br#"{"attribution": "Test tiles"}"#.to_vec())
        ));
        app.update();
        let config = app.world().resource::<net::ServerConfig>();
        assert_eq!(config.tile_attribution, "Test tiles");
    }
}
