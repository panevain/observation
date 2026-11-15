//! render_map on the demo street.
//!
//! `golden/render/*.txt` hold whole text maps. A failure there means the
//! map changed: if that is intended, regenerate them with
//!
//! ```sh
//! OBS_BLESS=1 cargo nextest run -p observation-mcp render
//! ```
//!
//! and review the diff.

use super::*;
use crate::area::demo;
use crate::edit::{Kind, NewModel, Place};
use crate::server::ObservationMcp;
use observation_core::footage::{LabelledArea, Target};
use std::path::{Path, PathBuf};

fn empty() -> Value {
    let s = edit::new_demo_scene().unwrap();
    edit::finish(&s).unwrap().1
}

/// A camera model fixed here, so that catalogue edits leave the golden maps
/// alone.
fn fixed_model(s: &mut SceneFile) -> String {
    edit::create_custom_model(
        s,
        NewModel {
            name: "Test bullet".into(),
            vendor: String::new(),
            kind: Kind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 50.0,
            resolution: [2560, 1440],
            ir_range_m: Some(20.0),
            source: None,
            notes: None,
        },
    )
    .unwrap()
}

fn place(s: &mut SceneFile, model: &str, x: f32, z: f32, yaw: f32, label: &str) -> u32 {
    edit::place_camera(
        s,
        catalog().unwrap(),
        Place {
            model_id: model.to_owned(),
            x,
            z,
            height_m: Some(2.5),
            yaw_deg: Some(yaw),
            pitch_deg: Some(-10.0),
            roll_deg: None,
            look_at: None,
            label: Some(label.into()),
        },
    )
    .unwrap()
}

/// Two cameras at the front of building 58 (x -6.9 to 7.2, z -21.6 to 5.7):
/// one looking south across the street, one disabled looking west; an own
/// area in front of the building, and a target.
fn street_scene() -> Value {
    let mut s = edit::new_demo_scene().unwrap();
    let m = fixed_model(&mut s);
    place(&mut s, &m, 0.0, 8.0, 180.0, "Porch");
    place(&mut s, &m, 6.0, 8.0, 270.0, "Side");
    s.cameras[1].enabled = false;
    s.areas.push(LabelledArea {
        id: 1,
        label: Some(AreaLabel::Own),
        name: Some("Front garden".into()),
        points: vec![[-7.0, 6.0], [7.0, 6.0], [7.0, 12.0], [-7.0, 12.0]],
        enabled: true,
    });
    s.targets.push(Target {
        id: 1,
        name: "Gate".into(),
        at: TargetPlace::Ground {
            points: vec![[-1.0, 11.0], [1.0, 11.0], [1.0, 12.0], [-1.0, 12.0]],
        },
    });
    edit::finish(&s).unwrap().1
}

fn args(scene: Value) -> RenderMapArgs {
    RenderMapArgs {
        scene,
        center: None,
        radius_m: None,
        layers: None,
        coverage: None,
        dori: None,
        lighting: None,
        format: None,
    }
}

/// The grid's rows from a text block: the lines between the ruler and the
/// legend, without their z labels.
fn grid_rows(text: &str, out: &RenderOut) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_start().starts_with("x "))
        .unwrap()
        + 2;
    let pad = ruler_pad(text);
    lines[start..start + out.rows]
        .iter()
        .map(|l| l.chars().skip(pad).collect())
        .collect()
}

/// The width of the ruler's left margin: the `x` that heads the top ruler
/// stands two characters before the grid's first column.
fn ruler_pad(text: &str) -> usize {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("x "))
        .unwrap();
    line.find('x').unwrap() + 3
}

/// The glyph in column `c` of row `row`, after the ruler's left margin.
fn glyph_at(text: &str, out: &RenderOut, c: usize, row: usize) -> Option<char> {
    grid_rows(text, out)[row].chars().nth(c)
}

#[test]
fn the_whole_area_fits_the_grid_with_round_square_cells() {
    let b = demo().unwrap();
    let v = choose_view(area::extent(b), None, None).unwrap();
    assert!(v.columns <= MAX_COLUMNS && v.rows <= MAX_ROWS, "{v:?}");
    assert!(CELL_SIZES_M.contains(&v.cell_m));
    let (amin, amax) = area::extent(b);
    let max = v.max();
    assert!(v.min[0] <= amin[0] && v.min[1] <= amin[1]);
    assert!(max[0] >= amax[0] && max[1] >= amax[1]);
    // A smaller cell would not fit.
    let smaller = CELL_SIZES_M.iter().rev().find(|&&c| c < v.cell_m).unwrap();
    assert!(
        (amax[0] - amin[0]) / smaller > MAX_COLUMNS as f32
            || (amax[1] - amin[1]) / smaller > MAX_ROWS as f32
    );
}

#[test]
fn a_centre_and_radius_zoom_in() {
    let b = demo().unwrap();
    let v = choose_view(area::extent(b), Some([0.0, 0.0]), Some(20.0)).unwrap();
    assert_eq!(v.cell_m, 1.0);
    assert_eq!((v.columns, v.rows), (40, 40));
    assert_eq!(v.min, [-20.0, -20.0]);
    assert_eq!(v.cell_of([-20.0, -20.0]), Some((0, 0)));
    assert_eq!(v.cell_of([19.9, 0.5]), Some((39, 20)));
    assert_eq!(v.cell_of([20.0, 0.0]), None);
    let ticks = v.ticks(0);
    assert_eq!(ticks, vec![(0, -20.0), (10, -10.0), (20, 0.0), (30, 10.0)]);
    // A centre alone shows the default radius.
    let d = choose_view(area::extent(b), Some([0.0, 0.0]), None).unwrap();
    assert_eq!(d.min, [-DEFAULT_RADIUS_M, -DEFAULT_RADIUS_M]);
    for bad in [0.0, 5.0, f32::NAN, 1e6] {
        assert!(
            choose_view(area::extent(b), None, Some(bad)).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn compass_words_round_to_the_nearest_point() {
    assert_eq!(compass_word(0.0), "north");
    assert_eq!(compass_word(359.0), "north");
    assert_eq!(compass_word(44.0), "north-east");
    assert_eq!(compass_word(-90.0), "west");
    assert_eq!(compass_word(180.0), "south");
    assert_eq!(metres(-0.2), "0");
    assert_eq!(short_metres(-0.0), "0");
    assert_eq!(short_metres(-25.5), "-25.5");
    assert_eq!(short_metres(150.0), "150");
}

#[test]
fn a_camera_glyph_is_its_id_where_it_can_be() {
    assert_eq!(id_glyph(1), Some('1'));
    assert_eq!(id_glyph(10), Some('A'));
    assert_eq!(id_glyph(29), Some('U'), "T is left out");
    assert_eq!(id_glyph(34), Some('Z'));
    assert_eq!((id_glyph(0), id_glyph(35)), (None, None));
    assert!((1..=40).filter_map(id_glyph).all(|g| g != TARGET));
    // After a deletion the ids keep their glyphs.
    assert_eq!(camera_glyphs(&[1, 3, 4]), ['1', '3', '4']);
    // An id without a glyph takes the first one no id names.
    assert_eq!(camera_glyphs(&[40, 1, 3]), ['2', '1', '3']);
    // Repeated ids (which a checked scene never has) both fall back.
    assert_eq!(camera_glyphs(&[2, 2]), ['1', '3']);
    let many: Vec<u32> = (100..140).collect();
    let g = camera_glyphs(&many);
    assert_eq!((g[0], g[33], g[34]), ('1', 'Z', '+'));
}

#[test]
fn after_a_deletion_each_camera_keeps_its_id_as_glyph() {
    let mut s = edit::new_demo_scene().unwrap();
    let m = fixed_model(&mut s);
    for (k, x) in [-20.0, 0.0, 20.0].into_iter().enumerate() {
        place(&mut s, &m, x, 8.0, 180.0, &format!("Cam {k}"));
    }
    edit::remove_camera(&mut s, 2).unwrap();
    let mut a = args(edit::finish(&s).unwrap().1);
    a.center = Some([0.0, 10.0]);
    a.radius_m = Some(30.0);
    let r = render(&a, None).unwrap();
    let v = choose_view(area::extent(demo().unwrap()), a.center, a.radius_m).unwrap();
    let (c, row) = v.cell_of([20.0, 8.0]).unwrap();
    assert_eq!(glyph_at(&r.text, &r.out, c, row), Some('3'));
    let all = grid_rows(&r.text, &r.out).concat();
    assert!(all.contains('1') && !all.contains('2'));
    let keys: Vec<(&str, u32)> = r
        .out
        .cameras
        .iter()
        .map(|k| (k.glyph.as_str(), k.camera_id))
        .collect();
    assert_eq!(keys, [("1", 1), ("3", 3)]);
    assert!(
        r.out
            .legend
            .iter()
            .any(|e| e.glyph == "3" && e.meaning.starts_with("camera 3 "))
    );
}

/// A model of `scene` on the demo street, drawn over `view`, with coverage.
fn model_of<'a>(
    scene: &'a SceneFile,
    view: View,
    show: Vec<Layer>,
    layers: &'a StaticLayers,
) -> MapModel<'a> {
    let (cameras, areas, targets) = marks(scene, &[]);
    MapModel {
        view,
        bundle: demo().unwrap(),
        layers,
        show,
        seen: Some(vec![0; layers.grid().len()]),
        params: scene.params,
        cameras,
        areas,
        targets,
        pinned: None,
    }
}

#[test]
fn far_off_huge_outlines_cost_only_what_is_shown() {
    // 64 areas of 256 corners zigzagging 20 km either side of a 20 m view:
    // sampled whole, that would be some 2e9 steps.
    let mut s = edit::new_demo_scene().unwrap();
    s.areas = (0..64)
        .map(|id| LabelledArea {
            id: id + 1,
            label: None,
            name: None,
            points: (0..256)
                .map(|k| {
                    let side = if k % 2 == 0 { -2.0e4 } else { 2.0e4 };
                    [side, -5.0e3 + k as f32 * 40.0 + id as f32]
                })
                .collect(),
            enabled: true,
        })
        .collect();
    let layers = crate::area::demo_layers().unwrap();
    let view = choose_view(area::extent(demo().unwrap()), Some([0.0, 0.0]), Some(10.0)).unwrap();
    let m = model_of(&s, view, Layer::ALL.to_vec(), layers);
    let t = std::time::Instant::now();
    let g = text::grid(&m);
    let (canvas, _) = image::draw(&m);
    assert!(t.elapsed().as_secs_f32() < 5.0, "{:?}", t.elapsed());
    assert!(g.cells.iter().flatten().any(|&c| c == 'u'));
    assert!(canvas.width > 0);
}

#[test]
fn cells_without_map_data_have_their_own_glyph() {
    let s = edit::new_demo_scene().unwrap();
    let layers = crate::area::demo_layers().unwrap();
    let (_, amax) = area::extent(demo().unwrap());
    // A view straddling the area's east edge.
    let view = View {
        min: [amax[0] - 20.0, 0.0],
        cell_m: 2.0,
        columns: 40,
        rows: 10,
    };
    let m = model_of(&s, view, Layer::ALL.to_vec(), layers);
    let g = text::grid(&m);
    assert_eq!(g.cells[0][39], NO_DATA);
    assert_ne!(g.cells[0][0], NO_DATA);
    let legend = text::legend(&m, &g);
    assert!(
        legend
            .iter()
            .any(|e| e.glyph == "~" && e.meaning.contains("outside the area's map data"))
    );
}

#[test]
fn a_ground_target_sits_inside_its_outline_and_an_empty_one_is_skipped() {
    let mut s = edit::new_demo_scene().unwrap();
    // A U whose vertex mean and area centroid both fall in its notch.
    let u = vec![
        [0.0, 0.0],
        [10.0, 0.0],
        [10.0, 10.0],
        [7.0, 10.0],
        [7.0, 2.0],
        [3.0, 2.0],
        [3.0, 10.0],
        [0.0, 10.0],
    ];
    s.targets = vec![
        Target {
            id: 1,
            name: "U".into(),
            at: TargetPlace::Ground { points: u.clone() },
        },
        Target {
            id: 2,
            name: "Nothing".into(),
            at: TargetPlace::Ground { points: vec![] },
        },
    ];
    let (_, _, targets) = marks(&s, &[]);
    assert_eq!(targets.len(), 1);
    let p = targets[0].at;
    let in_arm = (0.0..=3.0).contains(&p[0]) || (7.0..=10.0).contains(&p[0]);
    assert!(in_arm && (0.0..=10.0).contains(&p[1]), "{p:?}");
}

#[test]
fn with_streets_hidden_the_ground_glyphs_say_the_street_is_included() {
    let mut a = args(street_scene());
    a.center = Some([0.0, 10.0]);
    a.radius_m = Some(30.0);
    a.layers = Some(vec![Layer::Buildings, Layer::Cameras]);
    let r = render(&a, None).unwrap();
    let seen = r.out.legend.iter().find(|e| e.glyph == ",").unwrap();
    assert!(seen.meaning.contains("streets hidden"), "{}", seen.meaning);
    let open = r.out.legend.iter().find(|e| e.glyph == "(space)").unwrap();
    assert!(
        open.meaning
            .contains("mostly ruled out for every enabled camera")
    );
}

#[test]
fn an_empty_scene_shows_buildings_and_unseen_street() {
    let r = render(&args(empty()), None).unwrap();
    let rows = grid_rows(&r.text, &r.out);
    let all: String = rows.concat();
    assert!(all.contains(BUILDING) && all.contains(STREET));
    assert!(!all.contains(STREET_SEEN) && !all.contains(GROUND_SEEN));
    assert!(r.out.columns <= MAX_COLUMNS && r.out.rows <= MAX_ROWS);
    assert!(rows.iter().all(|l| l.chars().count() <= MAX_COLUMNS));
    assert!(r.out.legend.iter().any(|e| e.glyph == "#"));
    assert!(r.out.cameras.is_empty());
    assert!(r.png.is_none() && r.out.image.is_none());
    assert_eq!(r.out.caveats_summary.as_deref(), Some(CAVEATS_SUMMARY));
    assert!(r.text.contains(CAVEATS_SUMMARY));
    assert!(r.text.contains("OpenStreetMap data"));
}

#[test]
fn cameras_targets_and_areas_are_marked_and_explained() {
    let mut a = args(street_scene());
    a.center = Some([0.0, 10.0]);
    a.radius_m = Some(20.0);
    let r = render(&a, None).unwrap();
    let rows = grid_rows(&r.text, &r.out);
    let v = choose_view(area::extent(demo().unwrap()), a.center, a.radius_m).unwrap();
    // Camera 1 at [0, 8] is glyph 1 in its cell.
    let (c, row) = v.cell_of([0.0, 8.0]).unwrap();
    assert_eq!(glyph_at(&r.text, &r.out, c, row), Some('1'));
    let (c, row) = v.cell_of([0.0, 11.5]).unwrap();
    assert_eq!(glyph_at(&r.text, &r.out, c, row), Some(TARGET));
    let all: String = rows.concat();
    assert!(all.contains('2') && all.contains(TARGET) && all.contains('o'));
    assert!(
        all.contains(STREET_SEEN),
        "camera 1 looks across the street"
    );
    let porch = &r.out.cameras[0];
    assert_eq!((porch.glyph.as_str(), porch.camera_id), ("1", 1));
    assert_eq!(porch.faces, "south");
    assert!(porch.on_map && porch.not_counted.is_none());
    assert_eq!(r.out.cameras[1].not_counted.as_deref(), Some("disabled"));
    let meaning = |g: &str| {
        r.out
            .legend
            .iter()
            .find(|e| e.glyph == g)
            .map(|e| e.meaning.clone())
            .unwrap()
    };
    assert_eq!(
        meaning("1"),
        "camera 1 \"Porch\" at [0, 8], faces south (180°)"
    );
    assert!(meaning("2").contains("disabled, so not counted"));
    assert!(meaning("o").contains("labelled own: area 1 \"Front garden\""));
    assert!(meaning("T").contains("1 \"Gate\" at [0, 12]"));
    assert_eq!(r.out.cameras_skipped.len(), 1);
}

#[test]
fn coverage_and_layers_can_be_left_out() {
    let mut a = args(street_scene());
    a.coverage = Some(false);
    let r = render(&a, None).unwrap();
    let all = grid_rows(&r.text, &r.out).concat();
    assert!(!all.contains(STREET_SEEN) && !all.contains(GROUND_SEEN));
    assert!(r.out.coverage.is_none() && r.out.caveats_summary.is_none());
    assert!(r.out.cameras_skipped.is_empty());

    let mut a = args(street_scene());
    a.layers = Some(vec![Layer::Buildings]);
    let r = render(&a, None).unwrap();
    let all = grid_rows(&r.text, &r.out).concat();
    assert!(all.contains(BUILDING));
    assert!(!all.contains(STREET) && !all.contains(STREET_SEEN) && !all.contains('1'));
    assert!(r.out.cameras.iter().all(|c| !c.on_map));
    assert!(!r.out.legend.iter().any(|e| e.glyph == "1"));
}

#[test]
fn night_and_higher_detail_see_no_more_street() {
    let seen = |dori, lighting| {
        let mut a = args(street_scene());
        a.center = Some([0.0, 10.0]);
        a.radius_m = Some(40.0);
        a.dori = dori;
        a.lighting = lighting;
        let r = render(&a, None).unwrap();
        grid_rows(&r.text, &r.out)
            .concat()
            .chars()
            .filter(|&c| c == STREET_SEEN || c == GROUND_SEEN)
            .count()
    };
    let day = seen(None, None);
    assert!(day > 0);
    assert!(seen(Some(Dori::Identify), None) <= day);
    assert!(seen(None, Some(Light::Night)) <= day);
}

#[test]
fn the_shared_point_shows_only_on_its_area() {
    let mut a = args(empty());
    a.center = Some([0.0, 0.0]);
    a.radius_m = Some(20.0);
    let r = render(&a, Some((AreaRef::Demo, [3.0, 4.0]))).unwrap();
    assert!(grid_rows(&r.text, &r.out).concat().contains(PINNED));
    assert!(
        r.out
            .legend
            .iter()
            .any(|e| e.glyph == "@" && e.meaning.ends_with("[3, 4]"))
    );
    let elsewhere = AreaRef::Osm {
        bbox: observation_core::geo::BBox::new(47.0, -122.0, 47.002, -121.997).unwrap(),
    };
    let r = render(&a, Some((elsewhere, [3.0, 4.0]))).unwrap();
    assert!(!grid_rows(&r.text, &r.out).concat().contains(PINNED));
}

#[test]
fn a_centre_outside_the_area_is_refused() {
    let mut a = args(empty());
    a.center = Some([1.0e5, 0.0]);
    assert!(
        render(&a, None)
            .unwrap_err()
            .to_string()
            .contains("outside")
    );
}

fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let dec = ::png::Decoder::new(std::io::Cursor::new(png.to_vec()));
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    (info.width, info.height, buf)
}

#[test]
fn the_picture_is_a_png_of_at_most_1024_pixels() {
    let mut a = args(street_scene());
    a.format = Some(Format::Image);
    let r = render(&a, None).unwrap();
    let (w, h, px) = decode(r.png.as_deref().unwrap());
    assert_eq!(w.max(h), IMAGE_LONG_SIDE_PX);
    let info = r.out.image.as_ref().unwrap();
    assert_eq!((info.width_px, info.height_px), (w, h));
    // The coverage ramp's first colour shows somewhere, blended over the
    // street or the ground.
    let distinct: std::collections::HashSet<&[u8]> = px.chunks(3).collect();
    assert!(distinct.len() > 8, "{} colours", distinct.len());
    // Image alone: the legend of marks, no grid.
    assert!(r.text.contains("Picture:"));
    assert!(!r.text.lines().any(|l| l.trim_start().starts_with("x ")));
    assert!(r.text.contains("camera 1 \"Porch\""));

    a.format = Some(Format::Both);
    let both = render(&a, None).unwrap();
    assert!(both.png.is_some());
    assert!(both.text.lines().any(|l| l.trim_start().starts_with("x ")));
}

#[tokio::test]
async fn the_tool_returns_text_an_image_and_structured_content() {
    let mut a = args(street_scene());
    a.format = Some(Format::Both);
    let r = ObservationMcp::new()
        .render_map(Parameters(a))
        .await
        .unwrap();
    assert_eq!(r.content.len(), 2);
    let text = r.content[0].as_text().unwrap();
    assert!(text.text.contains("Legend:"));
    let image = r.content[1].as_image().unwrap();
    assert_eq!(image.mime_type, "image/png");
    let png = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .unwrap();
    assert_eq!(&png[1..4], b"PNG");
    let s = r.structured_content.unwrap();
    for key in [
        "cell_m", "min", "max", "columns", "rows", "legend", "cameras",
    ] {
        assert!(s.get(key).is_some(), "{key}");
    }
    assert!(s["caveats_summary"].is_string());
    assert!(s["image"]["width_px"].is_u64());

    let bad = ObservationMcp::new()
        .render_map(Parameters(RenderMapArgs {
            radius_m: Some(1.0),
            ..args(empty())
        }))
        .await;
    assert!(bad.unwrap_err().contains("radius_m"));
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/render")
}

/// Whether to rewrite the golden files (`OBS_BLESS=1`). Refused under CI.
fn blessing() -> bool {
    let bless = std::env::var_os("OBS_BLESS").is_some_and(|v| v == "1");
    assert!(
        !bless || std::env::var_os("CI").is_none(),
        "OBS_BLESS is for local use: CI must compare against the committed golden files"
    );
    bless
}

fn check(name: &str, actual: &str) {
    let path = golden_dir().join(name);
    if blessing() {
        std::fs::create_dir_all(golden_dir()).expect("create the golden directory");
        std::fs::write(&path, actual).expect("write the golden file");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden file {}: {e}\nRun with OBS_BLESS=1 to create it.",
            path.display()
        )
    });
    if expected != actual {
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(a, b)| a != b)
            .map_or_else(|| "length".to_owned(), |i| format!("line {}", i + 1));
        panic!(
            "{name} differs from the golden file (first at {line}).\n\
             If the change is intended, run with OBS_BLESS=1 and review the diff."
        );
    }
}

#[test]
fn the_whole_demo_street_matches_its_golden_map() {
    let r = render(&args(street_scene()), None).unwrap();
    check("demo-street.txt", &r.text);
}

#[test]
fn a_zoomed_map_matches_its_golden_map() {
    let mut a = args(street_scene());
    a.center = Some([0.0, 10.0]);
    a.radius_m = Some(25.0);
    let r = render(&a, Some((AreaRef::Demo, [-4.0, 16.0]))).unwrap();
    check("demo-zoomed.txt", &r.text);
}
