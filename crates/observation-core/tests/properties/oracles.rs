//! Line of sight, the view cone and sight lines checked against oracles
//! written independently of the code under test: plain trigonometry for the
//! cone, point sampling against the prisms for occlusion, and a scan of every
//! cell for a camera's sight lines.

use crate::config;
use crate::coverage::{Camera, House, camera, house, street};
use glam::Vec3;
use observation_core::frustum::ViewCone;
use observation_core::occlusion::Occluders;
use proptest::prelude::*;

/// How far façade sample points sit in front of the wall (5 cm, as
/// docs/design.md states).
const FACADE_OFFSET_M: f32 = 0.05;

/// Where `p` is relative to `house`'s solid: strictly inside it (by more
/// than `margin`), or within `margin` of it.
fn inside(h: &House, p: Vec3, margin: f32) -> bool {
    p.x > h.x + margin
        && p.x < h.x + h.w - margin
        && p.z > h.z + margin
        && p.z < h.z + h.d - margin
        && p.y > h.min + margin
        && p.y < h.h - margin
}

fn point() -> impl Strategy<Value = Vec3> {
    (-60.0..=60.0f32, 0.0..=20.0f32, -60.0..=60.0f32).prop_map(|(x, y, z)| Vec3::new(x, y, z))
}

/// Segments anywhere, and segments aimed through the middle of a house.
fn houses_and_segment() -> impl Strategy<Value = (Vec<House>, Vec3, Vec3)> {
    prop::collection::vec(house(), 1..4).prop_flat_map(|houses| {
        let n = houses.len();
        let through = (point(), 0..n, 0.0..=20.0f32).prop_map({
            let houses = houses.clone();
            move |(a, i, y)| {
                let h = houses[i];
                let c = Vec3::new(h.x + h.w / 2.0, y, h.z + h.d / 2.0);
                (a, c * 2.0 - a)
            }
        });
        let any = (point(), point());
        (Just(houses), prop_oneof![any, through]).prop_map(|(h, (a, b))| (h, a, b))
    })
}

/// A camera looking along `yaw` at `pitch`, and a point given by its offsets
/// across the image (`su`, `sw`, as fractions of the half-width and
/// half-height) at a distance `t` (as a fraction of the range).
fn cone_and_point() -> impl Strategy<Value = (Vec3, f32, f32, f32, f32, f32, f32, f32, f32)> {
    (
        point(),
        (0.0..360.0f32, -80.0..=80.0f32),
        (2.0..=170.0f32, 2.0..=170.0f32, 1.0..=100.0f32),
        (-1.5..=1.5f32, -1.5..=1.5f32, 0.01..=1.5f32),
    )
        .prop_map(|(eye, (yaw, pitch), (h, v, r), (su, sw, t))| {
            (eye, yaw, pitch, h, v, r, su, sw, t)
        })
}

proptest! {
    #![proptest_config(config(256))]

    /// The view cone is the rectangular pyramid docs/design.md describes:
    /// `forward = (sin yaw cos pitch, sin pitch, -cos yaw cos pitch)`, `right`
    /// horizontal from yaw, `up = right × forward`, half-angles from the
    /// fields of view, and a straight-line range.
    #[test]
    fn prop_view_cone_is_the_documented_pyramid(
        (eye, yaw, pitch, hfov, vfov, range, su, sw, t) in cone_and_point(),
    ) {
        let cone = ViewCone::from_angles(eye, yaw, pitch, 0.0, hfov, vfov, range);
        let (sy, cy) = (f64::from(yaw).to_radians().sin(), f64::from(yaw).to_radians().cos());
        let (sp, cp) = (f64::from(pitch).to_radians().sin(), f64::from(pitch).to_radians().cos());
        let forward = [sy * cp, sp, -cy * cp];
        let right = [cy, 0.0, sy];
        // right × forward
        let up = [
            right[1] * forward[2] - right[2] * forward[1],
            right[2] * forward[0] - right[0] * forward[2],
            right[0] * forward[1] - right[1] * forward[0],
        ];
        let half = |fov: f32| (f64::from(fov).to_radians() / 2.0).tan();
        let (u, w) = (f64::from(su) * half(hfov), f64::from(sw) * half(vfov));
        let dir: Vec<f64> = (0..3).map(|k| forward[k] + u * right[k] + w * up[k]).collect();
        let len = dir.iter().map(|c| c * c).sum::<f64>().sqrt();
        let dist = f64::from(t) * f64::from(range);
        let p = Vec3::new(
            eye.x + (dir[0] / len * dist) as f32,
            eye.y + (dir[1] / len * dist) as f32,
            eye.z + (dir[2] / len * dist) as f32,
        );
        let well_inside = su.abs() < 0.99 && sw.abs() < 0.99 && t < 0.99 && dist > 0.05;
        let well_outside = su.abs() > 1.01 || sw.abs() > 1.01 || t > 1.01;
        if well_inside {
            prop_assert!(cone.contains(p), "{p} should be inside");
        } else if well_outside {
            prop_assert!(!cone.contains(p), "{p} should be outside");
        }
    }

    /// A segment that passes through a building's solid is blocked; one that
    /// keeps half a metre from every building is not. Sampled every few
    /// centimetres, independently of the crossing and grid-walk logic.
    #[test]
    fn prop_occlusion_matches_point_sampling((houses, a, b) in houses_and_segment()) {
        let footprints: Vec<_> = houses.iter().map(House::footprint).collect();
        let blocked = Occluders::new(&footprints).segment_blocked(a, b);
        let samples = 2048;
        let at = |i: usize| a.lerp(b, i as f32 / samples as f32);
        let through = (0..=samples).any(|i| houses.iter().any(|h| inside(h, at(i), 1e-3)));
        let near = (0..=samples).any(|i| houses.iter().any(|h| inside(h, at(i), -0.5)));
        if through {
            prop_assert!(blocked, "{a} to {b} passes through a building");
        } else if !near {
            prop_assert!(!blocked, "{a} to {b} keeps clear of every building");
        }
    }
}

proptest! {
    #![proptest_config(config(24))]

    /// A camera's sight lines are the cells a scan of the whole area finds:
    /// every open ground cell and every façade patch facing the eye, tested
    /// with the cone and the line-of-sight test, with the distances
    /// `StaticLayers::sight` documents. Independent of how it picks
    /// candidate cells.
    #[test]
    fn prop_sight_is_a_scan_of_every_cell(
        s in street(),
        cam in camera(),
        subject in 0.0..=2.0f32,
    ) {
        let layers = s.layers();
        let cone: ViewCone = Camera::cone(&cam, &layers);
        let sight = layers.sight(&cone, subject);
        let mut ground = (Vec::new(), Vec::new());
        for i in 0..layers.grid.len() {
            if layers.building_mask[i] {
                continue;
            }
            let p = layers.ground_subject(i, subject);
            if cone.contains(p) && !layers.segment_blocked(cone.eye, p) {
                ground.0.push(i as u32);
                ground.1.push((p - cone.eye).length_squared());
            }
        }
        let mut facade = (Vec::new(), Vec::new());
        for (i, f) in layers.facades.iter().enumerate() {
            let to_eye = cone.eye - f.center;
            if to_eye.length_squared() > cone.range * cone.range || to_eye.dot(f.normal) <= 0.0 {
                continue;
            }
            let p = f.center + f.normal * FACADE_OFFSET_M;
            if cone.contains(p) && !layers.segment_blocked(cone.eye, p) {
                facade.0.push(i as u32);
                facade.1.push(to_eye.length_squared().max((p - cone.eye).length_squared()));
            }
        }
        prop_assert_eq!((&sight.ground, &sight.ground_d2), (&ground.0, &ground.1));
        prop_assert_eq!((&sight.facade, &sight.facade_d2), (&facade.0, &facade.1));
    }
}

proptest! {
    #![proptest_config(config(24))]

    /// The street mask is every open cell whose centre lies within half the
    /// road's width (6 m) of its centreline, measured here with plain f64
    /// geometry; cells within a millimetre of the band's edge are skipped.
    #[test]
    fn prop_street_mask_is_the_road_band(s in street()) {
        let layers = s.layers();
        let end = |x: f32| [f64::from(x), f64::from(s.road_z + (x + 50.0) / 10.0)];
        let (a, b) = (end(s.road_x.0), end(s.road_x.1));
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        for i in 0..layers.grid.len() {
            let c = layers.grid.center(i);
            let (px, pz) = (f64::from(c.x) - a[0], f64::from(c.y) - a[1]);
            let t = ((px * dx + pz * dz) / (dx * dx + dz * dz)).clamp(0.0, 1.0);
            let dist = (px - t * dx).hypot(pz - t * dz);
            if (dist - 3.0).abs() < 1e-3 {
                continue;
            }
            let want = dist < 3.0 && !layers.building_mask[i];
            prop_assert_eq!(layers.street_mask[i], want, "cell {} at {}, {} m from the road", i, c, dist);
        }
    }
}
