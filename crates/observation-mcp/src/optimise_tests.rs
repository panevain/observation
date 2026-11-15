//! The goal tools on the demo street.

use super::*;
use crate::area::demo;
use crate::edit::add_example_cameras;
use crate::places::{AreaEdit, LabelArg, PlaceArg, add_target, label_area};
use crate::queries::{gaps, verdicts_on_targets};
use std::time::Instant;

fn street() -> GoalSpec {
    GoalSpec::default()
}

fn goal(goal: GoalArg) -> GoalSpec {
    GoalSpec {
        goal,
        ..GoalSpec::default()
    }
}

fn value(s: &SceneFile) -> Value {
    edit::finish(s).unwrap().1
}

fn empty() -> SceneFile {
    edit::new_demo_scene().unwrap()
}

/// A scene with `n` example cameras.
fn with_examples(n: usize) -> SceneFile {
    let mut s = empty();
    add_example_cameras(&mut s, catalog().unwrap(), n, &[]).unwrap();
    s
}

fn rect(c: [f32; 2], half: f32) -> Vec<[f32; 2]> {
    let [x, z] = c;
    vec![
        [x - half, z - half],
        [x + half, z - half],
        [x + half, z + half],
        [x - half, z + half],
    ]
}

/// The point inside the largest gap in `s`'s street: a spot on the street.
fn on_street(s: &SceneFile) -> [f32; 2] {
    let [x, z] = gaps(&value(s), None, None, Some(1), None).unwrap().gaps[0].inside;
    [x as f32, z as f32]
}

fn with_target(mut s: SceneFile, name: &str, at: [f32; 2]) -> (SceneFile, u32) {
    let (id, _) = add_target(
        &mut s,
        name,
        PlaceArg::Ground {
            points: rect(at, 1.0),
        },
    )
    .unwrap();
    (s, id)
}

/// Whether target `id` is not ruled out by some camera at the scene's level
/// and lighting.
fn not_ruled_out(scene: &Value, id: u32) -> bool {
    let v = verdicts_on_targets(scene, Some(id)).unwrap();
    v.targets[0]
        .cameras
        .iter()
        .any(|c| c.selected == "not ruled out")
}

/// Every address on the demo street, as the app would print it.
fn addresses() -> Vec<String> {
    let b = demo().unwrap();
    b.buildings
        .iter()
        .filter_map(|x| x.address.as_ref())
        .chain(b.addresses.iter().map(|a| &a.address))
        .map(|a| a.label())
        .filter(|l| l.len() > 4)
        .collect()
}

fn no_addresses(out: &impl Serialize) {
    let mut text = serde_json::to_string(out).unwrap();
    // The scene itself may hold whatever the user typed; check the rest.
    if let Some(i) = text.find("\"scene\":") {
        text.truncate(i);
    }
    for a in addresses() {
        assert!(!text.contains(&a), "{a} in {text}");
    }
}

#[test]
fn compass_words() {
    assert_eq!(compass(0.0), "north");
    assert_eq!(compass(44.0), "north-east");
    assert_eq!(compass(135.0), "south-east");
    assert_eq!(compass(200.0), "south");
    assert_eq!(compass(-90.0), "west");
    assert_eq!(compass(337.6), "north");
    assert_eq!(angle_between(10.0, 350.0), 20.0);
    assert_eq!(angle_between(350.0, 10.0), 20.0);
}

#[test]
fn street_suggestions_add_street_best_first() {
    let s = value(&empty());
    let out = suggest(&s, &street(), Some(3), None, None, None, false).unwrap();
    assert_eq!(out.unit, "m²");
    assert_eq!(out.before, 0.0);
    assert_eq!(out.suggestions.len(), 3, "{}", out.summary);
    let gains: Vec<f32> = out.suggestions.iter().map(|g| g.gain).collect();
    assert!(gains.iter().all(|g| *g > 0.0), "{gains:?}");
    // Greedy: each pick adds no more than the one before it (to rounding,
    // since the fine search runs on the winner only).
    assert!(
        gains[0] >= gains[1] && gains[1] >= gains[2] * 0.9,
        "{gains:?}"
    );
    let last = out.suggestions.last().unwrap();
    assert_eq!(last.total_after, out.after);
    assert!((out.after - gains.iter().sum::<f32>()).abs() < 0.5);
    assert!(out.after <= out.out_of);
    for g in &out.suggestions {
        assert!((-180.0..=360.0).contains(&g.aim.yaw_deg));
        assert_eq!(g.aim.facing, compass(g.aim.yaw_deg));
        assert!(g.reason.contains("building"), "{}", g.reason);
        assert!(g.camera_id.is_none());
    }
    // Apart from each other.
    for (i, a) in out.suggestions.iter().enumerate() {
        for b in &out.suggestions[i + 1..] {
            let d = Vec2::from_array(a.at).distance(Vec2::from_array(b.at));
            assert!(d > APART_M, "{d}");
        }
    }
    assert!(out.summary.contains("not ruled out"), "{}", out.summary);
    assert!(out.scene.is_none() && out.camera_ids.is_none());
    no_addresses(&out);
    // Deterministic.
    let again = suggest(&s, &street(), Some(3), None, None, None, false).unwrap();
    assert_eq!(
        serde_json::to_value(&out).unwrap(),
        serde_json::to_value(&again).unwrap()
    );
}

#[test]
fn applied_suggestions_are_the_cameras_suggested() {
    let s = value(&with_examples(1));
    let out = suggest(&s, &street(), Some(2), None, None, None, true).unwrap();
    assert!(out.before > 0.0);
    assert_eq!(out.camera_ids, Some(vec![2, 3]));
    let scene = edit::parse(out.scene.as_ref().unwrap()).unwrap();
    assert_eq!(scene.cameras.len(), 3);
    for (g, c) in out.suggestions.iter().zip(&scene.cameras[1..]) {
        assert_eq!(g.camera_id, Some(c.id));
        assert_eq!(g.model_id, c.model_id);
        assert_eq!([c.position[0], c.position[2]], g.at);
        assert!((c.yaw_deg - g.aim.yaw_deg).abs() < 0.06);
    }
    // The street not ruled out with them, as find_gaps counts it.
    let before = gaps(&s, None, None, None, None).unwrap();
    let after = gaps(out.scene.as_ref().unwrap(), None, None, None, None).unwrap();
    let gained = before.unseen_street_m2 - after.unseen_street_m2;
    assert!(
        (gained - f64::from(out.after - out.before)).abs() < 0.5,
        "{gained}"
    );
    assert!(out.summary.contains("Added as camera(s) 2, 3"));
}

#[test]
fn a_target_far_from_every_camera_gets_a_camera() {
    let base = with_examples(2);
    let spot = on_street(&base);
    let (s, id) = with_target(base, "Gate", spot);
    let s = value(&s);
    assert!(!not_ruled_out(&s, id));
    let spec = GoalSpec {
        goal: GoalArg::Targets,
        targets: Some(vec![TargetRef::Name("gate".into())]),
        area_id: None,
    };
    let out = suggest(&s, &spec, Some(2), None, None, None, true).unwrap();
    assert_eq!(out.unit, "targets");
    assert_eq!((out.before, out.after, out.out_of), (0.0, 1.0, 1.0));
    // One camera is enough; a second would add no target.
    assert_eq!(out.suggestions.len(), 1, "{}", out.summary);
    assert_eq!(out.suggestions[0].gain, 1.0);
    assert!(not_ruled_out(out.scene.as_ref().unwrap(), id));
    // By id, and as a string id, too.
    for r in [TargetRef::Id(id), TargetRef::Name(id.to_string())] {
        let spec = GoalSpec {
            targets: Some(vec![r]),
            ..spec.clone()
        };
        let again = suggest(&s, &spec, None, None, None, None, false).unwrap();
        assert_eq!(again.suggestions[0].at, out.suggestions[0].at);
    }
    let unknown = GoalSpec {
        targets: Some(vec![TargetRef::Name("shed".into())]),
        ..spec
    };
    let err = suggest(&s, &unknown, None, None, None, None, false)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("\"Gate\""), "{err}");
}

#[test]
fn area_and_gap_goals_count_their_own_ground() {
    let base = with_examples(2);
    let spot = on_street(&base);
    let mut s = base;
    let area = label_area(
        &mut s,
        AreaEdit {
            points: Some(rect(spot, 12.0)),
            label: Some(LabelArg::Public),
            name: Some("Verge".into()),
            ..AreaEdit::default()
        },
    )
    .unwrap();
    let v = value(&s);
    let out = suggest(&v, &goal(GoalArg::Area), None, None, None, None, false).unwrap();
    assert!(out.goal.contains("Verge"), "{}", out.goal);
    assert!(out.out_of > 0.0 && out.out_of <= 24.0 * 24.0);
    assert!(out.suggestions[0].gain > 0.0, "{}", out.summary);
    assert!(out.after <= out.out_of);
    // With two areas, the area must be named.
    label_area(
        &mut s,
        AreaEdit {
            points: Some(rect([spot[0] + 30.0, spot[1]], 5.0)),
            ..AreaEdit::default()
        },
    )
    .unwrap();
    let v = value(&s);
    let err = suggest(&v, &goal(GoalArg::Area), None, None, None, None, false)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("area_id"), "{err}");
    let named = GoalSpec {
        goal: GoalArg::Area,
        area_id: Some(area),
        targets: None,
    };
    assert!(suggest(&v, &named, None, None, None, None, false).is_ok());

    let out = suggest(
        &v,
        &goal(GoalArg::Gaps),
        Some(2),
        None,
        Some(true),
        None,
        false,
    )
    .unwrap();
    assert_eq!(out.settings.lighting, "Night");
    assert_eq!(out.before, 0.0, "gaps are unseen to begin with");
    assert!(out.suggestions[0].gain > 0.0, "{}", out.summary);
    assert!(out.goal.contains("largest gaps"), "{}", out.goal);
}

#[test]
fn suggestions_refuse_bad_counts_and_models() {
    let s = value(&empty());
    assert!(suggest(&s, &street(), Some(0), None, None, None, false).is_err());
    assert!(suggest(&s, &street(), Some(5), None, None, None, false).is_err());
    let bad = vec!["no-such-model".to_owned()];
    assert!(suggest(&s, &street(), None, Some(&bad), None, None, false).is_err());
    let one = vec!["reolink-rlc-810a".to_owned()];
    let out = suggest(
        &s,
        &street(),
        None,
        Some(&one),
        None,
        Some(Dori::Observe),
        false,
    )
    .unwrap();
    assert_eq!(out.settings.level, "Observe");
    assert_eq!(out.suggestions[0].model_id, "reolink-rlc-810a");
    let err = suggest(&s, &goal(GoalArg::Targets), None, None, None, None, false)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("add_target"), "{err}");
}

#[test]
fn a_camera_turned_to_its_wall_is_turned_back() {
    let mut s = with_examples(1);
    let good = s.cameras[0].clone();
    s.cameras[0].yaw_deg = (good.yaw_deg + 180.0).rem_euclid(360.0);
    let v = value(&s);
    let out = optimise(&v, 1, &street(), false, None, None, true).unwrap();
    assert!(out.changed, "{}", out.summary);
    assert!(
        out.before.total < 1.0,
        "facing its own wall: {}",
        out.before.total
    );
    assert!(out.after.total > 10.0, "{}", out.summary);
    assert_eq!(out.gain, r1(out.after.total - out.before.total));
    assert_eq!(out.moved_m, 0.0);
    assert!(angle_between(out.after.aim.yaw_deg, good.yaw_deg) <= 95.0);
    let edited = edit::parse(out.scene.as_ref().unwrap()).unwrap();
    assert!((edited.cameras[0].yaw_deg - out.after.aim.yaw_deg).abs() < 0.06);
    no_addresses(&out);

    // Optimising again finds little more, and never less.
    let again = optimise(
        out.scene.as_ref().unwrap(),
        1,
        &street(),
        false,
        None,
        None,
        false,
    )
    .unwrap();
    assert!(again.gain >= 0.0);
    assert!(again.after.total >= out.after.total - 0.5);

    // Moving along the wall can only do as well or better.
    let moved = optimise(&v, 1, &street(), true, None, None, false).unwrap();
    assert!(
        moved.after.total >= out.after.total - 0.5,
        "{}",
        moved.summary
    );
    assert!(moved.moved_m <= 3.01);
    assert!(moved.searched.contains("spot"));
}

#[test]
fn optimise_aim_can_turn_a_camera_to_a_target() {
    let mut s = with_examples(1);
    let cam = s.cameras[0].clone();
    let fwd = Vec2::new(cam.forward().x, cam.forward().z).normalize();
    let spot = Vec2::new(cam.position[0], cam.position[2]) + fwd * 6.0;
    s.cameras[0].yaw_deg = (cam.yaw_deg + 180.0).rem_euclid(360.0);
    let (s, id) = with_target(s, "Path", spot.to_array());
    let spec = GoalSpec {
        goal: GoalArg::Targets,
        targets: Some(vec![TargetRef::Id(id)]),
        area_id: None,
    };
    let out = optimise(&value(&s), 1, &spec, false, None, None, true).unwrap();
    assert_eq!(out.unit, "targets");
    assert_eq!(
        (out.before.total, out.after.total),
        (0.0, 1.0),
        "{}",
        out.summary
    );
    assert!(
        out.summary.contains("1 more target(s) not ruled out"),
        "{}",
        out.summary
    );
    assert!(not_ruled_out(out.scene.as_ref().unwrap(), id));
}

#[test]
fn optimise_refuses_unknown_and_disabled_cameras() {
    let mut s = with_examples(1);
    let v = value(&s);
    assert!(optimise(&v, 9, &street(), false, None, None, false).is_err());
    s.cameras[0].enabled = false;
    let v = value(&s);
    let err = optimise(&v, 1, &street(), false, None, None, false)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("disabled"), "{err}");
}

#[test]
fn a_camera_off_its_wall_is_not_moved() {
    let mut s = with_examples(1);
    // Out in the open, 1 m up from the street.
    let spot = on_street(&s);
    let b = demo().unwrap();
    s.cameras[0].position = [
        spot[0],
        b.terrain.height_at(Vec2::from_array(spot)) + 2.0,
        spot[1],
    ];
    let out = optimise(&value(&s), 1, &street(), true, None, None, false).unwrap();
    assert_eq!(out.moved_m, 0.0);
    assert!(
        out.summary.contains("not on a building wall"),
        "{}",
        out.summary
    );
}

#[test]
fn cover_target_re_aims_a_camera_that_can_reach_it() {
    let mut s = with_examples(1);
    let cam = s.cameras[0].clone();
    // A target 6 m in front of the camera, which is turned away from it.
    let fwd = Vec2::new(cam.forward().x, cam.forward().z).normalize();
    let spot = Vec2::new(cam.position[0], cam.position[2]) + fwd * 6.0;
    s.cameras[0].yaw_deg = (cam.yaw_deg + 180.0).rem_euclid(360.0);
    let (s, id) = with_target(s, "Path", spot.to_array());
    let v = value(&s);
    assert!(!not_ruled_out(&v, id));
    let out = cover(&v, &TargetRef::Name("Path".into()), true).unwrap();
    assert_eq!(out.outcome, "re_aim", "{}", out.summary);
    assert_eq!(out.camera_id, Some(1));
    assert!(out.aim_after.is_some() && out.street_change_m2.is_some());
    assert!(out.at_night.starts_with("at night:"), "{}", out.at_night);
    assert_eq!(out.settings.lighting, "Day");
    let edited = out.scene.unwrap();
    assert!(not_ruled_out(&edited, id));
    no_addresses(&out.summary);

    // Now a camera does not rule it out: nothing to change.
    let done = cover(&edited, &TargetRef::Id(id), false).unwrap();
    assert_eq!(done.outcome, "already_not_ruled_out");
    assert_eq!(done.camera_id, Some(1));
    assert!(done.scene.is_none());
}

#[test]
fn cover_target_suggests_a_new_camera_when_none_can_reach() {
    let base = with_examples(1);
    let cam = Vec2::new(base.cameras[0].position[0], base.cameras[0].position[2]);
    // The largest gap's spot, if it is out of the camera's reach.
    let spot = on_street(&base);
    assert!(Vec2::from_array(spot).distance(cam) > 40.0, "{spot:?}");
    let (s, id) = with_target(base, "Bin store", spot);
    let out = cover(&value(&s), &TargetRef::Id(id), true).unwrap();
    assert_eq!(out.outcome, "new_camera", "{}", out.summary);
    let sug = out.suggestion.as_ref().unwrap();
    assert_eq!(out.new_camera_id, Some(2));
    assert_eq!(sug.camera_id, Some(2));
    assert!(not_ruled_out(out.scene.as_ref().unwrap(), id));
    assert!(out.summary.contains("at night"), "{}", out.summary);
}

/// Camera 1, an 87° bullet camera on a house front, aimed at target B 8 m
/// along its wall one way; target A lies 8 m the other way. No aim of
/// camera 1 leaves both not ruled out. Returns the scene and A's and B's
/// ids.
fn two_targets_one_camera() -> (SceneFile, u32, u32) {
    let mut s = with_examples(1);
    s.cameras[0].model_id = "reolink-rlc-810a".into();
    let cam = s.cameras[0].clone();
    let at = Vec2::new(cam.position[0], cam.position[2]);
    let fwd = Vec2::new(cam.forward().x, cam.forward().z).normalize();
    let along = Vec2::new(-fwd.y, fwd.x);
    let (s, b) = with_target(s, "B", (at + along * 8.0 + fwd * 3.0).to_array());
    let (mut s, a) = with_target(s, "A", (at - along * 8.0 + fwd * 3.0).to_array());
    let b_at = at + along * 8.0 + fwd * 3.0;
    let b_point = Vec3::new(
        b_at.x,
        demo().unwrap().terrain.height_at(b_at) + 1.0,
        b_at.y,
    );
    s.cameras[0].look_at(b_point);
    (s, a, b)
}

#[test]
fn cover_target_never_silently_loses_another_target() {
    let (s, a, b) = two_targets_one_camera();
    let v = value(&s);
    assert!(not_ruled_out(&v, b), "camera 1 is aimed at B");
    assert!(!not_ruled_out(&v, a));
    let out = cover(&v, &TargetRef::Id(a), true).unwrap();
    // Re-aiming camera 1 at A would lose B, so a new camera is offered, and
    // the summary says why camera 1 was not re-aimed.
    assert_eq!(out.outcome, "new_camera", "{}", out.summary);
    assert!(out.summary.contains("would rule out B"), "{}", out.summary);
    let edited = out.scene.as_ref().unwrap();
    assert!(not_ruled_out(edited, a) && not_ruled_out(edited, b));

    // With no room for a new camera, only the lossy re-aim is left: it is
    // offered with what it loses, and not applied.
    let mut full = s.clone();
    let mut spare = full.cameras[0].clone();
    spare.enabled = false;
    spare.label = String::new();
    while edit::camera_room(&full) > 0 {
        edit::add_camera(&mut full, spare.clone()).unwrap();
    }
    let out = cover(&value(&full), &TargetRef::Id(a), true).unwrap();
    assert_eq!(out.outcome, "re_aim", "{}", out.summary);
    assert_eq!(out.targets_lost, vec![b]);
    assert!(
        out.scene.is_none(),
        "a re-aim that loses a target is not applied"
    );
    assert!(out.summary.contains("Not applied"), "{}", out.summary);
    assert!(out.summary.contains("no room"), "{}", out.summary);
}

#[test]
fn optimise_aim_keeps_a_pose_that_already_reaches_the_targets() {
    // Camera 1 already leaves B not ruled out: another aim with more street
    // but the same targets is no change for the targets goal.
    let (s, _, b) = two_targets_one_camera();
    let spec = GoalSpec {
        goal: GoalArg::Targets,
        targets: Some(vec![TargetRef::Id(b)]),
        area_id: None,
    };
    let v = value(&s);
    let out = optimise(&v, 1, &spec, false, None, None, true).unwrap();
    assert!(!out.changed, "{}", out.summary);
    assert_eq!(
        (out.before.total, out.after.total, out.gain),
        (1.0, 1.0, 0.0)
    );
    assert_eq!(out.street_change_m2, Some(0.0));
    let kept = edit::parse(out.scene.as_ref().unwrap()).unwrap();
    assert_eq!(kept.cameras[0].yaw_deg, s.cameras[0].yaw_deg);
    assert_eq!(kept.cameras[0].pitch_deg, s.cameras[0].pitch_deg);
    // Every aim scored is counted: the coarse sweep alone is 469.
    let tried: usize = out
        .searched
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap();
    assert!(tried > 469 + 25, "{}", out.searched);
}

#[test]
fn the_wall_under_a_camera_is_found_among_its_neighbours_walls() {
    let s = with_examples(3);
    let b = demo().unwrap();
    for c in &s.cameras {
        let xz = Vec2::new(c.position[0], c.position[2]);
        // As a search over every wall of the area finds it.
        let every = observation_core::example::wall_mounts(b)
            .into_iter()
            .filter_map(|w| on_wall(&w, xz).map(|off| (off, w)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, w)| w);
        let found = wall_of(b, xz);
        assert!(found.is_some(), "camera {} is on a house front", c.id);
        assert_eq!(found, every);
    }
    // Out on the street, no wall.
    assert_eq!(wall_of(b, Vec2::from_array(on_street(&s))), None);
}

#[test]
fn a_full_scene_is_refused_before_the_search() {
    let mut s = with_examples(1);
    let mut spare = s.cameras[0].clone();
    spare.enabled = false;
    spare.label = String::new();
    while edit::camera_room(&s) > 1 {
        edit::add_camera(&mut s, spare.clone()).unwrap();
    }
    // Room for one: three asked for, one suggested and added.
    let out = suggest(&value(&s), &street(), Some(3), None, None, None, true).unwrap();
    assert_eq!(out.suggestions.len(), 1, "{}", out.summary);
    assert!(out.summary.contains("room for only 1"), "{}", out.summary);
    let full = out.scene.unwrap();
    let err = suggest(&full, &street(), None, None, None, None, false)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("remove one first"), "{err}");
}

/// How long each tool takes on the demo street, and a generous bound. In a
/// release build each call is well under the 3 s aimed for (see
/// docs/design.md); a debug build is several times slower.
#[test]
fn the_searches_are_bounded() {
    let base = with_examples(3);
    let spot = on_street(&base);
    let (s, id) = with_target(base, "Gate", spot);
    let v = value(&s);
    let _ = demo_warm();
    let time = |what: &str, f: &dyn Fn()| {
        let t = Instant::now();
        f();
        let e = t.elapsed();
        eprintln!("{what}: {:.0} ms", e.as_secs_f64() * 1000.0);
        e
    };
    let spec = GoalSpec {
        goal: GoalArg::Targets,
        targets: Some(vec![TargetRef::Id(id)]),
        area_id: None,
    };
    let runs = [
        time("suggest_placement street x4", &|| {
            suggest(&v, &street(), Some(4), None, None, None, false).unwrap();
        }),
        time("suggest_placement targets", &|| {
            suggest(&v, &spec, None, None, None, None, false).unwrap();
        }),
        time("optimise_aim with moves", &|| {
            optimise(&v, 1, &street(), true, None, None, false).unwrap();
        }),
        time("cover_target", &|| {
            cover(&v, &TargetRef::Id(id), false).unwrap();
        }),
    ];
    for r in runs {
        assert!(r.as_secs_f64() < 60.0, "{r:?}");
    }
}

/// Builds the demo street's layers before anything is timed.
fn demo_warm() -> Result<(), ToolError> {
    crate::area::demo_layers().map(|_| ())
}

/// Rounded figures read as the decimal they were rounded to, not as the
/// digits of an `f32` a hair below it (`12.059999`, `1.1999999`).
#[test]
fn rounded_figures_serialise_as_written() {
    for (v, two, one) in [
        (12.0612_f32, "12.06", "12.1"),
        (5.8199997, "5.82", "5.8"),
        (1.1999999, "1.2", "1.2"),
        (-60.719997, "-60.72", "-60.7"),
        (1023.83, "1023.83", "1023.8"),
    ] {
        let json = |x: f32| serde_json::to_string(&crate::geometry::widened::widen(x)).unwrap();
        assert_eq!(json(r2(v)), two, "{v}");
        assert_eq!(json(r1(v)), one, "{v}");
    }
}
