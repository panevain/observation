//! The demo bundle, built once at startup from the committed fixture.

use axum::body::Bytes;
use observation_core::bundle::build_bundle;
use observation_core::geo::BBox;
use thiserror::Error;

use crate::overpass::{ParseError, parse_response};

const OVERPASS_JSON: &[u8] = include_bytes!("../fixtures/demo-overpass.json");
const BBOX_TXT: &str = include_str!("../fixtures/demo-bbox.txt");

/// The embedded fixture could not be turned into a bundle.
#[derive(Debug, Error)]
pub enum DemoError {
    #[error("demo bbox: {0}")]
    BBox(#[from] observation_core::geo::BBoxError),
    #[error("demo overpass data: {0}")]
    Parse(#[from] ParseError),
}

/// Builds the encoded demo bundle.
pub fn build_demo(attribution: &str, generated_unix: u64) -> Result<Bytes, DemoError> {
    let bbox = BBox::parse(BBOX_TXT.trim())?;
    bbox.validate_size()?;
    let data = parse_response(OVERPASS_JSON)?;
    let bundle = build_bundle(
        bbox,
        &data.buildings,
        &data.roads,
        attribution,
        generated_unix,
    );
    Ok(Bytes::from(bundle.encode()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::bundle::AreaBundle;

    #[test]
    fn demo_bundle_decodes() {
        let bytes = build_demo("© OpenStreetMap contributors", 1).unwrap();
        let b = AreaBundle::decode(&bytes).unwrap();
        assert!(b.buildings.len() > 80, "{} buildings", b.buildings.len());
        assert!(!b.roads.is_empty());
        assert!(!b.mesh.is_empty());
    }
}
