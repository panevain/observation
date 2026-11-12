//! Where a camera sits and where it points.

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// Where a camera sits and where it points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraPlacement {
    pub id: u32,
    pub label: String,
    pub model_id: String,
    /// World-frame position, metres.
    pub position: [f32; 3],
    /// Compass bearing of the optical axis, degrees clockwise from north.
    pub yaw_deg: f32,
    /// Elevation of the optical axis, degrees (negative = looking down).
    pub pitch_deg: f32,
    /// Rotation about the optical axis, degrees.
    #[serde(default)]
    pub roll_deg: f32,
    /// Colour of the camera, its view cone and its per-camera overlay (sRGB).
    /// `None` uses the default colour for the model's
    /// [`CameraKind`](super::CameraKind). Files from before this field carried
    /// a `color_index`, which is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    #[serde(default = "crate::util::default_true")]
    pub enabled: bool,
    /// Group the camera is listed under, set by hand. `None` groups it by the
    /// address OpenStreetMap gives for its position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// Longest group name kept.
pub const MAX_GROUP_CHARS: usize = 80;

impl CameraPlacement {
    pub fn eye(&self) -> Vec3 {
        Vec3::from_array(self.position)
    }

    pub fn forward(&self) -> Vec3 {
        forward_from_yaw_pitch(self.yaw_deg, self.pitch_deg)
    }

    /// Points the camera at `target` (keeps roll).
    pub fn look_at(&mut self, target: Vec3) {
        if let Some((yaw, pitch)) = yaw_pitch_from_forward(target - self.eye()) {
            self.yaw_deg = yaw;
            self.pitch_deg = pitch;
        }
    }
}

/// Unit forward vector for a compass bearing and elevation (degrees).
pub fn forward_from_yaw_pitch(yaw_deg: f32, pitch_deg: f32) -> Vec3 {
    let (sy, cy) = yaw_deg.to_radians().sin_cos();
    let (sp, cp) = pitch_deg.to_radians().sin_cos();
    Vec3::new(sy * cp, sp, -cy * cp)
}

/// Inverse of [`forward_from_yaw_pitch`]; yaw in `[0, 360)`. `None` for zero vectors.
pub fn yaw_pitch_from_forward(v: Vec3) -> Option<(f32, f32)> {
    let v = v.try_normalize()?;
    let yaw = v.x.atan2(-v.z).to_degrees().rem_euclid(360.0);
    let pitch = v.y.clamp(-1.0, 1.0).asin().to_degrees();
    Some((yaw, pitch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaw_pitch_round_trip() {
        for (yaw, pitch) in [(0.0, 0.0), (90.0, -20.0), (225.0, 10.0), (359.0, -60.0)] {
            let f = forward_from_yaw_pitch(yaw, pitch);
            let (y2, p2) = yaw_pitch_from_forward(f).unwrap();
            assert!(
                (y2 - yaw).abs() < 1e-3 && (p2 - pitch).abs() < 1e-3,
                "{yaw},{pitch} -> {y2},{p2}"
            );
        }
        let north = forward_from_yaw_pitch(0.0, 0.0);
        assert!(north.z < -0.99);
        let east = forward_from_yaw_pitch(90.0, 0.0);
        assert!(east.x > 0.99);
    }

    #[test]
    fn look_at_points_camera() {
        let mut p = CameraPlacement {
            id: 1,
            label: "x".into(),
            model_id: "m".into(),
            position: [0.0, 2.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        };
        p.look_at(Vec3::new(10.0, 0.0, 0.0));
        assert!((p.yaw_deg - 90.0).abs() < 1e-3);
        assert!(p.pitch_deg < 0.0);
    }
}
