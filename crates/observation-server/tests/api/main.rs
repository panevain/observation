//! End-to-end tests of the HTTP API on `TestApp`: the application over fake
//! upstreams, a fake region agent and paused time (no sockets, no network).
//! The production adapters' socket tests are in `tests/adapters.rs`.
#![allow(clippy::unwrap_used)] // integration-test helpers live outside `#[test]` fns

mod area;
mod basics;
#[path = "../common/mod.rs"]
mod common;
mod elevation;
mod harness;
mod limits;
mod overpass;
mod regions;
mod static_files;
mod tiles;
