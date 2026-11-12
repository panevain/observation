//! OSM features as parsed, before projection.

use super::fingerprint::osm_fingerprint;
use crate::address::Address;
use crate::geo::{BBox, LatLon};

/// The OSM features an area's bundle is built from, as the server's parser
/// hands them over, and the date of the OSM data they come from.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OsmFeatures<'a> {
    pub buildings: &'a [OsmBuilding],
    pub roads: &'a [OsmRoad],
    pub addresses: &'a [OsmAddressPoint],
    /// When the OSM data was last updated (Overpass's `osm_base`), Unix
    /// seconds, if known.
    pub osm_base_unix: Option<u64>,
}

impl OsmFeatures<'_> {
    /// [`osm_fingerprint`] of these features in `bbox` (their date is not
    /// part of it).
    pub fn fingerprint(&self, bbox: &BBox) -> u64 {
        osm_fingerprint(bbox, self.buildings, self.roads, self.addresses)
    }
}

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
