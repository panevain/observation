//! An MCP server for Observation (spec 2026-07-28, through `rmcp`).
//!
//! It gives an AI agent the camera catalogue, editing of scene files, and
//! coverage figures, all computed in process with `observation-core` and
//! no network. Scenes are not stored: every scene tool takes a scene file
//! (the JSON the app saves and imports) and returns the edited one, which
//! [`SceneFile::from_json`](observation_core::scene::SceneFile::from_json)
//! has checked, so the app can always import the result.
//!
//! Only the demo street is available offline, so the tools that need an
//! area's buildings and terrain (placing a camera at a height above the
//! ground, coverage, addresses, example cameras) work on demo scenes only,
//! unless the user connects their open app ([`link`]): then the app's
//! server sends the real areas their scenes are on.
//!
//! | Module | Holds |
//! | --- | --- |
//! | [`catalog`] | the embedded camera catalogue and model lookup |
//! | [`area`] | the demo street, built once on first use, and the connection's real areas |
//! | [`link`] | the connection to the user's open app, through the server's agent bridge |
//! | [`app`] | the tools that connect to the app and read its state |
//! | [`propose`] | the tools that propose a change to the app's scene and follow the user's decision |
//! | [`edit`] | the scene edits, as plain functions on a [`SceneFile`](observation_core::scene::SceneFile) |
//! | [`figures`] | coverage figures for a whole scene |
//! | [`server`] | the MCP tools, resources and prompts |

pub mod app;
pub mod area;
pub mod catalog;
pub mod edit;
pub mod error;
pub mod figures;
pub mod link;
pub mod propose;
pub mod server;

pub use error::ToolError;
pub use server::ObservationMcp;
