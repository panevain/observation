//! Example cameras for a freshly loaded area: a handful of plausible
//! installations on house fronts, each facing the nearest street. The walls
//! they go on ([`wall_mounts`], [`wall_mounts_of`], [`WallMount`]) are
//! public for other callers that suggest where a camera could go.

use crate::building::Footprint;
use crate::bundle::AreaBundle;
use crate::camera::{CameraModel, CameraPlacement, yaw_pitch_from_forward};
use crate::occlusion::Occluders;
use crate::terrain::Terrain;
use crate::util::planar::{clean_ring, signed_area2};
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

/// Catalogue models used, in turn, for the example cameras on the demo street
/// (by the app, and by default in the MCP server): a mix of types and brands.
pub const EXAMPLE_MODELS: [&str; 10] = [
    "ring-wired-doorbell-pro",
    "ring-floodlight-cam-wired-plus",
    "google-nest-doorbell-battery",
    "reolink-rlc-810a",
    "arlo-pro-4",
    "eufy-eufycam-s3-pro",
    "ubiquiti-unifi-g5-dome",
    "wyze-cam-floodlight-pro",
    "tp-link-tapo-c320ws",
    "blink-outdoor-4",
];

/// A camera must sit at least this far below the roof of its building, metres.
const BELOW_ROOF_M: f32 = 0.5;

/// A wall of a ground-level building that a camera could be mounted on (see
/// [`wall_mounts`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallMount {
    /// The building's index in the bundle.
    pub building: usize,
    /// The wall's midpoint, world `[x, z]`.
    pub at: Vec2,
    /// The wall's outward normal, `[x, z]`, of unit length.
    pub normal: Vec2,
    /// The wall's length, metres.
    pub length_m: f32,
    /// The point of the street nearest the midpoint, world `[x, z]`, as
    /// [`AreaBundle::nearest_street`] chooses it; `None` in an area without
    /// streets.
    pub street: Option<Vec2>,
    /// That street's index in [`AreaBundle::roads`].
    pub road: Option<usize>,
    /// World `y` of the building's roof.
    pub roof_y: f32,
}

impl WallMount {
    /// The distance from the midpoint to [`Self::street`], metres (infinite
    /// without a street).
    pub fn street_distance_m(&self) -> f32 {
        self.street.map_or(f32::INFINITY, |s| s.distance(self.at))
    }

    /// Whether the wall faces its street: 2 to 30 m from it, with the street
    /// within about 45° of straight out from the wall. The example cameras
    /// go on such walls only.
    pub fn faces_street(&self) -> bool {
        let Some(street) = self.street else {
            return false;
        };
        let to_street = street - self.at;
        let dist = to_street.length();
        (STREET_DISTANCE_M.0..=STREET_DISTANCE_M.1).contains(&dist)
            && to_street.dot(self.normal) >= 0.7 * dist
    }

    /// Where a camera mounted `height_m` above the ground in front of the
    /// wall's midpoint sits, world `[x, y, z]`.
    pub fn eye(&self, terrain: &Terrain, height_m: f32) -> Vec3 {
        mount_point(terrain, self.at, self.normal, height_m)
    }

    /// Whether a camera at `eye` sits far enough below the roof.
    pub fn fits_under_roof(&self, eye: Vec3) -> bool {
        self.roof_y >= eye.y + BELOW_ROOF_M
    }

    /// Whether a camera at `eye` has a clear line, past `occluders` and over
    /// the terrain (whose [`Terrain::top`] is `terrain_top`), to a point 1 m
    /// above [`Self::street`]. False without a street.
    pub fn sees_street(
        &self,
        eye: Vec3,
        occluders: &Occluders,
        terrain: &Terrain,
        terrain_top: f32,
    ) -> bool {
        let Some(s) = self.street else {
            return false;
        };
        let street = Vec3::new(s.x, terrain.height_at(s) + 1.0, s.y);
        !occluders.segment_blocked(eye, street)
            && !terrain.segment_blocked(eye, street, terrain_top)
    }

    /// The compass bearing from the wall towards its street, degrees
    /// clockwise from north in `[0, 360)`; straight out from the wall
    /// without a street.
    pub fn street_yaw_deg(&self) -> f32 {
        let facing = self
            .street
            .map_or(self.normal, |s| (s - self.at).normalize_or(self.normal));
        yaw_pitch_from_forward(Vec3::new(facing.x, 0.0, facing.y)).map_or(0.0, |(y, _)| y)
    }
}

/// Every wall at least 3 m long of the buildings that stand on the ground
/// (a raised part has no wall to mount on at head height), in bundle order
/// and around each footprint, with the street nearest each.
pub fn wall_mounts(bundle: &AreaBundle) -> Vec<WallMount> {
    wall_mounts_of(bundle, |_, _| true)
}

/// [`wall_mounts`] of only the buildings `keep` accepts (given each
/// building's index and footprint), so a caller that wants one building or
/// a neighbourhood looks for streets from those walls alone.
pub fn wall_mounts_of(
    bundle: &AreaBundle,
    keep: impl Fn(usize, &Footprint) -> bool,
) -> Vec<WallMount> {
    let mut walls = Vec::new();
    for (building, fp) in bundle.footprints().enumerate() {
        if fp.min_height_m > 0.5 || !keep(building, fp) {
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
            let at = (a + b) * 0.5;
            let street = bundle.nearest_street(at.to_array());
            walls.push(WallMount {
                building,
                at,
                normal: Vec2::new(d.y, -d.x) * (sign / len),
                length_m: len,
                street: street.map(|s| Vec2::from_array(s.point)),
                road: street.map(|s| s.index),
                roof_y: fp.roof_y(),
            });
        }
    }
    walls
}

/// Up to `count` example placements on house fronts that face a street,
/// spread over the area and cycling through `models`. Ids are left at 0 and
/// labels empty, for [`crate::scene`] users to fill in.
pub fn example_cameras(
    bundle: &AreaBundle,
    models: &[&CameraModel],
    count: usize,
) -> Vec<CameraPlacement> {
    example_cameras_around(bundle, models, count, &[])
}

/// [`example_cameras`] for an area that already has cameras at `taken`
/// (world `[x, z]`): new placements keep the same distance from those as
/// from each other, so none lands where a camera already is.
pub fn example_cameras_around(
    bundle: &AreaBundle,
    models: &[&CameraModel],
    count: usize,
    taken: &[[f32; 2]],
) -> Vec<CameraPlacement> {
    if models.is_empty() || count == 0 {
        return Vec::new();
    }
    let occluders = Occluders::new(bundle.footprints());
    let terrain = &bundle.terrain;
    let top = terrain.top();
    let mut candidates: Vec<(f32, WallMount)> = wall_mounts(bundle)
        .into_iter()
        .filter(WallMount::faces_street)
        .map(|w| (w.street_distance_m(), w))
        .collect();
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
    let suits = |c: &WallMount, model: &CameraModel| {
        let eye = c.eye(terrain, model.kind.default_mount_height_m());
        c.fits_under_roof(eye) && c.sees_street(eye, &occluders, terrain, top)
    };
    let mut picked: Vec<(WallMount, &CameraModel)> = Vec::new();
    for spacing in [SPACING_M, SPACING_M / 2.0, 0.0] {
        for (_, c) in &candidates {
            if picked.len() == count {
                break;
            }
            let model = models[picked.len() % models.len()];
            let clear = spacing.max(1.0);
            if suits(c, model)
                && picked.iter().all(|(p, _)| p.at.distance(c.at) > clear)
                && taken
                    .iter()
                    .all(|&t| Vec2::from_array(t).distance(c.at) > clear)
            {
                picked.push((*c, model));
            }
        }
    }

    picked
        .iter()
        .map(|(c, model)| {
            let kind = model.kind;
            let eye = c.eye(terrain, kind.default_mount_height_m());
            let yaw = c.street_yaw_deg();
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
                group: None,
            }
        })
        .collect()
}

/// `height` above the ground in front of the wall at `at`.
fn mount_point(terrain: &Terrain, at: Vec2, normal: Vec2, height: f32) -> Vec3 {
    let p = at + normal * WALL_OFFSET_M;
    Vec3::new(p.x, terrain.height_at(p) + height, p.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::CameraKind;
    use crate::geo::{BBox, LatLon};
    use crate::osm::build::build_bundle;
    use crate::osm::features::{OsmBuilding, OsmRoad};

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
        street_on(Terrain::default())
    }

    /// [`street`] on the given ground.
    fn street_on(terrain: Terrain) -> AreaBundle {
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
                    address: None,
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
        build_bundle(bbox, &buildings, &roads, &[], terrain, "test", 0, None)
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
    fn keeps_clear_of_cameras_already_placed() {
        let b = street();
        let door = model("door", CameraKind::Doorbell);
        let first = example_cameras(&b, &[&door], 4);
        let taken: Vec<[f32; 2]> = first
            .iter()
            .map(|c| [c.position[0], c.position[2]])
            .collect();
        let more = example_cameras_around(&b, &[&door], 4, &taken);
        assert_eq!(more.len(), 4);
        for c in &more {
            for t in &taken {
                let d = Vec2::new(c.position[0], c.position[2]).distance(Vec2::from_array(*t));
                assert!(d > 10.0, "{d} m from a camera already placed: {c:?}");
            }
        }
        assert_eq!(example_cameras_around(&b, &[&door], 4, &[]), first);
    }

    #[test]
    fn cameras_mount_above_the_local_ground() {
        // Rising 1 m per 10 m southward: the houses south of the street sit
        // higher than those north of it.
        let terrain = Terrain::sample_from(
            Vec2::splat(-400.0),
            Vec2::splat(400.0),
            |p| Some(20.0 + p.y / 10.0),
            "",
        )
        .unwrap();
        let b = street_on(terrain.clone());
        let door = model("door", CameraKind::Doorbell);
        let cams = example_cameras(&b, &[&door], 10);
        assert_eq!(cams.len(), 10);
        for c in &cams {
            let ground = terrain.height_at(Vec2::new(c.position[0], c.position[2]));
            assert!((c.position[1] - ground - 1.2).abs() < 1e-3, "{c:?}");
        }
        assert!(
            cams.iter().any(|c| c.position[2] > 0.0) && cams.iter().any(|c| c.position[2] < 0.0)
        );
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
    fn wall_mounts_know_their_building_and_street() {
        let b = street();
        let walls = wall_mounts(&b);
        // Four walls of 12 m per house, all on the ground.
        assert_eq!(walls.len(), b.buildings.len() * 4);
        let fronts: Vec<&WallMount> = walls.iter().filter(|w| w.faces_street()).collect();
        assert_eq!(fronts.len(), b.buildings.len(), "one front per house");
        for w in &fronts {
            // The front is the wall nearest the street, 12 m from it, facing it.
            assert!((w.street_distance_m() - 12.0).abs() < 0.1, "{w:?}");
            assert!(w.normal.y * w.at.y < 0.0, "faces the street: {w:?}");
            let fp = &b.buildings[w.building].footprint;
            assert!(crate::address::distance_to_footprint(w.at.to_array(), fp) < 1e-3);
            let yaw = w.street_yaw_deg();
            let expected = if w.at.y > 0.0 { 0.0 } else { 180.0 };
            let off = (yaw - expected + 180.0).rem_euclid(360.0) - 180.0;
            assert!(off.abs() < 1e-3, "{yaw} for {w:?}");
            let eye = w.eye(&b.terrain, 2.6);
            assert!(w.fits_under_roof(eye));
            assert!(!w.fits_under_roof(w.eye(&b.terrain, 6.2)));
            let occ = Occluders::new(b.footprints());
            assert!(w.sees_street(eye, &occ, &b.terrain, b.terrain.top()));
        }
        assert!(fronts.iter().all(|w| w.road == Some(0)));
        // Only the walls of the buildings kept.
        let one = wall_mounts_of(&b, |i, _| i == 3);
        assert_eq!(one.len(), 4);
        assert!(one.iter().all(|w| w.building == 3));
        assert_eq!(
            one,
            walls
                .iter()
                .filter(|w| w.building == 3)
                .copied()
                .collect::<Vec<_>>()
        );
        // A back wall faces away from the street.
        assert!(
            walls
                .iter()
                .any(|w| !w.faces_street() && w.normal.y * w.at.y > 0.0)
        );
    }

    #[test]
    fn nothing_without_streets_or_models() {
        let mut b = street();
        assert!(example_cameras(&b, &[], 10).is_empty());
        b.roads.clear();
        let door = model("door", CameraKind::Doorbell);
        assert!(example_cameras(&b, &[&door], 10).is_empty());
        let walls = wall_mounts(&b);
        assert!(!walls.is_empty());
        assert!(
            walls
                .iter()
                .all(|w| w.street.is_none() && !w.faces_street())
        );
    }
}
