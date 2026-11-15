//! Reading a scene file: the single validation point for untrusted files.

use super::{
    MAX_AREAS, MAX_CAMERAS, MAX_CUSTOM_MODELS, MAX_ITEM_ID, MAX_TARGETS, SCENE_FORMAT,
    SCENE_VERSION, SceneError, SceneFile,
};
use crate::camera::{
    CUSTOM_ID_PREFIX, MAX_CUSTOM_PPM, MAX_GROUP_CHARS, MIN_CUSTOM_PPM, fresh_custom_id,
    is_https_url,
};
use crate::coverage::MAX_REGION_POINTS;
use crate::footage::{MAX_NAME_CHARS, TargetPlace, fresh_id};
use std::collections::{HashMap, HashSet};

impl SceneFile {
    /// Reads a scene file, refusing one the app cannot use and tidying what
    /// can be kept. The steps run in this order, and the first refusal wins.
    pub fn from_json(s: &str) -> Result<Self, SceneError> {
        let mut scene: Self = serde_json::from_str(s)?;
        scene.check_header()?;
        scene.check_counts()?;
        scene.tidy_camera_ids();
        scene.tidy_groups();
        scene.check_camera_poses()?;
        scene.tidy_areas();
        scene.tidy_targets();
        if let Some(p) = &mut scene.provenance {
            p.sanitise();
        }
        super::agent_log::sanitise(&mut scene.agent_log);
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

    /// At most [`MAX_CAMERAS`] cameras, [`MAX_CUSTOM_MODELS`] custom models,
    /// [`MAX_AREAS`] areas and [`MAX_TARGETS`] targets.
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
        if self.areas.len() > MAX_AREAS {
            return Err(SceneError::Invalid(format!(
                "{} areas, but at most {MAX_AREAS} are supported",
                self.areas.len()
            )));
        }
        if self.targets.len() > MAX_TARGETS {
            return Err(SceneError::Invalid(format!(
                "{} targets, but at most {MAX_TARGETS} are supported",
                self.targets.len()
            )));
        }
        Ok(())
    }

    /// A repeated camera id, or one above [`MAX_ITEM_ID`], is replaced by a
    /// fresh one; the first camera with an id in range keeps it. Nothing in
    /// the scene refers to a camera by id, so nothing else follows.
    fn tidy_camera_ids(&mut self) {
        make_ids_unique(self.cameras.iter_mut().map(|c| &mut c.id).collect());
    }

    /// A blank group means "by address"; an overlong one is cut short, and
    /// whitespace the cut leaves at its end is trimmed too, so the group
    /// loads the same the next time.
    fn tidy_groups(&mut self) {
        for c in &mut self.cameras {
            c.group = c
                .group
                .as_deref()
                .map(|g| tidy_text(g, MAX_GROUP_CHARS))
                .filter(|g| !g.is_empty());
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

    /// Drops each unusable area rather than the whole scene, as with the
    /// single region before: one with more than [`MAX_REGION_POINTS`] corners
    /// or a corner that is not a number within 10 000 m. Names are trimmed
    /// and cut to [`MAX_NAME_CHARS`] (blank means none), a repeated id or
    /// one above [`MAX_ITEM_ID`] is replaced by a fresh one (the first area
    /// with an id in range keeps it, and `limit_figures_to` follows an area
    /// whose id was out of range), and `limit_figures_to` is cleared if it
    /// names no area. An outline that
    /// encloses no area is kept, as the region's was: it just does not count.
    fn tidy_areas(&mut self) {
        self.areas
            .retain(|a| a.points.len() <= MAX_REGION_POINTS && a.points.iter().all(|p| finite(p)));
        for a in &mut self.areas {
            a.name = a
                .name
                .as_deref()
                .map(|n| tidy_text(n, MAX_NAME_CHARS))
                .filter(|n| !n.is_empty());
        }
        let moved = make_ids_unique(self.areas.iter_mut().map(|a| &mut a.id).collect());
        if let Some(id) = self.limit_figures_to
            && let Some(&new) = moved.get(&id)
        {
            self.limit_figures_to = Some(new);
        }
        if let Some(id) = self.limit_figures_to
            && !self.areas.iter().any(|a| a.id == id)
        {
            self.limit_figures_to = None;
        }
    }

    /// Drops each target that is unusable: a coordinate that is not a number
    /// within 10 000 m, or a [`TargetPlace::problem`] (too many corners, more
    /// than 30 m across, an outline that encloses no area, or a wall without
    /// a direction). Names are trimmed and cut to [`MAX_NAME_CHARS`], and a
    /// repeated id or one above [`MAX_ITEM_ID`] is replaced by a fresh one.
    fn tidy_targets(&mut self) {
        self.targets.retain(|t| {
            let in_range = match &t.at {
                TargetPlace::Ground { points } => {
                    points.len() <= MAX_REGION_POINTS && points.iter().all(|p| finite(p))
                }
                TargetPlace::Wall { point, normal } => finite(point) && finite(normal),
            };
            in_range && t.at.problem().is_none()
        });
        for t in &mut self.targets {
            t.name = tidy_text(&t.name, MAX_NAME_CHARS);
        }
        make_ids_unique(self.targets.iter_mut().map(|t| &mut t.id).collect());
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

/// Gives each id that repeats an earlier one, or lies above [`MAX_ITEM_ID`],
/// a fresh one ([`fresh_id`]): the first id within range keeps its value.
/// Returns, for each out-of-range id, the id its first holder now has, so
/// that what referred to it can follow.
fn make_ids_unique(mut ids: Vec<&mut u32>) -> HashMap<u32, u32> {
    let mut taken = HashSet::new();
    let keep: Vec<bool> = ids
        .iter()
        .map(|id| **id <= MAX_ITEM_ID && taken.insert(**id))
        .collect();
    let mut moved = HashMap::new();
    for (id, keep) in ids.iter_mut().zip(keep) {
        if keep {
            continue;
        }
        let new = fresh_id(taken.iter().copied());
        taken.insert(new);
        if **id > MAX_ITEM_ID {
            moved.entry(**id).or_insert(new);
        }
        **id = new;
    }
    moved
}

/// `s` trimmed and cut to `max` characters, then trimmed again, so that it
/// loads the same the next time.
fn tidy_text(s: &str, max: usize) -> String {
    let cut: String = s.trim().chars().take(max).collect();
    cut.trim_end().to_owned()
}

/// Whether every value is a number within 10 000 of zero.
fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() < 10_000.0)
}

#[cfg(test)]
mod tests {
    use super::super::tests::scene;
    use super::*;
    use crate::camera::{CameraKind, CameraModel, CameraPlacement, DoriLevel};
    use crate::coverage::CoverageParams;
    use crate::footage::{AreaLabel, LabelledArea, Target};
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
    fn a_group_cut_before_whitespace_loses_it() {
        let mut s = scene();
        s.cameras[0].group = Some(format!("{} b", "a".repeat(79)));
        let once = SceneFile::from_json(&s.to_json()).unwrap();
        assert_eq!(once.cameras[0].group, Some("a".repeat(79)));
        assert_eq!(SceneFile::from_json(&once.to_json()).unwrap(), once);
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

    fn area(id: u32, points: Vec<[f32; 2]>) -> LabelledArea {
        LabelledArea {
            id,
            label: None,
            name: None,
            points,
            enabled: true,
        }
    }

    fn square(x: f32, size: f32) -> Vec<[f32; 2]> {
        vec![[x, 0.0], [x + size, 0.0], [x + size, size], [x, size]]
    }

    #[test]
    fn step_tidy_areas() {
        let mut s = scene();
        for (points, kept) in [
            (vec![[0.0, 0.0]; MAX_REGION_POINTS], true),
            (vec![[0.0, 0.0]; MAX_REGION_POINTS + 1], false),
            (vec![[9_999.0, -9_999.0]], true),
            (vec![[0.0, 10_000.0]], false),
            (vec![[f32::INFINITY, 0.0]], false),
            (vec![[f32::NAN, 0.0]], false),
        ] {
            s.areas = vec![area(1, points.clone())];
            s.limit_figures_to = Some(1);
            s.tidy_areas();
            assert_eq!(s.areas.len(), usize::from(kept), "{}", points.len());
            assert_eq!(s.limit_figures_to.is_some(), kept, "the limit follows");
        }
        // Names are tidied, repeated ids replaced, and the limit names the
        // first area with its id.
        s.areas = vec![
            area(4, square(0.0, 1.0)),
            area(4, square(2.0, 1.0)),
            area(2, square(4.0, 1.0)),
            area(4, square(6.0, 1.0)),
        ];
        s.areas[0].name = Some("  Our garden  ".into());
        s.areas[1].name = Some(" \t ".into());
        s.areas[2].name = Some(format!("{} b", "a".repeat(MAX_NAME_CHARS - 1)));
        s.limit_figures_to = Some(4);
        s.tidy_areas();
        let ids: Vec<u32> = s.areas.iter().map(|a| a.id).collect();
        assert_eq!(ids, [4, 5, 2, 6]);
        let names: Vec<Option<&str>> = s.areas.iter().map(|a| a.name.as_deref()).collect();
        let long = "a".repeat(MAX_NAME_CHARS - 1);
        assert_eq!(names, [Some("Our garden"), None, Some(long.as_str()), None]);
        assert_eq!(s.figures_area().map(|a| a.points[0]), Some([0.0, 0.0]));
        let before = s.clone();
        s.tidy_areas();
        assert_eq!(s, before, "a second pass changes nothing");
        // A limit naming no area is cleared.
        s.limit_figures_to = Some(99);
        s.tidy_areas();
        assert_eq!(s.limit_figures_to, None);
    }

    #[test]
    fn step_tidy_targets() {
        let ground = |id, points| Target {
            id,
            name: String::new(),
            at: TargetPlace::Ground { points },
        };
        let wall = |id, point, normal| Target {
            id,
            name: String::new(),
            at: TargetPlace::Wall { point, normal },
        };
        let mut s = scene();
        for (t, kept) in [
            (ground(1, square(0.0, 2.0)), true),
            (ground(1, square(9_000.0, 30.0)), true),
            (ground(1, square(0.0, 31.0)), false),
            (ground(1, vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]]), false),
            (ground(1, square(9_999.0, 2.0)), false),
            (
                ground(1, vec![[f32::NAN, 0.0], [1.0, 0.0], [1.0, 1.0]]),
                false,
            ),
            (wall(1, [1.0, 2.0, 3.0], [0.0, 1.0]), true),
            (wall(1, [1.0, 2.0, 3.0], [0.0, 0.0]), false),
            (wall(1, [1.0, f32::INFINITY, 3.0], [0.0, 1.0]), false),
            (wall(1, [1.0, 2.0, 3.0], [1e5, 1.0]), false),
        ] {
            s.targets = vec![t.clone()];
            s.tidy_targets();
            assert_eq!(s.targets.len(), usize::from(kept), "{t:?}");
        }
        s.targets = vec![
            ground(3, square(0.0, 1.0)),
            ground(3, square(0.0, 1.0)),
            wall(3, [0.0; 3], [1.0, 0.0]),
        ];
        s.targets[0].name = "  Front door ".into();
        s.targets[1].name = "x".repeat(500);
        s.tidy_targets();
        let ids: Vec<u32> = s.targets.iter().map(|t| t.id).collect();
        assert_eq!(ids, [3, 4, 5]);
        assert_eq!(s.targets[0].name, "Front door");
        assert_eq!(s.targets[1].name.chars().count(), MAX_NAME_CHARS);
        let before = s.clone();
        s.tidy_targets();
        assert_eq!(s, before, "a second pass changes nothing");
    }

    #[test]
    fn ids_above_the_largest_are_replaced_like_repeated_ones() {
        let mut s = scene();
        let cam = s.cameras[0].clone();
        s.cameras = [u32::MAX, 7, MAX_ITEM_ID + 1, 7, MAX_ITEM_ID]
            .into_iter()
            .map(|id| CameraPlacement { id, ..cam.clone() })
            .collect();
        s.areas = vec![
            area(u32::MAX, square(0.0, 1.0)),
            area(u32::MAX, square(2.0, 1.0)),
            area(3, square(4.0, 1.0)),
        ];
        s.limit_figures_to = Some(u32::MAX);
        s.targets = vec![Target {
            id: u32::MAX,
            name: "Gate".into(),
            at: TargetPlace::Ground {
                points: square(0.0, 1.0),
            },
        }];
        let loaded = SceneFile::from_json(&s.to_json()).unwrap();
        let cameras: Vec<u32> = loaded.cameras.iter().map(|c| c.id).collect();
        // Past `MAX_ITEM_ID`, fresh ids are the lowest free ones.
        assert_eq!(cameras, [1, 7, 2, 3, MAX_ITEM_ID]);
        let areas: Vec<u32> = loaded.areas.iter().map(|a| a.id).collect();
        assert_eq!(areas, [4, 5, 3]);
        assert_eq!(
            loaded.figures_area().map(|a| a.points[0]),
            Some([0.0, 0.0]),
            "the limit follows the first area that had the id"
        );
        assert_eq!(loaded.targets[0].id, 1);
        assert_eq!(SceneFile::from_json(&loaded.to_json()).unwrap(), loaded);
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

    /// The JSON an app before labelled areas wrote: a `region`, no `areas`.
    fn with_old_region(region: &str) -> String {
        scene().to_json().replacen(
            "\"cameras\"",
            &format!("\"region\": {region},\n  \"cameras\""),
            1,
        )
    }

    #[test]
    fn an_old_region_loads_as_the_area_that_limits_the_figures() {
        let json = with_old_region(r#"{"points": [[0, 0], [10, 0], [10, 10]], "enabled": false}"#);
        let s = SceneFile::from_json(&json).unwrap();
        assert_eq!(
            s.areas,
            [LabelledArea {
                id: 1,
                label: None,
                name: None,
                points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
                enabled: false,
            }]
        );
        assert_eq!(s.limit_figures_to, Some(1));
        // `enabled` defaults to on, as before the field existed.
        let json = with_old_region(r#"{"points": [[0, 0], [10, 0], [10, 10]]}"#);
        assert!(SceneFile::from_json(&json).unwrap().areas[0].enabled);
        // Written back with areas, and with the region copied for older apps.
        let out = s.to_json();
        assert!(
            out.contains("\"areas\"") && out.contains("\"limit_figures_to\": 1"),
            "{out}"
        );
        let wire: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            wire["region"],
            serde_json::json!({"points": [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]], "enabled": false})
        );
        assert_eq!(SceneFile::from_json(&out).unwrap(), s);
        // Nothing of the kind when there is no area.
        let plain = scene().to_json();
        for field in ["region", "areas", "limit_figures_to", "targets"] {
            assert!(!plain.contains(field), "{field} omitted when absent");
        }
    }

    #[test]
    fn region_is_only_a_copy_when_areas_are_present() {
        let mut s = scene();
        s.areas = vec![area(3, square(0.0, 5.0)), area(8, square(10.0, 5.0))];
        // No limit: no region is written, and older apps count everything.
        assert!(!s.to_json().contains("region"));
        s.limit_figures_to = Some(8);
        let json = s.to_json();
        assert!(json.contains("\"region\""));
        assert_eq!(SceneFile::from_json(&json).unwrap(), s);
        // An edited copy is ignored: the areas win.
        let mut wire: serde_json::Value = serde_json::from_str(&json).unwrap();
        wire["region"] = serde_json::json!({"points": [[0, 0], [99, 0], [99, 99]]});
        let loaded = SceneFile::from_json(&wire.to_string()).unwrap();
        assert_eq!(loaded, s);
        // A bad old region is dropped, and the limit with it; the cameras
        // survive.
        let far = with_old_region(r#"{"points": [[0, 0], [1e9, 0], [10, 10]]}"#);
        let loaded = SceneFile::from_json(&far).unwrap();
        assert_eq!(
            (
                loaded.areas.len(),
                loaded.limit_figures_to,
                loaded.cameras.len()
            ),
            (0, None, 1)
        );
        let many = format!(
            "{{\"points\": {:?}}}",
            vec![[0.0f32, 0.0]; MAX_REGION_POINTS + 1]
        );
        let loaded = SceneFile::from_json(&with_old_region(&many)).unwrap();
        assert_eq!((loaded.areas.len(), loaded.limit_figures_to), (0, None));
    }

    #[test]
    fn areas_and_targets_round_trip() {
        let mut s = scene();
        s.areas = vec![LabelledArea {
            id: 2,
            label: Some(AreaLabel::Neighbour),
            name: Some("No. 12's garden".into()),
            points: square(0.0, 5.0),
            enabled: false,
        }];
        s.targets = vec![
            Target {
                id: 1,
                name: "Side gate".into(),
                at: TargetPlace::Ground {
                    points: square(1.0, 1.0),
                },
            },
            Target {
                id: 2,
                name: String::new(),
                at: TargetPlace::Wall {
                    point: [1.0, 1.5, -2.0],
                    normal: [0.0, -1.0],
                },
            },
        ];
        let json = s.to_json();
        assert!(json.contains("\"label\": \"neighbour\""), "{json}");
        assert!(json.contains("\"kind\": \"wall\""), "{json}");
        assert_eq!(SceneFile::from_json(&json).unwrap(), s);
        // An unknown label refuses the file, as any malformed JSON does.
        let bad = json.replace("\"neighbour\"", "\"enemy\"");
        assert!(matches!(
            SceneFile::from_json(&bad),
            Err(SceneError::Json(_))
        ));
    }

    #[test]
    fn too_many_areas_or_targets_refuse_the_file() {
        let mut s = scene();
        s.areas = (0..MAX_AREAS as u32)
            .map(|i| area(i, square(0.0, 1.0)))
            .collect();
        s.targets = (0..MAX_TARGETS as u32)
            .map(|id| Target {
                id,
                name: String::new(),
                at: TargetPlace::Wall {
                    point: [0.0; 3],
                    normal: [1.0, 0.0],
                },
            })
            .collect();
        assert!(s.check_counts().is_ok());
        s.areas.push(area(99, square(0.0, 1.0)));
        assert_eq!(
            invalid(s.check_counts()),
            "65 areas, but at most 64 are supported"
        );
        s.areas.pop();
        s.targets.push(s.targets[0].clone());
        assert_eq!(
            invalid(s.check_counts()),
            "65 targets, but at most 64 are supported"
        );
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
    fn the_agent_log_is_optional_lenient_and_tidied() {
        use crate::scene::agent_log::{AgentLogEntry, MAX_AGENT_LOG, MAX_LOG_SUMMARY_CHARS};
        let mut s = scene();
        assert!(!s.to_json().contains("agent_log"), "omitted when empty");
        s.agent_log
            .push(AgentLogEntry::new("Add a camera by the gate", 1_793_475_782).unwrap());
        let json = s.to_json();
        assert!(
            json.contains("\"accepted_at\": \"2026-10-31T19:43:02Z\""),
            "{json}"
        );
        assert_eq!(SceneFile::from_json(&json).unwrap(), s);
        let with_log = |log: &str| {
            scene().to_json().replacen(
                "\"cameras\"",
                &format!("\"agent_log\": {log},\n  \"cameras\""),
                1,
            )
        };
        // A malformed block reads as empty; a malformed entry is dropped.
        for bad in ["5", "\"x\"", "{}", "null"] {
            let loaded = SceneFile::from_json(&with_log(bad)).unwrap();
            assert_eq!(
                (loaded.agent_log.len(), loaded.cameras.len()),
                (0, 1),
                "{bad}"
            );
        }
        let loaded = SceneFile::from_json(&with_log(
            r#"[{"summary": "kept", "accepted_at": "2026-10-31T19:43:02Z"},
                {"summary": "bad time", "accepted_at": "yesterday"},
                {"summary": "   ", "accepted_at": "2026-10-31T19:43:02Z"},
                {"summary": 3}]"#,
        ))
        .unwrap();
        assert_eq!(loaded.agent_log.len(), 1);
        assert_eq!(loaded.agent_log[0].summary, "kept");
        // Summaries are cut and only the newest entries kept.
        let entries: Vec<String> = (0..MAX_AGENT_LOG + 3)
            .map(|i| {
                format!(
                    r#"{{"summary": "{i} {}", "accepted_at": "2026-10-31T19:43:02Z"}}"#,
                    "y".repeat(300)
                )
            })
            .collect();
        let loaded = SceneFile::from_json(&with_log(&format!("[{}]", entries.join(",")))).unwrap();
        assert_eq!(loaded.agent_log.len(), MAX_AGENT_LOG);
        assert!(loaded.agent_log[0].summary.starts_with("3 "));
        assert!(
            loaded
                .agent_log
                .iter()
                .all(|e| e.summary.chars().count() == MAX_LOG_SUMMARY_CHARS)
        );
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
