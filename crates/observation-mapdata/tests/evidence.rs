//! Golden evidence sheets (issue #91) on the demo street, one per tier.
//!
//! Each test composes a sheet from the committed demo fixtures, with fixed
//! camera models (not the catalogue, so that catalogue edits do not touch
//! these files), and compares it byte for byte with a file under
//! `tests/golden/`. A failure means the sheet changed: if that is intended,
//! regenerate the files with
//!
//! ```sh
//! OBS_BLESS=1 cargo nextest run -p observation-mapdata --test evidence
//! ```
//!
//! and review the diff (open the SVGs in a browser too).

use observation_core::bundle::AreaBundle;
use observation_core::camera::{CameraKind, CameraModel, CameraPlacement};
use observation_core::coverage::{CoverageParams, Sight, StaticLayers};
use observation_core::evidence::{SheetCamera, SheetInputs, Tier, evidence_sheet};
use observation_core::example::example_cameras;
use observation_core::footage::{AreaLabel, LabelledArea, Target, TargetPlace};
use observation_core::frustum::ViewCone;
use observation_core::provenance::Provenance;
use observation_mapdata::demo::demo_bundle;
use std::path::{Path, PathBuf};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
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

fn model(id: &str, kind: CameraKind, hfov: f32, res: [u32; 2], ir: Option<f32>) -> CameraModel {
    CameraModel {
        id: id.to_owned(),
        vendor: "Test".to_owned(),
        name: id.to_owned(),
        kind,
        hfov_deg: hfov,
        vfov_deg: hfov * 0.56,
        resolution: res,
        ir_range_m: ir,
        source: None,
        notes: None,
    }
}

struct Demo {
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

fn demo() -> Demo {
    let bundle = demo_bundle(
        observation_mapdata::OSM_ATTRIBUTION,
        "Elevation: USGS 3DEP via the demo fixtures",
        1_794_000_000,
    )
    .expect("demo street");
    let layers = StaticLayers::new(&bundle, 0.5, 1.0);
    let models = vec![
        model("doorbell", CameraKind::Doorbell, 150.0, [1536, 1536], None),
        model("bullet", CameraKind::Bullet, 90.0, [3840, 2160], Some(30.0)),
        model(
            "floodlight",
            CameraKind::Floodlight,
            130.0,
            [1920, 1080],
            Some(9.0),
        ),
    ];
    let refs: Vec<&CameraModel> = models.iter().collect();
    let mut cameras = example_cameras(&bundle, &refs, 4);
    for (i, c) in cameras.iter_mut().enumerate() {
        c.id = i as u32 + 1;
        c.label = format!("House {} front", i + 1);
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
    let [x, _, z] = cameras[0].position;
    let square = |x: f32, z: f32, s: f32| vec![[x, z], [x + s, z], [x + s, z + s], [x, z + s]];
    let areas = vec![
        LabelledArea {
            id: 1,
            label: Some(AreaLabel::Own),
            name: Some("Front garden".to_owned()),
            points: square(x - 8.0, z - 8.0, 16.0),
            enabled: true,
        },
        LabelledArea {
            id: 2,
            label: Some(AreaLabel::Neighbour),
            name: Some("Next door".to_owned()),
            points: square(x + 8.0, z - 8.0, 16.0),
            enabled: true,
        },
    ];
    let targets = vec![Target {
        id: 1,
        name: "Path to the door".to_owned(),
        at: TargetPlace::Ground {
            points: square(x + 1.0, z + 2.0, 3.0),
        },
    }];
    let provenance = Provenance::of(&bundle, "c47a10900000f00d");
    Demo {
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

fn sheet(d: &Demo, tier: Tier) -> String {
    let cameras: Vec<SheetCamera> = d
        .cameras
        .iter()
        .zip(&d.sights)
        .map(|(c, s)| SheetCamera {
            placement: c,
            model: d.models.iter().find(|m| m.id == c.model_id).expect("model"),
            sight: s,
        })
        .collect();
    let inputs = SheetInputs {
        title: "Demo street",
        generated_unix: 1_794_003_600,
        bundle: &d.bundle,
        layers: &d.layers,
        facade_cell_m: 1.0,
        provenance: &d.provenance,
        params: d.params,
        cameras: &cameras,
        areas: &d.areas,
        limit_figures_to: None,
        targets: &d.targets,
        agent_log: &[],
    };
    evidence_sheet(&inputs, tier).expect("sheet").svg
}

#[test]
fn aggregate_sheet_on_the_demo_street() {
    let d = demo();
    let svg = sheet(&d, Tier::Aggregate);
    // No drawn shapes and no camera labels on the shared sheet.
    for tag in ["<path", "<polygon", "<polyline", "<circle", "data:"] {
        assert!(!svg.contains(tag), "{tag}");
    }
    assert!(!svg.contains("House 1 front"));
    check("evidence-aggregate.svg", &svg);
}

#[test]
fn exact_sheet_on_the_demo_street() {
    let d = demo();
    let svg = sheet(&d, Tier::Exact);
    assert!(svg.contains("<path") && svg.contains("House 1 front"));
    check("evidence-exact.svg", &svg);
}
