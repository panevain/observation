//! End-to-end tests of the HTTP API on `TestApp`, the real router over fake
//! ports and paused time, apart from `regions.rs`, which still runs on local
//! sockets. The production adapters' socket tests are in `tests/adapters.rs`.
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
