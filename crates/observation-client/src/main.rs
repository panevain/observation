//! Observation: visualise what home surveillance cameras on a street can see.
//!
//! The app is a Bevy scene (buildings, map tiles, cameras, coverage overlays)
//! framed by egui panels. Heavy geometry work happens on the server; coverage
//! is computed here with the shared `observation-core` logic.

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

use bevy::prelude::*;
use bevy::window::{PresentMode, WindowPlugin};
use bevy_egui::EguiPlugin;
use observation_core::camera::Catalog;

// Curated camera catalog: every `.ron` file under `assets/catalog/`, discovered by build.rs.
include!(concat!(env!("OUT_DIR"), "/catalog_files.rs"));

fn main() {
    let catalog = match Catalog::from_ron_parts(CATALOG_FILES) {
        Ok(c) => c,
        Err(e) => {
            // The catalog is validated in CI; fall back to an empty list rather than crash.
            bevy::log::error!("camera catalog invalid: {e}");
            Catalog::default()
        }
    };

    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Observation".into(),
                        canvas: Some("#observation-canvas".into()),
                        fit_canvas_to_parent: true,
                        prevent_default_event_handling: true,
                        present_mode: PresentMode::AutoVsync,
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    meta_check: bevy::asset::AssetMetaCheck::Never,
                    ..default()
                }),
        )
        .add_plugins((MeshPickingPlugin, EguiPlugin::default()))
        .insert_resource(theme::Tokens::from_environment())
        .insert_resource(persist::load_font())
        .insert_resource(state::Models {
            catalog,
            custom: Vec::new(),
        })
        .init_resource::<state::Scene>()
        .init_resource::<state::UiState>()
        .init_resource::<state::UiLayout>()
        .add_plugins((
            net::NetPlugin,
            world::WorldPlugin,
            cameras::CamerasPlugin,
            addresses::AddressesPlugin,
            coverage::CoveragePlugin,
            input::InputPlugin,
            persist::PersistPlugin,
            history::HistoryPlugin,
            ui::UiPlugin,
        ))
        .add_systems(Startup, hide_loader)
        .run();
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
