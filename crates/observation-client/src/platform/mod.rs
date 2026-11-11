//! Everything that differs between the browser and the native build, behind
//! the ports of [`observation_client_model::platform`]: the browser glue
//! (`web_sys`, `js_sys`, `wasm_bindgen`) in `web.rs` and its native stand-ins
//! in `native.rs`, each implementing the same traits.
//!
//! [`PlatformPlugin`] inserts them as the one [`Platform`] resource, before
//! any other plugin is built. Apart from the coverage backend's worker link
//! (`coverage/backend.rs`), which moves with the coverage changes, this is
//! the only module of the library that tests `target_arch`.

use std::sync::Arc;

use bevy::prelude::*;
pub use observation_client_model::platform::{
    Clock, ColorScheme, FileDialogs, Http, HttpRequest, KeyValueStore, SaveFailure, unix_secs,
};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(not(target_arch = "wasm32"))]
use native as imp;
#[cfg(target_arch = "wasm32")]
use web as imp;

/// The loading screen is part of the page, not a port: this Startup system
/// removes it.
pub use imp::hide_loader;
/// For the coverage backend's worker link, until it is a port too.
#[cfg(target_arch = "wasm32")]
pub use web::js_cause;

/// The page the app runs in, one port per concern. Read-only: systems call
/// through `Res<Platform>`, and resources clone the ports they keep.
#[derive(Resource, Clone)]
pub struct Platform {
    pub http: Arc<dyn Http>,
    /// Survives reloads (`localStorage`).
    pub local: Arc<dyn KeyValueStore>,
    /// Lives as long as the browser tab (`sessionStorage`).
    pub session: Arc<dyn KeyValueStore>,
    pub files: Arc<dyn FileDialogs>,
    pub clock: Arc<dyn Clock>,
    pub scheme: Arc<dyn ColorScheme>,
}

impl Platform {
    /// This build's ports: the browser's, or the native stand-ins.
    pub fn current() -> Self {
        imp::platform()
    }

    /// The ports, which [`PlatformPlugin`] inserts before every other plugin
    /// is built.
    pub fn of(world: &World) -> &Self {
        world
            .get_resource::<Self>()
            .expect("PlatformPlugin is added before the plugins that use the Platform")
    }
}

/// Inserts this build's [`Platform`], unless one is there already (a test's
/// fakes).
pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<Platform>() {
            app.insert_resource(Platform::current());
        }
    }
}
