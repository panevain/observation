//! Region agent for Observation: runs next to Overpass's binaries, imports
//! Geofabrik extracts into per-region Overpass databases on request, and
//! answers Overpass queries for areas inside a ready region.
//!
//! The agent is an internal service for the Observation server; keep its port
//! off the public network and set `OBS_REGIONS_TOKEN` on both.
//!
//! | Route | |
//! | --- | --- |
//! | `GET /healthz` | liveness (no token) |
//! | `GET /regions?since=<version>` | [`RegionsSnapshot`](observation_core::regions::RegionsSnapshot); waits up to 25 s for a newer one |
//! | `GET /catalog` | importable extracts |
//! | `POST /catalog/sizes` | [`SizesRequest`](observation_core::regions::SizesRequest) -> `{"<id>": bytes or null}` |
//! | `POST /regions/import`, `/cancel`, `/delete` | `{"id": "…"}` |
//! | `POST /regions/settings` | `{"id": "…", "auto_refresh": true, "refresh_days": 30}` (either or both; `"refresh_days": null` returns to the default) |
//! | `POST /interpreter?bbox=s,w,n,e` | Overpass query (form field `data`) against the region holding the box; 404 if none. The answer carries `x-region` (the region's id) and, when the extract's date is known, `x-osm-base` (its `YYYY-MM-DDTHH:MM:SSZ` replication timestamp) |

pub mod catalog;
pub mod config;
pub mod import;
pub mod poly;
pub mod ports;
pub mod registry;

mod agent;
mod fs;
mod http;
mod query;
#[cfg(test)]
mod testing;
mod tools;

pub use crate::agent::{Agent, Ports, StartError};
pub use crate::fs::{write_atomic, write_atomic_blocking};
pub use crate::http::{AgentError, app};
