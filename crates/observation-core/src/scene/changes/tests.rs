use super::super::tests::scene;
use super::*;
use crate::camera::{CameraKind, DoriLevel, Lighting};
use crate::footage::AreaLabel;

fn camera(id: u32, x: f32) -> CameraPlacement {
    CameraPlacement {
        id,
        label: format!("Cam {id}"),
        model_id: "generic-doorbell-160".into(),
        position: [x, 3.0, -6.0],
        yaw_deg: 210.0,
        pitch_deg: -10.0,
        roll_deg: 0.0,
        color: None,
        enabled: true,
        group: None,
    }
}

fn model(id: &str) -> CameraModel {
    CameraModel {
        id: id.into(),
        vendor: "Acme".into(),
        name: "Eye".into(),
        kind: CameraKind::Bullet,
        hfov_deg: 90.0,
        vfov_deg: 50.0,
        resolution: [1920, 1080],
        ir_range_m: Some(10.0),
        source: None,
        notes: None,
    }
}

fn square(x: f32) -> Vec<[f32; 2]> {
    vec![[x, 0.0], [x + 10.0, 0.0], [x + 10.0, 10.0], [x, 10.0]]
}

fn area(id: u32) -> LabelledArea {
    LabelledArea {
        id,
        label: Some(AreaLabel::Own),
        name: Some("Garden".into()),
        points: square(0.0),
        enabled: true,
    }
}

fn target(id: u32, name: &str) -> Target {
    Target {
        id,
        name: name.into(),
        at: TargetPlace::Ground {
            points: vec![[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]],
        },
    }
}

/// The scene fixture with a second camera, a custom model, an area and a
/// target.
fn base() -> SceneFile {
    let mut s = scene();
    s.cameras.push(camera(3, 0.0));
    s.custom_models.push(model("custom-acme-eye-1"));
    s.areas.push(area(1));
    s.targets.push(target(1, "Side gate"));
    s
}

fn no_names(_: &str) -> Option<String> {
    None
}

#[test]
fn a_diff_names_each_item_with_its_base_and_new_value() {
    let b = base();
    let mut p = b.clone();
    p.cameras[1].position[0] = 2.1;
    p.cameras.retain(|c| c.id != 1);
    p.cameras.push(camera(12, 4.0));
    p.targets.clear();
    p.params.lighting = Lighting::Night;
    p.limit_figures_to = Some(1);
    let set = ChangeSet::between(&b, &p).unwrap();
    assert!(set.check().is_ok());
    let kinds: Vec<String> = set.changes.iter().map(Change::key).collect();
    assert_eq!(
        kinds,
        [
            "analysis settings",
            "camera 1",
            "camera 3",
            "camera 12",
            "target 1",
            "the area the figures are limited to"
        ]
    );
    let Change::Camera(removed) = &set.changes[1] else {
        panic!()
    };
    assert!(removed.is_remove());
    assert_eq!(removed.base.as_ref(), Some(&b.cameras[0]));
    let Change::Camera(added) = &set.changes[3] else {
        panic!()
    };
    assert!(added.is_add());
    // The same scene: nothing to propose.
    assert!(ChangeSet::between(&b, &b).unwrap().is_empty());
    assert_eq!(
        ChangeSet::between(&b, &b).unwrap().check(),
        Err(ChangeError::Empty)
    );
    // Another area cannot be proposed.
    let mut moved = b.clone();
    moved.origin = crate::geo::LatLon::new(5.0, 5.0);
    assert_eq!(ChangeSet::between(&b, &moved), Err(ChangeError::OtherArea));
}

#[test]
fn applying_keeps_the_users_unrelated_edits() {
    let b = base();
    let mut p = b.clone();
    p.cameras[1].yaw_deg = 90.0;
    p.cameras.push(camera(12, 4.0));
    p.custom_models.clear();
    p.cameras.retain(|c| c.model_id != "custom-acme-eye-1");
    let set = ChangeSet::between(&b, &p).unwrap();
    // Meanwhile the user renamed camera 1 and drew a second area.
    let mut now = b.clone();
    now.cameras[0].label = "Porch".into();
    let mut a2 = area(2);
    a2.points = square(20.0);
    now.areas.push(a2);
    assert!(set.conflicts(&now).is_empty());
    let out = set.apply(&now).unwrap();
    assert_eq!(out.cameras[0].label, "Porch", "the user's edit stays");
    assert_eq!(out.areas.len(), 2);
    assert_eq!(out.cameras[1].yaw_deg, 90.0);
    assert_eq!(out.cameras.last().unwrap().id, 12);
    assert!(out.custom_models.is_empty());
    // Applied to the base it gives the proposed scene.
    assert_eq!(
        set.apply(&b).unwrap(),
        SceneFile::from_json(&p.to_json()).unwrap()
    );
}

#[test]
fn a_set_is_stale_once_an_item_it_touches_differs_from_its_base() {
    let b = base();
    let mut p = b.clone();
    p.cameras[1].position[0] = 5.0;
    p.cameras.push(camera(12, 4.0));
    p.targets.clear();
    p.params.dori = DoriLevel::Identify;
    let set = ChangeSet::between(&b, &p).unwrap();

    let mut moved = b.clone();
    moved.cameras[1].position[2] = 1.0;
    assert_eq!(set.conflicts(&moved), ["camera 3"]);
    assert_eq!(
        set.apply(&moved),
        Err(ChangeError::Stale("camera 3".into()))
    );

    // The user took id 12 for a camera of their own.
    let mut taken = b.clone();
    taken.cameras.push(camera(12, -4.0));
    assert_eq!(set.conflicts(&taken), ["camera 12"]);

    // The user removed the target the proposal removes too: still stale.
    let mut gone = b.clone();
    gone.targets.clear();
    assert_eq!(set.conflicts(&gone), ["target 1"]);

    let mut settings = b.clone();
    settings.params.subject_height_m = 0.5;
    assert_eq!(set.conflicts(&settings), ["the analysis settings"]);
}

#[test]
fn malformed_sets_are_refused() {
    let set = |c: Change| ChangeSet { changes: vec![c] };
    let malformed = |s: &ChangeSet| matches!(s.check(), Err(ChangeError::Malformed(_)));
    assert!(malformed(&set(Change::Camera(ItemChange {
        id: 3,
        base: None,
        new: None
    }))));
    assert!(malformed(&set(Change::Camera(ItemChange {
        id: 3,
        base: Some(camera(3, 0.0)),
        new: Some(camera(3, 0.0))
    }))));
    assert!(malformed(&set(Change::Camera(ItemChange {
        id: 3,
        base: None,
        new: Some(camera(4, 0.0))
    }))));
    assert!(malformed(&set(Change::LimitFiguresTo {
        base: Some(1),
        new: Some(1)
    })));
    // Ids above `MAX_ITEM_ID`, which no scene may hold.
    let big = MAX_ITEM_ID + 1;
    assert!(malformed(&set(Change::Camera(ItemChange {
        id: big,
        base: None,
        new: Some(camera(big, 0.0))
    }))));
    assert!(malformed(&set(Change::Area(ItemChange {
        id: u32::MAX,
        base: None,
        new: Some(area(u32::MAX))
    }))));
    assert!(malformed(&set(Change::Target(ItemChange {
        id: big,
        base: Some(target(big, "a")),
        new: None
    }))));
    assert!(malformed(&set(Change::LimitFiguresTo {
        base: None,
        new: Some(big)
    })));
    assert!(
        set(Change::Camera(ItemChange {
            id: MAX_ITEM_ID,
            base: None,
            new: Some(camera(MAX_ITEM_ID, 0.0))
        }))
        .check()
        .is_ok(),
        "the largest id itself is allowed"
    );
    let twice = ChangeSet {
        changes: vec![
            Change::Target(ItemChange {
                id: 1,
                base: Some(target(1, "a")),
                new: None,
            }),
            Change::Target(ItemChange {
                id: 1,
                base: None,
                new: Some(target(1, "b")),
            }),
        ],
    };
    assert!(malformed(&twice));
    let many = ChangeSet {
        changes: (0..=MAX_CHANGES as u32)
            .map(|i| {
                Change::Camera(ItemChange {
                    id: i,
                    base: None,
                    new: Some(camera(i, 0.0)),
                })
            })
            .collect(),
    };
    assert_eq!(many.check(), Err(ChangeError::TooMany(MAX_CHANGES + 1)));
}

#[test]
fn a_set_that_leaves_an_invalid_scene_is_refused() {
    let b = base();
    let mut p = b.clone();
    p.params.subject_height_m = 50.0;
    let set = ChangeSet::between(&b, &p).unwrap();
    assert!(
        matches!(set.apply(&b), Err(ChangeError::Invalid(why)) if why.contains("subject height"))
    );
    let mut p = b.clone();
    p.cameras[1].position[0] = f32::NAN;
    let set = ChangeSet::between(&b, &p).unwrap();
    assert!(matches!(set.apply(&b), Err(ChangeError::Invalid(_))));
}

#[test]
fn sets_round_trip_through_json() {
    let b = base();
    let mut p = b.clone();
    p.cameras[0].position = [1.1, 2.7, -3.3];
    p.custom_models[0].hfov_deg = 101.5;
    p.areas[0].label = Some(AreaLabel::Neighbour);
    p.limit_figures_to = Some(1);
    let set = ChangeSet::between(&b, &p).unwrap();
    let text = serde_json::to_string(&set).unwrap();
    assert!(text.contains(r#""item":"camera""#), "{text}");
    assert!(
        text.contains(r#""item":"limit_figures_to","new":1"#),
        "{text}"
    );
    let back: ChangeSet = serde_json::from_str(&text).unwrap();
    assert_eq!(back, set);
    // Through a JSON value too (as the MCP server builds bodies): f32s come
    // back exactly, so base values still compare equal.
    let value = serde_json::to_value(&set).unwrap();
    let back: ChangeSet = serde_json::from_value(value).unwrap();
    assert_eq!(back, set);
    assert!(back.conflicts(&b).is_empty());
}

#[test]
fn lines_say_what_each_change_does() {
    let b = base();
    let mut p = b.clone();
    p.cameras.push(camera(12, 4.0));
    p.cameras[1].position[0] = 2.1;
    p.cameras[1].yaw_deg = 180.0;
    p.cameras.retain(|c| c.id != 1);
    p.targets.clear();
    p.custom_models[0].hfov_deg = 120.0;
    p.areas[0].label = Some(AreaLabel::Public);
    p.params.lighting = Lighting::Night;
    p.limit_figures_to = Some(1);
    let set = ChangeSet::between(&b, &p).unwrap();
    let names = |id: &str| (id == "generic-doorbell-160").then(|| "Generic Doorbell".to_owned());
    let lines = set.lines(&names);
    assert_eq!(
        lines,
        [
            "Analysis settings: night (was day)",
            "Custom model \u{2018}Acme Eye\u{2019}: field of view 120° by 50°",
            "Remove Camera 1 \u{2018}Front door\u{2019}",
            "Camera 3 \u{2018}Cam 3\u{2019}: move by 2.1 m to x 2.1, z \u{2212}6.0; aim at 180° (was 210°)",
            "Add Camera 12 \u{2018}Cam 12\u{2019} (\u{2018}Generic Doorbell\u{2019}) at x 4.0, z \u{2212}6.0, aimed 210°",
            "Area 1 \u{2018}Garden\u{2019}: label it Public",
            "Remove target \u{2018}Side gate\u{2019}",
            "Limit the figures to area 1",
        ]
    );
    // Untrusted names are quoted, flattened and cut.
    let mut long = b.clone();
    long.cameras[0].label = format!("a\nb {}", "x".repeat(100));
    let lines = ChangeSet::between(&b, &long).unwrap().lines(&no_names);
    assert!(lines[0].starts_with("Camera 1 \u{2018}Front door\u{2019}: rename to \u{2018}a b xxx"));
    assert!(lines[0].ends_with("x…\u{2019}"), "{}", lines[0]);
    assert!(lines[0].chars().count() < 120);
}
