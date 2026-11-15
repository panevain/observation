use super::*;
use crate::coverage::backend::CoverageBackend;
use crate::editor::Panels;
use crate::testing::{FakeClock, FakeFiles, FakeSheetOutput, ScriptedJobChannel};
use observation_core::bundle::{OsmBuilding, OsmRoad, build_bundle};
use observation_core::camera::{CameraKind, CameraModel, CameraPlacement, Catalog};
use observation_core::geo::{BBox, LatLon};
use observation_core::scene::AreaRef;
use observation_core::terrain::Terrain;
use std::sync::{Arc, Mutex};

/// A street with one house, one camera on it and the coverage counted.
struct Scene {
    bundle: AreaBundle,
    layers: StaticLayers,
    doc: Document,
    models: Models,
    coverage: CoverageState,
    backend: CoverageBackend,
}

impl Scene {
    fn new() -> Self {
        let bbox = BBox::around(LatLon::new(51.5, -0.12), 120.0);
        let c = bbox.center();
        let d = 0.0001;
        let at = |lat: f64, lon: f64| LatLon::new(c.lat + lat, c.lon + lon);
        let house = OsmBuilding {
            osm_id: 1,
            outer: vec![at(0.0, 0.0), at(0.0, d), at(d, d), at(d, 0.0), at(0.0, 0.0)],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.0,
            address: None,
        };
        let road = OsmRoad {
            osm_id: 2,
            highway: "residential".into(),
            name: Some("Test Street".into()),
            width_m: 6.0,
            points: vec![at(-d, -4.0 * d), at(-d, 4.0 * d)],
        };
        let bundle = build_bundle(
            bbox,
            &[house],
            &[road],
            &[],
            Terrain::default(),
            "© OpenStreetMap contributors",
            1_794_000_000,
            Some(1_793_990_000),
        );
        let layers = StaticLayers::new(&bundle, 0.5, 1.0);
        let mut models = Models::new(Catalog::default());
        models.custom.push(CameraModel {
            id: "custom-test".into(),
            vendor: String::new(),
            name: "Test".into(),
            kind: CameraKind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 60.0,
            resolution: [1920, 1080],
            ir_range_m: Some(8.0),
            source: None,
            notes: None,
        });
        let mut doc = Document::default();
        doc.area = Some(AreaRef::Demo);
        // On the house's south wall, facing the road.
        doc.add_camera(CameraPlacement {
            id: 0,
            label: "Porch at number 12".into(),
            model_id: "custom-test".into(),
            position: [5.0, 3.0, 0.5],
            yaw_deg: 180.0,
            pitch_deg: -15.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        });
        Self {
            bundle,
            layers,
            doc,
            models,
            coverage: CoverageState::default(),
            backend: CoverageBackend::default(),
        }
    }

    fn count(&mut self) {
        crate::coverage::update(
            &mut self.coverage,
            &self.doc,
            &self.models,
            &self.layers,
            Vec::new(),
            &mut self.backend,
        );
    }

    fn sheet(&self, tier: Tier) -> Result<SheetSvg, String> {
        build_sheet(
            &self.doc,
            &self.models,
            &self.coverage,
            &self.bundle,
            &self.layers,
            1.0,
            "Front of house",
            1_794_009_000,
            tier,
        )
    }
}

#[test]
fn a_sheet_waits_for_an_area_and_for_the_coverage() {
    let mut s = Scene::new();
    assert_eq!(
        not_ready(&s.doc, &s.models, &s.coverage, false).as_deref(),
        Some("Load an area first.")
    );
    let waiting = not_ready(&s.doc, &s.models, &s.coverage, true).expect("not counted yet");
    assert!(waiting.contains("still being worked out for camera 1"));
    assert_eq!(s.sheet(Tier::Aggregate), Err(waiting));
    s.count();
    assert_eq!(not_ready(&s.doc, &s.models, &s.coverage, true), None);
    // A camera without a model is left out, as the coverage leaves it out.
    s.doc.cameras[0].model_id = "gone".into();
    assert_eq!(not_ready(&s.doc, &s.models, &s.coverage, true), None);
}

#[test]
fn a_sheet_waits_while_a_moved_camera_is_worked_out_again() {
    let mut s = Scene::new();
    s.count();
    assert!(s.sheet(Tier::Aggregate).is_ok());
    // The coverage now goes to a worker that has not answered yet: the
    // camera moves, and its sight lines are still those of where it was.
    s.backend = CoverageBackend::new(
        Some(Box::new(ScriptedJobChannel::default())),
        Arc::new(FakeClock::at(0.0)),
    );
    s.doc.cameras[0].position[0] += 3.0;
    s.count();
    assert!(s.backend.busy(1), "the job is out");
    assert!(
        s.coverage.sight(1).is_some(),
        "the old sight lines are kept"
    );
    let waiting = not_ready(&s.doc, &s.models, &s.coverage, true).expect("not current");
    assert_eq!(
        waiting,
        "Coverage is still being worked out for camera 1; the sheet can be made once it is done."
    );
    assert_eq!(s.sheet(Tier::Exact), Err(waiting));
    // Another model with other fields of view needs new sight lines too.
    s.backend = CoverageBackend::default();
    s.count();
    assert_eq!(not_ready(&s.doc, &s.models, &s.coverage, true), None);
    s.models.custom[0].hfov_deg = 60.0;
    assert!(not_ready(&s.doc, &s.models, &s.coverage, true).is_some());
    s.count();
    assert_eq!(not_ready(&s.doc, &s.models, &s.coverage, true), None);
    assert!(s.sheet(Tier::Exact).is_ok());
}

#[test]
fn the_tier_chosen_decides_what_the_sheet_shows() {
    let mut s = Scene::new();
    s.count();
    assert_eq!(Panels::default().sheet_tier, Tier::Aggregate);
    let shared = s.sheet(Panels::default().sheet_tier).expect("a sheet");
    assert!(shared.svg.contains("Aggregate sheet"));
    assert!(!shared.svg.contains("<path") && !shared.svg.contains("Porch at number 12"));
    assert!(!shared.svg.contains("Test Street"));
    let own = s.sheet(Tier::Exact).expect("a sheet");
    assert!(own.svg.contains("Exact sheet") && own.svg.contains("<path"));
    assert!(own.svg.contains("Porch at number 12"));
    // The figures are the coverage's own: the street seen at the level shown.
    let seen = thousands(s.coverage.stats.street_seen_m2);
    assert!(s.coverage.stats.street_seen_m2 > 0.0);
    assert!(shared.svg.contains(&format!(">{seen} of ")), "{seen}");
}

fn thousands(v: f32) -> String {
    let n = v.round() as u64;
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[test]
fn sheet_files_are_named_after_the_scene_date_and_tier() {
    assert_eq!(
        sheet_file_stem("Front of house", 1_794_009_000, Tier::Aggregate),
        "Front of house evidence 2026-11-06 (aggregate)"
    );
    assert_eq!(
        sheet_file_stem("12/14 High St", 0, Tier::Exact),
        "12-14 High St evidence 1970-01-01 (exact)"
    );
    assert_eq!(
        sheet_file_stem(" . ", 0, Tier::Aggregate),
        "observation evidence 1970-01-01 (aggregate)"
    );
}

#[test]
fn each_format_goes_to_its_port_and_failures_are_reported() {
    let mut s = Scene::new();
    s.count();
    let sheet = s.sheet(Tier::Aggregate).expect("a sheet");
    let files = FakeFiles::default();
    let out = FakeSheetOutput::default();
    let results = Arc::new(Mutex::new(Vec::new()));
    let run = |format: SheetFormat| {
        let r = results.clone();
        export_sheet(
            format,
            &sheet,
            "Front of house evidence",
            &files,
            &out,
            Box::new(move |res| r.lock().expect("lock").push(res)),
        );
    };
    for f in SheetFormat::ALL {
        run(f);
    }
    assert_eq!(
        files.downloads(),
        [(
            "Front of house evidence.svg".to_owned(),
            SHEET_MIME.to_owned(),
            sheet.svg.clone()
        )]
    );
    assert_eq!(
        out.pngs(),
        [("Front of house evidence.png".to_owned(), sheet.pages)]
    );
    assert_eq!(out.prints(), [sheet.pages]);
    assert_eq!(*results.lock().expect("lock"), [Ok(()), Ok(()), Ok(())]);

    results.lock().expect("lock").clear();
    files.refuse_downloads(Some("disk full"));
    out.refuse(Some("no canvas"));
    for f in SheetFormat::ALL {
        run(f);
    }
    let got = results.lock().expect("lock").clone();
    assert_eq!(
        got,
        [
            Err("disk full".to_owned()),
            Err("no canvas".to_owned()),
            Err("no canvas".to_owned())
        ]
    );
    assert_eq!(
        failure_text(SheetFormat::Png, "no canvas"),
        "Could not make the PNG: no canvas"
    );
    for f in SheetFormat::ALL {
        assert!(!f.label().is_empty() && !f.tip().is_empty());
    }
}
