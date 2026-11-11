//! Engine-free client logic for Observation.
//!
//! The client's state and the rules that change it, without Bevy, egui or
//! `web-sys`, so they build for the native target and are tested with plain
//! `cargo test`. `observation-client` renders this model and feeds it input.

pub mod platform;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
