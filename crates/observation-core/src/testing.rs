//! Fixture builders for tests, in this crate and in the crates that use it.
//!
//! Compiled only for this crate's own tests and with the `testing` feature,
//! which other crates enable from their `[dev-dependencies]` (never from
//! `[dependencies]`). Every builder is deterministic: the same call gives
//! the same value on every platform, so golden files can be built from them.
//!
//! World builders ([`box_footprint`], [`slope`]) take metres in the world
//! frame (`x` east, `z` south, `y` up); the `sample_*` and `rich_*` builders
//! start from OSM latitudes and longitudes.

use crate::address::Address;
use crate::building::Footprint;
use crate::bundle::AreaBundle;
use crate::geo::{BBox, LatLon};
use crate::osm::build::build_bundle;
use crate::osm::features::{OsmAddressPoint, OsmBuilding, OsmRoad};
use crate::terrain::Terrain;
use glam::Vec2;

/// A box-shaped building standing on flat ground at `y = 0`: world `[x, z]`
/// corners `(x0, z0)` to `(x1, z1)`, `height` metres tall.
pub fn box_footprint(x0: f32, z0: f32, x1: f32, z1: f32, height: f32) -> Footprint {
    Footprint {
        outer: vec![[x0, z0], [x1, z0], [x1, z1], [x0, z1]],
        holes: vec![],
        min_height_m: 0.0,
        height_m: height,
        base_m: 0.0,
    }
}

/// A plane rising 1 m per 10 m eastward, `y = x / 10`, over ±200 m, with
/// the origin 100 m above sea level.
pub fn slope() -> Terrain {
    Terrain::sample_from(
        Vec2::splat(-200.0),
        Vec2::splat(200.0),
        |p| Some(100.0 + p.x / 10.0),
        "test",
    )
    .expect("slope(): sampling a plane over ±200 m gives valid terrain")
}

/// A small bundle on flat ground: one 6.5 m house about 7 m square at the
/// origin of a 200 m box in London, a road south of it and an address node
/// inside it.
pub fn sample_bundle() -> AreaBundle {
    sample_bundle_on(Terrain::default())
}

/// [`sample_bundle`] on the given ground.
pub fn sample_bundle_on(terrain: Terrain) -> AreaBundle {
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
        address: None,
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
    let a = OsmAddressPoint {
        address: Address {
            housenumber: Some("12".into()),
            street: Some("Test St".into()),
            ..Address::default()
        },
        point: LatLon::new(c.lat + d / 2.0, c.lon + d / 2.0),
    };
    build_bundle(
        bbox,
        &[b],
        &[r],
        &[a],
        terrain,
        "© OpenStreetMap contributors",
        1_700_000_000,
        Some(1_699_990_000),
    )
}

// ----- the rich area ---------------------------------------------------------
//
// An area that uses everything the bundle format carries, centred exactly on
// the equator so that its projection's cosine is exactly 1 and nothing built
// from it depends on the platform's trigonometry. The golden files are built
// from it, so changing it changes them.

/// A 0.001° × 0.0012° box (about 111 m × 134 m) centred on the equator at
/// 10° E.
pub fn rich_bbox() -> BBox {
    BBox {
        south: -0.0005,
        west: 9.9994,
        north: 0.0005,
        east: 10.0006,
    }
}

/// A closed ring through `corners` (lat, lon offsets from the box centre).
fn rich_ring(corners: &[(f64, f64)]) -> Vec<LatLon> {
    let c = rich_bbox().center();
    let mut r: Vec<LatLon> = corners
        .iter()
        .map(|&(lat, lon)| LatLon::new(c.lat + lat, c.lon + lon))
        .collect();
    r.extend(r.first().copied());
    r
}

/// Ground falling to the north-west and rising to the south-east, under
/// [`rich_bbox`] and its margin, written out sample by sample (no elevation
/// function involved).
pub fn rich_terrain() -> Terrain {
    let (cols, rows) = (12u32, 11u32);
    let mut heights_dm = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let (c, r) = (col as i16 - 6, row as i16 - 5);
            heights_dm.push(c * 4 + r * 3 + (c * r) % 5);
        }
    }
    Terrain {
        min: [-220.0, -210.0],
        cell_m: 40.0,
        cols,
        rows,
        heights_dm,
        datum_m: 123.5,
        attribution: "Elevation: golden fixture".to_owned(),
    }
}

/// OSM features in [`rich_bbox`] as the server's parser hands them over: a
/// house whose address lacks a street (filled from the nearest road), a
/// relation with a courtyard, a raised building part, three roads (one
/// running far beyond the box) and two address points.
pub fn rich_features() -> (Vec<OsmBuilding>, Vec<OsmRoad>, Vec<OsmAddressPoint>) {
    let d = 0.0001;
    let buildings = vec![
        OsmBuilding {
            osm_id: 101,
            outer: rich_ring(&[(0.0, -2.0 * d), (0.0, -d), (d, -d), (d, -2.0 * d)]),
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.5,
            address: Some(Address {
                housenumber: Some("1".into()),
                ..Address::default()
            }),
        },
        OsmBuilding {
            osm_id: -202,
            outer: rich_ring(&[
                (0.0, 0.5 * d),
                (0.0, 3.0 * d),
                (2.0 * d, 3.0 * d),
                (2.0 * d, 0.5 * d),
            ]),
            holes: vec![rich_ring(&[
                (0.6 * d, 1.2 * d),
                (1.4 * d, 1.2 * d),
                (1.4 * d, 2.3 * d),
                (0.6 * d, 2.3 * d),
            ])],
            min_height_m: 0.0,
            height_m: 12.0,
            address: Some(Address {
                housename: Some("Ring House".into()),
                street: Some("Equator Road".into()),
                unit: Some("B".into()),
                ..Address::default()
            }),
        },
        OsmBuilding {
            osm_id: 303,
            outer: rich_ring(&[(2.5 * d, -d), (2.5 * d, 0.2 * d), (3.2 * d, -0.4 * d)]),
            holes: vec![],
            min_height_m: 3.0,
            height_m: 9.0,
            address: None,
        },
    ];
    let c = rich_bbox().center();
    let at = |lat: f64, lon: f64| LatLon::new(c.lat + lat, c.lon + lon);
    let roads = vec![
        OsmRoad {
            osm_id: 7001,
            highway: "residential".into(),
            name: Some("Equator Road".into()),
            width_m: 6.0,
            points: vec![
                at(-2.0 * d, -0.2),
                at(-2.0 * d, -5.0 * d),
                at(-2.0 * d, 5.0 * d),
                at(-2.5 * d, 0.2),
            ],
        },
        OsmRoad {
            osm_id: 7002,
            highway: "footway".into(),
            name: None,
            width_m: 2.0,
            points: vec![at(-2.0 * d, 0.0), at(0.0, 0.2 * d), at(2.4 * d, 0.2 * d)],
        },
        OsmRoad {
            osm_id: 7003,
            highway: "service".into(),
            name: Some("Back Lane".into()),
            width_m: 6.5,
            points: vec![at(3.5 * d, -5.0 * d), at(3.5 * d, 5.0 * d)],
        },
    ];
    let addresses = vec![
        OsmAddressPoint {
            address: Address {
                housenumber: Some("3".into()),
                street: Some("Equator Road".into()),
                unit: Some("1".into()),
                ..Address::default()
            },
            point: at(1.0 * d, 1.0 * d),
        },
        OsmAddressPoint {
            address: Address {
                housenumber: Some("5".into()),
                ..Address::default()
            },
            point: at(-0.5 * d, 4.0 * d),
        },
    ];
    (buildings, roads, addresses)
}

/// [`rich_features`] built into a bundle on [`rich_terrain`].
pub fn rich_bundle() -> AreaBundle {
    let (buildings, roads, addresses) = rich_features();
    build_bundle(
        rich_bbox(),
        &buildings,
        &roads,
        &addresses,
        rich_terrain(),
        "© OpenStreetMap contributors",
        1_793_500_000,
        Some(1_793_475_782),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_are_what_they_say() {
        assert!(!slope().is_flat());
        assert!(rich_terrain().validate().is_ok());
        assert_eq!(rich_bundle().origin, LatLon::new(0.0, 10.0));
        let b = box_footprint(0.0, 1.0, 2.0, 3.0, 4.0);
        assert_eq!((b.outer[2], b.roof_y()), ([2.0, 3.0], 4.0));
    }
}
