//! The native build's side of [`crate::platform`]: no network, storage that
//! lasts for the run, downloads written to the working directory, a dark
//! colour scheme and no coverage worker.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use super::{
    Clock, ColorScheme, FileDialogs, Http, HttpRequest, KeyValueStore, PickedFile, Platform,
    SaveFailure,
};
use observation_client_model::platform::{JobChannel, Reply, StreamEvent, StreamStop};

pub fn platform() -> Platform {
    Platform {
        http: Arc::new(Offline),
        local: Arc::new(RunStore::default()),
        session: Arc::new(RunStore::default()),
        files: Arc::new(WorkingDirectory),
        clock: Arc::new(SystemClock),
        scheme: Arc::new(AlwaysDark),
    }
}

// The loading screen.

/// There is no HTML loading screen natively.
pub fn hide_loader() {}

// The coverage worker.

/// There are no workers natively: coverage is computed on the main thread.
pub fn coverage_worker() -> Option<Box<dyn JobChannel>> {
    None
}

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

    fn follow(&self, _: &str, _: &str, _: StreamStop, on: Box<dyn Fn(StreamEvent) + Send>) {
        on(StreamEvent::Closed(
            "live updates are only available in the browser build".to_owned(),
        ));
    }
}

// Storage and files.

/// Storage that lasts for the run, so switching scenes works natively too.
#[derive(Default)]
struct RunStore(Mutex<HashMap<String, String>>);

impl RunStore {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        // A panic while holding the lock cannot leave the map half-written.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl KeyValueStore for RunStore {
    fn get(&self, key: &str) -> Option<String> {
        self.map().get(key).cloned()
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SaveFailure> {
        self.map().insert(key.to_owned(), value.to_owned());
        Ok(())
    }
    fn remove(&self, key: &str) {
        self.map().remove(key);
    }
    fn keys(&self) -> Vec<String> {
        self.map().keys().cloned().collect()
    }
}

/// Downloads are written to the working directory; nothing can be picked.
struct WorkingDirectory;

impl FileDialogs for WorkingDirectory {
    fn download(&self, name: &str, _: &str, text: &str) -> Result<(), String> {
        std::fs::write(name, text).map_err(|e| format!("could not write {name} ({e})"))
    }
    fn pick_text(&self, _: &str, _: Box<dyn FnOnce(Result<PickedFile, String>) + Send>) {}
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
