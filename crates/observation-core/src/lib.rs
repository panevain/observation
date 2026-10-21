//! Pure, engine-free logic shared by the Observation server and client.
//!
//! # Coordinate system
//! All local geometry uses the *world* frame, which matches Bevy's convention:
//! `+X` = east, `+Y` = up, `-Z` = north (so `+Z` = south), in metres, relative
//! to an origin at the centre of the loaded area. 2D footprint coordinates are
//! `[x, z]` pairs in the same frame.

pub mod building;
pub mod bundle;
pub mod camera;
pub mod coverage;
pub mod example;
pub mod frustum;
pub mod geo;
pub mod occlusion;
pub mod regions;
pub mod scene;
pub mod tiles;

pub use glam;
