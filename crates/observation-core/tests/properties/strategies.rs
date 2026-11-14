//! Strategies shared by the suites.

use glam::Vec2;
use observation_core::geo::{BBox, LatLon};
use observation_core::terrain::Terrain;
use proptest::prelude::*;

/// A latitude the API accepts.
pub fn lat() -> impl Strategy<Value = f64> {
    -85.0..=85.0f64
}

pub fn lon() -> impl Strategy<Value = f64> {
    -180.0..=180.0f64
}

/// A box of 20 to 500 m a side (at its centre latitude) inside the accepted
/// coordinate range.
pub fn bbox() -> impl Strategy<Value = BBox> {
    (
        -84.9..=84.9f64,
        -179.9..=179.9f64,
        20.0..=500.0f64,
        20.0..=500.0f64,
    )
        .prop_map(|(lat, lon, w, h)| {
            let tall = BBox::around(LatLon::new(lat, lon), h);
            let wide = BBox::around(LatLon::new(lat, lon), w);
            BBox {
                south: tall.south,
                west: wide.west,
                north: tall.north,
                east: wide.east,
            }
        })
}

/// A valid grid of up to 12 × 12 samples, anywhere near the origin.
pub fn terrain() -> impl Strategy<Value = Terrain> {
    (
        1..=12u32,
        1..=12u32,
        1.0..=50.0f32,
        -100.0..=0.0f32,
        -100.0..=0.0f32,
    )
        .prop_flat_map(|(cols, rows, cell_m, x, z)| {
            prop::collection::vec(-3000..=3000i16, (cols * rows) as usize).prop_map(
                move |heights_dm| Terrain {
                    min: [x, z],
                    cell_m,
                    cols,
                    rows,
                    heights_dm,
                    datum_m: 50.0,
                    attribution: String::new(),
                },
            )
        })
}

pub fn point() -> impl Strategy<Value = Vec2> {
    (-300.0..=300.0f32, -300.0..=300.0f32).prop_map(|(x, z)| Vec2::new(x, z))
}
