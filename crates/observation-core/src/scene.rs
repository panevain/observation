//! Saved scenes: the user's area, cameras, custom models and settings, as JSON.

use crate::camera::{CameraModel, CameraPlacement, is_https_url};
use crate::coverage::{CoverageParams, MAX_REGION_POINTS, Region};
use crate::geo::{BBox, LatLon};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCENE_FORMAT: &str = "observation-scene";
pub const SCENE_VERSION: u32 = 1;

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
}

#[derive(Debug, Error)]
pub enum SceneError {
    #[error("not a valid scene file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not an Observation scene file")]
    Format,
    #[error("scene version {0} is newer than this app supports ({SCENE_VERSION})")]
    TooNew(u32),
    #[error("invalid custom model: {0}")]
    Model(#[from] crate::camera::CatalogError),
    #[error("scene has invalid values: {0}")]
    Invalid(&'static str),
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
        }
    }

    pub fn to_json(&self) -> String {
        // Plain data with no maps keyed by non-strings: cannot fail.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Result<Self, SceneError> {
        let mut scene: Self = serde_json::from_str(s)?;
        if scene.format != SCENE_FORMAT {
            return Err(SceneError::Format);
        }
        if scene.version > SCENE_VERSION {
            return Err(SceneError::TooNew(scene.version));
        }
        if scene.cameras.len() > MAX_CAMERAS || scene.custom_models.len() > MAX_CUSTOM_MODELS {
            return Err(SceneError::Invalid("too many cameras or models"));
        }
        let finite = |v: &[f32]| v.iter().all(|x| x.is_finite() && x.abs() < 10_000.0);
        for c in &scene.cameras {
            if !finite(&c.position) || !finite(&[c.yaw_deg, c.pitch_deg, c.roll_deg]) {
                return Err(SceneError::Invalid("camera position/orientation"));
            }
        }
        if let Some(r) = &scene.region
            && (r.points.len() > MAX_REGION_POINTS || !r.points.iter().all(|p| finite(p)))
        {
            return Err(SceneError::Invalid("region"));
        }
        if !(0.0..=10.0).contains(&scene.params.subject_height_m) {
            return Err(SceneError::Invalid("subject height"));
        }
        for m in &mut scene.custom_models {
            // Drop a non-https spec-sheet link rather than the whole scene: a
            // planted `javascript:` link is neutralised, and a scene autosaved
            // before links were checked still restores (losing only the link).
            if m.source.as_deref().is_some_and(|s| !is_https_url(s)) {
                m.source = None;
            }
            m.validate()?;
        }
        Ok(scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> SceneFile {
        let mut s = SceneFile::new(AreaRef::Demo, LatLon::new(1.0, 2.0));
        s.cameras.push(CameraPlacement {
            id: 1,
            label: "Front door".into(),
            model_id: "generic-doorbell-160".into(),
            position: [1.0, 1.2, -3.0],
            yaw_deg: 180.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: Some([10, 20, 30]),
            enabled: true,
        });
        s
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

    #[test]
    fn rejects_bad_files() {
        assert!(matches!(
            SceneFile::from_json("{}"),
            Err(SceneError::Json(_))
        ));
        let mut s = scene();
        s.format = "other".into();
        assert!(matches!(
            SceneFile::from_json(&s.to_json()),
            Err(SceneError::Format)
        ));
        let mut s = scene();
        s.version = 99;
        assert!(matches!(
            SceneFile::from_json(&s.to_json()),
            Err(SceneError::TooNew(99))
        ));
        let json = scene().to_json().replace("1.2", "1e9");
        assert!(matches!(
            SceneFile::from_json(&json),
            Err(SceneError::Invalid(_))
        ));
    }

    #[test]
    fn drops_script_links_from_custom_models() {
        let json = r#"{"format":"observation-scene","version":1,"area":{"kind":"demo"},
            "origin":{"lat":0.0,"lon":0.0},
            "custom_models":[{"id":"evil","vendor":"","name":"x","kind":"Bullet",
              "hfov_deg":90.0,"vfov_deg":50.0,"resolution":[1920,1080],
              "source":"javascript:alert(document.domain)"}]}"#;
        let s = SceneFile::from_json(json).unwrap();
        assert_eq!(s.custom_models.len(), 1, "the model survives");
        assert_eq!(s.custom_models[0].source, None, "the link does not");
        let ok = json.replace("javascript:alert(document.domain)", "https://example.com");
        assert_eq!(
            SceneFile::from_json(&ok).unwrap().custom_models[0]
                .source
                .as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn legacy_color_index_is_ignored() {
        let json = scene().to_json().replace(
            "\"color\": [\n        10,\n        20,\n        30\n      ],",
            "\"color_index\": 3,",
        );
        assert!(json.contains("color_index"), "{json}");
        let s = SceneFile::from_json(&json).unwrap();
        assert_eq!(s.cameras[0].color, None);
    }

    #[test]
    fn region_round_trips_and_is_checked() {
        let mut s = scene();
        s.region = Some(Region {
            points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
            enabled: false,
        });
        assert_eq!(SceneFile::from_json(&s.to_json()).unwrap(), s);
        assert!(!scene().to_json().contains("region"), "omitted when absent");

        let mut bad = s.clone();
        bad.region = Some(Region {
            points: vec![[0.0, 0.0]; MAX_REGION_POINTS + 1],
            enabled: true,
        });
        assert!(matches!(
            SceneFile::from_json(&bad.to_json()),
            Err(SceneError::Invalid("region"))
        ));
        let far = s.to_json().replace("10.0", "1e9");
        assert!(SceneFile::from_json(&far).is_err());
    }

    #[test]
    fn missing_optional_fields_default() {
        let json = r#"{"format":"observation-scene","version":1,"area":{"kind":"demo"},"origin":{"lat":0.0,"lon":0.0}}"#;
        let s = SceneFile::from_json(json).unwrap();
        assert!(s.cameras.is_empty());
        assert_eq!(s.params, CoverageParams::default());
    }
}
