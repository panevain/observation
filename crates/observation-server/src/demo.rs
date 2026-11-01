//! The demo bundle, built once at startup from the committed fixture.

use axum::body::Bytes;
use observation_core::bundle::build_bundle;
use observation_core::geo::BBox;
use observation_core::terrain::{Terrain, terrain_from_dem};
use observation_core::tiles::TileId;
use thiserror::Error;

use crate::elevation::decode_tile;
use crate::overpass::{ParseError, parse_response};

const OVERPASS_JSON: &[u8] = include_bytes!("../fixtures/demo-overpass.json");
const BBOX_TXT: &str = include_str!("../fixtures/demo-bbox.txt");
/// A tile's `(z, x, y)` and its PNG.
type DemFixture = ((u8, u32, u32), &'static [u8]);
/// The elevation tiles the demo terrain is sampled from (`fixtures/fetch-dem.sh`).
const DEM_TILES: [DemFixture; 2] = [
    (
        (15, 5260, 11429),
        include_bytes!("../fixtures/demo-dem-15-5260-11429.png"),
    ),
    (
        (15, 5260, 11430),
        include_bytes!("../fixtures/demo-dem-15-5260-11430.png"),
    ),
];

/// The embedded fixture could not be turned into a bundle.
#[derive(Debug, Error)]
pub enum DemoError {
    #[error("demo bbox: {0}")]
    BBox(#[from] observation_core::geo::BBoxError),
    #[error("demo overpass data: {0}")]
    Parse(#[from] ParseError),
    #[error("demo elevation tiles do not cover the demo street")]
    Terrain,
}

/// The demo street's terrain, from the recorded elevation tiles.
fn demo_terrain(bbox: &BBox, attribution: &str) -> Result<Terrain, DemoError> {
    let tiles = DEM_TILES
        .iter()
        .map(|&((z, x, y), png)| {
            let id = TileId::new(z, x, y).ok_or(DemoError::Terrain)?;
            decode_tile(id, png).map_err(|_| DemoError::Terrain)
        })
        .collect::<Result<Vec<_>, _>>()?;
    terrain_from_dem(bbox, &tiles, attribution).ok_or(DemoError::Terrain)
}

/// Builds the encoded demo bundle. Its terrain comes from the recorded tiles
/// whatever `OBS_DEM_URL` says, so the demo always works offline; it is
/// credited with `dem_attribution`.
pub fn build_demo(
    attribution: &str,
    dem_attribution: &str,
    generated_unix: u64,
) -> Result<Bytes, DemoError> {
    let bbox = BBox::parse(BBOX_TXT.trim())?;
    bbox.validate_size()?;
    let data = parse_response(OVERPASS_JSON)?;
    let bundle = build_bundle(
        bbox,
        &data.buildings,
        &data.roads,
        &data.addresses,
        demo_terrain(&bbox, dem_attribution)?,
        attribution,
        generated_unix,
    );
    Ok(Bytes::from(bundle.encode()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::bundle::AreaBundle;

    fn demo() -> AreaBundle {
        let bytes = build_demo("© OpenStreetMap contributors", "Elevation: test", 1).unwrap();
        AreaBundle::decode(&bytes).unwrap()
    }

    #[test]
    fn demo_bundle_decodes() {
        let b = demo();
        assert!(b.buildings.len() > 80, "{} buildings", b.buildings.len());
        assert!(!b.roads.is_empty());
        assert!(!b.mesh.is_empty());
    }

    #[test]
    fn demo_dem_tiles_match_the_bbox() {
        let bbox = BBox::parse(BBOX_TXT.trim()).unwrap();
        let need: Vec<(u8, u32, u32)> = observation_core::terrain::dem_tiles_for(&bbox)
            .iter()
            .map(|t| (t.z, t.x, t.y))
            .collect();
        let have: Vec<(u8, u32, u32)> = DEM_TILES.iter().map(|(id, _)| *id).collect();
        assert_eq!(need, have);
    }

    #[test]
    fn demo_street_is_on_a_slope() {
        let b = demo();
        let t = &b.terrain;
        assert_eq!(t.attribution, "Elevation: test");
        // Juanita lies some tens of metres above Lake Washington, on ground
        // that rises a few metres or more across the street.
        assert!((5.0..150.0).contains(&t.datum_m), "{}", t.datum_m);
        let (lo, hi) = t
            .heights_dm
            .iter()
            .fold((i16::MAX, i16::MIN), |(lo, hi), &h| (lo.min(h), hi.max(h)));
        assert!(hi - lo > 30, "relief {lo}..{hi} dm");
        // Every building stands at or below the ground around it.
        for bl in &b.buildings {
            let fp = &bl.footprint;
            for p in &fp.outer {
                let g = t.height_at(observation_core::glam::Vec2::from_array(*p));
                assert!(fp.base_m <= g + 1e-3, "{} above {g}", fp.base_m);
            }
        }
    }

    #[test]
    fn demo_street_fits_ten_example_cameras() {
        // The client seeds the demo street with these; see `example_cameras`.
        let b = demo();
        let catalog = observation_core::camera::CameraModel {
            id: "m".into(),
            vendor: String::new(),
            name: "m".into(),
            kind: observation_core::camera::CameraKind::Bullet,
            hfov_deg: 100.0,
            vfov_deg: 60.0,
            resolution: [1920, 1080],
            ir_range_m: None,
            source: None,
            notes: None,
        };
        let cams = observation_core::example::example_cameras(&b, &[&catalog], 10);
        assert_eq!(cams.len(), 10);
        // Most sit on a house with an address, so the list groups them.
        let addressed: Vec<String> = cams
            .iter()
            .filter_map(|c| b.address_at([c.position[0], c.position[2]]))
            .map(|a| a.label())
            .collect();
        assert!(addressed.len() >= 5, "{addressed:?}");
    }
}
