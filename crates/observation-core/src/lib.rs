//! Pure, engine-free logic shared by the Observation server and client.
//!
//! # Coordinate system
//! All local geometry uses the *world* frame, which matches Bevy's convention:
//! `+X` = east, `+Y` = up, `-Z` = north (so `+Z` = south), in metres, relative
//! to an origin at the centre of the loaded area. 2D footprint coordinates are
//! `[x, z]` pairs in the same frame.
//!
//! # Layers
//! Each module belongs to a layer and uses only its own layer or lower ones:
//!
//! | Layer | Modules | Holds |
//! | --- | --- | --- |
//! | 0 | `util` | stable hashing, UTC times, plan-view geometry, URL checks |
//! | 1 | [`geo`], [`tiles`] | coordinates, projection, map tiles |
//! | 2 | [`building`], `road`, [`address`], [`terrain`], [`camera`] | the world model |
//! | 3 | `osm`, [`bundle`] | OSM input, and the area bundle built from it |
//! | 4 | [`frustum`], [`occlusion`], [`coverage`], [`example`] | analysis |
//! | 5 | [`coverage_job`], [`regions`], [`provenance`], [`scene`] | messages and records exchanged with the outside |
//!
//! `testing` (fixture builders, only with the `testing` feature or in tests)
//! may use anything. `tests/layering.rs` enforces the rule; the few upward
//! edges that remain are listed there, each with its reason.

pub mod address;
pub mod building;
pub mod bundle;
pub mod camera;
pub mod coverage;
pub mod coverage_job;
pub mod example;
pub mod frustum;
pub mod geo;
pub mod occlusion;
pub(crate) mod osm;
pub mod provenance;
pub mod regions;
pub(crate) mod road;
pub mod scene;
pub mod terrain;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tiles;
pub(crate) mod util;

pub use glam;
