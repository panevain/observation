//! The native build's side of [`crate::platform`]: no network, no storage,
//! downloads written to the working directory, and a dark colour scheme.

use super::{HttpRequest, SaveFailure};
use crossbeam_channel::Sender;

// The loading screen.

/// There is no HTML loading screen natively.
pub fn hide_loader() {}

// Networking.

pub fn spawn_fetch(req: HttpRequest, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    done(Err(format!(
        "networking is only available in the browser build ({})",
        req.url
    )));
}

pub fn spawn_event_source(_: &str, _: &str, on: impl Fn(Result<Vec<u8>, String>) + 'static) {
    on(Err(
        "live updates are only available in the browser build".to_owned()
    ));
}

// Storage and files.

pub fn storage_get(_: &str) -> Option<String> {
    None
}
pub fn storage_set(_: &str, _: &str) -> Result<(), SaveFailure> {
    Ok(())
}
pub fn download(name: &str, text: &str) -> Result<(), String> {
    std::fs::write(name, text).map_err(|e| format!("could not write {name} ({e})"))
}
pub fn pick_file(_: Sender<Result<String, String>>) {}

// The clock and session storage.

/// Seconds since the Unix epoch by this computer's clock.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn session_get(_: &str) -> Option<String> {
    None
}

pub fn session_set(_: &str, _: &str) {}

// The colour scheme.

pub fn prefers_dark() -> bool {
    true
}

/// Native builds stay dark: there is no system setting to follow.
pub fn watch_scheme(_: Sender<bool>) {}
