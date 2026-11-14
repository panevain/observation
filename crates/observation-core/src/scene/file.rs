//! The scene file's shape, and its version history.

use crate::camera::{CameraModel, CameraPlacement};
use crate::coverage::{CoverageParams, Region};
use crate::geo::{BBox, LatLon};
use crate::provenance::{Provenance, lenient_provenance};
use crate::terrain::Terrain;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCENE_FORMAT: &str = "observation-scene";
/// Version 2: camera positions are over the terrain, with `y = 0` at the
/// ground at the origin. Version 1 assumed flat ground at `y = 0`; such
/// cameras are raised onto the terrain with [`lift_onto_terrain`].
pub const SCENE_VERSION: u32 = 2;
/// The last version that assumed flat ground.
pub const FLAT_GROUND_VERSION: u32 = 1;

/// Which area a scene was built on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AreaRef {
    /// The bundled demo street.
    Demo,
    /// An OSM area requested by bounding box.
    Osm { bbox: BBox },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneFile {
    pub format: String,
    pub version: u32,
    pub area: AreaRef,
    /// World-frame origin the camera positions are relative to.
    pub origin: LatLon,
    #[serde(default)]
    pub cameras: Vec<CameraPlacement>,
    #[serde(default)]
    pub custom_models: Vec<CameraModel>,
    #[serde(default)]
    pub params: CoverageParams,
    /// Area the coverage figures are limited to, if one was drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Region>,
    /// Where the figures came from: the OSM data date and fingerprint, the
    /// bundle build time, and the app, catalogue and caveats versions.
    /// Optional and additive: files without it load, and a malformed block
    /// is dropped rather than failing the file.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_provenance"
    )]
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Error)]
pub enum SceneError {
    #[error("not a valid scene file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not an Observation scene file (its format is `{0}`, not `{SCENE_FORMAT}`)")]
    Format(String),
    #[error(
        "scene version {0} is newer than this app reads (up to {SCENE_VERSION}); reload the page to update the app"
    )]
    TooNew(u32),
    #[error("a custom model in the file is invalid: {0}")]
    Model(#[from] crate::camera::CatalogError),
    #[error("scene has invalid values: {0}")]
    Invalid(String),
}

/// Limits that keep a hostile or corrupt file from exhausting memory/CPU.
pub const MAX_CAMERAS: usize = 256;
pub const MAX_CUSTOM_MODELS: usize = 64;

impl SceneFile {
    pub fn new(area: AreaRef, origin: LatLon) -> Self {
        Self {
            format: SCENE_FORMAT.to_owned(),
            version: SCENE_VERSION,
            area,
            origin,
            cameras: Vec::new(),
            custom_models: Vec::new(),
            params: CoverageParams::default(),
            region: None,
            provenance: None,
        }
    }

    pub fn to_json(&self) -> String {
        // Plain data with no maps keyed by non-strings: cannot fail.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

impl SceneFile {
    /// Whether the camera heights were measured from flat ground at `y = 0`
    /// and so need [`lift_onto_terrain`] once the area's terrain is known.
    pub fn on_flat_ground(&self) -> bool {
        self.version <= FLAT_GROUND_VERSION
    }
}

/// Raises cameras placed on flat ground (scene version 1) by the ground
/// height under each, so they keep their height above the ground.
pub fn lift_onto_terrain(cameras: &mut [CameraPlacement], terrain: &Terrain) {
    for c in cameras {
        let [x, y, z] = c.position;
        c.position[1] = y + terrain.height_at(glam::Vec2::new(x, z));
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::scene;
    use super::*;

    #[test]
    fn flat_ground_scenes_are_lifted_onto_the_terrain() {
        let old = scene()
            .to_json()
            .replace("\"version\": 2", "\"version\": 1");
        let mut s = SceneFile::from_json(&old).unwrap();
        assert!(s.on_flat_ground());
        assert!(!scene().on_flat_ground());
        let terrain = crate::testing::slope();
        lift_onto_terrain(&mut s.cameras, &terrain);
        // 1.2 m above the ground at x = 1, which is 0.1 m up the slope.
        assert!((s.cameras[0].position[1] - 1.3).abs() < 0.01);
        lift_onto_terrain(&mut s.cameras, &Terrain::default());
        assert!((s.cameras[0].position[1] - 1.3).abs() < 0.01);
    }

    #[test]
    fn round_trip() {
        let s = scene();
        assert_eq!(SceneFile::from_json(&s.to_json()).unwrap(), s);
        let osm = SceneFile::new(
            AreaRef::Osm {
                bbox: BBox::around(LatLon::new(1.0, 2.0), 100.0),
            },
            LatLon::new(1.0, 2.0),
        );
        assert!(osm.to_json().contains("\"kind\": \"osm\""));
    }
}
