//! Where footage lands: one camera's split (the Camera tab), every camera's
//! split side by side (the Analysis tab), each camera's verdict on each
//! target (the Places and Camera tabs), and the lists of areas and targets.
//!
//! Every figure says "as drawn", and a verdict is one-sided: "not ruled
//! out" or "ruled out" with the reason, never "seen". Buildings are named by
//! their index in the loaded area and their distance, never by address.

use crate::document::Document;
use crate::editor::Editor;
use crate::places::{area_title, label_text, target_title};
use observation_core::camera::{CameraPlacement, DoriLevel, Lighting};
use observation_core::coverage::{CoverageParams, RegionProblem, outline_area_m2};
use observation_core::footage::{
    AreaLabel, CameraVerdicts, DEFAULT_OFFSET_M, SeenRange, Split, Target, TargetPlace,
    WALL_TARGET_SIZE_M,
};

/// What every split says about its figures.
pub const AS_DRAWN_NOTE: &str = "As drawn: the areas and their labels are your own outlines, \
    not property boundaries. Each range moves every outline 1 m in and 1 m out; \
    the figure as drawn is in brackets.";

/// What every verdict says about its wording.
pub const ONE_SIDED_NOTE: &str = "Not ruled out: some of the target is visible in the model. \
    Hedges, fences, trees and vehicles are not modelled and can only block more, \
    so this never says the camera sees it. Ruled out: the model rules it out, for the reason given.";

/// The split's row for the street mask. It holds service ways (driveways)
/// and footpaths as well as roads, and those are often private, so it is
/// not called public.
pub const STREET_ROW: &str = "Street, driveways and footpaths (as mapped)";

/// `412 m²`.
pub fn m2(v: f32) -> String {
    format!("{v:.0} m²")
}

/// `30–41 m² (as drawn 36)`, or `36 m² (as drawn)` when the outlines'
/// offsets change nothing at this rounding.
pub fn range_text(r: &SeenRange) -> String {
    let (low, high) = (r.low_m2.round(), r.high_m2.round());
    if low == high {
        format!("{} (as drawn)", m2(r.as_drawn_m2))
    } else {
        format!("{low:.0}–{high:.0} m² (as drawn {:.0})", r.as_drawn_m2)
    }
}

/// What a row of the split is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitRowKind {
    /// Every area with this label together.
    Label(AreaLabel),
    /// One area, with its label.
    Area(Option<AreaLabel>),
    /// The street as mapped: roads, driveways and footpaths.
    Street,
    /// Ground neither on the street nor inside a labelled area.
    Unlabelled,
}

/// One row of a camera's split.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitRow {
    pub kind: SplitRowKind,
    pub what: String,
    pub value: String,
    pub tip: String,
}

/// The wall one camera sees on one building.
#[derive(Debug, Clone, PartialEq)]
pub struct WallRow {
    /// "Building 12 · 14 m away".
    pub what: String,
    pub value: String,
}

/// One camera's split, for the Camera tab.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitVm {
    /// "Ground seen: 412 m²".
    pub ground: String,
    /// Each label with its areas under it, the unlabelled areas, the street
    /// and the unlabelled ground.
    pub rows: Vec<SplitRow>,
    /// "Walls seen: 36 m² on 3 buildings".
    pub walls_total: String,
    /// Nearest building first.
    pub walls: Vec<WallRow>,
}

impl SplitVm {
    pub fn new(doc: &Document, split: &Split) -> Self {
        let mut rows = Vec::new();
        let area_row = |a: &observation_core::footage::AreaSeen| {
            let title = doc
                .areas
                .iter()
                .find(|d| d.id == a.area_id)
                .map_or_else(|| format!("Area {}", a.area_id), area_title);
            SplitRow {
                kind: SplitRowKind::Area(a.label),
                what: format!("{title} · outline {}", m2(a.polygon_m2)),
                value: range_text(&a.seen),
                tip: format!(
                    "Ground seen inside {title}: with its outline moved {DEFAULT_OFFSET_M:.0} m in, \
                     as drawn ({} enclosed), and moved {DEFAULT_OFFSET_M:.0} m out",
                    m2(a.polygon_m2)
                ),
            }
        };
        for l in &split.labels {
            rows.push(SplitRow {
                kind: SplitRowKind::Label(l.label),
                what: format!("{} (all)", l.label.label()),
                value: range_text(&l.seen),
                tip: format!(
                    "Ground seen inside any area labelled {}, each spot counted once",
                    l.label.label()
                ),
            });
            rows.extend(
                split
                    .areas
                    .iter()
                    .filter(|a| a.label == Some(l.label))
                    .map(area_row),
            );
        }
        rows.extend(
            split
                .areas
                .iter()
                .filter(|a| a.label.is_none())
                .map(area_row),
        );
        rows.push(SplitRow {
            kind: SplitRowKind::Street,
            what: STREET_ROW.to_owned(),
            value: m2(split.street_m2),
            tip: "Roads, driveways (service ways) and footpaths as mapped in OpenStreetMap, \
                  whatever areas are drawn over them. Driveways and footpaths are often \
                  private, so this is not all public ground"
                .to_owned(),
        });
        rows.push(SplitRow {
            kind: SplitRowKind::Unlabelled,
            what: "Unlabelled ground".to_owned(),
            value: range_text(&split.unlabelled),
            tip: "Ground seen that is neither on the mapped street, driveways and footpaths \
                  nor inside a labelled area \
                  (moving the outlines out leaves less of it)"
                .to_owned(),
        });
        let mut walls: Vec<_> = split.walls.iter().collect();
        walls.sort_by(|a, b| {
            a.distance_m
                .total_cmp(&b.distance_m)
                .then(a.building.cmp(&b.building))
        });
        let buildings = match split.walls.len() {
            1 => "1 building".to_owned(),
            n => format!("{n} buildings"),
        };
        Self {
            ground: format!("Ground seen: {}", m2(split.ground_m2)),
            rows,
            walls_total: format!("Walls seen: {} on {buildings}", m2(split.walls_m2)),
            walls: walls
                .into_iter()
                .map(|w| WallRow {
                    what: format!("Building {} · {:.0} m away", w.building, w.distance_m),
                    value: m2(w.seen_m2),
                })
                .collect(),
        }
    }
}

/// One cell of the table of every camera's split.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// The figure, or its range: `30–41`.
    pub main: String,
    /// `as drawn 36` under a range.
    pub sub: Option<String>,
}

impl Cell {
    fn single(v: f32) -> Self {
        Self {
            main: format!("{v:.0}"),
            sub: None,
        }
    }

    fn range(r: &SeenRange) -> Self {
        let (low, high) = (r.low_m2.round(), r.high_m2.round());
        Self {
            main: if low == high {
                format!("{:.0}", r.as_drawn_m2)
            } else {
                format!("{low:.0}–{high:.0}")
            },
            sub: Some(format!("as drawn {:.0}", r.as_drawn_m2)),
        }
    }
}

/// One camera's row of the table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableRow {
    pub camera_id: u32,
    pub camera: String,
    /// One per column after the camera's name.
    pub cells: Vec<Cell>,
}

/// Every enabled camera's split side by side, for the Analysis tab. All
/// figures are m².
#[derive(Debug, Clone, PartialEq)]
pub struct SplitTableVm {
    /// After "Camera": ground, street, each label some area has, unlabelled
    /// ground and walls.
    pub columns: Vec<String>,
    pub rows: Vec<TableRow>,
    /// Enabled cameras whose split is not worked out yet.
    pub pending: usize,
}

impl SplitTableVm {
    /// `split` gives an enabled camera's split once it is worked out.
    pub fn new<'a>(doc: &Document, split: impl Fn(u32) -> Option<&'a Split>) -> Self {
        let cameras: Vec<(&CameraPlacement, Option<&Split>)> = doc
            .cameras
            .iter()
            .filter(|c| c.enabled)
            .map(|c| (c, split(c.id)))
            .collect();
        let labels: Vec<AreaLabel> = AreaLabel::ALL
            .into_iter()
            .filter(|l| {
                cameras
                    .iter()
                    .filter_map(|(_, s)| *s)
                    .any(|s| s.labels.iter().any(|x| x.label == *l))
            })
            .collect();
        let mut columns = vec!["Ground".to_owned(), "Street".to_owned()];
        columns.extend(labels.iter().map(|l| l.label().to_owned()));
        columns.extend(["Unlabelled".to_owned(), "Walls".to_owned()]);
        let rows = cameras
            .iter()
            .filter_map(|(c, s)| {
                let s = (*s)?;
                let mut cells = vec![Cell::single(s.ground_m2), Cell::single(s.street_m2)];
                cells.extend(labels.iter().map(|l| {
                    s.labels
                        .iter()
                        .find(|x| x.label == *l)
                        .map_or_else(|| Cell::single(0.0), |x| Cell::range(&x.seen))
                }));
                cells.push(Cell::range(&s.unlabelled));
                cells.push(Cell::single(s.walls_m2));
                Some(TableRow {
                    camera_id: c.id,
                    camera: c.label.clone(),
                    cells,
                })
            })
            .collect();
        Self {
            columns,
            rows,
            pending: cameras.iter().filter(|(_, s)| s.is_none()).count(),
        }
    }
}

/// One rung of a verdict's ladder: a detail level under the current
/// lighting, or the other lighting at the selected level.
#[derive(Debug, Clone, PartialEq)]
pub struct VerdictRung {
    /// "Detect", "Custom 40 px/m", or "Night" / "Day".
    pub name: String,
    pub ruled_out: bool,
    /// The current settings.
    pub selected: bool,
    /// "Detect by day, out to 38 m: not ruled out".
    pub tip: String,
}

/// One camera's verdict on one target.
#[derive(Debug, Clone, PartialEq)]
pub struct VerdictVm {
    pub camera_id: u32,
    pub camera: String,
    pub target_id: u32,
    pub target: String,
    /// At the current settings: "not ruled out" or "ruled out: <reason>".
    pub now: String,
    pub ruled_out_now: bool,
    pub ladder: Vec<VerdictRung>,
    /// "nearest point 14 m away".
    pub nearest: String,
}

impl VerdictVm {
    pub fn new(
        params: &CoverageParams,
        camera: &CameraPlacement,
        target: &Target,
        v: &CameraVerdicts,
    ) -> Self {
        let level_name = |level: DoriLevel| match level {
            DoriLevel::Custom => format!("Custom {:.0} px/m", params.pixels_per_metre()),
            l => l.label().to_owned(),
        };
        let light = |l: Lighting| match l {
            Lighting::Day => "by day",
            Lighting::Night => "at night",
        };
        let rung = |name: String, lv: &observation_core::footage::LevelVerdict| VerdictRung {
            name,
            ruled_out: lv.verdict.is_ruled_out(),
            selected: lv.level == params.dori && lv.lighting == params.lighting,
            tip: format!(
                "{} {}, out to {:.1} m: {}",
                level_name(lv.level),
                light(lv.lighting),
                lv.range_m,
                lv.verdict.text()
            ),
        };
        let mut ladder: Vec<VerdictRung> = v
            .verdicts
            .iter()
            .filter(|lv| lv.lighting == params.lighting)
            .map(|lv| rung(level_name(lv.level), lv))
            .collect();
        let other = match params.lighting {
            Lighting::Day => Lighting::Night,
            Lighting::Night => Lighting::Day,
        };
        if let Some(lv) = v
            .verdicts
            .iter()
            .find(|lv| lv.level == params.dori && lv.lighting == other)
        {
            let name = match other {
                Lighting::Day => "Day",
                Lighting::Night => "Night",
            };
            ladder.push(rung(name.to_owned(), lv));
        }
        let now = v.at(params.dori, params.lighting);
        Self {
            camera_id: camera.id,
            camera: camera.label.clone(),
            target_id: target.id,
            target: target_title(target),
            now: now.map_or_else(|| "not worked out".to_owned(), |v| v.text()),
            ruled_out_now: now.is_some_and(|v| v.is_ruled_out()),
            ladder,
            nearest: format!("nearest point {:.0} m away", v.nearest_m),
        }
    }
}

/// One area in the Places tab.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaRowVm {
    pub id: u32,
    pub title: String,
    /// The name as stored, for the name field.
    pub name: String,
    pub label: Option<AreaLabel>,
    /// "Own", …, or "Unlabelled".
    pub label_text: &'static str,
    pub enabled: bool,
    /// "54 m²", the outline's own area as drawn.
    pub area: String,
    /// The coverage figures are limited to it.
    pub limits: bool,
    /// Why the outline cannot count, if it cannot.
    pub problem: Option<&'static str>,
    pub selected: bool,
}

/// One target in the Places tab.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetRowVm {
    pub id: u32,
    pub title: String,
    /// The name as stored, for the name field.
    pub name: String,
    /// "Wall, 1 m square" or "Ground, 3 m²".
    pub kind: String,
    pub selected: bool,
}

/// The Places tab's lists.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacesVm {
    pub areas: Vec<AreaRowVm>,
    pub targets: Vec<TargetRowVm>,
}

impl PlacesVm {
    pub fn new(doc: &Document, editor: &Editor) -> Self {
        let areas = doc
            .areas
            .iter()
            .map(|a| AreaRowVm {
                id: a.id,
                title: area_title(a),
                name: a.name.clone().unwrap_or_default(),
                label: a.label,
                label_text: label_text(a.label),
                enabled: a.enabled,
                area: m2(a.area_m2()),
                limits: doc.limit_figures_to == Some(a.id),
                problem: a.problem().map(RegionProblem::message),
                selected: editor.selected_area == Some(a.id),
            })
            .collect();
        let targets = doc
            .targets
            .iter()
            .map(|t| TargetRowVm {
                id: t.id,
                title: target_title(t),
                name: t.name.clone(),
                kind: match &t.at {
                    TargetPlace::Wall { .. } => {
                        format!("Wall, {WALL_TARGET_SIZE_M:.0} m square")
                    }
                    TargetPlace::Ground { points } => {
                        format!("Ground, {}", m2(outline_area_m2(points)))
                    }
                },
                selected: editor.selected_target == Some(t.id),
            })
            .collect();
        Self { areas, targets }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::footage::{
        AreaSeen, LabelSeen, LabelledArea, LevelVerdict, Reason, Verdict, WallSeen,
    };

    fn range(low: f32, as_drawn: f32, high: f32) -> SeenRange {
        SeenRange {
            low_m2: low,
            as_drawn_m2: as_drawn,
            high_m2: high,
        }
    }

    fn doc() -> Document {
        let mut doc = Document::default();
        let area = |id, label, name: Option<&str>| LabelledArea {
            id,
            label,
            name: name.map(str::to_owned),
            points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 5.0], [0.0, 5.0]],
            enabled: true,
        };
        doc.areas = vec![
            area(1, Some(AreaLabel::Own), Some("Garden")),
            area(2, Some(AreaLabel::Neighbour), None),
            area(3, None, Some("Drive")),
        ];
        doc.limit_figures_to = Some(3);
        doc.add_camera(CameraPlacement {
            id: 0,
            label: "Porch".into(),
            model_id: "m".into(),
            position: [0.0, 3.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        });
        doc
    }

    fn split() -> Split {
        Split {
            ground_m2: 412.2,
            street_m2: 120.0,
            unlabelled: range(200.0, 210.0, 220.4),
            areas: vec![
                AreaSeen {
                    area_id: 1,
                    label: Some(AreaLabel::Own),
                    polygon_m2: 50.0,
                    seen: range(30.0, 36.0, 41.0),
                },
                AreaSeen {
                    area_id: 2,
                    label: Some(AreaLabel::Neighbour),
                    polygon_m2: 50.0,
                    seen: range(0.0, 0.0, 0.2),
                },
                AreaSeen {
                    area_id: 3,
                    label: None,
                    polygon_m2: 50.0,
                    seen: range(5.0, 6.0, 7.0),
                },
            ],
            labels: vec![
                LabelSeen {
                    label: AreaLabel::Own,
                    seen: range(30.0, 36.0, 41.0),
                },
                LabelSeen {
                    label: AreaLabel::Neighbour,
                    seen: range(0.0, 0.0, 0.2),
                },
            ],
            walls_m2: 36.0,
            walls: vec![
                WallSeen {
                    building: 7,
                    distance_m: 21.6,
                    seen_m2: 10.0,
                },
                WallSeen {
                    building: 12,
                    distance_m: 14.2,
                    seen_m2: 26.0,
                },
            ],
        }
    }

    #[test]
    fn ranges_say_as_drawn() {
        assert_eq!(
            range_text(&range(30.0, 36.0, 41.4)),
            "30–41 m² (as drawn 36)"
        );
        assert_eq!(range_text(&range(0.0, 0.0, 0.2)), "0 m² (as drawn)");
        assert_eq!(m2(12.5), "12 m²");
    }

    #[test]
    fn the_split_groups_areas_under_their_labels() {
        let vm = SplitVm::new(&doc(), &split());
        let rows: Vec<(SplitRowKind, &str, &str)> = vm
            .rows
            .iter()
            .map(|r| (r.kind, r.what.as_str(), r.value.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    SplitRowKind::Label(AreaLabel::Own),
                    "Own (all)",
                    "30–41 m² (as drawn 36)"
                ),
                (
                    SplitRowKind::Area(Some(AreaLabel::Own)),
                    "Garden · outline 50 m²",
                    "30–41 m² (as drawn 36)"
                ),
                (
                    SplitRowKind::Label(AreaLabel::Neighbour),
                    "Neighbour (all)",
                    "0 m² (as drawn)"
                ),
                (
                    SplitRowKind::Area(Some(AreaLabel::Neighbour)),
                    "Area 2 · outline 50 m²",
                    "0 m² (as drawn)"
                ),
                (
                    SplitRowKind::Area(None),
                    "Drive · outline 50 m²",
                    "5–7 m² (as drawn 6)"
                ),
                (SplitRowKind::Street, STREET_ROW, "120 m²"),
                (
                    SplitRowKind::Unlabelled,
                    "Unlabelled ground",
                    "200–220 m² (as drawn 210)"
                ),
            ]
        );
        assert_eq!(vm.ground, "Ground seen: 412 m²");
        assert_eq!(vm.walls_total, "Walls seen: 36 m² on 2 buildings");
    }

    #[test]
    fn walls_are_named_by_building_and_distance_nearest_first() {
        let vm = SplitVm::new(&doc(), &split());
        assert_eq!(
            vm.walls,
            [
                WallRow {
                    what: "Building 12 · 14 m away".into(),
                    value: "26 m²".into()
                },
                WallRow {
                    what: "Building 7 · 22 m away".into(),
                    value: "10 m²".into()
                },
            ]
        );
    }

    #[test]
    fn the_table_has_a_column_per_label_in_use() {
        let doc = doc();
        let s = split();
        let vm = SplitTableVm::new(&doc, |_| Some(&s));
        assert_eq!(
            vm.columns,
            [
                "Ground",
                "Street",
                "Own",
                "Neighbour",
                "Unlabelled",
                "Walls"
            ]
        );
        let [row] = vm.rows.as_slice() else {
            panic!("one camera")
        };
        assert_eq!(row.camera, "Porch");
        let cells: Vec<(&str, Option<&str>)> = row
            .cells
            .iter()
            .map(|c| (c.main.as_str(), c.sub.as_deref()))
            .collect();
        assert_eq!(
            cells,
            [
                ("412", None),
                ("120", None),
                ("30–41", Some("as drawn 36")),
                ("0", Some("as drawn 0")),
                ("200–220", Some("as drawn 210")),
                ("36", None),
            ]
        );
        assert_eq!(vm.pending, 0);
        let waiting = SplitTableVm::new(&doc, |_| None);
        assert_eq!((waiting.rows.len(), waiting.pending), (0, 1));
        assert_eq!(waiting.columns, ["Ground", "Street", "Unlabelled", "Walls"]);
    }

    fn verdicts() -> CameraVerdicts {
        let lv = |level, lighting, range_m, verdict| LevelVerdict {
            level,
            lighting,
            range_m,
            verdict,
        };
        use DoriLevel::*;
        use Lighting::*;
        let out = Verdict::RuledOut(Reason::OutOfRange);
        CameraVerdicts {
            camera_id: 1,
            nearest_m: 9.6,
            verdicts: vec![
                lv(Detect, Day, 38.4, Verdict::NotRuledOut),
                lv(Detect, Night, 7.5, out),
                lv(Observe, Day, 15.5, Verdict::NotRuledOut),
                lv(Observe, Night, 7.5, out),
                lv(Recognise, Day, 7.7, out),
                lv(Recognise, Night, 7.5, out),
                lv(Identify, Day, 3.8, out),
                lv(Identify, Night, 3.8, out),
            ],
        }
    }

    #[test]
    fn a_verdict_reads_one_sided_with_its_ladder() {
        let doc = doc();
        let target = Target {
            id: 4,
            name: "Front door".into(),
            at: TargetPlace::Wall {
                point: [0.0, 1.5, 0.0],
                normal: [0.0, 1.0],
            },
        };
        let vm = VerdictVm::new(&doc.params, &doc.cameras[0], &target, &verdicts());
        assert_eq!(
            (vm.now.as_str(), vm.ruled_out_now, vm.target.as_str()),
            ("not ruled out", false, "Front door")
        );
        let ladder: Vec<(&str, bool, bool)> = vm
            .ladder
            .iter()
            .map(|r| (r.name.as_str(), r.ruled_out, r.selected))
            .collect();
        assert_eq!(
            ladder,
            [
                ("Detect", false, true),
                ("Observe", false, false),
                ("Recognise", true, false),
                ("Identify", true, false),
                ("Night", true, false),
            ]
        );
        assert_eq!(
            vm.ladder[0].tip,
            "Detect by day, out to 38.4 m: not ruled out"
        );
        assert_eq!(
            vm.ladder[4].tip,
            "Detect at night, out to 7.5 m: ruled out: out of range"
        );
        assert_eq!(vm.nearest, "nearest point 10 m away");
        for text in [&vm.now, &vm.ladder[0].tip, &vm.ladder[4].tip] {
            assert!(!text.contains("seen"), "one-sided: {text}");
        }

        // At night, by Recognise: the levels at night, then by day.
        let mut params = doc.params;
        params.lighting = Lighting::Night;
        params.dori = DoriLevel::Recognise;
        let vm = VerdictVm::new(&params, &doc.cameras[0], &target, &verdicts());
        assert_eq!(vm.now, "ruled out: out of range");
        let names: Vec<&str> = vm.ladder.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Detect", "Observe", "Recognise", "Identify", "Day"]);
        assert!(vm.ladder[2].selected && vm.ladder[4].ruled_out);
    }

    #[test]
    fn the_places_lists_name_label_and_measure() {
        let mut doc = doc();
        doc.areas[1].enabled = false;
        doc.areas[2].points.truncate(2);
        doc.targets = vec![
            Target {
                id: 1,
                name: String::new(),
                at: TargetPlace::Ground {
                    points: vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.5], [0.0, 1.5]],
                },
            },
            Target {
                id: 2,
                name: "Door".into(),
                at: TargetPlace::Wall {
                    point: [0.0; 3],
                    normal: [1.0, 0.0],
                },
            },
        ];
        let editor = Editor {
            selected_area: Some(2),
            selected_target: Some(2),
            ..Editor::default()
        };
        let vm = PlacesVm::new(&doc, &editor);
        let areas: Vec<_> = vm
            .areas
            .iter()
            .map(|a| {
                (
                    a.title.as_str(),
                    a.label_text,
                    a.enabled,
                    a.area.as_str(),
                    a.limits,
                    a.selected,
                )
            })
            .collect();
        assert_eq!(
            areas,
            [
                ("Garden", "Own", true, "50 m²", false, false),
                ("Area 2", "Neighbour", false, "50 m²", false, true),
                ("Drive", "Unlabelled", true, "0 m²", true, false),
            ]
        );
        assert_eq!(vm.areas[0].problem, None);
        assert!(vm.areas[2].problem.is_some(), "two corners");
        let targets: Vec<_> = vm
            .targets
            .iter()
            .map(|t| (t.title.as_str(), t.kind.as_str(), t.selected))
            .collect();
        assert_eq!(
            targets,
            [
                ("Target 1", "Ground, 3 m²", false),
                ("Door", "Wall, 1 m square", true),
            ]
        );
    }
}
