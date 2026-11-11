//! Projection, bounding boxes and tiles.

use crate::config;
use crate::strategies::{bbox, lat, lon};
use observation_core::geo::{BBox, LatLon, Projection};
use observation_core::tiles::{
    MAX_ZOOM, TileId, lat_to_y, lon_to_x, tiles_covering, x_to_lon, y_to_lat,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn prop_projection_round_trips(
        origin_lat in lat(),
        origin_lon in lon(),
        x in -300.0..=300.0f64,
        z in -300.0..=300.0f64,
    ) {
        let proj = Projection::new(LatLon::new(origin_lat, origin_lon));
        let back = proj.to_world(proj.to_latlon([x, z]));
        prop_assert!((back[0] - x).abs() < 1e-6 && (back[1] - z).abs() < 1e-6, "{back:?}");
        // +X is east and -Z north.
        let p = proj.to_latlon([x, z]);
        prop_assert_eq!(x > 0.0, p.lon > origin_lon);
        prop_assert_eq!(z < 0.0, p.lat > origin_lat);
    }

    #[test]
    fn prop_snapping_only_grows_the_box(b in bbox()) {
        let s = b.snapped(0.0001);
        // Edges within float noise of a grid line may land on it from
        // either side (`snapped` allows 1e-6 of a step).
        let noise = 1e-10;
        prop_assert!(s.south <= b.south + noise && s.west <= b.west + noise);
        prop_assert!(s.north >= b.north - noise && s.east >= b.east - noise);
        prop_assert_eq!(s.snapped(0.0001).cache_key(), s.cache_key());
        let key = s.cache_key();
        prop_assert!(
            key.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '_' | '-')),
            "{key}"
        );
        prop_assert!(!key.contains(".."));
    }

    #[test]
    fn prop_overpass_filter_parses_back(b in bbox()) {
        let parsed = BBox::parse(&b.overpass()).expect("the filter parses");
        for (a, p) in [
            (b.south, parsed.south),
            (b.west, parsed.west),
            (b.north, parsed.north),
            (b.east, parsed.east),
        ] {
            prop_assert!((a - p).abs() <= 5.1e-8, "{a} {p}");
        }
        // Formatting what was parsed gives the same text again.
        prop_assert_eq!(parsed.overpass(), b.overpass());
    }

    #[test]
    fn prop_tile_maths_inverts(lat in lat(), lon in lon(), z in 0..=MAX_ZOOM) {
        let back_lon = x_to_lon(lon_to_x(lon, z), z);
        let back_lat = y_to_lat(lat_to_y(lat, z), z);
        prop_assert!((back_lon - lon).abs() < 1e-9, "{back_lon}");
        prop_assert!((back_lat - lat).abs() < 1e-9, "{back_lat}");
        let x = lon_to_x(lon, z).floor().min(f64::from((1u32 << z) - 1)) as u32;
        let y = lat_to_y(lat, z).floor() as u32;
        let tile = TileId::new(z, x, y).expect("in the grid");
        let bounds = tile.bounds();
        prop_assert!(bounds.contains(LatLon::new(lat, lon)), "{tile:?} {bounds:?}");
    }

    #[test]
    fn prop_tiles_cover_the_box(b in bbox(), z in 10..=MAX_ZOOM) {
        let tiles = tiles_covering(&b, z);
        prop_assert!(!tiles.is_empty());
        let covered = |p: LatLon| tiles.iter().any(|t| t.bounds().contains(p));
        let c = b.center();
        for p in [
            LatLon::new(b.south, b.west),
            LatLon::new(b.south, b.east),
            LatLon::new(b.north, b.west),
            LatLon::new(b.north, b.east),
            c,
        ] {
            prop_assert!(covered(p), "{p:?} at zoom {z}");
        }
        // And every tile listed touches the box.
        for t in &tiles {
            let tb = t.bounds();
            prop_assert!(
                tb.south <= b.north && tb.north >= b.south && tb.west <= b.east && tb.east >= b.west,
                "{t:?}"
            );
        }
    }
}
