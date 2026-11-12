//! The pipeline from OSM features to a bundle.

use super::features::{OsmAddressPoint, OsmBuilding, OsmFeatures, OsmRoad};
use super::tags::names_addresses;
use crate::address::{AddressPoint, building_of};
use crate::building::{Footprint, MeshData, extrude};
use crate::bundle::{AreaBundle, Building};
use crate::geo::{BBox, LatLon, Projection};
use crate::road::Road;
use crate::terrain::Terrain;
use crate::util::planar::ring_bounds;

/// Facts about a bundle that do not come from its features.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildMeta<'a> {
    /// The data credits shown with the area.
    pub attribution: &'a str,
    /// When the bundle was built, Unix seconds.
    pub generated_unix: u64,
}

/// Projects, extrudes and packs OSM features into a bundle, standing each
/// building on the lowest ground under it: [`build`] with its inputs as
/// separate arguments.
pub fn build_bundle(
    bbox: BBox,
    buildings: &[OsmBuilding],
    roads: &[OsmRoad],
    addresses: &[OsmAddressPoint],
    terrain: Terrain,
    attribution: impl Into<String>,
    generated_unix: u64,
    osm_base_unix: Option<u64>,
) -> AreaBundle {
    let features = OsmFeatures {
        buildings,
        roads,
        addresses,
        osm_base_unix,
    };
    let attribution = attribution.into();
    let meta = BuildMeta {
        attribution: &attribution,
        generated_unix,
    };
    build(bbox, &features, terrain, meta)
}

/// Projects, extrudes and packs OSM features into a bundle, standing each
/// building on the lowest ground under it.
pub fn build(bbox: BBox, features: &OsmFeatures, terrain: Terrain, meta: BuildMeta) -> AreaBundle {
    let OsmFeatures {
        buildings,
        roads,
        addresses,
        osm_base_unix,
    } = *features;
    let osm_fingerprint = features.fingerprint(&bbox);
    let origin = bbox.center();
    let proj = Projection::new(origin);
    let to_w = |p: &LatLon| {
        let w = proj.to_world(*p);
        [w[0] as f32, w[1] as f32]
    };
    let mut mesh = MeshData::default();
    let mut out = Vec::with_capacity(buildings.len());
    for b in buildings {
        let outer: Vec<[f32; 2]> = b.outer.iter().map(to_w).collect();
        let footprint = Footprint {
            base_m: terrain.min_under(&outer),
            outer,
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
                outer: crate::util::planar::clean_ring(&footprint.outer),
                holes: footprint
                    .holes
                    .iter()
                    .map(|h| crate::util::planar::clean_ring(h))
                    .collect(),
                ..footprint
            };
            out.push(Building {
                osm_id: b.osm_id,
                footprint,
                index_range: [start, mesh.indices.len() as u32 - start],
                address: b.address.clone(),
            });
        }
    }
    let roads: Vec<Road> = roads
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
    let streets: Vec<(&str, &[[f32; 2]])> = roads
        .iter()
        .filter(|r| names_addresses(&r.highway))
        .filter_map(|r| Some((r.name.as_deref()?, r.points.as_slice())))
        .collect();
    for b in &mut out {
        if let Some(a) = &mut b.address {
            let (lo, hi) = ring_bounds(&b.footprint.outer);
            let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
            a.fill_street(centre, streets.iter().copied());
        }
    }
    let footprints: Vec<&Footprint> = out.iter().map(|b| &b.footprint).collect();
    let bounds: Vec<_> = footprints.iter().map(|f| ring_bounds(&f.outer)).collect();
    let addresses = addresses
        .iter()
        .map(|a| {
            let position = to_w(&a.point);
            let building = building_of(position, &footprints, &bounds);
            let mut address = a.address.clone();
            address.fill_street(position, streets.iter().copied());
            AddressPoint {
                address,
                position,
                building: building.map(|i| i as u32),
            }
        })
        .collect();
    AreaBundle {
        bbox,
        origin,
        buildings: out,
        roads,
        mesh,
        addresses,
        terrain,
        attribution: meta.attribution.to_owned(),
        generated_unix: meta.generated_unix,
        osm_base_unix,
        osm_fingerprint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::BundleError;
    use crate::osm::fingerprint::osm_fingerprint;
    use crate::testing::sample_bundle_on as sample_on;

    #[test]
    fn build_bundle_is_build_with_separate_arguments() {
        use crate::testing::{rich_bbox, rich_bundle, rich_features, rich_terrain};
        let (buildings, roads, addresses) = rich_features();
        let features = OsmFeatures {
            buildings: &buildings,
            roads: &roads,
            addresses: &addresses,
            osm_base_unix: Some(1_793_475_782),
        };
        let meta = BuildMeta {
            attribution: "© OpenStreetMap contributors",
            generated_unix: 1_793_500_000,
        };
        let built = build(rich_bbox(), &features, rich_terrain(), meta);
        assert_eq!(built.encode(), rich_bundle().encode());
        assert_eq!(built.osm_fingerprint, features.fingerprint(&rich_bbox()));
        assert_eq!(
            built.osm_fingerprint,
            osm_fingerprint(&rich_bbox(), &buildings, &roads, &addresses)
        );
        assert_eq!(built.osm_base_unix, Some(1_793_475_782));
        assert_eq!(built.generated_unix, 1_793_500_000);
        assert_eq!(built.attribution, "© OpenStreetMap contributors");
    }

    #[test]
    fn buildings_stand_on_the_lowest_ground_under_them() {
        // Falling 1 m per 10 m eastward; the house spans x = 0 to about 7.4.
        let terrain = Terrain::sample_from(
            glam::Vec2::splat(-300.0),
            glam::Vec2::splat(300.0),
            |p| Some(50.0 - p.x / 10.0),
            "dem",
        )
        .unwrap();
        let b = sample_on(terrain);
        let fp = &b.buildings[0].footprint;
        let east = fp.outer.iter().map(|p| p[0]).fold(f32::MIN, f32::max);
        assert!((fp.base_m + east / 10.0).abs() < 0.06, "{}", fp.base_m);
        let ys = || b.mesh.positions.iter().map(|p| p[1]);
        assert!((ys().fold(f32::MAX, f32::min) - fp.base_m).abs() < 1e-4);
        assert!((ys().fold(f32::MIN, f32::max) - fp.roof_y()).abs() < 1e-4);
        assert_eq!(AreaBundle::decode(&b.encode()).unwrap(), b);
        // A grid whose size does not match its samples is refused.
        let mut bad = b.clone();
        bad.terrain.heights_dm.pop();
        assert!(matches!(
            AreaBundle::decode(&bad.encode()),
            Err(BundleError::Decode(_))
        ));
    }
}
