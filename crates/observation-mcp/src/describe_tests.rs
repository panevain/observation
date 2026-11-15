//! The scene description tools on the demo street.

use super::*;
use crate::area::{self, demo};
use crate::edit::Place;
use crate::places::{AreaEdit, LabelArg, PlaceArg, add_target, label_area};

fn mcp() -> ObservationMcp {
    ObservationMcp::new()
}

fn empty() -> Value {
    let s = edit::new_demo_scene().unwrap();
    edit::finish(&s).unwrap().1
}

fn porch() -> Place {
    Place {
        model_id: "reolink-rlc-810a".into(),
        x: 0.0,
        z: 15.0,
        height_m: Some(2.5),
        yaw_deg: Some(0.0),
        pitch_deg: Some(-10.0),
        roll_deg: None,
        look_at: None,
        label: Some("Porch".into()),
    }
}

/// A scene with camera 1 "Porch" 2.5 m up at [0, 15], in front of building
/// 58 (x -6.9 to 7.2, z -21.6 to 5.7), looking north at it; an own area
/// around the camera; and a ground target in front of the building.
fn scene() -> Value {
    let cat = catalog().unwrap();
    let mut s = edit::new_demo_scene().unwrap();
    edit::place_camera(&mut s, cat, porch()).unwrap();
    label_area(
        &mut s,
        AreaEdit {
            points: Some(vec![[-5.0, 8.0], [5.0, 8.0], [5.0, 18.0], [-5.0, 18.0]]),
            label: Some(LabelArg::Own),
            name: Some("Front garden".into()),
            ..AreaEdit::default()
        },
    )
    .unwrap();
    add_target(
        &mut s,
        "path",
        PlaceArg::Ground {
            points: vec![[-1.0, 7.0], [1.0, 7.0], [1.0, 9.0], [-1.0, 9.0]],
        },
    )
    .unwrap();
    edit::finish(&s).unwrap().1
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
    let text = serde_json::to_string(out).unwrap();
    for a in addresses() {
        assert!(!text.contains(&a), "{a} in {text}");
    }
}

/// `mount_points` with its arguments in a row.
fn mounts(
    s: &Value,
    building: Option<usize>,
    near: Option<[f32; 2]>,
    facing: Option<FacingArg>,
    height_m: Option<f32>,
    limit: Option<usize>,
) -> Result<MountsOut, ToolError> {
    mount_points(
        s,
        MountQuery {
            building,
            near,
            radius_m: None,
            facing,
            height_m,
            limit,
        },
    )
}

fn json(out: &impl Serialize) -> Value {
    serde_json::to_value(out).unwrap()
}

#[test]
fn surroundings_name_buildings_streets_and_places_by_direction() {
    let s = scene();
    let out = surroundings(&s, [0.0, 15.0], None, None).unwrap();
    no_addresses(&out);
    assert_eq!(out.inside_building, None);
    let first = &out.buildings[0];
    assert_eq!(first.building, 58);
    assert_eq!(first.toward.compass, "north");
    assert!((9.0..10.0).contains(&first.toward.distance_m), "{first:?}");
    assert!(out.buildings.len() <= SURROUNDINGS_LIMIT.0);
    assert!(out.buildings_within >= out.buildings.len());
    assert!(
        out.buildings
            .windows(2)
            .all(|w| w[0].toward.distance_m <= w[1].toward.distance_m)
    );
    assert!(!out.streets.is_empty());
    assert!(out.streets.iter().all(|st| st.toward.distance_m <= 40.0));
    assert_eq!(out.cameras.len(), 1);
    assert_eq!(out.cameras[0].facing, "north");
    assert_eq!(out.cameras[0].height_m, 2.5);
    assert!(out.areas[0].contains);
    assert_eq!(out.targets[0].toward.compass, "north");
    assert!(out.ground.elevation_m.is_some());
    for want in [
        "building 58 9 m north",
        "camera 1 \"Porch\" here",
        "Inside area 1 (own, \"Front garden\")",
        "target 1 \"path\" 7 m north",
    ] {
        assert!(out.summary.contains(want), "{want} in {}", out.summary);
    }
    // Deterministic.
    assert_eq!(
        json(&out),
        json(&surroundings(&s, [0.0, 15.0], None, None).unwrap())
    );
}

#[test]
fn surroundings_inside_a_building_and_their_limits() {
    let s = empty();
    let out = surroundings(&s, [0.4, -8.2], Some(10.0), Some(2)).unwrap();
    assert_eq!(out.inside_building, Some(58));
    assert_eq!(out.buildings[0].toward.distance_m, 0.0);
    assert!(
        out.summary.contains("inside building 58"),
        "{}",
        out.summary
    );
    assert!(out.buildings.len() <= 2 && out.streets.len() <= 2);
    assert_eq!(out.radius_m, 10.0);
    // The radius is capped, and a point outside the area is refused.
    let wide = surroundings(&s, [0.0, 0.0], Some(1.0e6), None).unwrap();
    assert_eq!(wide.radius_m, f64::from(SURROUNDINGS_RADIUS_M.1));
    assert!(
        surroundings(&s, [1000.0, 0.0], None, None)
            .unwrap_err()
            .to_string()
            .contains("outside the area")
    );
}

#[test]
fn buildings_page_by_index_or_by_distance() {
    let s = empty();
    let total = demo().unwrap().buildings.len();
    let first = buildings(&s, None, None, None, None).unwrap();
    no_addresses(&first);
    assert_eq!(first.total, total);
    assert_eq!(first.buildings.len(), BUILDINGS_PAGE.0);
    assert_eq!(first.buildings[0].building, 0);
    assert_eq!(first.next_offset, Some(20));
    assert!(first.buildings.iter().all(|b| b.distance_m.is_none()));
    let last = buildings(&s, None, None, Some(50), Some(total - 3)).unwrap();
    assert_eq!(last.buildings.len(), 3);
    assert_eq!(last.next_offset, None);
    assert!(
        last.summary.contains(&format!("of {total}")),
        "{}",
        last.summary
    );

    let near = buildings(&s, Some([0.0, 15.0]), Some(30.0), None, None).unwrap();
    assert_eq!(near.buildings[0].building, 58);
    assert!(near.total < total);
    assert!(near.buildings.iter().all(|b| b.distance_m.unwrap() <= 30.0));
    let b58 = &near.buildings[0];
    assert_eq!(b58.front_faces, Some("south"));
    assert_eq!(b58.street.as_ref().unwrap().street, "Northeast 120th Lane");
    assert!(b58.size_m[0] > 10.0 && b58.size_m[1] > 10.0, "{b58:?}");
    assert!(b58.height_m > 2.0);

    assert!(buildings(&s, None, Some(30.0), None, None).is_err());
    assert!(buildings(&s, Some([0.0, 0.0]), Some(-1.0), None, None).is_err());
}

#[test]
fn streets_are_listed_once_per_name_with_short_lines() {
    let s = empty();
    let out = streets(&s).unwrap();
    no_addresses(&out);
    let (lo, hi) = extent(demo().unwrap());
    let named: Vec<&StreetOut> = out
        .streets
        .iter()
        .filter(|st| !st.street.starts_with("an unnamed"))
        .collect();
    assert!(named.len() >= 5);
    let mut names: Vec<&str> = named.iter().map(|st| st.street.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), named.len(), "each name once");
    for st in &out.streets {
        let corners: usize = st.lines.iter().map(Vec::len).sum();
        assert!((2..=MAX_LINE_POINTS).contains(&corners), "{st:?}");
        assert!(st.length_m > 0.0 && st.width_m > 0.0);
        for p in st.lines.iter().flatten() {
            assert!(p[0] >= f64::from(lo[0]) - 0.1 && p[0] <= f64::from(hi[0]) + 0.1);
            assert!(p[1] >= f64::from(lo[1]) - 0.1 && p[1] <= f64::from(hi[1]) + 0.1);
        }
    }
    let unnamed = out.streets.len() - named.len();
    assert!(unnamed <= MAX_UNNAMED_WAYS);
    assert!(out.summary.contains("Northeast 120th Lane"));
}

#[test]
fn clipping_keeps_the_part_inside() {
    let pieces = clip_polyline(
        &[[-20.0, 0.0], [20.0, 0.0], [20.0, 20.0], [0.0, 20.0]],
        [-10.0, -10.0],
        [10.0, 10.0],
    );
    assert_eq!(pieces, vec![vec![[-10.0, 0.0], [10.0, 0.0]]]);
    assert!(clip_polyline(&[[20.0, 20.0], [30.0, 30.0]], [-1.0, -1.0], [1.0, 1.0]).is_empty());
}

#[test]
fn mount_points_face_the_street_and_go_straight_to_place_camera() {
    let cat = catalog().unwrap();
    let s = empty();
    let out = mounts(&s, None, Some([0.0, 15.0]), None, None, None).unwrap();
    no_addresses(&out);
    assert_eq!(out.mounts.len(), MOUNTS_LIMIT.0);
    assert!(out.summary.contains("place_camera"));
    let b = demo().unwrap();
    for m in &out.mounts {
        assert!(m.faces_street, "{m:?}");
        assert_eq!(m.compass, compass(m.yaw_deg as f32));
        assert!(m.street.is_some());
        // On the wall of the building named: 0.15 m out.
        let fp = &b.buildings[m.building].footprint;
        let d =
            observation_core::address::distance_to_footprint([m.at[0] as f32, m.at[1] as f32], fp);
        assert!(d < 0.3, "{d} m from building {}: {m:?}", m.building);
    }
    // Nearest the point first.
    assert!(
        out.mounts
            .windows(2)
            .all(|w| w[0].distance_m <= w[1].distance_m)
    );
    // The first goes straight into place_camera.
    let m = &out.mounts[0];
    let mut scene = edit::parse(&s).unwrap();
    let id = edit::place_camera(
        &mut scene,
        cat,
        Place {
            model_id: "reolink-rlc-810a".into(),
            x: m.at[0] as f32,
            z: m.at[1] as f32,
            height_m: Some(m.height_m as f32),
            yaw_deg: Some(m.yaw_deg as f32),
            pitch_deg: Some(m.pitch_deg as f32),
            roll_deg: None,
            look_at: None,
            label: None,
        },
    )
    .unwrap();
    let placed = edit::finish(&scene).unwrap().1;
    // Now that wall is taken: no suggestion lands within 3 m of the camera.
    let again = mounts(&placed, None, Some([0.0, 15.0]), None, None, None).unwrap();
    let c = &scene.cameras.iter().find(|c| c.id == id).unwrap().position;
    for m in &again.mounts {
        let d = Vec2::new(m.at[0] as f32, m.at[1] as f32).distance(Vec2::new(c[0], c[2]));
        assert!(d > MOUNT_CLEAR_M, "{d}: {m:?}");
    }
    // The camera placed from it is described as on that wall.
    let summary = summarise(&placed).unwrap();
    assert!(
        summary
            .summary
            .contains(&format!("on building {}'s", m.building)),
        "{}",
        summary.summary
    );
}

#[test]
fn mount_points_on_one_building_or_facing_a_bearing() {
    let s = empty();
    let one = mounts(&s, Some(58), None, None, None, Some(20)).unwrap();
    assert!(!one.mounts.is_empty());
    assert!(one.mounts.iter().all(|m| m.building == 58));
    let north = mounts(
        &s,
        None,
        None,
        Some(FacingArg::Word("N".into())),
        None,
        None,
    )
    .unwrap();
    assert!(!north.mounts.is_empty());
    assert!(
        north
            .mounts
            .iter()
            .all(|m| m.yaw_deg == 0.0 && m.compass == "north")
    );
    let east = mounts(
        &s,
        Some(58),
        None,
        Some(FacingArg::Degrees(90.0)),
        None,
        None,
    )
    .unwrap();
    assert!(!east.mounts.is_empty());
    assert!(
        east.mounts
            .iter()
            .all(|m| m.yaw_deg == 90.0 && m.building == 58)
    );
    let street = mounts(
        &s,
        Some(58),
        None,
        Some(FacingArg::Word("street".into())),
        None,
        Some(20),
    )
    .unwrap();
    assert_eq!(json(&street), json(&one));
    // A doorbell height looks level.
    let low = mounts(&s, Some(58), None, None, Some(1.2), None).unwrap();
    assert!(
        low.mounts
            .iter()
            .all(|m| m.pitch_deg == 0.0 && m.height_m == 1.2)
    );
    // Too high for a 6 m house: none, with the reason.
    let high = mounts(&s, Some(58), None, None, Some(7.0), None).unwrap();
    assert!(high.mounts.is_empty());
    assert!(
        high.summary.contains("building 58 has no wall"),
        "{}",
        high.summary
    );
    for bad in [
        mounts(&s, Some(100_000), None, None, None, None),
        mounts(
            &s,
            None,
            None,
            Some(FacingArg::Word("up".into())),
            None,
            None,
        ),
        mounts(&s, None, None, None, Some(0.0), None),
        mounts(&s, None, Some([1000.0, 0.0]), None, None, None),
    ] {
        assert!(bad.is_err());
    }
}

#[test]
fn the_scene_summary_holds_every_item() {
    let s = scene();
    let out = summarise(&s).unwrap();
    no_addresses(&out);
    assert_eq!(out.area.kind, "demo street");
    assert_eq!(
        out.area.building_count,
        Some(demo().unwrap().buildings.len())
    );
    assert!(out.area.osm_data_as_of.is_some());
    assert_eq!(out.camera_count, 1);
    let c = &out.cameras[0];
    assert_eq!(c.facing, "north");
    assert_eq!(c.height_m, Some(2.5));
    assert_eq!(c.model.as_deref(), Some("Reolink RLC-810A"));
    assert_eq!(
        c.place.as_deref(),
        Some("9 m south of building 58, 6 m from Northeast 120th Lane")
    );
    assert_eq!(out.areas[0].area_m2, 100.0);
    assert_eq!(out.targets[0].at, [0.0, 8.0]);
    assert_eq!(out.figures_limited_to, None);
    for want in [
        "demo street",
        "Detect (25 px/m) by day",
        "camera 1 \"Porch\", Reolink RLC-810A, 2.5 m up 9 m south of building 58",
        "facing north and tilted 10° down",
        "area 1 (own, \"Front garden\") of 100 m²",
        "target 1 \"path\"",
        "cover the whole area",
    ] {
        assert!(out.summary.contains(want), "{want} in {}", out.summary);
    }
    // Limited figures are said so.
    let mut f = edit::parse(&s).unwrap();
    crate::places::limit_figures_to(&mut f, Some(1)).unwrap();
    let limited = summarise(&edit::finish(&f).unwrap().1).unwrap();
    assert_eq!(limited.figures_limited_to, Some(1));
    assert_eq!(limited.figures_limit_not_in_effect, None);
    assert!(limited.summary.contains("limited to area 1"));
    // Disabled, the area names the limit but it is not in effect.
    f.areas[0].enabled = false;
    let off = summarise(&edit::finish(&f).unwrap().1).unwrap();
    assert_eq!(off.figures_limited_to, None);
    assert_eq!(off.figures_limit_not_in_effect, Some(1));
    assert!(
        off.summary.contains("so they are not limited"),
        "{}",
        off.summary
    );
}

#[test]
fn an_unnamed_target_is_named_once() {
    let s = scene();
    let mut f = edit::parse(&s).unwrap();
    // The app keeps targets without a name; add_target refuses them.
    f.targets.push(Target {
        id: 2,
        name: String::new(),
        at: TargetPlace::Ground {
            points: vec![[2.0, 12.0], [4.0, 12.0], [4.0, 14.0], [2.0, 14.0]],
        },
    });
    let v = edit::finish(&f).unwrap().1;
    let around = surroundings(&v, [0.0, 15.0], None, None).unwrap();
    let summary = summarise(&v).unwrap();
    for text in [&around.summary, &summary.summary] {
        assert!(text.contains("target 2 "), "{text}");
        assert!(!text.contains("target 2 target 2"), "{text}");
        assert!(text.contains("target 1 \"path\""), "{text}");
    }
}

#[test]
fn a_scene_on_an_area_not_held_is_still_summarised() {
    use observation_core::geo::BBox;
    let bbox = BBox::new(47.0, -122.0, 47.002, -121.997).unwrap();
    let mut f = SceneFile::new(AreaRef::Osm { bbox }, bbox.center());
    // A camera 3 m above world y 0: with the ground unknown, so is its
    // height above it.
    f.cameras.push(CameraPlacement {
        id: 1,
        label: "Porch".into(),
        model_id: "reolink-rlc-810a".into(),
        position: [0.0, 3.0, 0.0],
        yaw_deg: 90.0,
        pitch_deg: -20.0,
        roll_deg: 0.0,
        color: None,
        enabled: true,
        group: None,
    });
    let v = serde_json::from_str::<Value>(&f.to_json()).unwrap();
    let out = summarise(&v).unwrap();
    assert_eq!(out.area.kind, "OpenStreetMap area");
    assert!(out.area.bbox.is_some() && out.area.building_count.is_none());
    assert!(out.summary.contains("connect the app"), "{}", out.summary);
    assert_eq!(out.cameras[0].height_m, None);
    assert!(!out.summary.contains(" m up"), "{}", out.summary);
    assert!(
        out.summary
            .contains("Reolink RLC-810A, at [0, 0], facing east"),
        "{}",
        out.summary
    );
    // The other tools need the area's data.
    assert!(surroundings(&v, [0.0, 0.0], None, None).is_err());
    // Converting needs only the origin.
    let p = convert(&v, None, Some([0.0, 0.0])).unwrap();
    assert_eq!(p.inside_area, None);
    assert!((p.lat_lon[0] - bbox.center().lat).abs() < 1e-6);
}

#[test]
fn the_camera_view_names_what_blocks_it() {
    let s = scene();
    let out = view(&s, 1).unwrap();
    no_addresses(&out);
    assert_eq!(out.ranges.len(), 4);
    assert_eq!(out.settings.level, "Detect");
    assert!(out.range_m > 30.0);
    // Building 58 fills the middle of the view.
    assert_eq!(out.centre_of_view.hits, "building");
    assert_eq!(out.centre_of_view.building, Some(58));
    assert_eq!(out.level_ahead.building, Some(58));
    let b58 = out.buildings.iter().find(|b| b.building == 58).unwrap();
    assert_eq!(b58.verdict, "not ruled out");
    assert_eq!(b58.compass, "north");
    assert!(out.buildings.len() <= MAX_VIEW_BUILDINGS);
    // Something behind it is blocked by it, and said so one-sidedly.
    assert!(
        out.buildings
            .iter()
            .any(|b| b.verdict == "ruled out: blocked by building 58"),
        "{:?}",
        out.buildings
    );
    for b in &out.buildings {
        assert!(b.distance_m <= out.range_m + 0.1);
        assert!(b.verdict == "not ruled out" || b.verdict.starts_with("ruled out: "));
    }
    let lane = out
        .streets
        .iter()
        .find(|st| st.street == "Northeast 120th Lane")
        .unwrap();
    assert!(lane.not_ruled_out_m > 0.0 && lane.not_ruled_out_m <= lane.in_view_m);
    assert_eq!(out.targets[0].verdict, "not ruled out");
    assert_eq!(out.areas[0].verdict, "not ruled out");
    assert!(out.wording.contains("one-sided"));
    assert!(out.sources.osm_data_as_of.is_some());
    for word in [" sees ", " seen "] {
        assert!(!out.summary.contains(word), "{word} in {}", out.summary);
    }
    assert!(out.summary.contains("blocked by building 58"));
    assert!(view(&s, 9).unwrap_err().to_string().contains('9'));
}

#[test]
fn a_target_behind_a_building_names_it() {
    let s = scene();
    let mut f = edit::parse(&s).unwrap();
    // A patch of ground on the far side of building 58 from the camera.
    add_target(
        &mut f,
        "back yard",
        PlaceArg::Ground {
            points: vec![[-1.0, -25.0], [1.0, -25.0], [1.0, -23.0], [-1.0, -23.0]],
        },
    )
    .unwrap();
    let out = view(&edit::finish(&f).unwrap().1, 1).unwrap();
    let t = out.targets.iter().find(|t| t.name == "back yard").unwrap();
    assert_eq!(t.verdict, "ruled out: blocked by building 58");
}

#[test]
fn positions_convert_both_ways() {
    let s = empty();
    let b = demo().unwrap();
    let origin = convert(&s, None, Some([0.0, 0.0])).unwrap();
    assert!((origin.lat_lon[0] - b.origin.lat).abs() < 1e-6);
    assert!((origin.lat_lon[1] - b.origin.lon).abs() < 1e-6);
    let there = convert(&s, None, Some([10.0, -20.0])).unwrap();
    assert_eq!(there.inside_area, Some(true));
    // North is negative z: further north than the origin.
    assert!(there.lat_lon[0] > b.origin.lat && there.lat_lon[1] > b.origin.lon);
    let back = convert(&s, Some(there.lat_lon), None).unwrap();
    assert_eq!(back.xz, [10.0, -20.0]);
    let far = convert(&s, Some([b.origin.lat + 0.01, b.origin.lon]), None).unwrap();
    assert_eq!(far.inside_area, Some(false));
    assert!(far.xz[1] < -1000.0);
    assert!(convert(&s, None, None).is_err());
    assert!(convert(&s, Some([0.0, 0.0]), Some([0.0, 0.0])).is_err());
    assert!(convert(&s, Some([91.0, 0.0]), None).is_err());
}

#[tokio::test]
async fn the_tools_answer_through_the_server() {
    let s = scene();
    let Json(out) = mcp()
        .describe_surroundings(Parameters(SurroundingsArgs {
            scene: s.clone(),
            at: [0.0, 15.0],
            radius_m: None,
            limit: None,
        }))
        .unwrap();
    assert_eq!(out.buildings[0].building, 58);
    let Json(view) = mcp()
        .camera_view(Parameters(CameraViewArgs {
            scene: s.clone(),
            id: 1,
        }))
        .await
        .unwrap();
    assert_eq!(view.camera_id, 1);
    // Facing may be given as a word or a number.
    let args: MountArgs = serde_json::from_value(serde_json::json!({
        "scene": s, "facing": 180, "building": 58
    }))
    .unwrap();
    let Json(m) = mcp().suggest_mount_points(Parameters(args)).await.unwrap();
    assert!(!m.mounts.is_empty());
    assert!(m.mounts.iter().all(|m| m.yaw_deg == 180.0));
    let args: MountArgs =
        serde_json::from_value(serde_json::json!({ "scene": empty(), "facing": "south" })).unwrap();
    assert!(matches!(args.facing, Some(FacingArg::Word(_))));
    // Outputs print short numbers.
    let text = serde_json::to_string(&json(&out)).unwrap();
    assert!(!text.contains("0000"), "{text}");
}

#[test]
fn mount_points_near_a_point_within_a_radius() {
    let s = empty();
    let b = demo().unwrap();
    let q = |radius_m| MountQuery {
        near: Some([0.0, 15.0]),
        radius_m,
        limit: Some(20),
        ..MountQuery::default()
    };
    let close = mount_points(&s, q(Some(12.0))).unwrap();
    assert!(!close.mounts.is_empty());
    for m in &close.mounts {
        let d = observation_core::address::distance_to_footprint(
            [0.0, 15.0],
            &b.buildings[m.building].footprint,
        );
        assert!(d <= 12.0, "building {} is {d} m away", m.building);
    }
    let all = mount_points(&s, q(None)).unwrap();
    assert!(all.mounts.len() > close.mounts.len());
    // The nearest ones are the same either way.
    assert_eq!(json(&all.mounts[0]), json(&close.mounts[0]));
    let no_near = MountQuery {
        radius_m: Some(10.0),
        ..MountQuery::default()
    };
    assert!(mount_points(&s, no_near).is_err());
    assert!(mount_points(&s, q(Some(-1.0))).is_err());
}

#[test]
fn buildings_and_mount_points_name_the_same_street() {
    let s = empty();
    let b = demo().unwrap();
    let out = mounts(&s, None, None, None, None, Some(20)).unwrap();
    assert!(!out.mounts.is_empty());
    for m in &out.mounts {
        // The street a mount faces is the one the descriptions give for the
        // wall it is on.
        let wall = observation_core::example::wall_mounts_of(b, |i, _| i == m.building)
            .into_iter()
            .find(|w| Vec2::new(m.at[0] as f32, m.at[1] as f32).distance(w.at) < 0.3)
            .unwrap();
        let named = nearest_street(b, wall.at.to_array()).unwrap();
        assert_eq!(m.street.as_deref(), Some(named.label.as_str()), "{m:?}");
        assert_eq!(wall.road, Some(named.index));
    }
}

#[test]
fn the_nearest_part_in_view_is_one_that_counts() {
    let layers = area::demo_layers().unwrap();
    let eye = Vec3::new(
        0.0,
        layers.terrain().height_at(Vec2::new(0.0, 15.0)) + 2.5,
        15.0,
    );
    let cone = ViewCone::from_angles(eye, 0.0, -10.0, 0.0, 90.0, 60.0, 50.0);
    let at = |z: f32| layers.subject_at(Vec2::new(0.0, z), 1.0);
    // The nearer point is on a wall facing away from the camera.
    let samples = [
        Sample {
            p: at(12.0),
            normal: Some(Vec2::new(0.0, -1.0)),
        },
        Sample {
            p: at(10.0),
            normal: None,
        },
    ];
    let t = tally(layers, &cone, &samples, false).unwrap();
    assert_eq!(t.in_cone, 2);
    assert_eq!(t.blocked, Blocked::Clear);
    assert!((t.nearest_m - (at(10.0) - eye).length()).abs() < 1e-4);
    assert_eq!(t.nearest, at(10.0));
    // Only the wall facing away: ruled out, and that point is all there is.
    let t = tally(layers, &cone, &samples[..1], false).unwrap();
    assert_eq!(t.blocked, Blocked::FacingAway);
    assert_eq!(t.nearest, at(12.0));
}

#[test]
fn a_footprint_with_a_height_that_is_not_a_number_is_skipped() {
    let layers = area::demo_layers().unwrap();
    let mut b = demo().unwrap().clone();
    b.buildings[58].footprint.height_m = f32::NAN;
    assert!(wall_samples(layers, &b, 58, 1.0).is_empty());
    b.buildings[58].footprint.height_m = 6.0;
    b.buildings[58].footprint.outer[0][0] = f32::INFINITY;
    assert!(wall_samples(layers, &b, 58, 1.0).is_empty());
    assert!(!wall_samples(layers, demo().unwrap(), 58, 1.0).is_empty());
}
