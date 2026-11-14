//! The place and query tools on the demo street.

use super::*;
use crate::area::demo;
use crate::catalog::catalog;
use crate::edit::{Light, Place};
use crate::queries::{
    CameraSplitArgs, FindGapsArgs, GapsOut, SplitOut, TargetVerdictsArgs, WhoSeesArgs,
};
use observation_core::footage::{AreaLabel, MAX_AREAS};

fn mcp() -> ObservationMcp {
    ObservationMcp::new()
}

fn empty() -> Value {
    let s = edit::new_demo_scene().unwrap();
    edit::finish(&s).unwrap().1
}

fn rect(x0: f32, z0: f32, x1: f32, z1: f32) -> Vec<[f32; 2]> {
    vec![[x0, z0], [x1, z0], [x1, z1], [x0, z1]]
}

/// A scene with one camera 2.5 m up at [0, 15], in front of building 58
/// (x -6.9 to 7.2, z -21.6 to 5.7), looking north at it.
fn with_camera() -> Value {
    let cat = catalog().unwrap();
    // The model that reaches furthest at Detect by day.
    let longest = cat
        .models
        .iter()
        .max_by(|a, b| {
            let r = |m: &observation_core::camera::CameraModel| {
                m.effective_range_m(25.0, observation_core::camera::Lighting::Day)
            };
            r(a).total_cmp(&r(b))
        })
        .unwrap();
    let mut s = edit::new_demo_scene().unwrap();
    edit::place_camera(
        &mut s,
        cat,
        Place {
            model_id: longest.id.clone(),
            x: 0.0,
            z: 15.0,
            height_m: Some(2.5),
            yaw_deg: Some(0.0),
            pitch_deg: Some(-10.0),
            roll_deg: None,
            look_at: None,
            label: Some("Porch".into()),
        },
    )
    .unwrap();
    edit::finish(&s).unwrap().1
}

/// `scene` with camera 1 disabled.
fn camera_off(scene: &Value) -> Value {
    let mut s = edit::parse(scene).unwrap();
    let update = edit::CameraUpdate {
        enabled: Some(false),
        ..edit::CameraUpdate::default()
    };
    edit::update_camera(&mut s, catalog().unwrap(), 1, update).unwrap();
    edit::finish(&s).unwrap().1
}

fn label(scene: Value, area: AreaEdit) -> Result<PlaceEdited, String> {
    mcp()
        .label_area(Parameters(LabelAreaArgs { scene, area }))
        .map(|Json(o)| o)
}

fn target(scene: Value, name: &str, at: PlaceArg) -> Result<PlaceEdited, String> {
    mcp()
        .add_target(Parameters(AddTargetArgs {
            scene,
            name: name.to_owned(),
            at,
        }))
        .map(|Json(o)| o)
}

fn ground(points: Vec<[f32; 2]>) -> PlaceArg {
    PlaceArg::Ground { points }
}

fn wall_near(x: f32, z: f32) -> PlaceArg {
    PlaceArg::Wall {
        x,
        z,
        height_m: None,
        normal: None,
        facing_camera: None,
        facing_street: false,
    }
}

#[test]
fn areas_are_added_changed_limited_and_removed() {
    let own = label(
        empty(),
        AreaEdit {
            points: Some(rect(-6.0, 6.0, 7.0, 14.0)),
            label: Some(LabelArg::Own),
            name: Some("  Front garden ".into()),
            ..AreaEdit::default()
        },
    )
    .unwrap();
    assert_eq!(own.area_id, Some(1));
    assert!(own.summary.contains("Own") && own.summary.contains("Front garden"));
    assert_eq!(own.scene["areas"][0]["label"], "own");
    assert_eq!(own.scene["areas"][0]["name"], "Front garden");

    let next = label(
        own.scene,
        AreaEdit {
            points: Some(rect(-20.0, 6.0, -8.0, 14.0)),
            label: Some(LabelArg::Neighbour),
            ..AreaEdit::default()
        },
    )
    .unwrap();
    assert_eq!(next.area_id, Some(2));
    // Changing one keeps what is not given; `none` and "" clear.
    let changed = label(
        next.scene,
        AreaEdit {
            area_id: Some(1),
            label: Some(LabelArg::None),
            name: Some(String::new()),
            enabled: Some(false),
            ..AreaEdit::default()
        },
    )
    .unwrap();
    let a = &changed.scene["areas"][0];
    assert_eq!((a.get("label"), a.get("name")), (None, None));
    assert_eq!(a["enabled"], false);
    assert_eq!(a["points"][2][0], 7.0);

    // Limit the figures to area 2, then remove it: the limit goes with it.
    let Json(limited) = mcp()
        .limit_figures_to(Parameters(LimitArgs {
            scene: changed.scene,
            area_id: Some(2),
        }))
        .unwrap();
    assert_eq!(limited.scene["limit_figures_to"], 2);
    assert!(limited.summary.contains("limited to area 2"));
    let Json(removed) = mcp()
        .remove_area(Parameters(AreaIdArgs {
            scene: limited.scene.clone(),
            area_id: 2,
        }))
        .unwrap();
    assert!(removed.summary.contains("no longer limited"));
    assert_eq!(removed.scene.get("limit_figures_to"), None);
    assert_eq!(removed.scene["areas"].as_array().map(Vec::len), Some(1));
    // Clearing the limit keeps the area.
    let Json(cleared) = mcp()
        .limit_figures_to(Parameters(LimitArgs {
            scene: limited.scene,
            area_id: None,
        }))
        .unwrap();
    assert_eq!(cleared.scene.get("limit_figures_to"), None);
    assert_eq!(cleared.scene["areas"].as_array().map(Vec::len), Some(2));
    // A disabled area is named, but limits nothing until it is enabled.
    let Json(off) = mcp()
        .limit_figures_to(Parameters(LimitArgs {
            scene: cleared.scene,
            area_id: Some(1),
        }))
        .unwrap();
    assert!(
        off.summary.contains("once it is enabled"),
        "{}",
        off.summary
    );
}

#[test]
fn bad_areas_are_refused_with_reasons() {
    let err = |area: AreaEdit| label(empty(), area).map(drop).expect_err("an error");
    let pts = |p: Vec<[f32; 2]>| AreaEdit {
        points: Some(p),
        ..AreaEdit::default()
    };
    assert!(err(AreaEdit::default()).contains("needs its corners"));
    assert!(err(pts(vec![[0.0, 0.0], [5.0, 0.0]])).contains("at least three corners"));
    assert!(err(pts(vec![[0.0, 0.0], [5.0, 0.0], [10.0, 0.0]])).contains("one line"));
    let bow = vec![[0.0, 0.0], [10.0, 10.0], [10.0, 0.0], [0.0, 10.0]];
    assert!(err(pts(bow)).contains("crosses itself"));
    assert!(err(pts(rect(0.0, 0.0, 500.0, 10.0))).contains("outside the area"));
    let many: Vec<[f32; 2]> = (0..300)
        .map(|i| {
            let a = i as f32 / 300.0 * std::f32::consts::TAU;
            [10.0 * a.cos(), 10.0 * a.sin()]
        })
        .collect();
    assert!(err(pts(many)).contains("at most 256 corners"));
    let long = AreaEdit {
        name: Some("x".repeat(61)),
        ..pts(rect(0.0, 0.0, 5.0, 5.0))
    };
    assert!(err(long).contains("at most 60 characters"));
    let control = AreaEdit {
        name: Some("a\u{7}b".into()),
        ..pts(rect(0.0, 0.0, 5.0, 5.0))
    };
    assert!(err(control).contains("control characters"));
    let missing = AreaEdit {
        area_id: Some(9),
        ..AreaEdit::default()
    };
    assert!(err(missing).contains("no area with id 9"));
    let e = mcp()
        .remove_area(Parameters(AreaIdArgs {
            scene: empty(),
            area_id: 3,
        }))
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("no area with id 3"));
    let e = mcp()
        .limit_figures_to(Parameters(LimitArgs {
            scene: empty(),
            area_id: Some(3),
        }))
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("no area with id 3"));
    // At most MAX_AREAS.
    let mut s = edit::parse(&empty()).unwrap();
    for _ in 0..MAX_AREAS {
        crate::places::label_area(&mut s, pts(rect(0.0, 0.0, 5.0, 5.0))).unwrap();
    }
    let full = edit::finish(&s).unwrap().1;
    let e = label(full, pts(rect(0.0, 0.0, 5.0, 5.0)))
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("at most 64 areas"));
}

#[test]
fn targets_are_added_on_the_ground_and_on_the_nearest_wall() {
    let gate = target(with_camera(), "Gate", ground(rect(-1.0, 9.0, 1.0, 11.0))).unwrap();
    assert_eq!(gate.target_id, Some(1));
    assert_eq!(gate.scene["targets"][0]["at"]["kind"], "ground");
    assert!(gate.wall.is_none());

    // Next to building 58's south front: the target goes on that wall,
    // facing out of the building (south, +z).
    let door = target(gate.scene, " Front door ", wall_near(0.0, 7.0)).unwrap();
    assert_eq!(door.target_id, Some(2));
    let w = door.wall.as_ref().expect("where it went");
    assert_eq!(w.building, Some(58));
    assert!(w.normal[1] > 0.9, "{w:?}");
    assert!(w.moved_m.is_some_and(|m| m > 0.5 && m < 2.0), "{w:?}");
    assert_eq!(w.height_m, 1.0, "the scene's subject height");
    assert!(door.summary.contains("building 58"));
    let t = &door.scene["targets"][1];
    assert_eq!(t["name"], "Front door");
    assert_eq!(t["at"]["kind"], "wall");

    // Facing the camera: the same wall. Facing the street: a wall facing it.
    let facing = |facing_camera, facing_street| PlaceArg::Wall {
        x: 0.0,
        z: 7.0,
        height_m: Some(1.5),
        normal: None,
        facing_camera,
        facing_street,
    };
    let to_cam = target(door.scene.clone(), "Door", facing(Some(1), false)).unwrap();
    assert_eq!(to_cam.wall.as_ref().and_then(|w| w.building), Some(58));
    assert_eq!(to_cam.wall.as_ref().map(|w| w.height_m), Some(1.5));
    let e = target(door.scene.clone(), "Door", facing(Some(7), false))
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("no camera with id 7"));

    // Given the wall's normal, the point is used as it is.
    let explicit = target(
        door.scene.clone(),
        "Window",
        PlaceArg::Wall {
            x: 0.0,
            z: 5.6,
            height_m: Some(2.0),
            normal: Some([0.0, 3.0]),
            facing_camera: None,
            facing_street: false,
        },
    )
    .unwrap();
    let w = explicit.wall.unwrap();
    assert_eq!((w.building, w.normal), (None, [0.0, 1.0]));

    // Renamed, moved and removed.
    let Json(moved) = mcp()
        .update_target(Parameters(UpdateTargetArgs {
            scene: door.scene,
            target_id: 1,
            name: Some("Side gate".into()),
            at: Some(ground(rect(-2.0, 9.0, 0.0, 11.0))),
        }))
        .unwrap();
    assert_eq!(moved.scene["targets"][0]["name"], "Side gate");
    assert_eq!(moved.scene["targets"][0]["at"]["points"][0][0], -2.0);
    let Json(gone) = mcp()
        .remove_target(Parameters(TargetIdArgs {
            scene: moved.scene,
            target_id: 1,
        }))
        .unwrap();
    assert_eq!(gone.scene["targets"].as_array().map(Vec::len), Some(1));
    assert_eq!(gone.scene["targets"][0]["id"], 2);
}

#[test]
fn bad_targets_are_refused_with_reasons() {
    let err = |name: &str, at| {
        target(with_camera(), name, at)
            .map(drop)
            .expect_err("an error")
    };
    assert!(err("  ", ground(rect(0.0, 9.0, 1.0, 10.0))).contains("needs a name"));
    assert!(err("Lawn", ground(rect(0.0, 9.0, 40.0, 12.0))).contains("at most 30 m across"));
    assert!(err("Lawn", ground(vec![[0.0, 9.0], [1.0, 9.0]])).contains("three corners"));
    assert!(err("Far", ground(rect(400.0, 0.0, 401.0, 1.0))).contains("outside the area"));
    let tall = PlaceArg::Wall {
        x: 0.0,
        z: 7.0,
        height_m: Some(20.0),
        normal: None,
        facing_camera: None,
        facing_street: false,
    };
    let e = err("Roof", tall);
    assert!(e.contains("building 58 is only"), "{e}");
    let flat = PlaceArg::Wall {
        x: 0.0,
        z: 7.0,
        height_m: None,
        normal: Some([0.0, 0.0]),
        facing_camera: None,
        facing_street: false,
    };
    assert!(err("Door", flat).contains("needs a direction"));
    // Somewhere on the demo street with no wall within 10 m.
    let b = demo().unwrap();
    let (lo, _) = crate::area::extent(b);
    let open = (0..60)
        .flat_map(|i| (0..50).map(move |j| (i, j)))
        .map(|(i, j)| [lo[0] + 5.0 + i as f32 * 4.8, lo[1] + 5.0 + j as f32 * 4.8])
        .find(|p| {
            observation_core::footage::nearest_wall(b.footprints(), *p, WALL_SNAP_M, |_| true)
                .is_none()
        })
        .expect("open ground somewhere");
    assert!(err("Door", wall_near(open[0], open[1])).contains("no building wall within 10 m"));
    let e = mcp()
        .update_target(Parameters(UpdateTargetArgs {
            scene: with_camera(),
            target_id: 4,
            name: None,
            at: None,
        }))
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("no target with id 4"));
    let e = mcp()
        .remove_target(Parameters(TargetIdArgs {
            scene: with_camera(),
            target_id: 4,
        }))
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("no target with id 4"));
}

/// The camera scene with four targets: in front of the camera, behind
/// building 58, on its front wall, and behind the camera.
fn with_targets() -> Value {
    let s = with_camera();
    let s = target(s, "Path", ground(rect(-1.0, 9.0, 1.0, 11.0)))
        .unwrap()
        .scene;
    let s = target(s, "Back yard", ground(rect(-1.0, -24.5, 1.0, -23.0)))
        .unwrap()
        .scene;
    let s = target(s, "Front door", wall_near(0.0, 7.0)).unwrap().scene;
    target(s, "Kerb", ground(rect(-1.0, 20.0, 1.0, 22.0)))
        .unwrap()
        .scene
}

#[tokio::test]
async fn target_verdicts_give_reasons_and_one_sided_wording() {
    let Json(out) = mcp()
        .target_verdicts(Parameters(TargetVerdictsArgs {
            scene: with_targets(),
            target_id: None,
        }))
        .await
        .unwrap();
    assert_eq!(out.targets.len(), 4);
    assert_eq!(out.settings.level, "Detect");
    assert!(out.sources.osm_data_as_of.is_some());
    assert!(out.sources.caveats.contains("trees"));
    assert!(out.wording.contains("Not ruled out"));
    let selected = |i: usize| out.targets[i].cameras[0].selected.clone();
    assert_eq!(selected(0), "not ruled out");
    assert_eq!(selected(1), "ruled out: blocked by a building");
    assert_eq!(selected(2), "not ruled out");
    assert_eq!(selected(3), "ruled out: outside the view");
    let behind = &out.targets[1].cameras[0];
    let detect_day = &behind.verdicts[0];
    assert_eq!(
        (detect_day.level.as_str(), detect_day.lighting.as_str()),
        ("Detect", "Day")
    );
    assert_eq!(detect_day.reason.as_deref(), Some("blocked_by_building"));
    // Four levels by day and at night.
    assert_eq!(behind.verdicts.len(), 8);
    assert_eq!(behind.camera_label, "Porch");
    assert!(out.targets[0].summary.starts_with("Path, at Detect by day"));
    assert!(
        out.targets[3]
            .summary
            .contains("ruled out for every camera")
    );
    assert!(out.targets[2].cameras[0].summary.starts_with("By day:"));
    assert_eq!(out.targets[2].kind, "wall");
    // Nothing says a camera "sees" a target.
    let text = serde_json::to_string(&out.targets).unwrap();
    assert!(!text.contains(" sees") && !text.contains("seen"), "{text}");

    // One target; a disabled camera is skipped and named.
    let off = camera_off(&with_targets());
    let Json(one) = mcp()
        .target_verdicts(Parameters(TargetVerdictsArgs {
            scene: off,
            target_id: Some(3),
        }))
        .await
        .unwrap();
    assert_eq!(one.targets.len(), 1);
    assert!(one.targets[0].cameras.is_empty());
    assert!(one.targets[0].summary.contains("no enabled cameras"));
    assert_eq!(one.cameras_skipped[0].reason, "disabled");

    // Errors: no targets, or no such target.
    let e = mcp()
        .target_verdicts(Parameters(TargetVerdictsArgs {
            scene: with_camera(),
            target_id: None,
        }))
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("no targets"));
    let e = mcp()
        .target_verdicts(Parameters(TargetVerdictsArgs {
            scene: with_targets(),
            target_id: Some(9),
        }))
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("no target with id 9"));
}

#[tokio::test]
async fn who_sees_a_point() {
    let server = mcp();
    let ask = |x: f32, z: f32, height_m: Option<f32>| {
        server.who_sees_point(Parameters(WhoSeesArgs {
            scene: with_camera(),
            x,
            z,
            height_m,
        }))
    };
    let Json(front) = ask(0.0, 10.0, None).await.unwrap();
    assert_eq!(front.cameras[0].selected, "not ruled out");
    assert!(!front.inside_building);
    assert!(
        front
            .summary
            .contains("not ruled out for camera(s) 1, ruled out for none")
    );
    assert!(front.sources.osm_data_as_of.is_some());
    let Json(behind) = ask(0.0, -23.5, Some(0.5)).await.unwrap();
    assert_eq!(behind.settings.subject_height_m, 0.5);
    assert_eq!(
        behind.cameras[0].verdicts[0].reason.as_deref(),
        Some("blocked_by_building")
    );
    let Json(inside) = ask(0.0, -10.0, None).await.unwrap();
    assert!(inside.inside_building);
    assert!(inside.summary.contains("inside a building"));
    assert!(
        ask(0.0, 400.0, None)
            .await
            .map(drop)
            .unwrap_err()
            .contains("outside the area")
    );
    assert!(
        ask(0.0, 10.0, Some(20.0))
            .await
            .map(drop)
            .unwrap_err()
            .contains("0 to 10 m")
    );
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

#[tokio::test]
async fn camera_split_counts_areas_street_and_walls_without_addresses() {
    let s = label(
        with_camera(),
        AreaEdit {
            points: Some(rect(-6.0, 6.0, 7.0, 14.0)),
            label: Some(LabelArg::Own),
            name: Some("Front garden".into()),
            ..AreaEdit::default()
        },
    )
    .unwrap()
    .scene;
    let s = label(
        s,
        AreaEdit {
            points: Some(rect(-20.0, 6.0, -8.0, 14.0)),
            label: Some(LabelArg::Neighbour),
            ..AreaEdit::default()
        },
    )
    .unwrap()
    .scene;
    let server = mcp();
    let run = |camera_id, lighting| {
        server.camera_split(Parameters(CameraSplitArgs {
            scene: s.clone(),
            camera_id,
            dori: None,
            lighting,
        }))
    };
    let Json(out): Json<SplitOut> = run(None, None).await.unwrap();
    assert_eq!(out.offset_m, 1.0);
    assert!(out.notes.contains("as the user drew them"));
    assert!(out.sources.osm_data_as_of.is_some());
    let c = &out.cameras[0];
    assert_eq!((c.camera_id, c.camera_label.as_str()), (1, "Porch"));
    assert_eq!(c.areas.len(), 2);
    let own = &c.areas[0];
    assert_eq!(own.label.as_deref(), Some("own"));
    assert_eq!(own.name.as_deref(), Some("Front garden"));
    assert!((own.polygon_m2 - 104.0).abs() < 0.1, "{}", own.polygon_m2);
    let r = own.seen;
    assert!(r.as_drawn_m2 > 10.0, "{r:?}");
    assert!(
        r.low_m2 <= r.as_drawn_m2 && r.as_drawn_m2 <= r.high_m2,
        "{r:?}"
    );
    let labels: Vec<&str> = c.labels.iter().map(|l| l.label.as_str()).collect();
    assert_eq!(labels, ["own", "neighbour"]);
    assert!(c.ground_m2 >= c.street_m2 + c.unlabelled.low_m2);
    // The house front it looks at, by index and distance.
    let front = c
        .walls
        .iter()
        .find(|w| w.building == 58)
        .unwrap_or_else(|| panic!("{c:?}"));
    assert!(
        front.distance_m > 8.0 && front.distance_m < 12.0,
        "{front:?}"
    );
    assert!(
        c.walls
            .windows(2)
            .all(|w| w[0].distance_m <= w[1].distance_m)
    );
    let text = serde_json::to_string(&out).unwrap();
    for a in addresses() {
        assert!(!text.contains(&a), "{a} in the split");
    }

    // At night, one camera: less or the same.
    let Json(night) = run(Some(1), Some(Light::Night)).await.unwrap();
    assert_eq!(night.settings.lighting, "Night");
    assert!(night.cameras[0].ground_m2 <= c.ground_m2);
    // Errors: no such camera, a disabled one.
    assert!(
        run(Some(5), None)
            .await
            .map(drop)
            .unwrap_err()
            .contains("no camera with id 5")
    );
    let off = camera_off(&s.clone());
    let e = mcp()
        .camera_split(Parameters(CameraSplitArgs {
            scene: off,
            camera_id: Some(1),
            dori: None,
            lighting: None,
        }))
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("disabled"));
}

async fn find_gaps(scene: Value, max_gaps: Option<usize>) -> Result<GapsOut, String> {
    mcp()
        .find_gaps(Parameters(FindGapsArgs {
            scene,
            dori: None,
            lighting: None,
            max_gaps,
            min_area_m2: None,
        }))
        .await
        .map(|Json(o)| o)
}

#[tokio::test]
async fn gaps_on_the_demo_street() {
    let mut ex = edit::new_demo_scene().unwrap();
    edit::add_example_cameras(&mut ex, catalog().unwrap(), 3, &[]).unwrap();
    let examples = edit::finish(&ex).unwrap().1;
    let none = find_gaps(empty(), None).await.unwrap();
    let some = find_gaps(examples.clone(), Some(3)).await.unwrap();
    assert!(none.street_m2 > 1000.0);
    assert_eq!(none.unseen_street_m2, none.street_m2);
    assert!(none.gaps[0].nearest_camera.is_none());
    assert!(none.gaps[0].description.contains("no enabled cameras"));
    assert_eq!(some.street_m2, none.street_m2);
    assert!(some.unseen_street_m2 < none.unseen_street_m2);
    assert_eq!(some.gaps.len(), 3);
    assert_eq!(some.cameras_counted.len(), 3);
    assert!(some.gaps.windows(2).all(|w| w[0].area_m2 >= w[1].area_m2));
    let g = &some.gaps[0];
    assert_eq!(g.rank, 1);
    assert!(g.min[0] <= g.inside[0] && g.inside[0] <= g.max[0]);
    assert!(g.min[1] <= g.inside[1] && g.inside[1] <= g.max[1]);
    assert!(g.nearest_camera.is_some());
    let street = g.street.as_ref().expect("a street");
    assert!(street.name.is_some(), "{street:?}");
    assert!(g.description.contains("m² of street"));
    assert!(some.notes.contains("never by address"));
    assert!(some.sources.osm_data_as_of.is_some());
    let text = serde_json::to_string(&some).unwrap();
    for a in addresses() {
        assert!(!text.contains(&a), "{a} in the gaps");
    }
    // The same scene gives the same gaps.
    let again = find_gaps(examples.clone(), Some(3)).await.unwrap();
    assert_eq!(text, serde_json::to_string(&again).unwrap());

    // Limited to an area for the figures, only its street counts.
    let s = label(
        examples,
        AreaEdit {
            points: Some(rect(-60.0, 60.0, 60.0, 127.0)),
            ..AreaEdit::default()
        },
    )
    .unwrap();
    let Json(limited) = mcp()
        .limit_figures_to(Parameters(LimitArgs {
            scene: s.scene,
            area_id: s.area_id,
        }))
        .unwrap();
    let inside = find_gaps(limited.scene, None).await.unwrap();
    assert!(inside.region_limited);
    assert!(inside.street_m2 < none.street_m2 && inside.street_m2 > 0.0);

    let e = mcp()
        .find_gaps(Parameters(FindGapsArgs {
            scene: empty(),
            dori: None,
            lighting: None,
            max_gaps: None,
            min_area_m2: Some(-1.0),
        }))
        .await
        .map(drop)
        .expect_err("an error");
    assert!(e.contains("min_area_m2"));
}

#[test]
fn place_edits_keep_cameras_and_take_json_strings() {
    let s = with_camera();
    let text = Value::String(s.to_string());
    let out = label(
        text,
        AreaEdit {
            points: Some(rect(-6.0, 6.0, 7.0, 14.0)),
            label: Some(LabelArg::Public),
            ..AreaEdit::default()
        },
    )
    .unwrap();
    assert_eq!(out.scene["cameras"][0]["label"], "Porch");
    let parsed = edit::parse(&out.scene).unwrap();
    assert_eq!(parsed.areas[0].label, Some(AreaLabel::Public));
    let mut s = edit::parse(&out.scene).unwrap();
    edit::remove_camera(&mut s, 1).unwrap();
    let removed = edit::finish(&s).unwrap().1;
    assert_eq!(removed["areas"][0]["label"], "public");
}
