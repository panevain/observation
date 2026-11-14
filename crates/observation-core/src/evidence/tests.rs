use super::render::metres;
use super::*;
use crate::camera::{
    ASSUMED_IR_RANGE_M, CameraKind, DoriLevel, Lighting, MAX_FOV_DEG, MAX_RANGE_M,
};
use crate::coverage::{CoverageCounts, CoverageStats, RegionMask};
use crate::example::example_cameras;
use crate::footage::{AreaLabel, TargetPlace};
use crate::frustum::ViewCone;
use crate::osm::tags::{LANE_WIDTH_M, ROAD_WIDTHS, WIDTH_TAG_M};
use crate::provenance::{CAVEATS, CAVEATS_BOUND, CAVEATS_VERSION};
use crate::testing::rich_bundle;

fn model(id: &str, hfov: f32, width: u32, ir: Option<f32>) -> CameraModel {
    CameraModel {
        id: id.to_owned(),
        vendor: "Acme".to_owned(),
        name: id.to_owned(),
        kind: CameraKind::Doorbell,
        hfov_deg: hfov,
        vfov_deg: hfov * 0.6,
        resolution: [width, width * 9 / 16],
        ir_range_m: ir,
        source: None,
        notes: None,
    }
}

/// The rich fixture with three example cameras (one disabled), an area of
/// each kind of interest, two targets, and every camera's sight lines.
struct Fixture {
    bundle: AreaBundle,
    layers: StaticLayers,
    models: Vec<CameraModel>,
    cameras: Vec<CameraPlacement>,
    sights: Vec<Sight>,
    areas: Vec<LabelledArea>,
    targets: Vec<Target>,
    params: CoverageParams,
    provenance: Provenance,
}

impl Fixture {
    fn new() -> Self {
        let bundle = rich_bundle();
        let layers = StaticLayers::new(&bundle, 0.5, 1.0);
        let models = vec![
            model("wide", 110.0, 1920, None),
            model("narrow", 60.0, 2560, Some(12.0)),
        ];
        let refs: Vec<&CameraModel> = models.iter().collect();
        let mut cameras = example_cameras(&bundle, &refs, 2);
        assert_eq!(cameras.len(), 2, "the fixture places cameras");
        // One on a pole south of building 101, facing its south wall, and a
        // disabled one.
        let mut pole = cameras[0].clone();
        pole.position = [-16.3, 3.1, 14.7];
        (pole.yaw_deg, pole.pitch_deg) = (0.0, -8.0);
        pole.model_id = "wide".to_owned();
        let mut off = cameras[1].clone();
        off.enabled = false;
        cameras.extend([pole, off]);
        for (i, c) in cameras.iter_mut().enumerate() {
            c.id = i as u32 + 1;
            c.label = format!("Porch of the yellow house {}", i + 1);
        }
        let params = CoverageParams::default();
        let sights = cameras
            .iter()
            .map(|c| {
                let m = models.iter().find(|m| m.id == c.model_id).expect("model");
                layers.sight(
                    &ViewCone::new(c, m, params.sight_range_m(m)),
                    params.subject_height_m,
                )
            })
            .collect();
        let c0 = cameras[0].position;
        let square = |x: f32, z: f32, s: f32| vec![[x, z], [x + s, z], [x + s, z + s], [x, z + s]];
        let areas = vec![
            LabelledArea {
                id: 1,
                label: Some(AreaLabel::Own),
                name: Some("My front garden".to_owned()),
                points: square(c0[0] - 6.0, c0[2] - 6.0, 12.0),
                enabled: true,
            },
            LabelledArea {
                id: 2,
                label: Some(AreaLabel::Neighbour),
                name: None,
                points: square(-60.0, -50.0, 40.0),
                enabled: true,
            },
        ];
        let targets = vec![
            Target {
                id: 1,
                name: "Side gate".to_owned(),
                at: TargetPlace::Ground {
                    points: square(c0[0] + 2.0, c0[2] + 2.0, 2.0),
                },
            },
            Target {
                id: 2,
                name: String::new(),
                at: TargetPlace::Ground {
                    points: square(50.0, 40.0, 2.0),
                },
            },
        ];
        let provenance = Provenance::of(&bundle, "0123456789abcdef");
        Self {
            bundle,
            layers,
            models,
            cameras,
            sights,
            areas,
            targets,
            params,
            provenance,
        }
    }

    fn model(&self, c: &CameraPlacement) -> &CameraModel {
        self.models
            .iter()
            .find(|m| m.id == c.model_id)
            .expect("model")
    }

    fn sheet_cameras(&self) -> Vec<SheetCamera<'_>> {
        self.cameras
            .iter()
            .zip(&self.sights)
            .map(|(c, s)| SheetCamera {
                placement: c,
                model: self.model(c),
                sight: s,
            })
            .collect()
    }

    fn sheet(&self, tier: Tier, limit: Option<u32>, log: &[AgentChange]) -> SheetSvg {
        let cameras = self.sheet_cameras();
        let inputs = SheetInputs {
            title: "Oak Close <draft> & notes",
            generated_unix: 1_794_000_000,
            bundle: &self.bundle,
            layers: &self.layers,
            facade_cell_m: 1.0,
            provenance: &self.provenance,
            params: self.params,
            cameras: &cameras,
            areas: &self.areas,
            limit_figures_to: limit,
            targets: &self.targets,
            agent_log: log,
        };
        evidence_sheet(&inputs, tier).expect("sheet")
    }

    /// Enabled cameras' models and sight lines.
    fn enabled(&self) -> Vec<(&CameraModel, &Sight)> {
        self.cameras
            .iter()
            .zip(&self.sights)
            .filter(|(c, _)| c.enabled)
            .map(|(c, s)| (self.model(c), s))
            .collect()
    }
}

/// The text of every `<text>` element, unescaped, joined by spaces: what a
/// reader of the sheet sees, in the order it was written.
fn words(svg: &str) -> String {
    let mut out = Vec::new();
    for part in svg.split("<text").skip(1) {
        let Some(start) = part.find('>') else {
            continue;
        };
        let Some(end) = part.find("</text>") else {
            continue;
        };
        out.push(
            part[start + 1..end]
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .replace("&amp;", "&"),
        );
    }
    out.join(" ")
}

/// Whether `text` names a building by its number ("building 12").
fn numbered_building(text: &str) -> bool {
    text.match_indices("building ").any(|(i, m)| {
        text[i + m.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    })
}

#[test]
fn the_aggregate_sheet_gives_no_position_away() {
    let f = Fixture::new();
    let svg = f.sheet(Tier::Aggregate, None, &[]).svg;
    // No drawing of any shape: no map, no coverage raster, no footprints,
    // no camera markers, no embedded image.
    for tag in [
        "<path",
        "<polygon",
        "<polyline",
        "<circle",
        "<image",
        "clip-path",
        "data:",
        "transform=\"translate",
    ] {
        assert!(!svg.contains(tag), "the Aggregate sheet has {tag}");
    }
    // Only text, rules and plain rectangles.
    for el in svg.split('<').skip(1) {
        let name: String = el
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        assert!(
            ["", "svg", "title", "desc", "rect", "text", "line"].contains(&name.as_str()),
            "unexpected element <{name}"
        );
    }
    let text = words(&svg);
    // No camera position, in any form a map or a table would print it.
    for c in &f.cameras {
        let [x, _, z] = c.position;
        for (a, b) in [(x, z), (z, x)] {
            for d in [0, 1, 2] {
                let (a, b) = (super::svg::num(a, d), super::svg::num(b, d));
                for pair in [format!("{a},{b}"), format!("{a} {b}"), format!("{a}, {b}")] {
                    assert!(!svg.contains(&pair), "camera position {pair} printed");
                }
            }
        }
        assert!(!text.contains(&c.label), "camera label printed");
    }
    // No addresses or street names, no building numbers or distances.
    for word in ["Equator Road", "Ring House", "Back Lane", " m away"] {
        assert!(!text.contains(word), "{word:?} printed");
    }
    assert!(!numbered_building(&text), "a building by its number");
    // The figures and the user's own names are there.
    assert!(text.contains("My front garden"));
    assert!(text.contains("Side gate"));
    assert!(text.contains("Target 2"));
    assert!(text.contains("Camera 1") && text.contains("Camera 2"));
    assert!(
        !text.contains("Camera 4"),
        "the disabled camera is left out"
    );
    assert!(text.contains("no camera positions"));
}

#[test]
fn the_aggregate_sheet_does_not_depend_on_what_it_leaves_out() {
    let f = Fixture::new();
    let before = f.sheet(Tier::Aggregate, None, &[]).svg;
    let mut g = Fixture::new();
    for c in &mut g.cameras {
        c.label = "Elsewhere".to_owned();
        c.group = Some("12 Somewhere Road".to_owned());
    }
    for b in &mut g.bundle.buildings {
        b.address = None;
    }
    for r in &mut g.bundle.roads {
        r.name = Some("Renamed Street".to_owned());
    }
    assert_eq!(g.sheet(Tier::Aggregate, None, &[]).svg, before);
}

#[test]
fn the_exact_sheet_shows_the_map_and_says_so() {
    let f = Fixture::new();
    let sheet = f.sheet(Tier::Exact, None, &[]);
    let svg = &sheet.svg;
    for tag in ["<path", "<polyline", "<polygon", "<circle", "clip-path"] {
        assert!(svg.contains(tag), "the Exact sheet lacks {tag}");
    }
    let text = words(svg);
    assert!(text.contains("SHOWS CAMERA POSITIONS"));
    assert!(text.contains("shows camera positions"));
    assert!(text.contains("ESTIMATE · modelled from map data, not recorded footage"));
    assert!(
        text.contains(&f.cameras[0].label),
        "labels on the owner's sheet"
    );
    assert!(numbered_building(&text), "walls per building");
    // Still no addresses.
    for word in ["Equator Road", "Ring House", "Back Lane"] {
        assert!(!text.contains(word), "{word:?} printed");
    }
    // The map's stamps sit inside the map's frame.
    let aggregate = f.sheet(Tier::Aggregate, None, &[]);
    assert!(sheet.svg.len() > aggregate.svg.len());
}

#[test]
fn sheets_are_deterministic_and_escaped() {
    let f = Fixture::new();
    for tier in Tier::ALL {
        let a = f.sheet(tier, Some(1), &[]);
        let b = f.sheet(tier, Some(1), &[]);
        assert_eq!(a, b, "{tier:?}");
        assert!(a.pages >= 2, "{tier:?}: {} pages", a.pages);
        let height = super::svg::num(a.pages as f32 * PAGE_HEIGHT, 0);
        assert!(
            a.svg
                .starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"794\"")
        );
        assert!(a.svg.contains(&format!("height=\"{height}\"")));
        assert!(a.svg.ends_with("</svg>\n"));
        assert!(a.svg.contains("Oak Close &lt;draft&gt; &amp; notes"));
        assert!(!a.svg.contains("<draft>"));
        assert!(words(&a.svg).contains(&format!("Page {} of {}", a.pages, a.pages)));
    }
}

#[test]
fn the_grid_agrees_with_the_coverage_figures_at_every_level() {
    let f = Fixture::new();
    for limit in [None, Some(&f.areas[1])] {
        let region = limit.map(|a| RegionMask::new(&f.layers, &a.points));
        for lighting in [Lighting::Day, Lighting::Night] {
            let params = CoverageParams {
                dori: DoriLevel::Custom,
                custom_ppm: 40.0,
                lighting,
                ..CoverageParams::default()
            };
            // Sight lines long enough for the lower Custom density.
            let sights: Vec<Sight> = f
                .cameras
                .iter()
                .map(|c| {
                    let m = f.model(c);
                    f.layers.sight(
                        &ViewCone::new(c, m, params.sight_range_m(m)),
                        params.subject_height_m,
                    )
                })
                .collect();
            let cams: Vec<(&CameraModel, &Sight)> = f
                .cameras
                .iter()
                .zip(&sights)
                .filter(|(c, _)| c.enabled)
                .map(|(c, s)| (f.model(c), s))
                .collect();
            let grid = detail_grid(&f.layers, &f.bundle.roads, region.as_ref(), &params, &cams);
            assert_eq!(grid.rows.len(), 5, "four levels and Custom");
            for row in &grid.rows {
                for l in [Lighting::Day, Lighting::Night] {
                    // Directly: each enabled camera's visibility at its range.
                    let mut counts = CoverageCounts::new(&f.layers);
                    for c in f.cameras.iter().filter(|c| c.enabled) {
                        let m = f.model(c);
                        let cone = ViewCone::new(c, m, m.effective_range_m(row.ppm, l));
                        counts.add(&f.layers.visibility(&cone, params.subject_height_m));
                    }
                    let want = CoverageStats::compute_in(&f.layers, &counts, region.as_ref());
                    let got = row.at(l).stats;
                    assert_eq!(got.street_m2, want.street_m2);
                    assert_eq!(
                        got.street_seen_m2, want.street_seen_m2,
                        "{:?} {l:?}",
                        row.level
                    );
                    assert_eq!(got.street_seen_multi_m2, want.street_seen_multi_m2);
                    assert_eq!(got.open_ground_seen_m2, want.open_ground_seen_m2);
                    assert!((got.facade_m2 - want.facade_m2).abs() < 1e-2);
                    assert!((got.facade_seen_m2 - want.facade_seen_m2).abs() < 1e-2);
                    let kinds = row.at(l).street_seen;
                    let sum = kinds.carriageway_m2 + kinds.footpath_service_m2;
                    assert!((sum - got.street_seen_m2).abs() < 1e-3);
                }
                // Higher detail never sees more; night never more than day.
                assert!(row.night.stats.street_seen_m2 <= row.day.stats.street_seen_m2);
            }
            let total = grid.street.carriageway_m2 + grid.street.footpath_service_m2;
            assert!((total - grid.rows[0].day.stats.street_m2).abs() < 1e-3);
            assert!(grid.street.carriageway_m2 > 0.0 || limit.is_some());
        }
    }
}

#[test]
fn the_street_is_split_by_road_kind() {
    let f = Fixture::new();
    let grid = detail_grid(&f.layers, &f.bundle.roads, None, &f.params, &f.enabled());
    // The fixture has a residential road, a footway and a service lane.
    assert!(grid.street.carriageway_m2 > 0.0);
    assert!(grid.street.footpath_service_m2 > 0.0);
}

#[test]
fn a_camera_whose_sight_lines_fall_short_stops_the_sheet() {
    let f = Fixture::new();
    let mut short = f.sights.clone();
    short[1].range = 1.0;
    let cameras: Vec<SheetCamera> = f
        .cameras
        .iter()
        .zip(&short)
        .map(|(c, s)| SheetCamera {
            placement: c,
            model: f.model(c),
            sight: s,
        })
        .collect();
    let inputs = SheetInputs {
        title: "x",
        generated_unix: 0,
        bundle: &f.bundle,
        layers: &f.layers,
        facade_cell_m: 1.0,
        provenance: &f.provenance,
        params: f.params,
        cameras: &cameras,
        areas: &[],
        limit_figures_to: None,
        targets: &[],
        agent_log: &[],
    };
    assert_eq!(
        evidence_sheet(&inputs, Tier::Aggregate),
        Err(SheetError::NotComputed {
            camera_id: f.cameras[1].id
        })
    );
}

#[test]
fn the_methodology_is_written_from_the_constants() {
    let params = CoverageParams {
        subject_height_m: 1.65,
        ..CoverageParams::default()
    };
    let text: String = methodology(&params, 0.5, 1.0, &[])
        .iter()
        .flat_map(|s| std::iter::once(s.heading.clone()).chain(s.paragraphs.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    for (types, w) in ROAD_WIDTHS {
        let group = format!("{} ({w} m)", types.join(", "));
        assert!(text.contains(&group), "lacks {group}");
    }
    for v in [
        format!("{LANE_WIDTH_M} m a lane"),
        format!("below {} m", WIDTH_TAG_M.end),
        format!("at most {MAX_FOV_DEG}°"),
        format!("more than {MAX_RANGE_M} m"),
        format!("{ASSUMED_IR_RANGE_M} m where none"),
        format!("{} m above the ground", metres(params.subject_height_m)),
        "every 0.5 m square cell".to_owned(),
        format!("caveats version {CAVEATS_VERSION}"),
    ] {
        assert!(text.contains(&v), "lacks {v:?}");
    }
    for l in DoriLevel::ALL {
        if let (Some(ppm), Some(reading)) = (l.pixels_per_metre(), l.reading()) {
            let v = format!("{} {ppm} px/m ({reading})", l.label());
            assert!(text.contains(&v), "lacks {v:?}");
        }
    }
    for c in CAVEATS {
        assert!(text.contains(c));
    }
    assert!(text.contains(CAVEATS_BOUND));
    assert!(
        !text.contains(AGENT_LOG_HEADING),
        "no agent log, no section"
    );
}

#[test]
fn the_sheet_prints_the_methodology_caveats_and_settings() {
    let f = Fixture::new();
    let svg = f.sheet(Tier::Aggregate, Some(1), &[]).svg;
    let text = words(&svg);
    for s in methodology(&f.params, 0.5, 1.0, &[]) {
        assert!(text.contains(&s.heading), "lacks {}", s.heading);
        for p in s.paragraphs {
            let p = p.strip_prefix("• ").map_or(p.clone(), |r| format!("• {r}"));
            let body = p.trim_start_matches("• ");
            assert!(text.contains(body), "lacks {body:?}");
        }
    }
    let pv = &f.provenance;
    for v in [
        crate::provenance::format_utc_minute(1_794_000_000),
        crate::provenance::format_utc_minute(pv.osm_data_unix.expect("data date")),
        pv.osm_fingerprint.clone().expect("fingerprint"),
        pv.catalog_version.clone(),
        format!("Observation {}", crate::provenance::ENGINE_VERSION),
        "Detect (25 px/m)".to_owned(),
        "by day".to_owned(),
        "1.0 m above the ground".to_owned(),
        "My front garden (Own, outline 144 m² as drawn)".to_owned(),
        "Elevation: golden fixture".to_owned(),
        "© OpenStreetMap contributors (ODbL)".to_owned(),
        "ESTIMATE · modelled, not recorded footage".to_owned(),
    ] {
        assert!(text.contains(&v), "lacks {v:?}");
    }
    // Every level of the grid, each with a reading.
    for l in &DoriLevel::ALL[..4] {
        assert!(text.contains(l.reading().expect("reading")));
    }
}

#[test]
fn the_agent_log_is_listed_when_there_is_one() {
    let f = Fixture::new();
    let log = [AgentChange {
        accepted_unix: 1_793_999_000,
        summary: "Turned camera 2 away from the neighbour's garden".to_owned(),
    }];
    let text = words(&f.sheet(Tier::Aggregate, None, &log).svg);
    assert!(text.contains(AGENT_LOG_HEADING));
    assert!(text.contains("Turned camera 2 away from the neighbour's garden"));
}

#[test]
fn tiers_say_what_they_hold() {
    assert_eq!(Tier::default(), Tier::Aggregate);
    assert!(!Tier::Aggregate.shows_positions() && Tier::Exact.shows_positions());
    for t in Tier::ALL {
        assert!(!t.summary().is_empty() && !t.badge().is_empty());
        assert_eq!(t.key(), t.label().to_lowercase());
    }
}
