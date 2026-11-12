//! End-to-end tests of the HTTP API against local mock upstreams (no network).
#![allow(clippy::unwrap_used)] // integration-test helpers live outside `#[test]` fns

mod area;
mod basics;
mod elevation;
mod harness;
mod limits;
// Also compiled into `tests/adapters.rs`, which alone uses the socket tile mock.
#[allow(dead_code)]
mod mocks;
mod overpass;
mod regions;
mod static_files;
mod tiles;
