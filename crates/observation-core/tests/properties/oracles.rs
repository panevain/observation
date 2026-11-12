//! Line of sight, the view cone and sight lines checked against oracles
//! that share no decision code with the code under test: plain trigonometry
//! for the cone, point sampling against the boxes for occlusion, both of
//! those for a scan of every cell a camera might see, and point-to-segment
//! distance for the street band. Each states the band it does not check.

use crate::config;
use crate::coverage::{Camera, House, Street, camera, house, street};
use glam::{Vec2, Vec3};
use observation_core::frustum::ViewCone;
use observation_core::occlusion::Occluders;
use observation_core::terrain::Terrain;
use proptest::prelude::*;
use proptest::test_runner::{TestCaseError, TestRunner};
use std::cell::Cell;

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

/// Segments anywhere, segments aimed through the middle of a house, and
/// segments that clip a house's corner (through a point up to 30 cm inside
/// it, which is not the segment's middle).
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
        let corner = (
            point(),
            0..n,
            (any::<bool>(), any::<bool>()),
            0.01..=0.3f32,
            0.0..=1.0f32,
            1.2..=3.0f32,
        )
            .prop_map({
                let houses = houses.clone();
                move |(a, i, (east, south), inset, up, beyond)| {
                    let h = houses[i];
                    let x = if east { h.x + h.w - inset } else { h.x + inset };
                    let z = if south {
                        h.z + h.d - inset
                    } else {
                        h.z + inset
                    };
                    let c = Vec3::new(x, h.min + (h.h - h.min) * up, z);
                    (a, a + (c - a) * beyond)
                }
            });
        let any = (point(), point());
        (
            Just(houses),
            prop_oneof![2 => any, 1 => through, 1 => corner],
        )
            .prop_map(|(h, (a, b))| (h, a, b))
    })
}

/// Vectors in `f64`, for the oracles.
type D3 = [f64; 3];

fn d3(v: Vec3) -> D3 {
    [f64::from(v.x), f64::from(v.y), f64::from(v.z)]
}

fn sub(a: D3, b: D3) -> D3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: D3, b: D3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: D3, b: D3) -> D3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// `a + (b - a) t`
fn lerp(a: D3, b: D3, t: f64) -> D3 {
    [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t)
}

/// The view volume docs/design.md describes, in `f64` trigonometry:
/// `forward = (sin yaw cos pitch, sin pitch, -cos yaw cos pitch)`, `right`
/// horizontal from yaw (`(cos yaw, 0, sin yaw)`) and then turned by roll about
/// the view axis, `up = right × forward`, half-angles from the fields of view
/// and a straight-line range.
///
/// docs/design.md gives no sign for roll. This uses the engine's: a positive
/// roll turns `right` towards `up` (`right cos roll + up sin roll`), which is
/// how the client's view fan, drawn from the cone, turns.
struct Pyramid {
    eye: D3,
    forward: D3,
    right: D3,
    up: D3,
    tan_h: f64,
    tan_v: f64,
    range: f64,
}

impl Pyramid {
    fn new(eye: Vec3, yaw: f32, pitch: f32, roll: f32, hfov: f32, vfov: f32, range: f32) -> Self {
        let rad = |deg: f32| f64::from(deg).to_radians();
        let ((sy, cy), (sp, cp), (sr, cr)) = (
            rad(yaw).sin_cos(),
            rad(pitch).sin_cos(),
            rad(roll).sin_cos(),
        );
        let forward = [sy * cp, sp, -cy * cp];
        let right0 = [cy, 0.0, sy];
        let up0 = cross(right0, forward);
        let right = [0, 1, 2].map(|k| right0[k] * cr + up0[k] * sr);
        let up = cross(right, forward);
        let half = |fov: f32| (rad(fov) / 2.0).tan();
        Self {
            eye: d3(eye),
            forward,
            right,
            up,
            tan_h: half(hfov),
            tan_v: half(vfov),
            range: f64::from(range),
        }
    }

    /// The point at image offsets `su`, `sw` (fractions of the half-width and
    /// half-height) and distance `dist` from the eye.
    fn point(&self, su: f64, sw: f64, dist: f64) -> D3 {
        let (u, w) = (su * self.tan_h, sw * self.tan_v);
        let dir = [0, 1, 2].map(|k| self.forward[k] + u * self.right[k] + w * self.up[k]);
        let len = dot(dir, dir).sqrt();
        [0, 1, 2].map(|k| self.eye[k] + dir[k] / len * dist)
    }

    /// How far `p` lies inside the volume, in metres: its distance to the
    /// nearest of the four side planes and the range sphere, negative
    /// outside.
    fn margin(&self, p: D3) -> f64 {
        let d = sub(p, self.eye);
        let z = dot(d, self.forward);
        let side = |axis: D3, tan: f64| (z * tan - dot(d, axis).abs()) / tan.hypot(1.0);
        side(self.right, self.tan_h)
            .min(side(self.up, self.tan_v))
            .min(self.range - dot(d, d).sqrt())
    }
}

/// Roll anywhere in a turn and either way (the inspector offers ±180°;
/// files may hold more), and pitch over the inspector's ±90° (exact ends
/// included) with some beyond, which files may hold and the formula covers.
fn yaw_pitch_roll() -> impl Strategy<Value = (f32, f32, f32)> {
    (
        0.0..360.0f32,
        prop_oneof![
            6 => -90.0..=90.0f32,
            1 => prop::sample::select(vec![-90.0f32, 90.0]),
            1 => -180.0..=180.0f32,
        ],
        prop_oneof![5 => -180.0..=180.0f32, 1 => -360.0..=360.0f32],
    )
}

/// A camera looking along `yaw` at `pitch` with `roll`, and a point given by
/// its offsets across the image (`su`, `sw`, as fractions of the half-width
/// and half-height) at a distance `t` (as a fraction of the range).
#[allow(clippy::type_complexity)]
fn cone_and_point()
-> impl Strategy<Value = (Vec3, (f32, f32, f32), (f32, f32, f32), (f32, f32, f32))> {
    (
        point(),
        yaw_pitch_roll(),
        (2.0..=170.0f32, 2.0..=170.0f32, 1.0..=100.0f32),
        (-1.5..=1.5f32, -1.5..=1.5f32, 0.01..=1.5f32),
    )
}

proptest! {
    #![proptest_config(config(256))]

    /// The view cone is the [`Pyramid`] docs/design.md describes. Points
    /// within 1 mm of its surface are not checked (`f32` rounding is a few
    /// micrometres here); the rest must be classified correctly.
    #[test]
    fn prop_view_cone_is_the_documented_pyramid(
        (eye, (yaw, pitch, roll), (hfov, vfov, range), (su, sw, t)) in cone_and_point(),
    ) {
        let cone = ViewCone::from_angles(eye, yaw, pitch, roll, hfov, vfov, range);
        let pyramid = Pyramid::new(eye, yaw, pitch, roll, hfov, vfov, range);
        let dist = f64::from(t) * f64::from(range);
        let q = pyramid.point(f64::from(su), f64::from(sw), dist);
        let p = Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32);
        let margin = pyramid.margin(d3(p));
        if margin > 1e-3 {
            prop_assert!(cone.contains(p), "{p} should be inside");
        } else if margin < -1e-3 {
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

/// What the scan oracle says about one sample point.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Verdict {
    /// Seen, at this squared distance from the eye.
    Seen(f64),
    Unseen,
    /// Too close to an edge of the cone, the range, a building or the
    /// ground's blocking threshold to call.
    Unsure,
}

/// Whether segment `a`–`b` passes through `h`'s solid (by more than 1 mm),
/// comes near it (5 cm; 2 cm in the last 60 cm, where a façade's sample point
/// sits 5 cm in front of its own wall), or keeps clear. Sampled every
/// centimetre over the stretch inside the house's box widened by 5 cm (cut
/// out with the slab method only to save time).
fn house_hit(h: &House, a: D3, b: D3) -> Verdict {
    let lo = [h.x, h.min, h.z].map(f64::from);
    let hi = [h.x + h.w, h.h, h.z + h.d].map(f64::from);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for k in 0..3 {
        let d = b[k] - a[k];
        let (l, u) = (lo[k] - 0.05, hi[k] + 0.05);
        if d.abs() < 1e-12 {
            if a[k] < l || a[k] > u {
                return Verdict::Seen(0.0);
            }
        } else {
            let (p, q) = ((l - a[k]) / d, (u - a[k]) / d);
            t0 = t0.max(p.min(q));
            t1 = t1.min(p.max(q));
        }
    }
    if t0 > t1 {
        return Verdict::Seen(0.0);
    }
    let len = dot(sub(b, a), sub(b, a)).sqrt();
    let n = ((t1 - t0) * len / 0.01).ceil().max(1.0) as u32;
    let mut near = false;
    for i in 0..=n {
        let t = t0 + (t1 - t0) * f64::from(i) / f64::from(n);
        let p = lerp(a, b, t);
        let within = |m: f64| (0..3).all(|k| p[k] > lo[k] + m && p[k] < hi[k] - m);
        if within(1e-3) {
            return Verdict::Unseen;
        }
        near |= within(if (1.0 - t) * len < 0.6 { -0.02 } else { -0.05 });
    }
    if near {
        Verdict::Unsure
    } else {
        Verdict::Seen(0.0)
    }
}

/// Whether the ground surely does not block segment `a`–`b`: docs/design.md's
/// rule is that it blocks where it rises more than 0.1 m above the segment,
/// skipping the 0.75 m at each end (measured across the ground). Sampled every
/// 25 cm from 0.45 m in, and clear when the ground stays 5 cm under that
/// threshold everywhere. The ground here is a plane or flat, so a segment
/// between two points above it is never truly blocked: the oracle does not
/// claim blocking, only clearance.
fn ground_clear(terrain: &Terrain, a: D3, b: D3) -> bool {
    let len = (b[0] - a[0]).hypot(b[2] - a[2]);
    if len <= 1.5 {
        return true;
    }
    let n = (len / 0.25).ceil() as u32;
    (0..=n).all(|i| {
        let t = f64::from(i) / f64::from(n);
        let (along, p) = (t * len, lerp(a, b, t));
        let ground = terrain.height_at(Vec2::new(p[0] as f32, p[2] as f32));
        along < 0.45 || len - along < 0.45 || f64::from(ground) < p[1] + 0.05
    })
}

/// A street, and a camera that is a general one ([`camera`]), one inside the
/// area tilted down at the ground with a wide view (so that most cases see
/// ground cells), or one 8 to 25 m from a house looking at it (so that many
/// see façades).
fn street_and_scan_camera() -> impl Strategy<Value = (Street, Camera)> {
    let down = (
        (-40.0..=40.0f32, -40.0..=40.0f32, 1.5..=6.0f32),
        (0.0..360.0f32, -50.0..=-15.0f32, -30.0..=30.0f32),
        (40.0..=120.0f32, 30.0..=90.0f32, 10.0..=40.0f32),
    )
        .prop_map(
            |((x, z, above), (yaw, pitch, roll), (hfov, vfov, range))| Camera {
                at: Vec2::new(x, z),
                above,
                yaw,
                pitch,
                roll,
                hfov,
                vfov,
                range,
            },
        );
    street().prop_flat_map(move |s| {
        let houses = s.houses.clone();
        let facing = (
            any::<prop::sample::Index>(),
            (0.0..360.0f32, 8.0..=25.0f32, 1.5..=6.0f32),
            (-20.0..=0.0f32, -30.0..=30.0f32),
            (60.0..=120.0f32, 40.0..=90.0f32, 30.0..=40.0f32),
        )
            .prop_map(
                move |(i, (bearing, dist, above), (pitch, roll), (hfov, vfov, range))| {
                    // The house's centre (or the area's, if there is none).
                    let c = houses
                        .get(i.index(houses.len().max(1)))
                        .map_or(Vec2::ZERO, |h| Vec2::new(h.x + h.w / 2.0, h.z + h.d / 2.0));
                    let (sb, cb) = bearing.to_radians().sin_cos();
                    Camera {
                        at: c - Vec2::new(sb, -cb) * dist,
                        above,
                        yaw: bearing,
                        pitch,
                        roll,
                        hfov,
                        vfov,
                        range,
                    }
                },
            );
        (Just(s), prop_oneof![camera(), down.clone(), facing])
    })
}

/// A camera's sight lines, against a scan of every open ground cell and every
/// façade patch that shares no decision code with the engine: the
/// [`Pyramid`] for the view and the range, point sampling against the houses'
/// boxes and the ground for line of sight, and docs/design.md's sample points
/// (a cell's centre on the ground, lifted by the subject height; a patch's
/// centre pushed 5 cm out, which must face the eye, with both points in
/// range). It takes from the engine only the inputs: the grid, which cells
/// are buildings, the façade patches and the ground's height.
///
/// Points the oracle cannot call (within 1 cm of the view's surface, 5 cm of
/// a building, or 5 cm of the ground's blocking threshold) are not checked. Every other point must be listed, at the squared distance the
/// oracle finds (to a relative 1e-5), or not listed. At least 60% of the
/// cases must see some ground cell the oracle is sure of (about 85% do), at
/// least 15% some façade patch (about a third do: on tilted ground the
/// houses are buried), and at most 5% of the points in or near the view may
/// go unchecked (about 0.5% are); see `SCAN_CASES` for the margins.
#[test]
fn prop_sight_matches_an_independent_scan() {
    // The floors below are counted over this many cases. Measured over 300
    // seeds at 96 cases: ground seen in 84.5% of cases (sd 3.8 points), a
    // façade in 33.5% (sd 5.1, binomial), and 0.54% of points unchecked (sd
    // 0.22, at most 1.9%). At 128 cases a floor of 60% for ground is 7.7 sd
    // under its mean and 15% for façades 4.4 sd, so a false alarm at any seed
    // is under 1 in 100,000; the 5% cap on unchecked points is over twice the
    // largest share seen.
    const SCAN_CASES: u32 = 128;
    let (cases, sees_ground, sees_facade) = (Cell::new(0u32), Cell::new(0u32), Cell::new(0u32));
    let (called, unsure) = (Cell::new(0u64), Cell::new(0u64));
    let mut runner = TestRunner::new(config(SCAN_CASES));
    let result = runner.run(
        &(street_and_scan_camera(), 0.0..=2.0f32),
        |((s, cam), subject)| {
            let layers = s.layers();
            let terrain = layers.terrain();
            let eye = Vec3::new(cam.at.x, terrain.height_at(cam.at) + cam.above, cam.at.y);
            let cone = ViewCone::from_angles(
                eye, cam.yaw, cam.pitch, cam.roll, cam.hfov, cam.vfov, cam.range,
            );
            let pyramid = Pyramid::new(
                eye, cam.yaw, cam.pitch, cam.roll, cam.hfov, cam.vfov, cam.range,
            );
            let sight = layers.sight(&cone, subject);
            let e = d3(eye);
            // The verdict for `p`, given its margin inside the view.
            let judge = |p: D3, margin: f64| {
                if margin < -0.01 {
                    return Verdict::Unseen;
                }
                if margin <= 0.01 {
                    return Verdict::Unsure;
                }
                let d2 = dot(sub(p, e), sub(p, e));
                let mut v = Verdict::Seen(d2);
                for h in &s.houses {
                    match house_hit(h, e, p) {
                        Verdict::Unseen => return Verdict::Unseen,
                        Verdict::Unsure => v = Verdict::Unsure,
                        Verdict::Seen(_) => {}
                    }
                }
                if v != Verdict::Unsure && !ground_clear(terrain, e, p) {
                    v = Verdict::Unsure;
                }
                v
            };
            let (mut ground_seen, mut facade_seen) = (false, false);
            // Checks point `i` of a list, `v` being the oracle's verdict and
            // `margin` its margin inside the view.
            let check = |listed: &[u32], listed_d2: &[f32], i: usize, v: Verdict, margin: f64| {
                let at = listed.iter().position(|&j| j as usize == i);
                if margin >= -0.01 {
                    called.set(called.get() + u64::from(v != Verdict::Unsure));
                    unsure.set(unsure.get() + u64::from(v == Verdict::Unsure));
                }
                match (v, at) {
                    (Verdict::Seen(d2), Some(k)) => {
                        let got = f64::from(listed_d2[k]);
                        prop_assert!(
                            (got - d2).abs() <= 1e-5 * d2 + 1e-4,
                            "cell {i}: d2 {got}, want {d2}"
                        );
                        Ok(())
                    }
                    (Verdict::Seen(_), None) => Err(TestCaseError::fail(format!(
                        "cell {i} is seen but not listed"
                    ))),
                    (Verdict::Unseen, Some(_)) => Err(TestCaseError::fail(format!(
                        "cell {i} is listed but not seen"
                    ))),
                    _ => Ok(()),
                }
            };
            for i in 0..layers.grid.len() {
                if layers.building_mask[i] {
                    prop_assert!(
                        !sight.ground.contains(&(i as u32)),
                        "building cell {i} listed"
                    );
                    continue;
                }
                let c = layers.grid.center(i);
                let p = [
                    f64::from(c.x),
                    f64::from(terrain.height_at(c)) + f64::from(subject),
                    f64::from(c.y),
                ];
                let margin = pyramid.margin(p);
                let v = judge(p, margin);
                ground_seen |= matches!(v, Verdict::Seen(_));
                check(&sight.ground, &sight.ground_d2, i, v, margin)?;
            }
            for (i, f) in layers.facades.iter().enumerate() {
                let (centre, normal) = (d3(f.center), d3(f.normal));
                let to_eye = sub(e, centre);
                let facing = dot(to_eye, normal) / dot(to_eye, to_eye).sqrt();
                let p = [0, 1, 2].map(|k| centre[k] + normal[k] * f64::from(FACADE_OFFSET_M));
                let margin = pyramid
                    .margin(p)
                    .min(pyramid.range - dot(to_eye, to_eye).sqrt());
                let v = match judge(p, margin) {
                    _ if facing < -1e-3 => Verdict::Unseen,
                    Verdict::Seen(_) if facing < 1e-3 => Verdict::Unsure,
                    Verdict::Seen(d2) => Verdict::Seen(d2.max(dot(to_eye, to_eye))),
                    v => v,
                };
                facade_seen |= matches!(v, Verdict::Seen(_));
                check(&sight.facade, &sight.facade_d2, i, v, margin)?;
            }
            cases.set(cases.get() + 1);
            sees_ground.set(sees_ground.get() + u32::from(ground_seen));
            sees_facade.set(sees_facade.get() + u32::from(facade_seen));
            Ok(())
        },
    );
    if let Err(e) = result {
        panic!("{e}");
    }
    let (n, g) = (cases.get(), sees_ground.get());
    assert!(
        5 * g >= 3 * n,
        "only {g} of {n} cases see a ground cell the oracle is sure of"
    );
    let (c, u) = (called.get(), unsure.get());
    assert!(
        20 * u <= c + u,
        "{u} of {} points in or near the view went unchecked",
        c + u
    );
    let f = sees_facade.get();
    assert!(
        20 * f >= 3 * n,
        "only {f} of {n} cases see a facade the oracle is sure of"
    );
    eprintln!(
        "sight scan: {g}/{n} cases see ground, {f}/{n} a facade; {u} of {} points unchecked",
        c + u
    );
}

proptest! {
    #![proptest_config(config(24))]

    /// The street mask is every open cell whose centre lies within half the
    /// road's 6 m width of its centreline, measured here with plain f64
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
