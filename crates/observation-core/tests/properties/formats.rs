//! Round trips of the bundle, the coverage worker's messages and scene files.

use crate::config;
use glam::{Vec2, Vec3};
use observation_core::address::{Address, AddressPoint};
use observation_core::building::{Footprint, MeshData};
use observation_core::bundle::{AreaBundle, Building, Road};
use observation_core::camera::{CameraKind, CameraModel, CameraPlacement, DoriLevel, Lighting};
use observation_core::coverage::{CoverageParams, GroundGrid, Region, Sight};
use observation_core::coverage_job::{Job, JobRequest, JobResponse, JobResult, LayerInputs};
use observation_core::frustum::ViewCone;
use observation_core::geo::{BBox, LatLon};
use observation_core::provenance::Provenance;
use observation_core::scene::{AreaRef, SceneFile};
use observation_core::terrain::Terrain;
use proptest::prelude::*;

// ----- building blocks -------------------------------------------------------

/// A finite number, mostly in the range real data uses, sometimes far out.
fn num() -> impl Strategy<Value = f32> {
    prop_oneof![8 => -500.0..=500.0f32, 1 => -2.0e4..=2.0e4f32]
}

fn xz() -> impl Strategy<Value = [f32; 2]> {
    [num(), num()]
}

/// Any text, including whitespace and characters outside ASCII.
fn text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..max).prop_map(String::from_iter)
}

fn opt_text() -> impl Strategy<Value = Option<String>> {
    prop::option::of(text(12))
}

fn address() -> impl Strategy<Value = Address> {
    (opt_text(), opt_text(), opt_text(), opt_text()).prop_map(
        |(housenumber, street, housename, unit)| Address {
            housenumber,
            street,
            housename,
            unit,
        },
    )
}

fn ring() -> impl Strategy<Value = Vec<[f32; 2]>> {
    prop::collection::vec(xz(), 0..6)
}

fn footprint() -> impl Strategy<Value = Footprint> {
    (
        ring(),
        prop::collection::vec(ring(), 0..2),
        num(),
        num(),
        num(),
    )
        .prop_map(|(outer, holes, min_height_m, height_m, base_m)| Footprint {
            outer,
            holes,
            min_height_m,
            height_m,
            base_m,
        })
}

fn road() -> impl Strategy<Value = Road> {
    (
        any::<i64>(),
        text(10),
        opt_text(),
        num(),
        prop::collection::vec(xz(), 0..5),
    )
        .prop_map(|(osm_id, highway, name, width_m, points)| Road {
            osm_id,
            highway,
            name,
            width_m,
            points,
        })
}

/// A grid that passes [`Terrain::validate`], or flat ground.
fn terrain() -> impl Strategy<Value = Terrain> {
    prop_oneof![
        1 => Just(Terrain::default()),
        3 => (1..=6u32, 1..=6u32, 1.0..=1000.0f32, xz(), num(), text(8)).prop_flat_map(
            |(cols, rows, cell_m, min, datum_m, attribution)| {
                prop::collection::vec(any::<i16>(), (cols * rows) as usize).prop_map(
                    move |heights_dm| Terrain {
                        min,
                        cell_m,
                        cols,
                        rows,
                        heights_dm,
                        datum_m,
                        attribution: attribution.clone(),
                    },
                )
            },
        ),
    ]
}

fn vec3() -> impl Strategy<Value = Vec3> {
    (num(), num(), num()).prop_map(|(x, y, z)| Vec3::new(x, y, z))
}

// ----- bundle ----------------------------------------------------------------

fn bundle() -> impl Strategy<Value = AreaBundle> {
    let building = (
        any::<i64>(),
        footprint(),
        any::<[u32; 2]>(),
        prop::option::of(address()),
    )
        .prop_map(|(osm_id, footprint, index_range, address)| Building {
            osm_id,
            footprint,
            index_range,
            address,
        });
    let point = (address(), xz(), prop::option::of(any::<u32>())).prop_map(
        |(address, position, building)| AddressPoint {
            address,
            position,
            building,
        },
    );
    let mesh = (
        prop::collection::vec([num(), num(), num()], 0..6),
        prop::collection::vec([num(), num(), num()], 0..6),
        prop::collection::vec(any::<u32>(), 0..9),
    )
        .prop_map(|(positions, normals, indices)| MeshData {
            positions,
            normals,
            indices,
        });
    (
        (any::<[f64; 4]>(), any::<[f64; 2]>()),
        prop::collection::vec(building, 0..4),
        prop::collection::vec(road(), 0..3),
        mesh,
        prop::collection::vec(point, 0..3),
        terrain(),
        (text(20), any::<u64>(), any::<Option<u64>>(), any::<u64>()),
    )
        .prop_map(
            |(
                ([south, west, north, east], [lat, lon]),
                buildings,
                roads,
                mesh,
                addresses,
                terrain,
                (attribution, generated_unix, osm_base_unix, osm_fingerprint),
            )| AreaBundle {
                bbox: BBox {
                    south,
                    west,
                    north,
                    east,
                },
                origin: LatLon::new(lat, lon),
                buildings,
                roads,
                mesh,
                addresses,
                terrain,
                attribution,
                generated_unix,
                osm_base_unix,
                osm_fingerprint,
            },
        )
        // NaN breaks the equality the round trip is checked with, not the
        // round trip.
        .prop_filter("no NaN in the box", |b| {
            [
                b.bbox.south,
                b.bbox.west,
                b.bbox.north,
                b.bbox.east,
                b.origin.lat,
                b.origin.lon,
            ]
            .iter()
            .all(|v| !v.is_nan())
        })
}

// ----- coverage worker messages ----------------------------------------------

fn cone() -> impl Strategy<Value = ViewCone> {
    (vec3(), vec3(), vec3(), vec3(), num(), num(), num()).prop_map(
        |(eye, forward, right, up, tan_half_h, tan_half_v, range)| ViewCone {
            eye,
            forward,
            right,
            up,
            tan_half_h,
            tan_half_v,
            range,
        },
    )
}

fn sight() -> impl Strategy<Value = Sight> {
    (
        num(),
        prop::collection::vec((any::<u32>(), num()), 0..20),
        prop::collection::vec((any::<u32>(), num()), 0..20),
    )
        .prop_map(|(range, ground, facade)| {
            let (ground, ground_d2) = ground.into_iter().unzip();
            let (facade, facade_d2) = facade.into_iter().unzip();
            Sight {
                range,
                ground,
                ground_d2,
                facade,
                facade_d2,
            }
        })
}

fn request() -> impl Strategy<Value = JobRequest> {
    let grid =
        (xz(), num(), any::<u32>(), any::<u32>()).prop_map(|(min, cell_m, w, h)| GroundGrid {
            min: Vec2::from_array(min),
            cell_m,
            w,
            h,
        });
    let inputs = (
        grid,
        prop::collection::vec(footprint(), 0..3),
        prop::collection::vec(road(), 0..3),
        num(),
        terrain(),
    )
        .prop_map(
            |(grid, footprints, roads, facade_cell_m, terrain)| LayerInputs {
                grid,
                footprints,
                roads,
                facade_cell_m,
                terrain,
            },
        );
    let job = (any::<u32>(), any::<u32>(), any::<u64>(), cone(), num()).prop_map(
        |(generation, camera_id, key, cone, subject_height_m)| Job {
            generation,
            camera_id,
            key,
            cone,
            subject_height_m,
        },
    );
    prop_oneof![
        (any::<u32>(), inputs)
            .prop_map(|(generation, inputs)| JobRequest::LoadArea { generation, inputs }),
        job.prop_map(JobRequest::Compute),
    ]
}

fn response() -> impl Strategy<Value = JobResponse> {
    let ids = (any::<u32>(), any::<u32>(), any::<u64>());
    prop_oneof![
        Just(JobResponse::Ready),
        (ids, sight()).prop_map(|((generation, camera_id, key), sight)| {
            JobResponse::Done(JobResult {
                generation,
                camera_id,
                key,
                sight,
            })
        }),
        ids.prop_map(|(generation, camera_id, key)| JobResponse::Stale {
            generation,
            camera_id,
            key,
        }),
        text(30).prop_map(JobResponse::Failed),
    ]
}

// ----- scene files -----------------------------------------------------------

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

fn model() -> impl Strategy<Value = CameraModel> {
    let link = prop::option::of(prop::sample::select(vec![
        "https://example.com/spec",
        "javascript:alert(1)",
        "http://example.com",
        "https://user@example.com",
    ]));
    (
        (
            model_id(),
            text(6),
            text(10),
            prop::sample::select(CameraKind::ALL.to_vec()),
        ),
        (0.5..=400.0f32, 0.5..=400.0f32, 8..=4000u32, 8..=4000u32),
        (prop::option::of(-10.0..=250.0f32), link, opt_text()),
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

fn placement() -> impl Strategy<Value = CameraPlacement> {
    let group = prop::option::of(prop_oneof![
        text(8),
        Just("   ".to_owned()),
        Just(format!("  {}  ", "g".repeat(100))),
    ]);
    (
        (any::<u32>(), text(10), model_id()),
        ([num(), num(), num()], num(), num(), num()),
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

fn provenance() -> impl Strategy<Value = Provenance> {
    let fingerprint = prop::option::of(prop_oneof![
        Just("0123456789ABCDEF".to_owned()),
        Just("not-hex".to_owned()),
        text(16),
    ]);
    (
        prop::option::of(0..=253_402_300_799u64),
        prop::option::of(0..=253_402_300_799u64),
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

fn scene() -> impl Strategy<Value = SceneFile> {
    let area = prop_oneof![
        Just(AreaRef::Demo),
        [
            -90.0..=90.0f64,
            -180.0..=180.0f64,
            -90.0..=90.0f64,
            -180.0..=180.0f64
        ]
        .prop_map(|[south, west, north, east]| AreaRef::Osm {
            bbox: BBox {
                south,
                west,
                north,
                east
            }
        }),
    ];
    let params = (
        prop::sample::select(DoriLevel::ALL.to_vec()),
        prop::sample::select(vec![Lighting::Day, Lighting::Night]),
        -1.0..=12.0f32,
        0.0..=2500.0f32,
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
        (prop::collection::vec(xz(), 0..6), any::<bool>())
            .prop_map(|(points, enabled)| Region { points, enabled }),
    );
    (
        area,
        (-90.0..=90.0f64, -180.0..=180.0f64),
        prop::collection::vec(placement(), 0..4),
        prop::collection::vec(model(), 0..4),
        params,
        region,
        prop::option::of(provenance()),
    )
        .prop_map(
            |(area, (lat, lon), cameras, custom_models, params, region, provenance)| {
                let mut s = SceneFile::new(area, LatLon::new(lat, lon));
                s.cameras = cameras;
                s.custom_models = custom_models;
                s.params = params;
                s.region = region;
                s.provenance = provenance;
                s
            },
        )
}

proptest! {
    #![proptest_config(config(128))]

    #[test]
    fn prop_bundles_round_trip(b in bundle()) {
        let bytes = b.encode();
        prop_assert_eq!(AreaBundle::decode(&bytes), Ok(b));
    }

    #[test]
    fn prop_job_requests_round_trip(r in request()) {
        prop_assert_eq!(JobRequest::decode(&r.encode()), Ok(r));
    }

    #[test]
    fn prop_job_responses_round_trip(r in response()) {
        prop_assert_eq!(JobResponse::decode(&r.encode()), Ok(r));
    }

    #[test]
    fn prop_loading_a_scene_is_idempotent(s in scene()) {
        // Whatever a file holds, what loads from it exports to a file that
        // loads to the same scene: every check and repair is done once.
        if let Ok(loaded) = SceneFile::from_json(&s.to_json()) {
            let again = SceneFile::from_json(&loaded.to_json()).expect("an export loads");
            prop_assert_eq!(&again, &loaded);
            prop_assert_eq!(again.to_json(), loaded.to_json());
        }
    }
}
