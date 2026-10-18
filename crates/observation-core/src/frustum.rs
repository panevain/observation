//! A camera's view volume: rectangular field of view clipped to a range.

use crate::camera::{CameraModel, CameraPlacement};
use glam::{Quat, Vec3};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewCone {
    pub eye: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub tan_half_h: f32,
    pub tan_half_v: f32,
    pub range: f32,
}

impl ViewCone {
    pub fn new(placement: &CameraPlacement, model: &CameraModel, range: f32) -> Self {
        Self::from_angles(
            placement.eye(),
            placement.yaw_deg,
            placement.pitch_deg,
            placement.roll_deg,
            model.hfov_clamped(),
            model.vfov_clamped(),
            range,
        )
    }

    pub fn from_angles(
        eye: Vec3,
        yaw_deg: f32,
        pitch_deg: f32,
        roll_deg: f32,
        hfov_deg: f32,
        vfov_deg: f32,
        range: f32,
    ) -> Self {
        let forward = crate::camera::forward_from_yaw_pitch(yaw_deg, pitch_deg);
        // Right is horizontal and derived from yaw so it is stable at ±90° pitch.
        let (sy, cy) = yaw_deg.to_radians().sin_cos();
        let right0 = Vec3::new(cy, 0.0, sy);
        let roll = Quat::from_axis_angle(forward, -roll_deg.to_radians());
        let right = roll * right0;
        let up = right.cross(forward).normalize();
        Self {
            eye,
            forward,
            right,
            up,
            tan_half_h: (hfov_deg.to_radians() / 2.0).tan(),
            tan_half_v: (vfov_deg.to_radians() / 2.0).tan(),
            range,
        }
    }

    /// Whether `p` lies inside the field of view and within range.
    pub fn contains(&self, p: Vec3) -> bool {
        let d = p - self.eye;
        let z = d.dot(self.forward);
        if z <= 1e-4 || d.length_squared() > self.range * self.range {
            return false;
        }
        d.dot(self.right).abs() <= z * self.tan_half_h
            && d.dot(self.up).abs() <= z * self.tan_half_v
    }

    /// Unit direction through normalised image coordinates `u, v ∈ [-1, 1]`.
    pub fn ray(&self, u: f32, v: f32) -> Vec3 {
        (self.forward + self.right * (u * self.tan_half_h) + self.up * (v * self.tan_half_v))
            .normalize()
    }

    /// Closed outline of the image rectangle as unit directions
    /// (`per_edge` samples per edge), for drawing the range "fan".
    pub fn outline_rays(&self, per_edge: usize) -> Vec<Vec3> {
        let n = per_edge.max(1);
        let t = |i: usize| -1.0 + 2.0 * i as f32 / n as f32;
        let mut out = Vec::with_capacity(4 * n);
        out.extend((0..n).map(|i| self.ray(t(i), 1.0)));
        out.extend((0..n).map(|i| self.ray(1.0, -t(i))));
        out.extend((0..n).map(|i| self.ray(-t(i), -1.0)));
        out.extend((0..n).map(|i| self.ray(-1.0, t(i))));
        out
    }

    /// The four corner directions (top-left, top-right, bottom-right, bottom-left).
    pub fn corner_rays(&self) -> [Vec3; 4] {
        [
            self.ray(-1.0, 1.0),
            self.ray(1.0, 1.0),
            self.ray(1.0, -1.0),
            self.ray(-1.0, -1.0),
        ]
    }

    /// Conservative `[min, max]` bounds of the view volume in the `x, z` plane.
    pub fn xz_bounds(&self) -> ([f32; 2], [f32; 2]) {
        let r = self.range;
        (
            [self.eye.x - r, self.eye.z - r],
            [self.eye.x + r, self.eye.z + r],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cone(yaw: f32, pitch: f32, roll: f32) -> ViewCone {
        ViewCone::from_angles(Vec3::new(0.0, 2.0, 0.0), yaw, pitch, roll, 90.0, 60.0, 20.0)
    }

    #[test]
    fn contains_basic() {
        let c = cone(0.0, 0.0, 0.0); // looking north (-Z)
        assert!(c.contains(Vec3::new(0.0, 2.0, -10.0)));
        assert!(!c.contains(Vec3::new(0.0, 2.0, 10.0)), "behind");
        assert!(!c.contains(Vec3::new(0.0, 2.0, -25.0)), "out of range");
        assert!(
            c.contains(Vec3::new(9.0, 2.0, -10.0)),
            "inside 45° half-angle"
        );
        assert!(
            !c.contains(Vec3::new(11.0, 2.0, -10.0)),
            "outside 45° half-angle"
        );
        assert!(
            !c.contains(Vec3::new(0.0, 9.0, -10.0)),
            "outside 30° vertical"
        );
    }

    #[test]
    fn basis_is_orthonormal_and_right_handed() {
        for (y, p, r) in [(0.0, 0.0, 0.0), (130.0, -35.0, 10.0), (270.0, 89.0, 0.0)] {
            let c = cone(y, p, r);
            assert!(c.forward.dot(c.right).abs() < 1e-5);
            assert!(c.forward.dot(c.up).abs() < 1e-5);
            assert!(
                c.right.cross(c.up).dot(-c.forward) > 0.99,
                "right × up = back"
            );
        }
        let c = cone(0.0, 0.0, 0.0);
        assert!(c.right.x > 0.99 && c.up.y > 0.99);
    }

    #[test]
    fn outline_rays_inside_cone() {
        let c = cone(45.0, -20.0, 0.0);
        let rays = c.outline_rays(4);
        assert_eq!(rays.len(), 16);
        for r in rays {
            let p = c.eye + r * 10.0 * 0.999 + c.forward * 0.01;
            assert!(c.contains(p));
        }
    }
}
