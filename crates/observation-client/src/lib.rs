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

/// The app's plugins, in the order they are built: the shared resources
/// first, then one plugin per area of the app.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClientPlugins;

impl PluginGroup for ClientPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
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
/// preferences, the camera catalog, the document and the UI state.
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
        app.insert_resource(theme::Tokens::from_environment())
            .insert_resource(persist::load_font())
            .insert_resource(persist::load_address_jump())
            .insert_resource(state::Models::new(catalog))
            .init_resource::<state::Scene>()
            .init_resource::<state::UiState>()
            .init_resource::<state::UiLayout>();
    }
}

/// Hides the HTML loading screen at startup.
struct LoaderPlugin;

impl Plugin for LoaderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, hide_loader);
    }
}

/// Removes the HTML loading screen once the app is running.
fn hide_loader() {
    #[cfg(target_arch = "wasm32")]
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("loader"))
    {
        el.remove();
    }
}
