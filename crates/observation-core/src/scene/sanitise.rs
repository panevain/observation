//! Reading a scene file: the single validation point for untrusted files.

use super::{MAX_CAMERAS, MAX_CUSTOM_MODELS, SCENE_FORMAT, SCENE_VERSION, SceneError, SceneFile};
use crate::camera::{
    CUSTOM_ID_PREFIX, MAX_CUSTOM_PPM, MAX_GROUP_CHARS, MIN_CUSTOM_PPM, fresh_custom_id,
    is_https_url,
};
use crate::coverage::MAX_REGION_POINTS;
use std::collections::{HashMap, HashSet};

impl SceneFile {
    /// Reads a scene file, refusing one the app cannot use and tidying what
    /// can be kept. The steps run in this order, and the first refusal wins.
    pub fn from_json(s: &str) -> Result<Self, SceneError> {
        let mut scene: Self = serde_json::from_str(s)?;
        scene.check_header()?;
        scene.check_counts()?;
        scene.tidy_groups();
        scene.check_camera_poses()?;
        scene.drop_unusable_region();
        if let Some(p) = &mut scene.provenance {
            p.sanitise();
        }
        scene.check_params()?;
        scene.check_custom_models()?;
        scene.make_custom_ids_unique();
        Ok(scene)
    }

    /// The format name and a version this build reads.
    fn check_header(&self) -> Result<(), SceneError> {
        if self.format != SCENE_FORMAT {
            // Cut short: the value comes from the file and may be huge.
            let found: String = self.format.chars().take(40).collect();
            return Err(SceneError::Format(found));
        }
        if self.version > SCENE_VERSION {
            return Err(SceneError::TooNew(self.version));
        }
        Ok(())
    }

    /// At most [`MAX_CAMERAS`] cameras and [`MAX_CUSTOM_MODELS`] custom models.
    fn check_counts(&self) -> Result<(), SceneError> {
        if self.cameras.len() > MAX_CAMERAS {
            return Err(SceneError::Invalid(format!(
                "{} cameras, but at most {MAX_CAMERAS} are supported",
                self.cameras.len()
            )));
        }
        if self.custom_models.len() > MAX_CUSTOM_MODELS {
            return Err(SceneError::Invalid(format!(
                "{} custom models, but at most {MAX_CUSTOM_MODELS} are supported",
                self.custom_models.len()
            )));
        }
        Ok(())
    }

    /// A blank group means "by address"; an overlong one is cut short.
    fn tidy_groups(&mut self) {
        for c in &mut self.cameras {
            c.group = c
                .group
                .as_deref()
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(|g| g.chars().take(MAX_GROUP_CHARS).collect());
        }
    }

    /// Every camera position and angle is a number within 10 000 (m or °).
    fn check_camera_poses(&self) -> Result<(), SceneError> {
        for (i, c) in self.cameras.iter().enumerate() {
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
        Ok(())
    }

    /// Drops an unusable region rather than the whole scene: losing the
    /// outline is a small price, losing every camera with it is not.
    fn drop_unusable_region(&mut self) {
        if self.region.as_ref().is_some_and(|r| {
            r.points.len() > MAX_REGION_POINTS || !r.points.iter().all(|p| finite(p))
        }) {
            self.region = None;
        }
    }

    /// The subject height and the custom pixel density are in range.
    fn check_params(&self) -> Result<(), SceneError> {
        let height = self.params.subject_height_m;
        if !(0.0..=10.0).contains(&height) {
            return Err(SceneError::Invalid(format!(
                "subject height is {height} m, but must be 0 to 10 m"
            )));
        }
        let ppm = self.params.custom_ppm;
        if !(MIN_CUSTOM_PPM..=MAX_CUSTOM_PPM).contains(&ppm) {
            return Err(SceneError::Invalid(format!(
                "custom pixel density is {ppm} px/m, but must be {MIN_CUSTOM_PPM} to {MAX_CUSTOM_PPM} px/m"
            )));
        }
        Ok(())
    }

    /// Drops each custom model's spec-sheet link unless it is https, then
    /// refuses the scene if a model is invalid. Dropping the link rather than
    /// the whole scene neutralises a planted `javascript:` link, and a scene
    /// autosaved before links were checked still restores (losing only the
    /// link).
    fn check_custom_models(&mut self) -> Result<(), SceneError> {
        for m in &mut self.custom_models {
            if m.source.as_deref().is_some_and(|s| !is_https_url(s)) {
                m.source = None;
            }
            m.validate()?;
        }
        Ok(())
    }

    /// Gives every custom model a reachable `custom-` id.
    ///
    /// The app looks models up by id, custom models first: a second custom
    /// model with an id cannot be reached (the editor would pick, save and
    /// delete the first), and one with a catalogue id replaces that model.
    /// The app only makes unique `custom-` ids, so this is a hand-edited
    /// file; give each offender a fresh id rather than refusing the scene
    /// (as with regions and links, an older autosave still restores).
    fn make_custom_ids_unique(&mut self) {
        let mut taken: HashSet<String> = self
            .custom_models
            .iter()
            .map(|m| m.id.clone())
            .chain(self.cameras.iter().map(|c| c.model_id.clone()))
            .collect();
        let mut seen = HashSet::new();
        let mut renamed = HashMap::new();
        for (i, m) in self.custom_models.iter_mut().enumerate() {
            let first = seen.insert(m.id.clone());
            if first && m.id.starts_with(CUSTOM_ID_PREFIX) {
                continue;
            }
            let id = fresh_custom_id(&m.name, i + 1, |id| taken.contains(id));
            taken.insert(id.clone());
            let old = std::mem::replace(&mut m.id, id);
            // Cameras showed the first model with an id, so they follow it;
            // a later duplicate starts out unused.
            if first {
                renamed.insert(old, m.id.clone());
            }
        }
        for c in &mut self.cameras {
            if let Some(id) = renamed.get(&c.model_id) {
                c.model_id.clone_from(id);
            }
        }
    }
}

/// Whether every value is a number within 10 000 of zero.
fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() < 10_000.0)
}

#[cfg(test)]
mod tests {
    use super::super::tests::scene;
    use super::*;
    use crate::camera::{CameraKind, CameraModel, DoriLevel};
    use crate::coverage::{CoverageParams, Region};
    use crate::provenance::Provenance;

    fn invalid(r: Result<(), SceneError>) -> String {
        match r {
            Err(SceneError::Invalid(why)) => why,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn step_check_header() {
        let mut s = scene();
        assert!(s.check_header().is_ok());
        s.format = "x".repeat(100);
        assert!(matches!(s.check_header(), Err(SceneError::Format(f)) if f == "x".repeat(40)));
        let mut s = scene();
        s.version = SCENE_VERSION;
        assert!(s.check_header().is_ok());
        s.version = SCENE_VERSION + 1;
        assert!(matches!(s.check_header(), Err(SceneError::TooNew(v)) if v == SCENE_VERSION + 1));
    }

    #[test]
    fn step_check_counts() {
        let mut s = scene();
        s.cameras = vec![s.cameras[0].clone(); MAX_CAMERAS];
        s.custom_models = vec![custom_model("custom-a-1"); MAX_CUSTOM_MODELS];
        assert!(s.check_counts().is_ok());
        s.cameras.push(s.cameras[0].clone());
        assert_eq!(
            invalid(s.check_counts()),
            "257 cameras, but at most 256 are supported"
        );
        s.cameras.pop();
        s.custom_models.push(custom_model("custom-a-1"));
        assert_eq!(
            invalid(s.check_counts()),
            "65 custom models, but at most 64 are supported"
        );
    }

    #[test]
    fn step_tidy_groups() {
        let mut s = scene();
        let mut c = s.cameras[0].clone();
        for g in [None, Some("  a b  "), Some(" \t "), Some(&*"é".repeat(90))] {
            c.group = g.map(str::to_owned);
            s.cameras.push(c.clone());
        }
        s.tidy_groups();
        let groups: Vec<Option<&str>> = s.cameras.iter().map(|c| c.group.as_deref()).collect();
        let long = "é".repeat(MAX_GROUP_CHARS);
        assert_eq!(groups, [None, None, Some("a b"), None, Some(long.as_str())]);
    }

    #[test]
    fn step_check_camera_poses() {
        let mut s = scene();
        s.cameras.push(s.cameras[0].clone());
        s.cameras[1].position = [9_999.0, -9_999.0, 0.0];
        assert!(s.check_camera_poses().is_ok());
        s.cameras[1].roll_deg = f32::NAN;
        assert_eq!(
            invalid(s.check_camera_poses()),
            "camera 2 has an angle that is not a number or lies beyond 10 000°"
        );
        s.cameras[1].position[2] = -10_000.0;
        assert_eq!(
            invalid(s.check_camera_poses()),
            "camera 2 has a position that is not a number or lies beyond 10 000 m"
        );
    }

    #[test]
    fn step_drop_unusable_region() {
        let region = |points: Vec<[f32; 2]>| {
            Some(Region {
                points,
                enabled: true,
            })
        };
        let mut s = scene();
        for (points, kept) in [
            (vec![[0.0, 0.0]; MAX_REGION_POINTS], true),
            (vec![[0.0, 0.0]; MAX_REGION_POINTS + 1], false),
            (vec![[9_999.0, -9_999.0]], true),
            (vec![[0.0, 10_000.0]], false),
            (vec![[f32::INFINITY, 0.0]], false),
        ] {
            s.region = region(points.clone());
            s.drop_unusable_region();
            assert_eq!(s.region.is_some(), kept, "{}", points.len());
        }
        s.region = None;
        s.drop_unusable_region();
        assert_eq!(s.region, None);
    }

    #[test]
    fn step_check_params() {
        let mut s = scene();
        for h in [0.0, 10.0] {
            s.params.subject_height_m = h;
            assert!(s.check_params().is_ok(), "{h}");
        }
        s.params.subject_height_m = 10.5;
        assert_eq!(
            invalid(s.check_params()),
            "subject height is 10.5 m, but must be 0 to 10 m"
        );
        s.params.subject_height_m = 1.0;
        for ppm in [MIN_CUSTOM_PPM, MAX_CUSTOM_PPM] {
            s.params.custom_ppm = ppm;
            assert!(s.check_params().is_ok(), "{ppm}");
        }
        s.params.custom_ppm = f32::NAN;
        assert_eq!(
            invalid(s.check_params()),
            format!(
                "custom pixel density is NaN px/m, but must be {MIN_CUSTOM_PPM} to {MAX_CUSTOM_PPM} px/m"
            )
        );
    }

    #[test]
    fn step_check_custom_models() {
        let mut s = scene();
        let mut sources = Vec::new();
        for link in [
            None,
            Some("https://example.com/a"),
            Some("http://example.com/a"),
            Some("javascript:x"),
        ] {
            let mut m = custom_model("custom-a-1");
            m.source = link.map(str::to_owned);
            sources.push(m);
        }
        s.custom_models = sources;
        assert!(s.check_custom_models().is_ok());
        let kept: Vec<Option<&str>> = s
            .custom_models
            .iter()
            .map(|m| m.source.as_deref())
            .collect();
        assert_eq!(kept, [None, Some("https://example.com/a"), None, None]);
        s.custom_models[2].hfov_deg = 0.0;
        assert!(matches!(s.check_custom_models(), Err(SceneError::Model(_))));
    }

    #[test]
    fn step_make_custom_ids_unique() {
        let mut s = scene();
        s.custom_models = vec![custom_model("custom-a-1"), custom_model("custom-a-1")];
        s.cameras[0].model_id = "custom-a-1".into();
        s.make_custom_ids_unique();
        let ids: Vec<&str> = s.custom_models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["custom-a-1", "custom-mine-2"]);
        assert_eq!(s.cameras[0].model_id, "custom-a-1");
        let before = s.clone();
        s.make_custom_ids_unique();
        assert_eq!(s, before, "a second pass changes nothing");
    }

    #[test]
    fn the_first_refusal_wins() {
        // Each later step's problem is present too; the earlier one is reported.
        let mut s = scene();
        s.params.subject_height_m = 99.0;
        s.custom_models = vec![CameraModel {
            hfov_deg: 0.0,
            ..custom_model("custom-a-1")
        }];
        let params = SceneFile::from_json(&s.to_json()).unwrap_err().to_string();
        assert!(params.contains("subject height"), "{params}");
        s.cameras[0].yaw_deg = 1e6;
        let pose = SceneFile::from_json(&s.to_json()).unwrap_err().to_string();
        assert!(pose.contains("camera 1 has an angle"), "{pose}");
        s.cameras = vec![s.cameras[0].clone(); MAX_CAMERAS + 1];
        let count = SceneFile::from_json(&s.to_json()).unwrap_err().to_string();
        assert!(count.contains("257 cameras"), "{count}");
        s.version = SCENE_VERSION + 1;
        assert!(matches!(
            SceneFile::from_json(&s.to_json()),
            Err(SceneError::TooNew(_))
        ));
        // Inside a step, too: two limits broken in one step, the first
        // check reports.
        let mut s = scene();
        s.format = "other".into();
        s.version = SCENE_VERSION + 1;
        assert!(matches!(
            SceneFile::from_json(&s.to_json()),
            Err(SceneError::Format(_))
        ));
        let mut s = scene();
        s.cameras = vec![s.cameras[0].clone(); MAX_CAMERAS + 1];
        s.custom_models = vec![custom_model("custom-a-1"); MAX_CUSTOM_MODELS + 1];
        let both = SceneFile::from_json(&s.to_json()).unwrap_err().to_string();
        assert!(both.contains("257 cameras"), "{both}");
        let mut s = scene();
        s.params.subject_height_m = 99.0;
        s.params.custom_ppm = 0.0;
        let both = SceneFile::from_json(&s.to_json()).unwrap_err().to_string();
        assert!(both.contains("subject height"), "{both}");
        // Without the earlier problems, the custom model is refused last.
        let mut s = scene();
        s.custom_models = vec![CameraModel {
            hfov_deg: 0.0,
            ..custom_model("custom-a-1")
        }];
        assert!(matches!(
            SceneFile::from_json(&s.to_json()),
            Err(SceneError::Model(_))
        ));
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

    fn custom_model(id: &str) -> CameraModel {
        CameraModel {
            id: id.into(),
            vendor: String::new(),
            name: "Mine".into(),
            kind: CameraKind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 50.0,
            resolution: [1920, 1080],
            ir_range_m: None,
            source: None,
            notes: None,
        }
    }

    #[test]
    fn custom_model_ids_are_made_unique_and_prefixed() {
        let mut s = scene();
        s.custom_models = vec![custom_model("custom-a-1"), custom_model("custom-a-2")];
        assert_eq!(SceneFile::from_json(&s.to_json()).unwrap(), s, "left alone");

        // A duplicate is renamed; cameras keep the first model, which they showed.
        s.custom_models[1].id = "custom-a-1".into();
        s.cameras[0].model_id = "custom-a-1".into();
        let loaded = SceneFile::from_json(&s.to_json()).unwrap();
        let ids: Vec<&str> = loaded.custom_models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["custom-a-1", "custom-mine-2"]);
        assert_eq!(loaded.cameras[0].model_id, "custom-a-1");

        // An id without the prefix (here a catalogue id, which the custom model
        // replaced) is renamed, and the cameras that showed it follow.
        s.custom_models[1].id = "custom-mine-3".into();
        s.custom_models[0].id = "generic-doorbell-160".into();
        s.cameras[0].model_id = "generic-doorbell-160".into();
        let loaded = SceneFile::from_json(&s.to_json()).unwrap();
        let ids: Vec<&str> = loaded.custom_models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["custom-mine-1", "custom-mine-3"]);
        assert_eq!(loaded.cameras[0].model_id, "custom-mine-1");
        assert_eq!(SceneFile::from_json(&loaded.to_json()).unwrap(), loaded);

        // A fresh id skips every id in the file, used by a model or a camera,
        // and a duplicated unprefixed id: cameras follow the first copy only.
        s.custom_models = vec![
            custom_model("acme"),
            custom_model("acme"),
            custom_model("custom-mine-2"),
        ];
        s.cameras[0].model_id = "custom-mine-1".into();
        let mut second = s.cameras[0].clone();
        second.id = 2;
        second.model_id = "acme".into();
        s.cameras.push(second);
        let loaded = SceneFile::from_json(&s.to_json()).unwrap();
        let ids: Vec<&str> = loaded.custom_models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["custom-mine-3", "custom-mine-4", "custom-mine-2"]);
        let used: Vec<&str> = loaded.cameras.iter().map(|c| c.model_id.as_str()).collect();
        assert_eq!(used, ["custom-mine-1", "custom-mine-3"]);
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
    fn provenance_is_optional_and_lenient() {
        let mut s = scene();
        assert!(!s.to_json().contains("provenance"), "omitted when absent");
        s.provenance = Some(Provenance {
            osm_data_unix: Some(1_793_475_782),
            bundle_built_unix: Some(1_793_500_000),
            osm_fingerprint: Some("0123456789abcdef".into()),
            app_version: "0.1.0".into(),
            catalog_version: "fedcba9876543210".into(),
            caveats_version: 1,
        });
        let json = s.to_json();
        assert!(
            json.contains("\"osm_data_as_of\": \"2026-10-31T19:43:02Z\""),
            "{json}"
        );
        assert_eq!(SceneFile::from_json(&json).unwrap(), s);
        // A malformed block is dropped; the cameras survive.
        for bad in ["5", "\"yesterday\"", "true", "{\"app_version\": 3}"] {
            let json = scene().to_json().replacen(
                "\"cameras\"",
                &format!("\"provenance\": {bad},\n  \"cameras\""),
                1,
            );
            let loaded = SceneFile::from_json(&json).unwrap();
            assert_eq!(
                (loaded.provenance, loaded.cameras.len()),
                (None, 1),
                "{bad}"
            );
        }
        // `null` and an empty block load too.
        for ok in ["null", "{}"] {
            let json = scene().to_json().replacen(
                "\"cameras\"",
                &format!("\"provenance\": {ok},\n  \"cameras\""),
                1,
            );
            assert!(SceneFile::from_json(&json).is_ok(), "{ok}");
        }
        // Values from the file are tidied.
        let long = json.replace("\"0.1.0\"", &format!("\"{}\"", "9".repeat(500)));
        let loaded = SceneFile::from_json(&long).unwrap();
        assert_eq!(loaded.provenance.unwrap().app_version.len(), 64);
    }

    #[test]
    fn groups_are_tidied() {
        let mut s = scene();
        assert!(!s.to_json().contains("group"), "omitted when automatic");
        s.cameras[0].group = Some("  Garage  ".into());
        let loaded = SceneFile::from_json(&s.to_json()).unwrap();
        assert_eq!(loaded.cameras[0].group.as_deref(), Some("Garage"));
        s.cameras[0].group = Some("   ".into());
        assert_eq!(
            SceneFile::from_json(&s.to_json()).unwrap().cameras[0].group,
            None
        );
        s.cameras[0].group = Some("x".repeat(1000));
        let loaded = SceneFile::from_json(&s.to_json()).unwrap();
        assert_eq!(
            loaded.cameras[0].group.as_ref().map(String::len),
            Some(MAX_GROUP_CHARS)
        );
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
        assert!(matches!(
            SceneFile::from_json(&s.to_json()),
            Err(SceneError::Invalid(_))
        ));
    }

    #[test]
    fn params_without_custom_ppm_load_with_the_default() {
        // Scenes saved before the Custom level have `params` but no `custom_ppm`.
        let json = r#"{"format":"observation-scene","version":1,"area":{"kind":"demo"},
            "origin":{"lat":0.0,"lon":0.0},
            "params":{"dori":"Recognise","lighting":"Night","subject_height_m":1.6}}"#;
        let s = SceneFile::from_json(json).unwrap();
        assert_eq!(s.params.dori, DoriLevel::Recognise);
        assert_eq!(s.params.custom_ppm, CoverageParams::default().custom_ppm);
    }
}
