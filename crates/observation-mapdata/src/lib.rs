//! Map data for Observation, turned into an area bundle without any I/O:
//! the Overpass parser, the elevation tile decoder and the demo street built
//! from them. The server fetches the data for live areas; anything that
//! only needs the demo street (the MCP server, tests) gets it here without
//! the server's HTTP stack.
//!
//! | Module | Holds |
//! | --- | --- |
//! | [`overpass`] | parsing Overpass `out geom` JSON into buildings, roads and addresses |
//! | [`terrarium`] | decoding Terrarium elevation tiles (PNG) into [`DemTile`](observation_core::terrain::DemTile) values |
//! | [`demo`] | the demo street, built from the committed fixtures in `fixtures/` |

pub mod demo;
pub mod overpass;
pub mod terrarium;
#[cfg(test)]
mod test_rng;

/// The credit OpenStreetMap data needs, the server's default
/// `OBS_TILE_ATTRIBUTION`.
pub const OSM_ATTRIBUTION: &str = "© OpenStreetMap contributors";
