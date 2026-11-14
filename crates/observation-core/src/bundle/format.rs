//! The bundle's types and its byte encoding.

use crate::address::{Address, AddressPoint};
use crate::building::{Footprint, MeshData};
use crate::geo::{BBox, LatLon, Projection};
use crate::road::Road;
use crate::terrain::Terrain;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const BUNDLE_MAGIC: [u8; 4] = *b"OBSB";
pub const BUNDLE_VERSION: u16 = 4;
/// MIME type used on the wire.
pub const BUNDLE_CONTENT_TYPE: &str = "application/vnd.observation.bundle";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Building {
    pub osm_id: i64,
    pub footprint: Footprint,
    /// `[first_index, index_count]` into [`AreaBundle::mesh`] indices.
    pub index_range: [u32; 2],
    /// From the building's own `addr:*` tags.
    pub address: Option<Address>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaBundle {
    pub bbox: BBox,
    /// World-frame origin (the bbox centre).
    pub origin: LatLon,
    pub buildings: Vec<Building>,
    pub roads: Vec<Road>,
    pub mesh: MeshData,
    /// Addresses mapped as nodes rather than on a building.
    pub addresses: Vec<AddressPoint>,
    /// Ground heights over the area and its margin (flat without
    /// elevation data).
    pub terrain: Terrain,
    pub attribution: String,
    /// When the server built this bundle, Unix seconds (0 if unknown). Not
    /// when the map data was current: see [`Self::osm_base_unix`].
    pub generated_unix: u64,
    /// When the OSM data was current, Unix seconds: Overpass's
    /// `osm3s.timestamp_osm_base`, the time of the last change its database
    /// holds. `None` when the source did not say.
    pub osm_base_unix: Option<u64>,
    /// [`osm_fingerprint`](super::osm_fingerprint) of the map features the
    /// bundle was built from, so a saved scene can tell whether its area
    /// changed (the data date alone cannot: public Overpass's moves every
    /// minute).
    pub osm_fingerprint: u64,
}

#[derive(Debug, Error, PartialEq)]
pub enum BundleError {
    #[error("the reply is not an area bundle (a proxy or sign-in page may have answered)")]
    Magic,
    #[error("the server sent bundle version {0}, but this app reads version {BUNDLE_VERSION}")]
    Version(u16),
    #[error("the area data is damaged or cut short ({0})")]
    Decode(String),
}

impl AreaBundle {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.mesh.positions.len() * 24);
        out.extend_from_slice(&BUNDLE_MAGIC);
        out.extend_from_slice(&BUNDLE_VERSION.to_le_bytes());
        // Serialising plain data into a Vec cannot fail.
        postcard::to_extend(self, out).unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, BundleError> {
        if bytes.len() < 6 || bytes[..4] != BUNDLE_MAGIC {
            return Err(BundleError::Magic);
        }
        let v = u16::from_le_bytes([bytes[4], bytes[5]]);
        if v != BUNDLE_VERSION {
            return Err(BundleError::Version(v));
        }
        let bundle: Self =
            postcard::from_bytes(&bytes[6..]).map_err(|e| BundleError::Decode(e.to_string()))?;
        bundle
            .terrain
            .validate()
            .map_err(|_| BundleError::Decode("inconsistent terrain grid".to_owned()))?;
        Ok(bundle)
    }

    pub fn projection(&self) -> Projection {
        Projection::new(self.origin)
    }

    /// Footprints only (for occlusion), in bundle order.
    pub fn footprints(&self) -> impl Iterator<Item = &Footprint> {
        self.buildings.iter().map(|b| &b.footprint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_bundle as sample;

    #[test]
    fn round_trip() {
        let b = sample();
        assert_eq!(b.buildings.len(), 1);
        assert_eq!(b.buildings[0].index_range[0], 0);
        assert_eq!(b.buildings[0].index_range[1] as usize, b.mesh.indices.len());
        // The address node lies inside the building.
        assert_eq!(b.addresses[0].building, Some(0));
        let wall = b.buildings[0].footprint.outer[0];
        assert_eq!(
            b.address_at([wall[0], wall[1] + 0.15]).map(Address::label),
            Some("12 Test St".to_owned())
        );
        assert_eq!(b.buildings_at(|a| a.label() == "12 Test St"), vec![0]);
        assert!(b.buildings_at(|a| a.label() == "14 Test St").is_empty());
        // A building tagged with the address itself, also holding the point.
        let mut tagged = b.clone();
        let own = Address {
            housenumber: Some("12".into()),
            street: Some("Test St".into()),
            ..Address::default()
        };
        tagged.buildings[0].address = Some(own.clone());
        tagged.addresses.clear();
        assert_eq!(tagged.buildings_at(|a| *a == own), vec![0]);
        let bytes = b.encode();
        assert_eq!(&bytes[..4], b"OBSB");
        assert_eq!(AreaBundle::decode(&bytes).unwrap(), b);
        assert_eq!(b.osm_base_unix, Some(1_699_990_000));
        assert_ne!(b.osm_fingerprint, 0);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(AreaBundle::decode(b"nope"), Err(BundleError::Magic));
        let mut bytes = sample().encode();
        bytes[4] = 99;
        assert_eq!(AreaBundle::decode(&bytes), Err(BundleError::Version(99)));
        let mut bytes = sample().encode();
        bytes.truncate(20);
        assert!(matches!(
            AreaBundle::decode(&bytes),
            Err(BundleError::Decode(_))
        ));
    }
}
