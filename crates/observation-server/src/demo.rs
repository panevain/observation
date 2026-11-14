//! The demo bundle, built once at startup from the committed fixtures (see
//! `observation_mapdata::demo`).

use axum::body::Bytes;

pub use observation_mapdata::demo::DemoError;

/// Builds the encoded demo bundle. Its terrain comes from the recorded tiles
/// whatever `OBS_DEM_URL` says, so the demo always works offline; it is
/// credited with `dem_attribution`.
pub fn build_demo(
    attribution: &str,
    dem_attribution: &str,
    generated_unix: u64,
) -> Result<Bytes, DemoError> {
    observation_mapdata::demo::demo_bundle(attribution, dem_attribution, generated_unix)
        .map(|b| Bytes::from(b.encode()))
}
