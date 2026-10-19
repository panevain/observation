//! Example cameras for a freshly loaded area: a handful of plausible
//! installations on house fronts, each facing the nearest street.

use crate::building::{clean_ring, signed_area2};
use crate::bundle::{AreaBundle, FOOT_WAYS};
use crate::camera::{CameraModel, CameraPlacement, yaw_pitch_from_forward};
use crate::coverage::closest_on_segment;
use crate::occlusion::Occluders;
use glam::{Vec2, Vec3};

/// Cameras sit this far in front of the wall they are mounted on, metres.
const WALL_OFFSET_M: f32 = 0.15;
/// Walls shorter than this are not used, metres.
const MIN_WALL_M: f32 = 3.0;
/// A wall must be at least this far from the street, and at most the far
/// value, to get a camera, metres.
const STREET_DISTANCE_M: (f32, f32) = (2.0, 30.0);
/// Preferred spacing between example cameras, metres. Halved if the area
/// is too small to fit them all.
const SPACING_M: f32 = 30.0;

/// A camera must sit at least this far below the roof of its building, metres.
const BELOW_ROOF_M: f32 = 0.5;

/// A wall facing a street.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    /// Wall midpoint, `[x, z]`.
    at: Vec2,
    /// Outward wall normal, `[x, z]`.
    normal: Vec2,
    /// Closest point on a street, `[x, z]`.
    street: Vec2,
    /// Roof height of the building, metres.
    roof_m: f32,
}

/// Up to `count` example placements on house fronts that face a street,
/// spread over the area and cycling through `models`. Ids are left at 0 and
/// labels empty, for [`crate::scene`] users to fill in.
pub fn example_cameras(
    bundle: &AreaBundle,
    models: &[&CameraModel],
    count: usize,
) -> Vec<CameraPlacement> {
    if models.is_empty() || count == 0 {
        return Vec::new();
    }
    let footprints: Vec<_> = bundle.footprints().collect();
    let occluders = Occluders::new(footprints.iter().copied());
    // Houses face proper streets; fall back to driveways and service roads
    // only where there are none.
    let segments = |keep: &dyn Fn(&str) -> bool| -> Vec<(Vec2, Vec2)> {
        bundle
            .roads
            .iter()
            .filter(|r| keep(r.highway.as_str()))
            .flat_map(|r| {
                r.points
                    .windows(2)
                    .map(|w| (Vec2::from_array(w[0]), Vec2::from_array(w[1])))
            })
            .collect()
    };
    let mut streets = segments(&|h| !FOOT_WAYS.contains(&h) && h != "service");
    if streets.is_empty() {
        streets = segments(&|h| !FOOT_WAYS.contains(&h));
    }
    if streets.is_empty() {
        return Vec::new();
    }

    let mut candidates: Vec<(f32, Candidate)> = Vec::new();
    for fp in &footprints {
        // Only ground-level buildings: a raised part has no front door.
        if fp.min_height_m > 0.5 {
            continue;
        }
        let ring = clean_ring(&fp.outer);
        if ring.len() < 3 {
            continue;
        }
        let sign = signed_area2(&ring).signum();
        for i in 0..ring.len() {
            let (a, b) = (
                Vec2::from_array(ring[i]),
                Vec2::from_array(ring[(i + 1) % ring.len()]),
            );
            let d = b - a;
            let len = d.length();
            if !len.is_finite() || len < MIN_WALL_M {
                continue;
            }
            let normal = Vec2::new(d.y, -d.x) * (sign / len);
            let at = (a + b) * 0.5;
            let street = streets
                .iter()
                .map(|&(p, q)| closest_on_segment(at, p, q))
                .min_by(|p, q| p.distance_squared(at).total_cmp(&q.distance_squared(at)))
                .unwrap_or(at);
            let to_street = street - at;
            let dist = to_street.length();
            if dist < STREET_DISTANCE_M.0
                || dist > STREET_DISTANCE_M.1
                || to_street.dot(normal) < 0.7 * dist
            {
                continue;
            }
            candidates.push((
                dist,
                Candidate {
                    at,
                    normal,
                    street,
                    roof_m: fp.height_m,
                },
            ));
        }
    }
    // Closest to the street first; ties broken by position so the result is
    // deterministic for a given bundle.
    candidates.sort_by(|(da, a), (db, b)| {
        da.total_cmp(db)
            .then(a.at.x.total_cmp(&b.at.x))
            .then(a.at.y.total_cmp(&b.at.y))
    });

    // Each slot cycles through `models`; a wall suits the slot's model if the
    // camera fits under the roof and sees the street point it faces from its
    // own mount height.
    let suits = |c: &Candidate, model: &CameraModel| {
        let h = model.kind.default_mount_height_m();
        let eye = mount_point(c.at, c.normal, h);
        c.roof_m >= h + BELOW_ROOF_M
            && !occluders.segment_blocked(eye, Vec3::new(c.street.x, 1.0, c.street.y))
    };
    let mut picked: Vec<(Candidate, &CameraModel)> = Vec::new();
    for spacing in [SPACING_M, SPACING_M / 2.0, 0.0] {
        for (_, c) in &candidates {
            if picked.len() == count {
                break;
            }
            let model = models[picked.len() % models.len()];
            if suits(c, model)
                && picked
                    .iter()
                    .all(|(p, _)| p.at.distance(c.at) > spacing.max(1.0))
            {
                picked.push((*c, model));
            }
        }
    }

    picked
        .iter()
        .map(|(c, model)| {
            let kind = model.kind;
            let eye = mount_point(c.at, c.normal, kind.default_mount_height_m());
            let facing = (c.street - c.at).normalize_or(c.normal);
            let yaw =
                yaw_pitch_from_forward(Vec3::new(facing.x, 0.0, facing.y)).map_or(0.0, |(y, _)| y);
            CameraPlacement {
                id: 0,
                label: String::new(),
                model_id: model.id.clone(),
                position: eye.to_array(),
                yaw_deg: yaw,
                pitch_deg: kind.default_pitch_deg(),
                roll_deg: 0.0,
                color: None,
                enabled: true,
            }
        })
        .collect()
}

fn mount_point(at: Vec2, normal: Vec2, height: f32) -> Vec3 {
    let p = at + normal * WALL_OFFSET_M;
    Vec3::new(p.x, height, p.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::{OsmBuilding, OsmRoad, build_bundle};
    use crate::camera::CameraKind;
    use crate::geo::{BBox, LatLon};

    fn model(id: &str, kind: CameraKind) -> CameraModel {
        CameraModel {
            id: id.into(),
            vendor: String::new(),
            name: id.into(),
            kind,
            hfov_deg: 100.0,
            vfov_deg: 60.0,
            resolution: [1920, 1080],
            ir_range_m: None,
            source: None,
            notes: None,
        }
    }

    /// A street running east-west through the middle with a row of houses
    /// on each side.
    fn street() -> AreaBundle {
        let bbox = BBox::around(LatLon::new(47.7, -122.2), 300.0);
        let o = bbox.center();
        let m_lat = 1.0 / 111_195.0;
        let m_lon = m_lat / o.lat.to_radians().cos();
        let at = |x: f64, y: f64| LatLon::new(o.lat + y * m_lat, o.lon + x * m_lon);
        let mut buildings = Vec::new();
        for i in 0..12 {
            for side in [-1.0, 1.0] {
                let x0 = -130.0 + f64::from(i) * 22.0;
                let y0 = side * 12.0;
                let y1 = side * 24.0;
                buildings.push(OsmBuilding {
                    osm_id: i64::from(i) * 2 + i64::from(side > 0.0),
                    outer: vec![
                        at(x0, y0),
                        at(x0 + 12.0, y0),
                        at(x0 + 12.0, y1),
                        at(x0, y1),
                        at(x0, y0),
                    ],
                    holes: vec![],
                    min_height_m: 0.0,
                    height_m: 6.5,
                });
            }
        }
        let roads = vec![OsmRoad {
            osm_id: 1,
            highway: "residential".into(),
            name: None,
            width_m: 6.0,
            points: vec![at(-150.0, 0.0), at(150.0, 0.0)],
        }];
        build_bundle(bbox, &buildings, &roads, "test", 0)
    }

    #[test]
    fn places_spread_out_cameras_facing_the_street() {
        let b = street();
        let (door, flood) = (
            model("door", CameraKind::Doorbell),
            model("flood", CameraKind::Floodlight),
        );
        let cams = example_cameras(&b, &[&door, &flood], 10);
        assert_eq!(cams.len(), 10);
        assert_eq!(cams[0].model_id, "door");
        assert_eq!(cams[1].model_id, "flood");
        assert_eq!(cams[0].position[1], 1.2);
        assert_eq!(cams[1].position[1], 2.7);
        for c in &cams {
            // Houses are 12-24 m either side of z = 0; cameras sit on the
            // street-facing walls and look across the street.
            let z = c.position[2];
            assert!((11.0..12.5).contains(&z.abs()), "{z}");
            let f = c.forward();
            assert!(f.z * z < 0.0, "faces the street: {c:?}");
        }
        for (i, a) in cams.iter().enumerate() {
            for b in &cams[i + 1..] {
                let d = Vec3::from_array(a.position).distance(Vec3::from_array(b.position));
                assert!(d > 10.0, "{d}");
            }
        }
        // Deterministic.
        assert_eq!(cams, example_cameras(&b, &[&door, &flood], 10));
    }

    #[test]
    fn cameras_stay_under_the_roof() {
        let mut b = street();
        // Every house becomes a 2.4 m shed: too low for a floodlight at 2.7 m.
        for building in &mut b.buildings {
            building.footprint.height_m = 2.4;
        }
        let flood = model("flood", CameraKind::Floodlight);
        assert!(example_cameras(&b, &[&flood], 10).is_empty());
        let door = model("door", CameraKind::Doorbell);
        assert_eq!(example_cameras(&b, &[&door], 10).len(), 10);
    }

    #[test]
    fn nothing_without_streets_or_models() {
        let mut b = street();
        assert!(example_cameras(&b, &[], 10).is_empty());
        b.roads.clear();
        let door = model("door", CameraKind::Doorbell);
        assert!(example_cameras(&b, &[&door], 10).is_empty());
    }
}
