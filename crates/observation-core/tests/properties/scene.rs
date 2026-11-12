//! Loading scene files: what loads is clean, a clean file round-trips
//! exactly, each repair keeps what docs/design.md says it keeps, a file is
//! refused exactly when it breaks a hard limit, and loading is idempotent.

use crate::config;
use crate::formats::{opt_text, text};
use crate::strategies::lon;
use observation_core::camera::{CameraKind, CameraModel, CameraPlacement, DoriLevel, Lighting};
use observation_core::coverage::{CoverageParams, Region};
use observation_core::geo::{BBox, LatLon};
use observation_core::provenance::Provenance;
use observation_core::scene::{AreaRef, SceneError, SceneFile};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;

/// The last second of 9999, the latest time a provenance block can hold.
const LAST_UTC: u64 = 253_402_300_799;

/// Mostly within the limits a file must respect, now and then beyond them.
fn scene_num() -> impl Strategy<Value = f32> {
    prop_oneof![19 => -500.0..=500.0f32, 1 => -2.0e4..=2.0e4f32]
}

/// Latitudes to ±90, wider than `strategies::lat` on purpose: a stored box
/// is not range-checked on import.
fn any_lat() -> std::ops::RangeInclusive<f64> {
    -90.0..=90.0f64
}

/// Ids that collide with each other, lack the custom prefix or name a
/// catalogue model, so loading has duplicates to rename.
fn model_id() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "custom-a-1",
        "custom-a-2",
        "custom-mine-1",
        "acme",
        "generic-doorbell-160",
    ])
    .prop_map(str::to_owned)
}

/// Names that slug alike, so two renamed models compete for one fresh id.
fn model_name() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["Mine", "mine", "Spot", "Yard Cam"]).prop_map(str::to_owned)
}

fn junk_model() -> impl Strategy<Value = CameraModel> {
    let link = prop::option::of(prop::sample::select(vec![
        "https://example.com/spec",
        "HTTPS://example.com/spec",
        "javascript:alert(1)",
        "http://example.com",
        "https://user@example.com",
        "https://exa mple.com",
    ]));
    (
        (
            model_id(),
            text(6),
            model_name(),
            prop::sample::select(CameraKind::ALL.to_vec()),
        ),
        (
            prop_oneof![19 => 1.0..=360.0f32, 1 => 0.5..=400.0f32],
            prop_oneof![19 => 1.0..=360.0f32, 1 => 0.5..=400.0f32],
            prop_oneof![19 => 16..=4000u32, 1 => 8..=4000u32],
            prop_oneof![19 => 16..=4000u32, 1 => 8..=4000u32],
        ),
        (
            prop::option::of(prop_oneof![19 => 0.0..=200.0f32, 1 => -10.0..=250.0f32]),
            link,
            opt_text(),
        ),
    )
        .prop_map(
            |(
                (id, vendor, name, kind),
                (hfov_deg, vfov_deg, w, h),
                (ir_range_m, source, notes),
            )| {
                CameraModel {
                    id,
                    vendor,
                    name,
                    kind,
                    hfov_deg,
                    vfov_deg,
                    resolution: [w, h],
                    ir_range_m,
                    source: source.map(str::to_owned),
                    notes,
                }
            },
        )
}

fn junk_placement() -> impl Strategy<Value = CameraPlacement> {
    let group = prop::option::of(prop_oneof![
        text(8),
        Just("   ".to_owned()),
        Just("  Front  ".to_owned()),
        Just(format!("  {}  ", "g".repeat(100))),
    ]);
    (
        (any::<u32>(), text(10), model_id()),
        (
            [scene_num(), scene_num(), scene_num()],
            scene_num(),
            scene_num(),
            scene_num(),
        ),
        (prop::option::of(any::<[u8; 3]>()), any::<bool>(), group),
    )
        .prop_map(
            |(
                (id, label, model_id),
                (position, yaw_deg, pitch_deg, roll_deg),
                (color, enabled, group),
            )| {
                CameraPlacement {
                    id,
                    label,
                    model_id,
                    position,
                    yaw_deg,
                    pitch_deg,
                    roll_deg,
                    color,
                    enabled,
                    group,
                }
            },
        )
}

fn junk_provenance() -> impl Strategy<Value = Provenance> {
    let fingerprint = prop::option::of(prop_oneof![
        Just("0123456789ABCDEF".to_owned()),
        Just("not-hex".to_owned()),
        text(16),
    ]);
    (
        prop::option::of(0..=LAST_UTC),
        prop::option::of(0..=LAST_UTC),
        fingerprint,
        text(80),
        text(80),
        any::<u32>(),
    )
        .prop_map(
            |(
                osm_data_unix,
                bundle_built_unix,
                osm_fingerprint,
                app_version,
                catalog_version,
                caveats_version,
            )| {
                Provenance {
                    osm_data_unix,
                    bundle_built_unix,
                    osm_fingerprint,
                    app_version,
                    catalog_version,
                    caveats_version,
                }
            },
        )
}

fn area(lat: impl Strategy<Value = f64> + Clone) -> impl Strategy<Value = AreaRef> {
    prop_oneof![
        Just(AreaRef::Demo),
        (lat.clone(), lon(), lat, lon()).prop_map(|(south, west, north, east)| AreaRef::Osm {
            bbox: BBox {
                south,
                west,
                north,
                east
            }
        }),
    ]
}

/// A camera that breaks no rule, showing a catalogue model.
fn plain_camera(id: u32) -> CameraPlacement {
    CameraPlacement {
        id,
        label: String::new(),
        model_id: "generic-doorbell-160".to_owned(),
        position: [0.0, 2.0, 0.0],
        yaw_deg: 0.0,
        pitch_deg: 0.0,
        roll_deg: 0.0,
        color: None,
        enabled: true,
        group: None,
    }
}

/// A custom model that breaks no rule.
fn plain_model(n: usize) -> CameraModel {
    CameraModel {
        id: format!("custom-plain-{n}"),
        vendor: String::new(),
        name: "Plain".to_owned(),
        kind: CameraKind::ALL[0],
        hfov_deg: 90.0,
        vfov_deg: 60.0,
        resolution: [1920, 1080],
        ir_range_m: None,
        source: None,
        notes: None,
    }
}

/// Any file a user might import: mostly loadable, with each repair
/// exercised (including a region over 256 points, which is dropped), and now
/// and then each refusal: out-of-range values, an invalid custom model, a
/// wrong format, a version newer than 2, 257 cameras or 65 custom models.
fn junk_scene() -> impl Strategy<Value = SceneFile> {
    let params = (
        prop::sample::select(DoriLevel::ALL.to_vec()),
        prop::sample::select(vec![Lighting::Day, Lighting::Night]),
        prop_oneof![19 => 0.0..=10.0f32, 1 => -1.0..=12.0f32],
        prop_oneof![19 => 1.0..=2000.0f32, 1 => 0.0..=2500.0f32],
    )
        .prop_map(
            |(dori, lighting, subject_height_m, custom_ppm)| CoverageParams {
                dori,
                lighting,
                subject_height_m,
                custom_ppm,
            },
        );
    let points = prop_oneof![
        9 => prop::collection::vec([scene_num(), scene_num()], 0..6),
        1 => prop::collection::vec([scene_num(), scene_num()], 257..=260),
    ];
    let region = prop::option::of(
        (points, any::<bool>()).prop_map(|(points, enabled)| Region { points, enabled }),
    );
    (
        (
            area(any_lat()),
            any_lat(),
            lon(),
            prop_oneof![1 => Just(1u32), 3 => Just(2u32)],
        ),
        prop::collection::vec(junk_placement(), 0..4),
        prop::collection::vec(junk_model(), 0..4),
        params,
        region,
        (
            prop::option::of(junk_provenance()),
            prop_oneof![46 => Just(0u8), 1 => Just(1u8), 1 => Just(2u8), 1 => Just(3u8), 1 => Just(4u8)],
        ),
    )
        .prop_map(
            |(
                (area, lat, lon, version),
                cameras,
                custom_models,
                params,
                region,
                (provenance, refusal),
            )| {
                let mut s = SceneFile::new(area, LatLon::new(lat, lon));
                s.version = version;
                s.cameras = cameras;
                s.custom_models = custom_models;
                s.params = params;
                s.region = region;
                s.provenance = provenance;
                match refusal {
                    1 => s.format = "observation-scenes".to_owned(),
                    2 => s.version = 3,
                    3 => s.cameras.extend((0..257).map(plain_camera)),
                    4 => s.custom_models.extend((0..65).map(plain_model)),
                    _ => {}
                }
                s
            },
        )
}

/// A value as it comes back from one JSON trip. serde_json without its
/// `float_roundtrip` feature moves about 12% of coordinates by one ULP on the
/// first save and load (and then no more), so a clean file stores them so.
fn after_one_trip(v: f64) -> f64 {
    serde_json::from_str(&serde_json::to_string(&v).unwrap_or_default()).unwrap_or(v)
}

/// A file that needs no repair: every value within limits, unique `custom-`
/// ids, `https` or no links, trimmed short groups, a well-formed provenance.
fn clean_scene() -> impl Strategy<Value = SceneFile> {
    let small = || -500.0..=500.0f32;
    let models = prop::collection::vec(
        (
            (
                text(6),
                text(10),
                prop::sample::select(CameraKind::ALL.to_vec()),
            ),
            (1.0..=170.0f32, 1.0..=170.0f32, 16..=4000u32, 16..=4000u32),
            (
                prop::option::of(0.0..=200.0f32),
                prop::option::of(Just("https://example.com/spec".to_owned())),
                opt_text(),
            ),
        ),
        0..4,
    )
    .prop_map(|models| {
        models
            .into_iter()
            .enumerate()
            .map(
                |(
                    i,
                    ((vendor, name, kind), (hfov_deg, vfov_deg, w, h), (ir_range_m, source, notes)),
                )| CameraModel {
                    id: format!("custom-model-{}", i + 1),
                    vendor,
                    name,
                    kind,
                    hfov_deg,
                    vfov_deg,
                    resolution: [w, h],
                    ir_range_m,
                    source,
                    notes,
                },
            )
            .collect::<Vec<_>>()
    });
    let camera = (
        (
            any::<u32>(),
            text(10),
            prop::sample::select(vec![
                "custom-model-1",
                "custom-model-2",
                "generic-doorbell-160",
            ]),
        ),
        (
            [small(), small(), small()],
            -360.0..=360.0f32,
            -90.0..=90.0f32,
            -180.0..=180.0f32,
        ),
        (
            prop::option::of(any::<[u8; 3]>()),
            any::<bool>(),
            prop::option::of(prop::sample::select(vec!["Front", "Back yard", "Garage"])),
        ),
    )
        .prop_map(
            |(
                (id, label, model_id),
                (position, yaw_deg, pitch_deg, roll_deg),
                (color, enabled, group),
            )| {
                CameraPlacement {
                    id,
                    label,
                    model_id: model_id.to_owned(),
                    position,
                    yaw_deg,
                    pitch_deg,
                    roll_deg,
                    color,
                    enabled,
                    group: group.map(str::to_owned),
                }
            },
        );
    let provenance = (
        prop::option::of(0..=LAST_UTC),
        prop::option::of(0..=LAST_UTC),
        prop::option::of(any::<u64>().prop_map(|f| format!("{f:016x}"))),
        (text(64), text(64), any::<u32>()),
    )
        .prop_map(
            |(
                osm_data_unix,
                bundle_built_unix,
                osm_fingerprint,
                (app_version, catalog_version, caveats_version),
            )| {
                Provenance {
                    osm_data_unix,
                    bundle_built_unix,
                    osm_fingerprint,
                    app_version,
                    catalog_version,
                    caveats_version,
                }
            },
        );
    let params = (
        prop::sample::select(DoriLevel::ALL.to_vec()),
        prop::sample::select(vec![Lighting::Day, Lighting::Night]),
        0.0..=10.0f32,
        1.0..=2000.0f32,
    )
        .prop_map(
            |(dori, lighting, subject_height_m, custom_ppm)| CoverageParams {
                dori,
                lighting,
                subject_height_m,
                custom_ppm,
            },
        );
    let region = prop::option::of(
        (
            prop::collection::vec([small(), small()], 0..8),
            any::<bool>(),
        )
            .prop_map(|(points, enabled)| Region { points, enabled }),
    );
    (
        (area(-90.0..=90.0f64), any_lat(), lon(), 1..=2u32),
        prop::collection::vec(camera, 0..4),
        models,
        params,
        region,
        prop::option::of(provenance),
    )
        .prop_map(
            |((area, lat, lon, version), cameras, custom_models, params, region, provenance)| {
                let area = match area {
                    AreaRef::Osm { bbox } => AreaRef::Osm {
                        bbox: BBox {
                            south: after_one_trip(bbox.south),
                            west: after_one_trip(bbox.west),
                            north: after_one_trip(bbox.north),
                            east: after_one_trip(bbox.east),
                        },
                    },
                    demo => demo,
                };
                let origin = LatLon::new(after_one_trip(lat), after_one_trip(lon));
                let mut s = SceneFile::new(area, origin);
                s.version = version;
                s.cameras = cameras;
                s.custom_models = custom_models;
                s.params = params;
                s.region = region;
                s.provenance = provenance;
                s
            },
        )
}

/// Every rule `from_json` enforces, stated as a property of what it returns.
fn is_clean(input: &SceneFile, loaded: &SceneFile) -> Result<(), TestCaseError> {
    let within = |v: f32| v.is_finite() && v.abs() < 10_000.0;
    prop_assert!(loaded.cameras.len() <= 256 && loaded.custom_models.len() <= 64);
    for c in &loaded.cameras {
        prop_assert!(c.position.iter().all(|&v| within(v)));
        prop_assert!(
            [c.yaw_deg, c.pitch_deg, c.roll_deg]
                .iter()
                .all(|&v| within(v))
        );
        if let Some(g) = &c.group {
            prop_assert!(
                !g.is_empty() && g.trim() == g && g.chars().count() <= 80,
                "{:?}",
                g
            );
        }
    }
    let p = &loaded.params;
    prop_assert!((0.0..=10.0).contains(&p.subject_height_m));
    prop_assert!((1.0..=2000.0).contains(&p.custom_ppm));
    let mut ids = std::collections::HashSet::new();
    for m in &loaded.custom_models {
        prop_assert!(
            m.id.starts_with("custom-") && ids.insert(m.id.clone()),
            "{}",
            m.id
        );
        if let Some(s) = &m.source {
            // An https URL with a host and no user info.
            let lower = s.to_ascii_lowercase();
            let authority = s
                .get(8..)
                .unwrap_or_default()
                .split(['/', '\\', '?', '#'])
                .next();
            let authority = authority.unwrap_or_default();
            prop_assert!(
                lower.starts_with("https://")
                    && !s.contains(char::is_whitespace)
                    && !authority.is_empty()
                    && !authority.contains('@'),
                "{}",
                s
            );
        }
    }
    if let Some(r) = &loaded.region {
        prop_assert!(r.points.len() <= 256 && r.points.iter().flatten().all(|&v| within(v)));
    }
    if let Some(p) = &loaded.provenance {
        prop_assert!(
            p.app_version.chars().count() <= 64 && p.catalog_version.chars().count() <= 64
        );
        if let Some(f) = &p.osm_fingerprint {
            prop_assert!(
                f.len() == 16
                    && f.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{}",
                f
            );
        }
    }
    // A camera that showed a custom model shows the same model (the first
    // with its id), under whatever id that model now has.
    prop_assert_eq!(loaded.custom_models.len(), input.custom_models.len());
    for (was, now) in input.cameras.iter().zip(&loaded.cameras) {
        let first = input
            .custom_models
            .iter()
            .position(|m| m.id == was.model_id);
        let want = first.map_or(&was.model_id, |i| &loaded.custom_models[i].id);
        prop_assert_eq!(&now.model_id, want);
    }
    Ok(())
}

proptest! {
    #![proptest_config(config(128))]

    /// What loads is clean; and a file loads exactly when it breaks no hard
    /// limit.
    #[test]
    fn prop_loaded_scenes_are_clean(s in junk_scene()) {
        match SceneFile::from_json(&s.to_json()) {
            Ok(loaded) => {
                prop_assert!(!breaks_a_limit(&s), "a file that breaks a limit loaded");
                is_clean(&s, &loaded)?;
            }
            Err(e) => prop_assert!(breaks_a_limit(&s), "a file that breaks no limit was refused: {}", e),
        }
    }

    #[test]
    fn prop_loading_a_scene_is_idempotent(s in junk_scene()) {
        // Whatever a file holds, what loads from it exports to a file that
        // loads to the same scene: every check and repair is done once.
        if let Ok(loaded) = SceneFile::from_json(&s.to_json()) {
            let again = SceneFile::from_json(&loaded.to_json()).expect("an export loads");
            prop_assert_eq!(&again, &loaded);
            prop_assert_eq!(again.to_json(), loaded.to_json());
        }
    }

    #[test]
    fn prop_bad_links_are_dropped_not_refused(
        s in clean_scene(),
        link in prop::sample::select(vec![
            "http://example.com",
            "javascript:alert(1)",
            "data:text/html,x",
            "https://user@example.com",
            "//example.com",
        ]),
    ) {
        let mut with_link = s.clone();
        let Some(m) = with_link.custom_models.first_mut() else { return Ok(()) };
        m.source = Some(link.to_owned());
        let mut want = s;
        want.custom_models[0].source = None;
        prop_assert_eq!(SceneFile::from_json(&with_link.to_json()).ok(), Some(want));
    }

    #[test]
    fn prop_clean_scenes_round_trip_exactly(s in clean_scene()) {
        let loaded = SceneFile::from_json(&s.to_json());
        prop_assert!(loaded.is_ok(), "{:?}", loaded.err());
        prop_assert_eq!(loaded.ok(), Some(s));
    }
}

/// Most generated files load (70% at the fixed seed), so the properties above test the repairs
/// rather than the refusals; a drop here means `from_json` started refusing
/// files it used to accept, or the generator drifted.
#[test]
fn prop_most_junk_scenes_load() {
    let mut runner = TestRunner::new(config(400));
    let strategy = junk_scene();
    let total = 400;
    let loaded = (0..total)
        .filter(|_| {
            let s = strategy
                .new_tree(&mut runner)
                .map(|t| t.current())
                .expect("the strategy generates");
            SceneFile::from_json(&s.to_json()).is_ok()
        })
        .count();
    assert!(loaded * 100 >= total * 60, "only {loaded} of {total} load");
}

// ----- refusals --------------------------------------------------------------

/// Whether a file breaks a hard limit of docs/design.md's scene-file row, so
/// that it must be refused: its format or version (newer than 2), more than
/// 256 cameras or 64 custom models, a position or angle that is not finite or
/// not below 10,000, a subject height outside 0-10 m, a custom density
/// outside 1-2000 px/m, or an invalid custom model (an id that is not
/// lowercase letters, digits and dashes, a field of view outside 1-360°, a
/// side under 16 px, an IR range outside 0-200 m). Links are not a limit: a
/// bad one is dropped.
fn breaks_a_limit(s: &SceneFile) -> bool {
    let within = |v: f32| v.is_finite() && v.abs() < 10_000.0;
    let bad_model = |m: &CameraModel| {
        m.id.is_empty()
            || !m
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            || ![m.hfov_deg, m.vfov_deg]
                .iter()
                .all(|f| (1.0..=360.0).contains(f))
            || m.resolution.iter().any(|&side| side < 16)
            || m.ir_range_m.is_some_and(|r| !(0.0..=200.0).contains(&r))
    };
    s.format != "observation-scene"
        || s.version > 2
        || s.cameras.len() > 256
        || s.custom_models.len() > 64
        || s.cameras.iter().any(|c| {
            !c.position
                .iter()
                .chain(&[c.yaw_deg, c.pitch_deg, c.roll_deg])
                .all(|&v| within(v))
        })
        || !(0.0..=10.0).contains(&s.params.subject_height_m)
        || !(1.0..=2000.0).contains(&s.params.custom_ppm)
        || s.custom_models.iter().any(bad_model)
}

/// One way to break a hard limit.
#[derive(Debug, Clone)]
enum Breach {
    Format(String),
    Version(u32),
    Cameras(usize),
    Models(usize),
    /// A camera's position (axis 0-2) or angle (3-5: yaw, pitch, roll).
    Placement(usize, f32),
    Subject(f32),
    Density(f32),
    /// The first custom model's id is not kebab-case.
    ModelId,
    ModelFov(f32),
    ModelSide(u32),
    ModelIr(f32),
}

fn breach() -> impl Strategy<Value = Breach> {
    let beyond = || {
        prop_oneof![
            Just(10_000.0f32),
            Just(-10_000.0f32),
            10_000.0..=1.0e30f32,
            -1.0e30..=-10_000.0f32,
        ]
    };
    prop_oneof![
        text(60)
            .prop_filter("another format", |f| f != "observation-scene")
            .prop_map(Breach::Format),
        // The first unsupported version, and any later one.
        prop_oneof![Just(3u32), 3..=u32::MAX].prop_map(Breach::Version),
        (257..=300usize).prop_map(Breach::Cameras),
        (65..=80usize).prop_map(Breach::Models),
        (0..6usize, beyond()).prop_map(|(k, v)| Breach::Placement(k, v)),
        prop_oneof![-1.0e3..=-1.0e-3f32, 10.001..=1.0e3f32].prop_map(Breach::Subject),
        prop_oneof![0.0..=0.999f32, 2000.01..=1.0e5f32].prop_map(Breach::Density),
        Just(Breach::ModelId),
        prop_oneof![0.0..=0.99f32, 360.01..=1.0e3f32].prop_map(Breach::ModelFov),
        (0..16u32).prop_map(Breach::ModelSide),
        prop_oneof![-1.0e3..=-0.01f32, 200.01..=1.0e3f32].prop_map(Breach::ModelIr),
    ]
}

proptest! {
    #![proptest_config(config(128))]

    /// A file that is clean but for one hard limit is refused with the error
    /// for that limit: the format (its first 40 characters quoted), the
    /// version (quoted), or invalid values or an invalid model.
    #[test]
    fn prop_files_that_break_a_limit_are_refused(s in clean_scene(), b in breach()) {
        let mut s = s;
        if s.cameras.is_empty() {
            s.cameras.push(plain_camera(1));
        }
        if s.custom_models.is_empty() {
            s.custom_models.push(plain_model(1));
        }
        match &b {
            Breach::Format(f) => s.format.clone_from(f),
            Breach::Version(v) => s.version = *v,
            Breach::Cameras(n) => s.cameras = (0..*n as u32).map(plain_camera).collect(),
            Breach::Models(n) => s.custom_models = (0..*n).map(plain_model).collect(),
            Breach::Placement(k, v) => {
                let c = &mut s.cameras[0];
                match k {
                    0..=2 => c.position[*k] = *v,
                    3 => c.yaw_deg = *v,
                    4 => c.pitch_deg = *v,
                    _ => c.roll_deg = *v,
                }
            }
            Breach::Subject(v) => s.params.subject_height_m = *v,
            Breach::Density(v) => s.params.custom_ppm = *v,
            Breach::ModelId => s.custom_models[0].id = "custom-Bad".to_owned(),
            Breach::ModelFov(v) => s.custom_models[0].vfov_deg = *v,
            Breach::ModelSide(v) => s.custom_models[0].resolution[1] = *v,
            Breach::ModelIr(v) => s.custom_models[0].ir_range_m = Some(*v),
        }
        prop_assert!(breaks_a_limit(&s), "{:?} breaks no limit", b);
        let got = SceneFile::from_json(&s.to_json());
        let ok = match (&b, &got) {
            (Breach::Format(f), Err(SceneError::Format(found))) => {
                *found == f.chars().take(40).collect::<String>()
            }
            (Breach::Version(v), Err(SceneError::TooNew(found))) => found == v,
            (
                Breach::ModelId | Breach::ModelFov(_) | Breach::ModelSide(_) | Breach::ModelIr(_),
                Err(SceneError::Model(_)),
            ) => true,
            (
                Breach::Cameras(_)
                | Breach::Models(_)
                | Breach::Placement(..)
                | Breach::Subject(_)
                | Breach::Density(_),
                Err(SceneError::Invalid(_)),
            ) => true,
            _ => false,
        };
        prop_assert!(ok, "{:?} gave {:?}", b, got.map(|_| ()));
    }
}

// ----- one repair at a time --------------------------------------------------

/// Whitespace that trimming removes, ASCII and not.
fn pad() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec![' ', '\t', '\n', '\u{3000}']),
        0..4,
    )
    .prop_map(String::from_iter)
}

/// Text that neither starts nor ends with whitespace (it may hold spaces
/// inside), up to about 120 characters.
fn inner_text() -> impl Strategy<Value = Vec<char>> {
    let visible = any::<char>().prop_filter("not whitespace", |c| !c.is_whitespace());
    prop::collection::vec((visible, any::<bool>()), 0..80).prop_map(|words| {
        let mut out = Vec::new();
        for (c, space) in words {
            if space && !out.is_empty() {
                out.push(' ');
            }
            out.push(c);
        }
        out
    })
}

/// One thing in an otherwise clean file that loading repairs.
#[derive(Debug, Clone)]
enum Repair {
    /// Camera `k`'s group: padding, a body, padding.
    Group(usize, String, Vec<char>, String),
    /// A valid fingerprint with the letters whose bits are set upper-cased.
    Fingerprint(u64, u16),
    /// A fingerprint that is not 16 hex digits.
    BadFingerprint(String),
    /// The app (`true`) or catalogue version label, of any length.
    Label(bool, Vec<char>),
}

fn repair() -> impl Strategy<Value = Repair> {
    let not_hex = prop_oneof![
        (any::<u128>(), 0..=32usize)
            .prop_filter("not 16 digits", |(_, n)| *n != 16)
            .prop_map(|(v, n)| format!("{v:032x}")[..n].to_owned()),
        (
            any::<u64>(),
            0..16usize,
            prop::sample::select(vec!['g', 'z', '-', ' ', 'é'])
        )
            .prop_map(|(v, at, c)| {
                let mut f: Vec<char> = format!("{v:016x}").chars().collect();
                f[at] = c;
                String::from_iter(f)
            }),
    ];
    prop_oneof![
        3 => (0..4usize, pad(), inner_text(), pad()).prop_map(|(k, l, b, r)| Repair::Group(k, l, b, r)),
        1 => (any::<u64>(), any::<u16>()).prop_map(|(f, upper)| Repair::Fingerprint(f, upper)),
        1 => not_hex.prop_map(Repair::BadFingerprint),
        2 => (any::<bool>(), prop::collection::vec(any::<char>(), 0..=100))
            .prop_map(|(app, label)| Repair::Label(app, label)),
    ]
}

proptest! {
    #![proptest_config(config(128))]

    /// A file that is clean but for one thing loading repairs loads to the
    /// clean file with exactly the documented repair, worked out here from
    /// the input: a group is trimmed and cut to 80 characters, and a blank
    /// one removed; a fingerprint of 16 hex digits in either case is kept,
    /// lower-cased, and any other is dropped; a version label is cut to its
    /// first 64 characters. Nothing else changes, and nothing is dropped
    /// instead of repaired.
    #[test]
    fn prop_each_repair_keeps_what_it_should(s in clean_scene(), r in repair()) {
        let (mut input, mut want) = (s.clone(), s);
        if input.cameras.is_empty() {
            input.cameras.push(plain_camera(1));
            want.cameras.push(plain_camera(1));
        }
        let plain = Provenance::default();
        let prov = |f: &mut SceneFile| f.provenance.get_or_insert_with(|| plain.clone()).clone();
        let (mut pin, mut pwant) = (prov(&mut input), prov(&mut want));
        // A cut group may end in whitespace; whether it is trimmed again
        // is not documented, so either is accepted.
        let mut also = None;
        match &r {
            Repair::Group(k, l, body, r) => {
                let k = k % input.cameras.len();
                input.cameras[k].group = Some(format!("{l}{}{r}", String::from_iter(body)));
                let cut: String = body.iter().take(80).collect();
                let mut alt = want.clone();
                alt.cameras[k].group = Some(cut.trim_end().to_owned()).filter(|g| !g.is_empty());
                want.cameras[k].group = Some(cut).filter(|g| !g.is_empty());
                also = Some(alt);
            }
            Repair::Fingerprint(f, upper) => {
                let lower = format!("{f:016x}");
                let mixed = lower.chars().enumerate().map(|(i, c)| {
                    if upper & (1 << i) != 0 { c.to_ascii_uppercase() } else { c }
                });
                pin.osm_fingerprint = Some(mixed.collect());
                pwant.osm_fingerprint = Some(lower);
            }
            Repair::BadFingerprint(f) => {
                pin.osm_fingerprint = Some(f.clone());
                pwant.osm_fingerprint = None;
            }
            Repair::Label(app, label) => {
                let (full, cut) = (String::from_iter(label), label.iter().take(64).collect());
                if *app {
                    (pin.app_version, pwant.app_version) = (full, cut);
                } else {
                    (pin.catalog_version, pwant.catalog_version) = (full, cut);
                }
            }
        }
        input.provenance = Some(pin);
        want.provenance = Some(pwant.clone());
        if let Some(alt) = &mut also {
            alt.provenance = Some(pwant);
        }
        let loaded = SceneFile::from_json(&input.to_json()).ok();
        prop_assert!(
            loaded.as_ref() == Some(&want) || (also.is_some() && loaded == also),
            "{:?}: loaded {:?}, want {:?}",
            r,
            loaded,
            want
        );
    }
}
