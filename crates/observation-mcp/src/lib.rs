//! An MCP server for Observation (spec 2026-07-28, through `rmcp`), over
//! stdio or Streamable HTTP.
//!
//! It gives an AI agent the camera catalogue, editing of scene files, and
//! coverage figures, all computed in process with `observation-core` and
//! no network. Every scene tool takes a scene file (the JSON the app saves
//! and imports), or the id of a working scene the server holds in memory
//! ([`store`]), and returns the edited one, which
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
//! | [`view`] | the tools that move the app's view, select and mark things, change view settings, find places and ask to load an area |
//! | [`edit`] | the scene edits, as plain functions on a [`SceneFile`](observation_core::scene::SceneFile) |
//! | [`figures`] | coverage figures for a whole scene |
//! | [`places`] | labelled areas and targets, and the tools that edit them |
//! | [`queries`] | verdicts, per-camera splits and street gaps, and their tools |
//! | [`describe`] | the scene in words (surroundings, buildings, streets, mount points, a camera's view), and [`geometry`], its bearings, compass words and street helpers |
//! | [`render`] | a top-down map of a scene, as a character grid or a PNG, and its tool |
//! | [`optimise`] | the goal tools, which search placements and aims for a goal |
//! | [`schema`] | the schema of tool fields held as untyped JSON objects |
//! | [`server`] | the MCP tools, resources and prompts |
//! | [`guide`] | `get_started`: the workflow, conventions and wording in one short call |
//! | [`toolsets`] | tool groups and profiles, the trimmed schemas small models read |
//! | [`store`] | working scenes held by id (`s1`), `scene_id` on every scene tool, and the tools that open, export, undo, compare and batch-edit them |
//! | [`http`] | the Streamable HTTP transport and its bearer-token authentication |
//!
//! The binary serves over stdio, or over HTTP when `OBS_MCP_LISTEN` or
//! `--http` gives an address.

pub mod app;
pub mod area;
pub mod catalog;
pub mod describe;
pub mod edit;
pub mod error;
pub mod figures;
pub mod geometry;
pub mod guide;
pub mod http;
pub mod link;
pub mod optimise;
pub mod places;
pub mod propose;
pub mod queries;
pub mod render;
pub mod schema;
pub mod server;
pub mod store;
pub mod toolsets;
pub mod view;

pub use error::ToolError;
pub use server::ObservationMcp;
