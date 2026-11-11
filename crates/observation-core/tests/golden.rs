//! Golden files for every format that leaves the process: the OBSB area
//! bundle, the coverage worker's messages, saved scene files (and the
//! localStorage autosave, which is the same JSON) and the camera catalogue's
//! fingerprint.
//!
//! Each test builds its output from a fixed fixture and compares it byte for
//! byte with a file under `tests/golden/`. A failure means a format changed:
//! if that is intended (a version bump, or a deliberate change to what a
//! scene exports), regenerate the files with
//!
//! ```sh
//! OBS_BLESS=1 cargo nextest run -p observation-core --test golden
//! ```
//!
//! and review the diff. Old files must keep loading, so every test also reads
//! the committed bytes back.
//!
//! The fixture area lies on the equator, so its projection's cosine is exactly
//! 1 and the bytes do not depend on the platform's `cos`. The view cone is
//! written out rather than built from angles for the same reason.

use glam::Vec3;
use observation_core::address::Address;
use observation_core::bundle::{
    AreaBundle, BUNDLE_MAGIC, BUNDLE_VERSION, OsmAddressPoint, OsmBuilding, OsmRoad, build_bundle,
};
use observation_core::camera::Catalog;
use observation_core::coverage_job::{JOB_VERSION, Job, JobRequest, JobResponse, LayerInputs};
use observation_core::frustum::ViewCone;
use observation_core::geo::{BBox, LatLon};
use observation_core::scene::SceneFile;
use observation_core::terrain::Terrain;
use std::path::{Path, PathBuf};

// ----- harness ---------------------------------------------------------------

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn blessing() -> bool {
    std::env::var_os("OBS_BLESS").is_some_and(|v| v == "1")
}

/// Reads a committed fixture (an input, never rewritten).
fn read(name: &str) -> Vec<u8> {
    let path = golden_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Compares `actual` with the golden file `name`, or with `OBS_BLESS=1`
/// writes it there. Returns the golden bytes, so callers can check that they
/// still decode.
fn check(name: &str, actual: &[u8]) -> Vec<u8> {
    let path = golden_dir().join(name);
    if blessing() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("create the golden directory");
        }
        std::fs::write(&path, actual).expect("write the golden file");
        return actual.to_vec();
    }
    let expected = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden file {}: {e}\n\
             Run with OBS_BLESS=1 to create it.",
            path.display()
        )
    });
    if expected != actual {
        panic!(
            "{} differs from the golden file {}.\n\
             If the format change is intended, run with OBS_BLESS=1 and review the diff.",
            describe(&expected, actual),
            path.display()
        );
    }
    expected
}

/// Where two outputs first differ, for the failure message.
fn describe(expected: &[u8], actual: &[u8]) -> String {
    let at = expected
        .iter()
        .zip(actual)
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));
    match (std::str::from_utf8(expected), std::str::from_utf8(actual)) {
        (Ok(e), Ok(a)) => {
            let line = e[..at].matches('\n').count();
            let pick = |s: &str| s.lines().nth(line).unwrap_or("<end of file>").to_owned();
            format!(
                "output (line {}: {:?}, expected {:?})",
                line + 1,
                pick(a),
                pick(e)
            )
        }
        _ => format!(
            "output ({} bytes, expected {}; first difference at byte {at})",
            actual.len(),
            expected.len()
        ),
    }
}

// ----- fixtures --------------------------------------------------------------

/// A 0.001° × 0.0012° box (about 111 m × 134 m) centred on the equator at
/// 10° E.
fn fixture_bbox() -> BBox {
    BBox {
        south: -0.0005,
        west: 9.9994,
        north: 0.0005,
        east: 10.0006,
    }
}

/// A closed ring through `corners` (lat, lon offsets from the box centre).
fn ring(corners: &[(f64, f64)]) -> Vec<LatLon> {
    let c = fixture_bbox().center();
    let mut r: Vec<LatLon> = corners
        .iter()
        .map(|&(lat, lon)| LatLon::new(c.lat + lat, c.lon + lon))
        .collect();
    r.push(r[0]);
    r
}

/// Ground falling to the north-west and rising to the south-east, written
/// out sample by sample (no elevation function involved).
fn fixture_terrain() -> Terrain {
    let (cols, rows) = (12u32, 11u32);
    let mut heights_dm = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let (c, r) = (col as i16 - 6, row as i16 - 5);
            heights_dm.push(c * 4 + r * 3 + (c * r) % 5);
        }
    }
    Terrain {
        min: [-220.0, -210.0],
        cell_m: 40.0,
        cols,
        rows,
        heights_dm,
        datum_m: 123.5,
        attribution: "Elevation: golden fixture".to_owned(),
    }
}

/// OSM features as the server's parser hands them over: a house whose
/// address lacks a street (filled from the nearest road), a relation with a
/// courtyard, a raised building part, three roads (one running far beyond
/// the box) and two address points.
fn fixture_features() -> (Vec<OsmBuilding>, Vec<OsmRoad>, Vec<OsmAddressPoint>) {
    let d = 0.0001;
    let buildings = vec![
        OsmBuilding {
            osm_id: 101,
            outer: ring(&[(0.0, -2.0 * d), (0.0, -d), (d, -d), (d, -2.0 * d)]),
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.5,
            address: Some(Address {
                housenumber: Some("1".into()),
                ..Address::default()
            }),
        },
        OsmBuilding {
            osm_id: -202,
            outer: ring(&[
                (0.0, 0.5 * d),
                (0.0, 3.0 * d),
                (2.0 * d, 3.0 * d),
                (2.0 * d, 0.5 * d),
            ]),
            holes: vec![ring(&[
                (0.6 * d, 1.2 * d),
                (1.4 * d, 1.2 * d),
                (1.4 * d, 2.3 * d),
                (0.6 * d, 2.3 * d),
            ])],
            min_height_m: 0.0,
            height_m: 12.0,
            address: Some(Address {
                housename: Some("Ring House".into()),
                street: Some("Equator Road".into()),
                unit: Some("B".into()),
                ..Address::default()
            }),
        },
        OsmBuilding {
            osm_id: 303,
            outer: ring(&[(2.5 * d, -d), (2.5 * d, 0.2 * d), (3.2 * d, -0.4 * d)]),
            holes: vec![],
            min_height_m: 3.0,
            height_m: 9.0,
            address: None,
        },
    ];
    let c = fixture_bbox().center();
    let at = |lat: f64, lon: f64| LatLon::new(c.lat + lat, c.lon + lon);
    let roads = vec![
        OsmRoad {
            osm_id: 7001,
            highway: "residential".into(),
            name: Some("Equator Road".into()),
            width_m: 6.0,
            points: vec![
                at(-2.0 * d, -0.2),
                at(-2.0 * d, -5.0 * d),
                at(-2.0 * d, 5.0 * d),
                at(-2.5 * d, 0.2),
            ],
        },
        OsmRoad {
            osm_id: 7002,
            highway: "footway".into(),
            name: None,
            width_m: 2.0,
            points: vec![at(-2.0 * d, 0.0), at(0.0, 0.2 * d), at(2.4 * d, 0.2 * d)],
        },
        OsmRoad {
            osm_id: 7003,
            highway: "service".into(),
            name: Some("Back Lane".into()),
            width_m: 6.5,
            points: vec![at(3.5 * d, -5.0 * d), at(3.5 * d, 5.0 * d)],
        },
    ];
    let addresses = vec![
        OsmAddressPoint {
            address: Address {
                housenumber: Some("3".into()),
                street: Some("Equator Road".into()),
                unit: Some("1".into()),
                ..Address::default()
            },
            point: at(1.0 * d, 1.0 * d),
        },
        OsmAddressPoint {
            address: Address {
                housenumber: Some("5".into()),
                ..Address::default()
            },
            point: at(-0.5 * d, 4.0 * d),
        },
    ];
    (buildings, roads, addresses)
}

fn fixture_bundle() -> AreaBundle {
    let (buildings, roads, addresses) = fixture_features();
    build_bundle(
        fixture_bbox(),
        &buildings,
        &roads,
        &addresses,
        fixture_terrain(),
        "© OpenStreetMap contributors",
        1_793_500_000,
        Some(1_793_475_782),
    )
}

/// Looking north from the road, written out rather than built from angles.
fn fixture_job() -> Job {
    Job {
        generation: 4,
        camera_id: 9,
        key: 0x0123_4567_89ab_cdef,
        cone: ViewCone {
            eye: Vec3::new(1.5, 6.0, 26.0),
            forward: Vec3::new(0.0, 0.0, -1.0),
            right: Vec3::new(1.0, 0.0, 0.0),
            up: Vec3::new(0.0, 1.0, 0.0),
            tan_half_h: 1.0,
            tan_half_v: 0.5,
            range: 40.0,
        },
        subject_height_m: 1.0,
    }
}

// ----- area bundle -----------------------------------------------------------

#[test]
fn golden_bundle() {
    let bundle = fixture_bundle();
    // The fixture covers what the format carries.
    assert_eq!(
        bundle.buildings.len(),
        3,
        "every building survives extrusion"
    );
    assert!(bundle.buildings.iter().any(|b| b.osm_id < 0));
    assert!(
        bundle
            .buildings
            .iter()
            .any(|b| !b.footprint.holes.is_empty())
    );
    assert!(
        bundle
            .buildings
            .iter()
            .any(|b| b.footprint.min_height_m > 0.0)
    );
    assert!(bundle.buildings.iter().any(|b| b.footprint.base_m != 0.0));
    assert_eq!(
        bundle.buildings[0]
            .address
            .as_ref()
            .and_then(|a| a.street.as_deref()),
        Some("Equator Road"),
        "a street filled from the nearest road"
    );
    assert!(bundle.addresses.iter().any(|a| a.building.is_some()));
    assert!(bundle.addresses.iter().any(|a| a.building.is_none()));
    assert!(!bundle.terrain.is_flat());
    assert_eq!(bundle.origin, LatLon::new(0.0, 10.0));

    let golden = check("area.obsb", &bundle.encode());
    assert_eq!(golden[..4], BUNDLE_MAGIC);
    assert_eq!(golden[4..6], BUNDLE_VERSION.to_le_bytes());
    assert_eq!(AreaBundle::decode(&golden), Ok(bundle));
}

// ----- coverage worker messages ----------------------------------------------

#[test]
fn golden_job_messages() {
    let inputs = LayerInputs::new(&fixture_bundle(), 1.0, 1.0);
    let layers = inputs.build();
    let job = fixture_job();
    let result = job.run(&layers);
    assert!(
        !result.sight.ground.is_empty() && !result.sight.facade.is_empty(),
        "the camera sees ground and walls"
    );
    let requests = [
        (
            "job/request_load_area.bin",
            JobRequest::LoadArea {
                generation: 4,
                inputs,
            },
        ),
        ("job/request_compute.bin", JobRequest::Compute(job)),
    ];
    for (name, msg) in requests {
        let golden = check(name, &msg.encode());
        assert_eq!(golden[0], JOB_VERSION);
        assert_eq!(JobRequest::decode(&golden), Ok(msg), "{name}");
    }
    let responses = [
        ("job/response_ready.bin", JobResponse::Ready),
        ("job/response_done.bin", JobResponse::Done(result)),
        (
            "job/response_stale.bin",
            JobResponse::Stale {
                generation: 3,
                camera_id: 9,
                key: 0xfedc_ba98_7654_3210,
            },
        ),
        (
            "job/response_failed.bin",
            JobResponse::Failed("unreadable coverage message (golden)".into()),
        ),
    ];
    for (name, msg) in responses {
        let golden = check(name, &msg.encode());
        assert_eq!(golden[0], JOB_VERSION);
        assert_eq!(JobResponse::decode(&golden), Ok(msg), "{name}");
    }

    // The committed answer is the engine's answer to the committed request,
    // so the files describe one computation.
    let Ok(JobRequest::LoadArea { inputs, .. }) =
        JobRequest::decode(&read("job/request_load_area.bin"))
    else {
        panic!("request_load_area.bin is not a LoadArea request");
    };
    let Ok(JobRequest::Compute(job)) = JobRequest::decode(&read("job/request_compute.bin")) else {
        panic!("request_compute.bin is not a Compute request");
    };
    let Ok(JobResponse::Done(done)) = JobResponse::decode(&read("job/response_done.bin")) else {
        panic!("response_done.bin is not a Done response");
    };
    assert_eq!(job.run(&inputs.build()), done);
}

// ----- scene files -----------------------------------------------------------

/// Imports `scene/<stem>.json` (a file as an older or current app saved it)
/// and checks the re-export against `scene/<stem>.export.json`.
fn scene_round_trip(stem: &str) -> SceneFile {
    let input = String::from_utf8(read(&format!("scene/{stem}.json"))).expect("UTF-8");
    let scene = SceneFile::from_json(&input).unwrap_or_else(|e| panic!("{stem}.json: {e}"));
    let mut export = scene.to_json();
    export.push('\n');
    let golden = check(&format!("scene/{stem}.export.json"), export.as_bytes());
    // The export loads again, to the same scene and the same export.
    let golden = String::from_utf8(golden).expect("UTF-8");
    let again = SceneFile::from_json(&golden).unwrap_or_else(|e| panic!("{stem} export: {e}"));
    assert_eq!(again, scene, "{stem}: the export reads back");
    assert_eq!(again.to_json() + "\n", golden, "{stem}: export is stable");
    scene
}

#[test]
fn golden_scene_v1_flat() {
    let s = scene_round_trip("v1_flat");
    assert!(s.on_flat_ground());
    assert_eq!(s.version, 1, "kept until the area's terrain is known");
}

#[test]
fn golden_scene_v2() {
    let s = scene_round_trip("v2");
    assert!(!s.on_flat_ground());
    assert!(s.region.is_some());
}

#[test]
fn golden_scene_v2_provenance() {
    let s = scene_round_trip("v2_provenance");
    assert!(s.provenance.is_some());
    assert_eq!(s.region, None, "an out-of-range region is dropped");
}

#[test]
fn golden_scene_legacy_color_index() {
    let s = scene_round_trip("legacy_color_index");
    assert!(s.cameras.iter().all(|c| c.color.is_none()));
}

// ----- camera catalogue ------------------------------------------------------

#[test]
fn golden_catalog_fingerprint() {
    let parts: Vec<String> = ["catalog/acme.ron", "catalog/generic.ron"]
        .iter()
        .map(|p| String::from_utf8(read(p)).expect("UTF-8"))
        .collect();
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let catalog = Catalog::from_ron_parts(&parts).expect("the fixture catalogue is valid");
    assert_eq!(catalog.models.len(), 4);
    let mut out = catalog.fingerprint();
    out.push('\n');
    check("catalog/fingerprint.txt", out.as_bytes());
}
