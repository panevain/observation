//! `SceneFile::from_json` on arbitrary text: users import scene files from
//! anywhere. Accepted scenes must stay within the documented limits and
//! round-trip through `to_json`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use observation_core::scene::{MAX_CAMERAS, MAX_CUSTOM_MODELS, SceneFile};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(scene) = SceneFile::from_json(text) {
        assert!(scene.cameras.len() <= MAX_CAMERAS);
        assert!(scene.custom_models.len() <= MAX_CUSTOM_MODELS);
        for c in &scene.cameras {
            assert!(c.position.iter().all(|v| v.is_finite()));
        }
        SceneFile::from_json(&scene.to_json()).expect("exported scene re-imports");
    }
});
