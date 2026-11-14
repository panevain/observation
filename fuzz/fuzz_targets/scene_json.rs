//! `SceneFile::from_json` on arbitrary text: users import scene files from
//! anywhere. Accepted scenes must stay within the documented limits and
//! round-trip through `to_json`, and custom model ids must be unique and
//! start with `custom-` (so none shadows another model).
#![no_main]

use libfuzzer_sys::fuzz_target;
use observation_core::camera::CUSTOM_ID_PREFIX;
use observation_core::scene::{MAX_AREAS, MAX_CAMERAS, MAX_CUSTOM_MODELS, MAX_TARGETS, SceneFile};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(scene) = SceneFile::from_json(text) {
        assert!(scene.cameras.len() <= MAX_CAMERAS);
        assert!(scene.custom_models.len() <= MAX_CUSTOM_MODELS);
        assert!(scene.areas.len() <= MAX_AREAS && scene.targets.len() <= MAX_TARGETS);
        assert!(scene.targets.iter().all(|t| t.at.problem().is_none()));
        for c in &scene.cameras {
            assert!(c.position.iter().all(|v| v.is_finite()));
        }
        let mut ids = std::collections::HashSet::new();
        for m in &scene.custom_models {
            assert!(m.id.starts_with(CUSTOM_ID_PREFIX), "{}", m.id);
            assert!(ids.insert(m.id.as_str()), "duplicate {}", m.id);
        }
        let again = SceneFile::from_json(&scene.to_json()).expect("exported scene re-imports");
        assert_eq!(
            again.custom_models, scene.custom_models,
            "ids are already repaired"
        );
        assert_eq!(again.areas, scene.areas, "areas are already tidied");
        assert_eq!(again.targets, scene.targets, "targets are already tidied");
    }
});
