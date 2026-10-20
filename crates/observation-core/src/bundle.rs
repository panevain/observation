//! The `AreaBundle` wire format the server sends and the client renders.
//!
//! Layout: `b"OBSB"` magic, `u16` little-endian format version, then a
//! postcard-encoded [`AreaBundle`].

use crate::building::{Footprint, MeshData, extrude};
use crate::geo::{BBox, LatLon, Projection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const BUNDLE_MAGIC: [u8; 4] = *b"OBSB";
pub const BUNDLE_VERSION: u16 = 1;
/// MIME type used on the wire.
pub const BUNDLE_CONTENT_TYPE: &str = "application/vnd.observation.bundle";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Building {
    pub osm_id: i64,
    pub footprint: Footprint,
    /// `[first_index, index_count]` into [`AreaBundle::mesh`] indices.
    pub index_range: [u32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Road {
    pub osm_id: i64,
    /// OSM `highway=*` value.
    pub highway: String,
    pub name: Option<String>,
    /// Carriageway width used for street-coverage statistics, metres.
    pub width_m: f32,
    /// Polyline in world `[x, z]`.
    pub points: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaBundle {
    pub bbox: BBox,
    /// World-frame origin (the bbox centre).
    pub origin: LatLon,
    pub buildings: Vec<Building>,
    pub roads: Vec<Road>,
    pub mesh: MeshData,
    pub attribution: String,
    pub generated_unix: u64,
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
        postcard::from_bytes(&bytes[6..]).map_err(|e| BundleError::Decode(e.to_string()))
    }

    pub fn projection(&self) -> Projection {
        Projection::new(self.origin)
    }

    /// Footprints only (for occlusion), in bundle order.
    pub fn footprints(&self) -> impl Iterator<Item = &Footprint> {
        self.buildings.iter().map(|b| &b.footprint)
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

/// `highway=*` values for ways people walk or cycle on rather than drive.
pub const FOOT_WAYS: [&str; 5] = ["footway", "path", "pedestrian", "cycleway", "steps"];

/// Estimated carriageway width from OSM `highway`, `lanes` and `width` tags.
/// Returns `None` for highway types that are not part of the street surface.
pub fn road_width_m(highway: &str, lanes: Option<&str>, width: Option<&str>) -> Option<f32> {
    let base = match highway {
        "motorway" | "trunk" => 14.0,
        "primary" => 10.0,
        "secondary" => 8.0,
        "tertiary" => 7.0,
        "residential" | "unclassified" => 6.0,
        "living_street" => 5.0,
        "service" => 4.0,
        h if FOOT_WAYS.contains(&h) => 2.0,
        "motorway_link" | "trunk_link" | "primary_link" | "secondary_link" | "tertiary_link" => 6.0,
        _ => return None,
    };
    if let Some(w) = width
        .and_then(crate::building::parse_length_m)
        .filter(|w| (1.0..60.0).contains(w))
    {
        return Some(w);
    }
    if let Some(l) = lanes
        .and_then(|l| l.trim().parse::<f32>().ok())
        .filter(|l| (1.0..12.0).contains(l))
    {
        return Some(l * 3.25);
    }
    Some(base)
}

/// Projects, extrudes and packs OSM features into a bundle.
pub fn build_bundle(
    bbox: BBox,
    buildings: &[OsmBuilding],
    roads: &[OsmRoad],
    attribution: impl Into<String>,
    generated_unix: u64,
) -> AreaBundle {
    let origin = bbox.center();
    let proj = Projection::new(origin);
    let to_w = |p: &LatLon| {
        let w = proj.to_world(*p);
        [w[0] as f32, w[1] as f32]
    };
    let mut mesh = MeshData::default();
    let mut out = Vec::with_capacity(buildings.len());
    for b in buildings {
        let footprint = Footprint {
            outer: b.outer.iter().map(to_w).collect(),
            holes: b
                .holes
                .iter()
                .map(|h| h.iter().map(to_w).collect())
                .collect(),
            min_height_m: b.min_height_m,
            height_m: b.height_m,
        };
        let start = mesh.indices.len() as u32;
        if extrude(&footprint, &mut mesh) {
            let footprint = Footprint {
                outer: crate::building::clean_ring(&footprint.outer),
                holes: footprint
                    .holes
                    .iter()
                    .map(|h| crate::building::clean_ring(h))
                    .collect(),
                ..footprint
            };
            out.push(Building {
                osm_id: b.osm_id,
                footprint,
                index_range: [start, mesh.indices.len() as u32 - start],
            });
        }
    }
    let roads = roads
        .iter()
        .filter(|r| r.points.len() >= 2)
        .map(|r| Road {
            osm_id: r.osm_id,
            highway: r.highway.clone(),
            name: r.name.clone(),
            width_m: r.width_m,
            points: r.points.iter().map(to_w).collect(),
        })
        .collect();
    AreaBundle {
        bbox,
        origin,
        buildings: out,
        roads,
        mesh,
        attribution: attribution.into(),
        generated_unix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AreaBundle {
        let bbox = BBox::around(LatLon::new(51.5, -0.12), 200.0);
        let c = bbox.center();
        let d = 0.0001;
        let b = OsmBuilding {
            osm_id: 42,
            outer: vec![
                LatLon::new(c.lat, c.lon),
                LatLon::new(c.lat, c.lon + d),
                LatLon::new(c.lat + d, c.lon + d),
                LatLon::new(c.lat + d, c.lon),
                LatLon::new(c.lat, c.lon),
            ],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.5,
        };
        let r = OsmRoad {
            osm_id: 7,
            highway: "residential".into(),
            name: Some("Test St".into()),
            width_m: 6.0,
            points: vec![
                LatLon::new(c.lat - d, c.lon - d),
                LatLon::new(c.lat - d, c.lon + 2.0 * d),
            ],
        };
        build_bundle(
            bbox,
            &[b],
            &[r],
            "© OpenStreetMap contributors",
            1_700_000_000,
        )
    }

    #[test]
    fn round_trip() {
        let b = sample();
        assert_eq!(b.buildings.len(), 1);
        assert_eq!(b.buildings[0].index_range[0], 0);
        assert_eq!(b.buildings[0].index_range[1] as usize, b.mesh.indices.len());
        let bytes = b.encode();
        assert_eq!(&bytes[..4], b"OBSB");
        assert_eq!(AreaBundle::decode(&bytes).unwrap(), b);
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

    #[test]
    fn road_widths() {
        assert_eq!(road_width_m("residential", None, None), Some(6.0));
        assert_eq!(road_width_m("residential", Some("2"), None), Some(6.5));
        assert_eq!(road_width_m("primary", Some("4"), Some("12 m")), Some(12.0));
        assert_eq!(road_width_m("proposed", None, None), None);
    }
}
