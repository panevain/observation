//! Engine-free client logic for Observation.
//!
//! The client's state and the rules that change it, without Bevy, egui or
//! `web-sys`, so they build for the native target and are tested with plain
//! `cargo test`. `observation-client` renders this model and feeds it input.

pub mod agent;
pub mod commands;
pub mod coverage;
pub mod document;
pub mod editor;
pub mod evidence;
pub mod footage;
pub mod history;
pub mod net;
pub mod persist;
pub mod place;
pub mod places;
pub mod platform;
pub mod proposal;
pub mod scenes;
pub mod slots;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod toasts;
pub mod vm;
