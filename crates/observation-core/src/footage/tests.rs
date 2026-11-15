//! Synthetic layouts for the split and the verdicts.

use super::*;
use crate::camera::{CameraKind, CameraModel, CameraPlacement, DoriLevel, Lighting};
use crate::coverage::{CoverageParams, GroundGrid, StaticLayers};
use crate::frustum::ViewCone;
use crate::road::Road;
use crate::terrain::Terrain;
use crate::testing::box_footprint;
use crate::util::planar::{dist_to_segment, point_in_ring};
use glam::Vec2;

/// A 3840 px wide, 90° camera: Detect 76.8 m, Observe 31.0 m, Recognise
/// 15.4 m, Identify 7.7 m by day; 7.5 m at night (no IR range given).
fn model() -> CameraModel {
    CameraModel {
        id: "test".into(),
        vendor: String::new(),
        name: "Test".into(),
        kind: CameraKind::Bullet,
        hfov_deg: 90.0,
        vfov_deg: 60.0,
        resolution: [3840, 2160],
        ir_range_m: None,
        source: None,
        notes: None,
    }
}

/// A camera 2.5 m up at `(x, z)`, looking along `yaw`, 10° down.
fn camera(id: u32, x: f32, z: f32, yaw: f32) -> CameraPlacement {
    CameraPlacement {
        id,
        label: format!("Camera {id}"),
        model_id: "test".into(),
        position: [x, 2.5, z],
        yaw_deg: yaw,
        pitch_deg: -10.0,
        roll_deg: 0.0,
        color: None,
        enabled: true,
        group: None,
    }
}

/// On `terrain`: a 10 × 10 m, 6 m tall house at x -5..5, z -25..-15
/// (building 0), a shed at x 30..34, z -4..-8 (building 1) and a 6 m road
/// along z = 0, on 1 m cells over ±50 m.
fn street_on(terrain: Terrain) -> StaticLayers {
    let house = box_footprint(-5.0, -25.0, 5.0, -15.0, 6.0);
    let shed = box_footprint(30.0, -8.0, 34.0, -4.0, 3.0);
    let road = Road {
        osm_id: 1,
        highway: "residential".into(),
        name: None,
        width_m: 6.0,
        points: vec![[-50.0, 0.0], [50.0, 0.0]],
    };
    let grid = GroundGrid::covering(Vec2::splat(-50.0), Vec2::splat(50.0), 1.0);
    StaticLayers::from_parts(grid, &[&house, &shed], &[road], 1.0, terrain)
}

fn street() -> StaticLayers {
    street_on(Terrain::default())
}

fn rect(x0: f32, z0: f32, x1: f32, z1: f32) -> Vec<[f32; 2]> {
    vec![[x0, z0], [x1, z0], [x1, z1], [x0, z1]]
}

fn ground_target(id: u32, points: Vec<[f32; 2]>) -> Target {
    Target {
        id,
        name: format!("Target {id}"),
        at: TargetPlace::Ground { points },
    }
}

fn verdict(v: &CameraVerdicts, level: DoriLevel, lighting: Lighting) -> Verdict {
    v.at(level, lighting)
        .expect("a verdict at every ladder level")
}

const NOT: Verdict = Verdict::NotRuledOut;

fn out(r: Reason) -> Verdict {
    Verdict::RuledOut(r)
}

#[test]
fn every_reason_on_a_small_street() {
    let l = street();
    let m = model();
    let params = CoverageParams::default();
    // Looking north up the street from 10 m south of the road, and the same
    // camera turned round.
    let (north, south) = (camera(1, 0.0, 10.0, 0.0), camera(2, 0.0, 10.0, 180.0));
    let cams = [
        FootageCamera {
            placement: &north,
            model: &m,
        },
        FootageCamera {
            placement: &south,
            model: &m,
        },
    ];
    let targets = [
        // On the road, about 10 m in front.
        ground_target(1, rect(-1.0, -1.0, 1.0, 1.0)),
        // North of the house, about 41 m away: hidden by it.
        ground_target(2, rect(-1.0, -32.0, 1.0, -30.0)),
        // The house's south wall faces the camera; its north wall does not.
        Target {
            id: 3,
            name: "Front door".into(),
            at: TargetPlace::Wall {
                point: [0.0, 2.0, -15.0],
                normal: [0.0, 1.0],
            },
        },
        Target {
            id: 4,
            name: "Back door".into(),
            at: TargetPlace::Wall {
                point: [0.0, 2.0, -25.0],
                normal: [0.0, -2.0],
            },
        },
    ];
    let v = target_verdicts(&l, &params, &cams, &targets);
    assert_eq!(v.len(), 4);
    let ids: Vec<u32> = v[0].cameras.iter().map(|c| c.camera_id).collect();
    assert_eq!(ids, [1, 2]);
    // Ladder × day and night: four levels without Custom.
    assert_eq!(v[0].cameras[0].verdicts.len(), 8);

    let road = &v[0].cameras[0];
    assert!((road.nearest_m - 9.0).abs() < 1.0, "{}", road.nearest_m);
    assert_eq!(verdict(road, DoriLevel::Detect, Lighting::Day), NOT);
    assert_eq!(verdict(road, DoriLevel::Recognise, Lighting::Day), NOT);
    assert_eq!(
        verdict(road, DoriLevel::Identify, Lighting::Day),
        out(Reason::OutOfRange)
    );
    assert_eq!(
        verdict(road, DoriLevel::Detect, Lighting::Night),
        out(Reason::OutOfRange)
    );
    let away = &v[0].cameras[1];
    assert!(
        away.verdicts
            .iter()
            .all(|l| l.verdict == out(Reason::OutsideView))
    );

    let behind = &v[1].cameras[0];
    assert_eq!(
        verdict(behind, DoriLevel::Detect, Lighting::Day),
        out(Reason::BlockedByBuilding)
    );
    // Beyond Observe's 31 m: out of range comes before blocked.
    assert_eq!(
        verdict(behind, DoriLevel::Observe, Lighting::Day),
        out(Reason::OutOfRange)
    );

    let front = &v[2].cameras[0];
    assert_eq!(verdict(front, DoriLevel::Detect, Lighting::Day), NOT);
    assert_eq!(verdict(front, DoriLevel::Observe, Lighting::Day), NOT);
    assert_eq!(
        verdict(front, DoriLevel::Recognise, Lighting::Day),
        out(Reason::OutOfRange)
    );
    let back = &v[3].cameras[0];
    assert_eq!(
        verdict(back, DoriLevel::Detect, Lighting::Day),
        out(Reason::BlockedByBuilding),
        "the far side of a wall"
    );
}

#[test]
fn hills_rule_out_by_terrain() {
    // A 6 m mound across z = -12 to -6, on otherwise level ground.
    let mound = Terrain::sample_from(
        Vec2::splat(-60.0),
        Vec2::splat(60.0),
        |p| {
            Some(if (-12.0..=-6.0).contains(&p.y) {
                6.0
            } else {
                0.0
            })
        },
        "",
    )
    .expect("a valid mound");
    let l = street_on(mound);
    let m = model();
    let c = camera(1, 20.0, 10.0, 0.0);
    let cams = [FootageCamera {
        placement: &c,
        model: &m,
    }];
    let params = CoverageParams::default();
    // Behind the mound and clear of the buildings.
    let t = ground_target(1, rect(19.0, -22.0, 21.0, -20.0));
    let v = target_verdicts(&l, &params, &cams, &[t]);
    assert_eq!(
        verdict(&v[0].cameras[0], DoriLevel::Detect, Lighting::Day),
        out(Reason::BlockedByTerrain)
    );
    // Flat, the same place is not ruled out.
    let flat = street();
    let t = ground_target(1, rect(19.0, -22.0, 21.0, -20.0));
    let v = target_verdicts(&flat, &params, &cams, &[t]);
    assert_eq!(
        verdict(&v[0].cameras[0], DoriLevel::Detect, Lighting::Day),
        NOT
    );
}

#[test]
fn ground_verdicts_agree_with_the_overlay() {
    // A ground target is not ruled out at a range exactly when the overlay
    // shows one of its cells seen within that range.
    let l = street();
    let m = model();
    let params = CoverageParams {
        dori: DoriLevel::Custom,
        custom_ppm: 40.0,
        ..CoverageParams::default()
    };
    let targets: Vec<Target> = [
        rect(-1.0, -1.0, 1.0, 1.0),
        rect(-3.0, -32.0, 3.0, -28.0),
        rect(6.0, -30.0, 9.0, -27.0),
        rect(28.0, -12.0, 36.0, -9.0),
        vec![[-20.0, -5.0], [-8.0, -14.0], [-14.0, 2.0]],
    ]
    .into_iter()
    .enumerate()
    .map(|(i, p)| ground_target(i as u32, p))
    .collect();
    for (x, z, yaw) in [(0.0, 10.0, 0.0), (15.0, 5.0, 300.0), (-30.0, -30.0, 110.0)] {
        let c = camera(1, x, z, yaw);
        let cams = [FootageCamera {
            placement: &c,
            model: &m,
        }];
        let sight = l.sight(
            &ViewCone::new(&c, &m, params.sight_range_m(&m)),
            params.subject_height_m,
        );
        let v = target_verdicts(&l, &params, &cams, &targets);
        for (t, tv) in targets.iter().zip(&v) {
            let TargetPlace::Ground { points } = &t.at else {
                unreachable!()
            };
            for lv in &tv.cameras[0].verdicts {
                let shown = sight
                    .ground_within(lv.range_m)
                    .any(|i| point_in_ring(l.grid().center(i as usize), points));
                assert_eq!(
                    lv.verdict == NOT,
                    shown,
                    "target {} from ({x}, {z}) at {:?} {:?}: {:?}",
                    t.id,
                    lv.level,
                    lv.lighting,
                    lv.verdict
                );
            }
        }
    }
}

#[test]
fn custom_levels_and_disabled_cameras() {
    let l = street();
    let m = model();
    let mut off = camera(1, 0.0, 10.0, 0.0);
    off.enabled = false;
    let on = camera(2, 0.0, 10.0, 0.0);
    let cams = [
        FootageCamera {
            placement: &off,
            model: &m,
        },
        FootageCamera {
            placement: &on,
            model: &m,
        },
    ];
    let params = CoverageParams {
        dori: DoriLevel::Custom,
        custom_ppm: 400.0,
        ..CoverageParams::default()
    };
    let v = point_verdicts(&l, &params, &cams, [0.0, 0.0]);
    assert_eq!(v.len(), 1, "the disabled camera is skipped");
    assert_eq!(v[0].camera_id, 2);
    assert_eq!(v[0].verdicts.len(), 10, "Custom is asked about too");
    // 400 px/m reaches 4.8 m, short of the road 10 m away.
    assert_eq!(
        verdict(&v[0], DoriLevel::Custom, Lighting::Day),
        out(Reason::OutOfRange)
    );
    assert_eq!(verdict(&v[0], DoriLevel::Detect, Lighting::Day), NOT);
}

#[test]
fn point_query() {
    let l = street();
    let m = model();
    let (north, south) = (camera(1, 0.0, 10.0, 0.0), camera(2, 0.0, 10.0, 180.0));
    let cams = [
        FootageCamera {
            placement: &north,
            model: &m,
        },
        FootageCamera {
            placement: &south,
            model: &m,
        },
    ];
    let params = CoverageParams::default();
    let at = |x: f32, z: f32| point_verdicts(&l, &params, &cams, [x, z]);
    let road = at(0.0, 0.0);
    assert_eq!(verdict(&road[0], DoriLevel::Detect, Lighting::Day), NOT);
    assert_eq!(
        verdict(&road[1], DoriLevel::Detect, Lighting::Day),
        out(Reason::OutsideView)
    );
    // Inside the house: hidden by it from wherever it is in view.
    let inside = at(0.0, -20.0);
    assert_eq!(
        verdict(&inside[0], DoriLevel::Detect, Lighting::Day),
        out(Reason::BlockedByBuilding)
    );
    assert_eq!(
        verdict(&inside[1], DoriLevel::Detect, Lighting::Day),
        out(Reason::OutsideView)
    );
    // The same classifier as a target of the same single point.
    let probe = Probe::point(&l, [0.0, -40.0], params.subject_height_m);
    assert_eq!(probe.len(), 1);
    let one = camera_verdicts(&l, &params, &cams[0], &probe);
    assert_eq!(one, at(0.0, -40.0)[0]);
    assert!(one.verdicts.iter().all(|v| v.verdict.is_ruled_out()));
    assert_eq!(
        Verdict::RuledOut(Reason::BlockedByTerrain).text(),
        "ruled out: blocked by the terrain"
    );
    assert_eq!(NOT.text(), "not ruled out");
}

#[test]
fn tiny_targets_still_have_points() {
    let l = street();
    // Between cell centres (which lie on the half metre).
    let t = ground_target(1, rect(0.1, 0.1, 0.4, 0.4));
    let p = Probe::target(&l, &t, 1.0);
    assert!(!p.is_empty() && p.len() <= 16, "{}", p.len());
    // A sliver with no lattice point inside still has its mean.
    let t = ground_target(1, vec![[0.1, 0.1], [0.11, 0.1], [0.11, 0.12]]);
    assert_eq!(Probe::target(&l, &t, 1.0).len(), 1);
}

fn split_cams<'a>(m: &'a CameraModel, cams: &'a [CameraPlacement]) -> Vec<FootageCamera<'a>> {
    cams.iter()
        .map(|placement| FootageCamera {
            placement,
            model: m,
        })
        .collect()
}

fn area(id: u32, label: Option<AreaLabel>, points: Vec<[f32; 2]>) -> LabelledArea {
    LabelledArea {
        id,
        label,
        name: None,
        points,
        enabled: true,
    }
}

/// How deep inside an outline cell centre `c` lies, by brute force: 3
/// beyond `off` inside, 2 inside, 1 within `off` outside, 0 beyond.
fn depth(c: Vec2, points: &[[f32; 2]], off: f32) -> u8 {
    let n = points.len();
    let edge = (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            dist_to_segment(c, Vec2::from_array(a), Vec2::from_array(b))
        })
        .fold(f32::INFINITY, f32::min);
    match (point_in_ring(c, points), edge <= off) {
        (true, false) => 3,
        (true, true) => 2,
        (false, true) => 1,
        (false, false) => 0,
    }
}

#[test]
fn split_counts_each_area_label_street_and_wall() {
    let l = street();
    let m = model();
    let cams = [camera(1, 0.0, 10.0, 0.0), camera(2, 40.0, 10.0, 300.0)];
    let areas = vec![
        // Our front garden and half the road.
        area(1, Some(AreaLabel::Own), rect(-10.0, -10.0, 10.0, 0.0)),
        // The neighbour's lot round the house.
        area(
            2,
            Some(AreaLabel::Neighbour),
            rect(-10.0, -30.0, 10.0, -10.0),
        ),
        // Another neighbour, concave.
        area(
            3,
            Some(AreaLabel::Neighbour),
            vec![
                [10.0, -30.0],
                [40.0, -30.0],
                [40.0, 0.0],
                [25.0, -15.0],
                [10.0, -15.0],
            ],
        ),
        // Unlabelled, overlapping the first.
        area(4, None, rect(-5.0, -5.0, 5.0, 5.0)),
        // Off: left out.
        LabelledArea {
            enabled: false,
            ..area(5, Some(AreaLabel::Public), rect(-50.0, -50.0, 50.0, 50.0))
        },
        // Unusable: left out.
        area(6, Some(AreaLabel::Other), vec![[0.0, 0.0], [1.0, 1.0]]),
    ];
    let params = CoverageParams::default();
    let masks = AreaMasks::new(&l, &areas, DEFAULT_OFFSET_M);
    assert_eq!(masks.area_ids().collect::<Vec<_>>(), [1, 2, 3, 4]);
    let splits = camera_splits(&l, &params, &split_cams(&m, &cams), &masks);
    assert_eq!(splits.len(), 2);
    for (c, cs) in cams.iter().zip(&splits) {
        assert_eq!(cs.camera_id, c.id);
        let range = m.effective_range_m(25.0, Lighting::Day);
        assert_eq!(cs.range_m, range);
        let seen = l.visibility(&ViewCone::new(c, &m, range), 1.0);
        let s = &cs.split;
        assert_eq!(s.ground_m2, seen.ground.len() as f32);
        let street = seen
            .ground
            .iter()
            .filter(|&&i| l.street_mask()[i as usize])
            .count();
        assert_eq!(s.street_m2, street as f32);
        // Every area figure, recounted by brute force.
        let count = |points: &[[f32; 2]], min: u8| {
            seen.ground
                .iter()
                .filter(|&&i| depth(l.grid().center(i as usize), points, 1.0) >= min)
                .count() as f32
        };
        let active: Vec<&LabelledArea> = areas.iter().filter(|a| a.is_active()).collect();
        assert_eq!(s.areas.len(), active.len());
        for (a, got) in active.iter().zip(&s.areas) {
            assert_eq!((got.area_id, got.label), (a.id, a.label));
            assert_eq!(got.polygon_m2, a.area_m2());
            let want = SeenRange {
                low_m2: count(&a.points, 3),
                as_drawn_m2: count(&a.points, 2),
                high_m2: count(&a.points, 1),
            };
            assert_eq!(got.seen, want, "area {}", a.id);
        }
        // Labels: each cell once, at its deepest in any area of the label.
        let labels: Vec<AreaLabel> = s.labels.iter().map(|l| l.label).collect();
        assert_eq!(labels, [AreaLabel::Own, AreaLabel::Neighbour]);
        let deepest = |i: u32, label: Option<AreaLabel>| {
            active
                .iter()
                .filter(|a| a.label.is_some() && (label.is_none() || a.label == label))
                .map(|a| depth(l.grid().center(i as usize), &a.points, 1.0))
                .max()
                .unwrap_or(0)
        };
        for ls in &s.labels {
            let n = |min| {
                seen.ground
                    .iter()
                    .filter(|&&i| deepest(i, Some(ls.label)) >= min)
                    .count() as f32
            };
            assert_eq!(
                ls.seen,
                SeenRange {
                    low_m2: n(3),
                    as_drawn_m2: n(2),
                    high_m2: n(1)
                }
            );
        }
        let off_street = |max| {
            seen.ground
                .iter()
                .filter(|&&i| !l.street_mask()[i as usize] && deepest(i, None) < max)
                .count() as f32
        };
        assert_eq!(
            s.unlabelled,
            SeenRange {
                low_m2: off_street(1),
                as_drawn_m2: off_street(2),
                high_m2: off_street(3)
            }
        );
        for r in s
            .areas
            .iter()
            .map(|a| a.seen)
            .chain(s.labels.iter().map(|l| l.seen))
            .chain([s.unlabelled])
        {
            assert!(
                r.low_m2 <= r.as_drawn_m2 && r.as_drawn_m2 <= r.high_m2,
                "{r:?}"
            );
        }
        // Walls by building, adding up to all the wall seen.
        let total: f32 = seen
            .facade
            .iter()
            .map(|&i| l.facades()[i as usize].area_m2())
            .sum();
        assert!((s.walls_m2 - total).abs() < 1e-3);
        let per: f32 = s.walls.iter().map(|w| w.seen_m2).sum();
        assert!((per - s.walls_m2).abs() < 1e-3);
    }
    // The first camera sees the house's south wall, 25 m off.
    let w = &splits[0].split.walls;
    assert_eq!(w[0].building, 0);
    assert!((w[0].distance_m - 25.0).abs() < 0.6, "{}", w[0].distance_m);
    assert!(splits[0].split.areas[0].seen.as_drawn_m2 > 50.0);
    // The second, looking west-north-west from x = 40, sees the shed too.
    let b: Vec<u32> = splits[1].split.walls.iter().map(|w| w.building).collect();
    assert!(b.contains(&1), "{b:?}");
}

#[test]
fn a_generous_outline_shows_in_the_range() {
    // An area traced along the road's north kerb, z = -3: as drawn it holds
    // no road, moved out by a metre it takes a strip of it, and the polygon
    // area says how big the outline is.
    let l = street();
    let m = model();
    let cams = [camera(1, 0.0, 10.0, 0.0)];
    let kerb = vec![area(
        1,
        Some(AreaLabel::Neighbour),
        rect(-10.0, -13.0, 10.0, -3.0),
    )];
    let masks = AreaMasks::new(&l, &kerb, 1.0);
    let s = &camera_splits(
        &l,
        &CoverageParams::default(),
        &split_cams(&m, &cams),
        &masks,
    )[0];
    let r = s.split.areas[0].seen;
    assert_eq!(s.split.areas[0].polygon_m2, 200.0);
    assert!(
        r.high_m2 > r.as_drawn_m2 && r.as_drawn_m2 > r.low_m2,
        "{r:?}"
    );
    // With no offset the three readings agree.
    let flat = AreaMasks::new(&l, &kerb, 0.0);
    let r0 = camera_splits(
        &l,
        &CoverageParams::default(),
        &split_cams(&m, &cams),
        &flat,
    )[0]
    .split
    .areas[0]
        .seen;
    assert_eq!((r0.low_m2, r0.high_m2), (r0.as_drawn_m2, r0.as_drawn_m2));
    assert_eq!(r0.as_drawn_m2, r.as_drawn_m2);
    // A bad offset counts as none.
    assert_eq!(AreaMasks::new(&l, &kerb, f32::NAN).offset_m(), 0.0);
}

#[test]
fn results_are_deterministic() {
    let l = street();
    let m = model();
    let cams = [camera(1, 0.0, 10.0, 0.0), camera(2, 40.0, 10.0, 300.0)];
    let areas = vec![
        area(1, Some(AreaLabel::Own), rect(-10.0, -10.0, 10.0, 0.0)),
        area(2, None, rect(20.0, -20.0, 45.0, 5.0)),
    ];
    let targets = [
        ground_target(1, rect(-1.0, -1.0, 1.0, 1.0)),
        ground_target(2, rect(30.0, -12.0, 36.0, -9.0)),
    ];
    let params = CoverageParams::default();
    let run = || {
        let masks = AreaMasks::new(&l, &areas, DEFAULT_OFFSET_M);
        let fc = split_cams(&m, &cams);
        let splits = camera_splits(&l, &params, &fc, &masks);
        let verdicts = target_verdicts(&l, &params, &fc, &targets);
        let point = point_verdicts(&l, &params, &fc, [12.0, -2.0]);
        serde_json::to_string(&(splits, verdicts, point)).unwrap_or_default()
    };
    let first = run();
    assert!(first.len() > 100);
    assert_eq!(first, run());
    // Rebuilt layers give the same answers too.
    let again = street();
    let masks = AreaMasks::new(&again, &areas, DEFAULT_OFFSET_M);
    let fc = split_cams(&m, &cams);
    assert_eq!(
        camera_splits(&again, &params, &fc, &masks),
        camera_splits(
            &l,
            &params,
            &fc,
            &AreaMasks::new(&l, &areas, DEFAULT_OFFSET_M)
        )
    );
}

#[test]
fn a_camera_splits_the_unseen_street_in_two() {
    let l = street();
    let m = model();
    let params = CoverageParams::default();
    let street_m2: f32 = l.street_mask().iter().filter(|s| **s).count() as f32;
    // No cameras: the whole road is one gap.
    let none = street_gaps(&l, &params, &[], None);
    assert_eq!(none.len(), 1);
    assert_eq!(none[0].area_m2, street_m2);
    assert!(none[0].centroid[0].abs() < 0.5 && none[0].centroid[1].abs() < 0.5);
    assert_eq!((none[0].min[0], none[0].max[0]), (-50.0, 50.0));

    // One camera looking north across the road from 10 m south of it sees a
    // stretch in the middle, leaving a gap on each side.
    let cams = [camera(1, 0.0, 10.0, 0.0)];
    let fc = split_cams(&m, &cams);
    let gaps = street_gaps(&l, &params, &fc, None);
    let seen = seen_ground(&l, &params, &fc);
    let seen_street = l
        .street_mask()
        .iter()
        .zip(&seen)
        .filter(|(s, v)| **s && **v)
        .count() as f32;
    assert!(seen_street > 0.0);
    let big: Vec<&Gap> = gaps.iter().filter(|g| g.area_m2 > 50.0).collect();
    assert_eq!(big.len(), 2, "{gaps:?}");
    assert!(big.iter().any(|g| g.centroid[0] < -10.0));
    assert!(big.iter().any(|g| g.centroid[0] > 10.0));
    let unseen: f32 = gaps.iter().map(|g| g.area_m2).sum();
    assert_eq!(unseen + seen_street, street_m2);
    assert!(gaps.windows(2).all(|w| w[0].area_m2 >= w[1].area_m2));
    // Every gap cell is street the camera does not see.
    for g in &gaps {
        for &i in &g.cells {
            assert!(l.street_mask()[i as usize] && !seen[i as usize]);
        }
    }
    // The camera is in neither gap; its nearest cell of each is on the road.
    for g in &big {
        let (d, at) = g.nearest_to(l.grid(), [0.0, 10.0]);
        assert!(d > 7.0 && at[1].abs() < 3.0, "{d} {at:?}");
    }
    // A disabled camera sees nothing.
    let mut off = cams.clone();
    off[0].enabled = false;
    let off = split_cams(&m, &off);
    assert_eq!(street_gaps(&l, &params, &off, None), none);
    // Night shortens the range, so the gaps grow.
    let night = CoverageParams {
        lighting: Lighting::Night,
        ..params
    };
    let dark: f32 = street_gaps(&l, &night, &fc, None)
        .iter()
        .map(|g| g.area_m2)
        .sum();
    assert!(dark > unseen, "{dark} > {unseen}");
    // Limited to a region, only the street inside it counts.
    let west = crate::coverage::RegionMask::new(&l, &rect(-50.0, -10.0, -20.0, 10.0));
    let inside = street_gaps(&l, &params, &fc, Some(&west));
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].area_m2, 30.0 * 6.0);
    // The same inputs give the same gaps.
    assert_eq!(street_gaps(&l, &params, &fc, None), gaps);
}

#[test]
fn fresh_ids_stay_in_range_and_never_collide() {
    assert_eq!(fresh_id([]), 1);
    assert_eq!(fresh_id([4, 2]), 5, "one above the highest");
    assert_eq!(fresh_id([MAX_ITEM_ID - 1]), MAX_ITEM_ID);
    // Past the largest id, the lowest free one, so that ids handed out one
    // after another never repeat (they used to stick at `u32::MAX`).
    assert_eq!(fresh_id([MAX_ITEM_ID, 1, 3]), 2);
    let mut ids = vec![u32::MAX, MAX_ITEM_ID];
    for _ in 0..MAX_TARGETS {
        let id = fresh_id(ids.iter().copied());
        assert!(
            (1..=MAX_ITEM_ID).contains(&id) && !ids.contains(&id),
            "{id}"
        );
        ids.push(id);
    }
}
