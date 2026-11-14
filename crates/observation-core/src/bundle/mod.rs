//! The `AreaBundle` wire format the server sends and the client renders.
//!
//! Layout: `b"OBSB"` magic, `u16` little-endian format version, then a
//! postcard-encoded [`AreaBundle`].
//!
//! Version 3 added the [`Terrain`](crate::terrain::Terrain) grid and
//! [`Footprint::base_m`](crate::building::Footprint::base_m); version 4
//! the OSM data date ([`AreaBundle::osm_base_unix`]) and
//! [`AreaBundle::osm_fingerprint`].

mod format;
mod query;

pub use crate::osm::build::{BuildMeta, build, build_bundle};
pub use crate::osm::features::{OsmAddressPoint, OsmBuilding, OsmFeatures, OsmRoad};
pub use crate::osm::fingerprint::osm_fingerprint;
pub use crate::osm::tags::{FOOT_WAYS, road_width_m};
pub use crate::road::Road;
pub use format::{
    AreaBundle, BUNDLE_CONTENT_TYPE, BUNDLE_MAGIC, BUNDLE_VERSION, Building, BundleError,
};
