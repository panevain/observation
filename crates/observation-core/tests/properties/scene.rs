//! Loading scene files: what loads is clean, a clean file round-trips
//! exactly, and loading is idempotent.

use crate::config;
use crate::formats::{opt_text, text};
use crate::strategies::lon;
use observation_core::camera::{CameraKind, CameraModel, CameraPlacement, DoriLevel, Lighting};
use observation_core::coverage::{CoverageParams, Region};
use observation_core::geo::{BBox, LatLon};
use observation_core::provenance::Provenance;
use observation_core::scene::{AreaRef, SceneFile};
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

/// Any file a user might import: mostly loadable, with each repair (and now
/// and then each refusal) exercised.
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
        prop::option::of(junk_provenance()),
    )
        .prop_map(
            |((area, lat, lon, version), cameras, custom_models, params, region, provenance)| {
                let mut s = SceneFile::new(area, LatLon::new(lat, lon));
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
            let lower = s.to_ascii_lowercase();
            prop_assert!(
                lower.starts_with("https://") && !s.contains(char::is_whitespace),
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

    #[test]
    fn prop_loaded_scenes_are_clean(s in junk_scene()) {
        if let Ok(loaded) = SceneFile::from_json(&s.to_json()) {
            is_clean(&s, &loaded)?;
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

/// Most generated files load (72% at the fixed seed), so the properties above test the repairs
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
