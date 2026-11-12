//! Saved scenes: the user's area, cameras, custom models and settings, as JSON.

mod file;
mod sanitise;

pub use file::{
    AreaRef, FLAT_GROUND_VERSION, MAX_CAMERAS, MAX_CUSTOM_MODELS, SCENE_FORMAT, SCENE_VERSION,
    SceneError, SceneFile, lift_onto_terrain,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::CameraPlacement;
    use crate::geo::LatLon;

    pub(super) fn scene() -> SceneFile {
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
            group: None,
        });
        s
    }
}
