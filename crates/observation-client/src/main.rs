//! The Observation web app: Bevy with WebGL2 in the page's
//! `#observation-canvas`, egui, and [`ClientPlugins`].

use bevy::prelude::*;
use bevy::window::{PresentMode, WindowPlugin};
use bevy_egui::EguiPlugin;
use observation_client::ClientPlugins;

fn main() {
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
        .add_plugins(ClientPlugins)
        .run();
}
