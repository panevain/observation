//! The scene file's shape, and its version history.

use crate::camera::{CameraModel, CameraPlacement, Catalog};
use crate::coverage::{CoverageParams, Region};
use crate::footage::{FootageCamera, LabelledArea, Target, area_by_id, set_figures_area};
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

/// A saved scene. Its JSON also carries, for older apps, a copy of the area
/// the figures are limited to as the single `region` they read; a file from
/// such an app has that `region` and no `areas`, and it loads as one
/// unlabelled area that limits the figures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "SceneWire", into = "SceneWire")]
pub struct SceneFile {
    pub format: String,
    pub version: u32,
    pub area: AreaRef,
    /// World-frame origin the camera positions are relative to.
    pub origin: LatLon,
    pub cameras: Vec<CameraPlacement>,
    pub custom_models: Vec<CameraModel>,
    pub params: CoverageParams,
    /// Areas the user drew, labelled or not, in drawing order.
    pub areas: Vec<LabelledArea>,
    /// The id of the area the coverage figures are limited to (while that
    /// area is enabled and its outline usable), if any. A scene's old
    /// single `region` loads as an unlabelled area named here.
    pub limit_figures_to: Option<u32>,
    /// Named places to get verdicts on.
    pub targets: Vec<Target>,
    /// Where the figures came from: the OSM data date and fingerprint, the
    /// bundle build time, and the app, catalogue and caveats versions.
    /// Optional and additive: files without it load, and a malformed block
    /// is dropped rather than failing the file.
    pub provenance: Option<Provenance>,
}

/// The scene file's JSON, field for field. Scenes from before labelled areas
/// have a single `region` instead of `areas` and `limit_figures_to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SceneWire {
    format: String,
    version: u32,
    area: AreaRef,
    origin: LatLon,
    #[serde(default)]
    cameras: Vec<CameraPlacement>,
    #[serde(default)]
    custom_models: Vec<CameraModel>,
    #[serde(default)]
    params: CoverageParams,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    areas: Vec<LabelledArea>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_figures_to: Option<u32>,
    /// The area the figures are limited to, as apps before labelled areas
    /// read it. Written as a copy of that area so that an older app still
    /// limits its figures the same way (it ignores the fields it does not
    /// know); read only from a file without `areas`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    region: Option<Region>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    targets: Vec<Target>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_provenance"
    )]
    provenance: Option<Provenance>,
}

/// The id an old file's single `region` gets as an area.
const MIGRATED_REGION_ID: u32 = 1;

impl From<SceneWire> for SceneFile {
    fn from(w: SceneWire) -> Self {
        let (areas, limit_figures_to) = match w.region {
            // Written by an app before labelled areas (or by hand): the
            // region becomes the area the figures are limited to.
            Some(r) if w.areas.is_empty() => (
                vec![LabelledArea::from_region(MIGRATED_REGION_ID, r)],
                Some(MIGRATED_REGION_ID),
            ),
            // Otherwise `region` is only the copy written for older apps.
            _ => (w.areas, w.limit_figures_to),
        };
        Self {
            format: w.format,
            version: w.version,
            area: w.area,
            origin: w.origin,
            cameras: w.cameras,
            custom_models: w.custom_models,
            params: w.params,
            areas,
            limit_figures_to,
            targets: w.targets,
            provenance: w.provenance,
        }
    }
}

impl From<SceneFile> for SceneWire {
    fn from(s: SceneFile) -> Self {
        let region = s.figures_area().map(LabelledArea::to_region);
        Self {
            format: s.format,
            version: s.version,
            area: s.area,
            origin: s.origin,
            cameras: s.cameras,
            custom_models: s.custom_models,
            params: s.params,
            areas: s.areas,
            limit_figures_to: s.limit_figures_to,
            region,
            targets: s.targets,
            provenance: s.provenance,
        }
    }
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
pub use crate::footage::{MAX_AREAS, MAX_TARGETS};

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
            areas: Vec::new(),
            limit_figures_to: None,
            targets: Vec::new(),
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

    /// The area the coverage figures are limited to, if one is named (it
    /// limits them only while [`LabelledArea::is_active`]).
    pub fn figures_area(&self) -> Option<&LabelledArea> {
        area_by_id(&self.areas, self.limit_figures_to)
    }

    /// Replaces the outline and switch of the area the figures are limited
    /// to, adds one, or with `None` removes it: see [`set_figures_area`].
    pub fn set_figures_area(&mut self, region: Option<Region>) {
        set_figures_area(&mut self.areas, &mut self.limit_figures_to, region);
    }

    /// The enabled cameras whose model is known, with their models (the
    /// scene's custom models first, then `catalog`), for the footage figures.
    pub fn footage_cameras<'a>(&'a self, catalog: &'a Catalog) -> Vec<FootageCamera<'a>> {
        self.cameras
            .iter()
            .filter(|c| c.enabled)
            .filter_map(|c| {
                let model = self
                    .custom_models
                    .iter()
                    .find(|m| m.id == c.model_id)
                    .or_else(|| catalog.get(&c.model_id))?;
                Some(FootageCamera {
                    placement: c,
                    model,
                })
            })
            .collect()
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
