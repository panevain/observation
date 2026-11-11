//! OSM features as parsed, before projection.

use crate::address::Address;
use crate::geo::LatLon;

/// A building as parsed from OSM, before projection.
#[derive(Debug, Clone, PartialEq)]
pub struct OsmBuilding {
    pub osm_id: i64,
    pub outer: Vec<LatLon>,
    pub holes: Vec<Vec<LatLon>>,
    pub min_height_m: f32,
    pub height_m: f32,
    pub address: Option<Address>,
}

/// An address node as parsed from OSM, before projection.
#[derive(Debug, Clone, PartialEq)]
pub struct OsmAddressPoint {
    pub address: Address,
    pub point: LatLon,
}

/// A road as parsed from OSM, before projection.
#[derive(Debug, Clone, PartialEq)]
pub struct OsmRoad {
    pub osm_id: i64,
    pub highway: String,
    pub name: Option<String>,
    pub width_m: f32,
    pub points: Vec<LatLon>,
}
