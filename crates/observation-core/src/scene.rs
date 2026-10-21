//! Saved scenes: the user's area, cameras, custom models and settings, as JSON.

use crate::camera::{CameraModel, CameraPlacement, MAX_CUSTOM_PPM, MIN_CUSTOM_PPM, is_https_url};
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
        }
    }

    pub fn to_json(&self) -> String {
        // Plain data with no maps keyed by non-strings: cannot fail.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Result<Self, SceneError> {
        let mut scene: Self = serde_json::from_str(s)?;
        if scene.format != SCENE_FORMAT {
            // Cut short: the value comes from the file and may be huge.
            let found: String = scene.format.chars().take(40).collect();
            return Err(SceneError::Format(found));
        }
        if scene.version > SCENE_VERSION {
            return Err(SceneError::TooNew(scene.version));
        }
        if scene.cameras.len() > MAX_CAMERAS {
            return Err(SceneError::Invalid(format!(
                "{} cameras, but at most {MAX_CAMERAS} are supported",
                scene.cameras.len()
            )));
        }
        if scene.custom_models.len() > MAX_CUSTOM_MODELS {
            return Err(SceneError::Invalid(format!(
                "{} custom models, but at most {MAX_CUSTOM_MODELS} are supported",
                scene.custom_models.len()
            )));
        }
        let finite = |v: &[f32]| v.iter().all(|x| x.is_finite() && x.abs() < 10_000.0);
        for (i, c) in scene.cameras.iter().enumerate() {
            if !finite(&c.position) {
                return Err(SceneError::Invalid(format!(
                    "camera {} has a position that is not a number or lies beyond 10 000 m",
                    i + 1
                )));
            }
            if !finite(&[c.yaw_deg, c.pitch_deg, c.roll_deg]) {
                return Err(SceneError::Invalid(format!(
                    "camera {} has an angle that is not a number or lies beyond 10 000°",
                    i + 1
                )));
            }
        }
        // Drop an unusable region rather than the whole scene: losing the
        // outline is a small price, losing every camera with it is not.
        if scene.region.as_ref().is_some_and(|r| {
            r.points.len() > MAX_REGION_POINTS || !r.points.iter().all(|p| finite(p))
        }) {
            scene.region = None;
        }
        let height = scene.params.subject_height_m;
        if !(0.0..=10.0).contains(&height) {
            return Err(SceneError::Invalid(format!(
                "subject height is {height} m, but must be 0 to 10 m"
            )));
        }
        let ppm = scene.params.custom_ppm;
        if !(MIN_CUSTOM_PPM..=MAX_CUSTOM_PPM).contains(&ppm) {
            return Err(SceneError::Invalid(format!(
                "custom pixel density is {ppm} px/m, but must be {MIN_CUSTOM_PPM} to {MAX_CUSTOM_PPM} px/m"
            )));
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
    use crate::camera::DoriLevel;

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
            Err(SceneError::Format(_))
        ));
        let mut s = scene();
        s.version = 99;
        assert!(matches!(
            SceneFile::from_json(&s.to_json()),
            Err(SceneError::TooNew(99))
        ));
        let json = scene().to_json().replace("1.2", "1e9");
        let err = SceneFile::from_json(&json).unwrap_err();
        assert!(matches!(err, SceneError::Invalid(_)), "{err}");
        assert!(err.to_string().contains("camera 1"), "{err}");
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

        // A bad region is dropped; the cameras survive.
        let mut bad = s.clone();
        bad.region = Some(Region {
            points: vec![[0.0, 0.0]; MAX_REGION_POINTS + 1],
            enabled: true,
        });
        let loaded = SceneFile::from_json(&bad.to_json()).unwrap();
        assert_eq!((loaded.region, loaded.cameras.len()), (None, 1));
        let far = s.to_json().replace("10.0", "1e9");
        let loaded = SceneFile::from_json(&far).unwrap();
        assert_eq!((loaded.region, loaded.cameras.len()), (None, 1));
    }

    #[test]
    fn missing_optional_fields_default() {
        let json = r#"{"format":"observation-scene","version":1,"area":{"kind":"demo"},"origin":{"lat":0.0,"lon":0.0}}"#;
        let s = SceneFile::from_json(json).unwrap();
        assert!(s.cameras.is_empty());
        assert_eq!(s.params, CoverageParams::default());
    }

    #[test]
    fn custom_ppm_round_trip_and_bounds() {
        let mut s = scene();
        s.params.dori = DoriLevel::Custom;
        s.params.custom_ppm = 400.0;
        let loaded = SceneFile::from_json(&s.to_json()).unwrap();
        assert_eq!(loaded.params.pixels_per_metre(), 400.0);
        s.params.custom_ppm = 0.0;
        assert!(SceneFile::from_json(&s.to_json()).is_err());
    }
}
