//! The native build's side of [`crate::platform`]: no network, no storage,
//! downloads written to the working directory, and a dark colour scheme.

use std::sync::Arc;

use super::{
    Clock, ColorScheme, FileDialogs, Http, HttpRequest, KeyValueStore, Platform, SaveFailure,
};
use observation_client_model::platform::Reply;

pub fn platform() -> Platform {
    let store = Arc::new(NullStore);
    Platform {
        http: Arc::new(Offline),
        local: store.clone(),
        session: store,
        files: Arc::new(WorkingDirectory),
        clock: Arc::new(SystemClock),
        scheme: Arc::new(AlwaysDark),
    }
}

// The loading screen.

/// There is no HTML loading screen natively.
pub fn hide_loader() {}

// Networking.

/// Answers every request at once with an error.
struct Offline;

impl Http for Offline {
    fn fetch(&self, req: HttpRequest, done: Box<dyn FnOnce(Reply) + Send>) {
        done(Err(format!(
            "networking is only available in the browser build ({})",
            req.url
        )));
    }

    fn subscribe(&self, _: &str, _: &str, on: Box<dyn Fn(Reply) + Send>) {
        on(Err(
            "live updates are only available in the browser build".to_owned()
        ));
    }
}

// Storage and files.

/// Stores nothing, and says it did.
struct NullStore;

impl KeyValueStore for NullStore {
    fn get(&self, _: &str) -> Option<String> {
        None
    }
    fn set(&self, _: &str, _: &str) -> Result<(), SaveFailure> {
        Ok(())
    }
    fn remove(&self, _: &str) {}
}

/// Downloads are written to the working directory; nothing can be picked.
struct WorkingDirectory;

impl FileDialogs for WorkingDirectory {
    fn download(&self, name: &str, _: &str, text: &str) -> Result<(), String> {
        std::fs::write(name, text).map_err(|e| format!("could not write {name} ({e})"))
    }
    fn pick_text(&self, _: &str, _: Box<dyn FnOnce(Result<String, String>) + Send>) {}
}

// The clock.

/// This computer's clock.
struct SystemClock;

impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "native only: SystemTime::now panics only on wasm32-unknown-unknown"
    )]
    fn unix_ms(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_millis() as f64)
    }
}

// The colour scheme.

/// Native builds stay dark: there is no system setting to follow.
struct AlwaysDark;

impl ColorScheme for AlwaysDark {
    fn prefers_dark(&self) -> bool {
        true
    }
    fn watch(&self, _: Box<dyn Fn(bool) + Send>) {}
}
