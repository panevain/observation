//! Round trips of the bundle and the coverage worker's messages.

use crate::config;
use glam::{Vec2, Vec3};
use observation_core::address::{Address, AddressPoint};
use observation_core::building::{Footprint, MeshData};
use observation_core::bundle::{AreaBundle, Building, Road};
use observation_core::coverage::{GroundGrid, Sight};
use observation_core::coverage_job::{Job, JobRequest, JobResponse, JobResult, LayerInputs};
use observation_core::frustum::ViewCone;
use observation_core::geo::{BBox, LatLon};
use observation_core::terrain::Terrain;
use proptest::prelude::*;

// ----- building blocks -------------------------------------------------------

/// A finite number, mostly in the range real data uses, sometimes far out.
fn finite() -> impl Strategy<Value = f32> {
    prop_oneof![8 => -500.0..=500.0f32, 1 => -2.0e4..=2.0e4f32]
}

/// Mostly [`finite`], sometimes any bit pattern: NaN (with any payload), the
/// infinities and signed zeros, which the codecs must carry unchanged.
fn num() -> impl Strategy<Value = f32> {
    prop_oneof![9 => finite(), 1 => any::<f32>()]
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

/// A grid that passes [`Terrain::validate`] (which the decoder applies), or
/// flat ground. Wider than `strategies::terrain` (any height, any
/// attribution, finite numbers anywhere), as a codec should be tested.
fn any_terrain() -> impl Strategy<Value = Terrain> {
    prop_oneof![
        1 => Just(Terrain::default()),
        3 => (1..=6u32, 1..=6u32, 1.0..=1000.0f32, [finite(), finite()], finite(), text(8)).prop_flat_map(
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
        any_terrain(),
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
        any_terrain(),
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

proptest! {
    #![proptest_config(config(128))]

    #[test]
    fn prop_bundles_round_trip(b in bundle()) {
        let bytes = b.encode();
        let back = AreaBundle::decode(&bytes);
        prop_assert_eq!(back.as_ref().map(AreaBundle::encode), Ok(bytes));
        // NaN is not equal to itself, so compare values only without it.
        if b == b {
            prop_assert_eq!(back, Ok(b));
        }
    }

    #[test]
    fn prop_job_requests_round_trip(r in request()) {
        let bytes = r.encode();
        let back = JobRequest::decode(&bytes);
        prop_assert_eq!(back.as_ref().map(JobRequest::encode), Ok(bytes));
        if r == r {
            prop_assert_eq!(back, Ok(r));
        }
    }

    #[test]
    fn prop_job_responses_round_trip(r in response()) {
        let bytes = r.encode();
        let back = JobResponse::decode(&bytes);
        prop_assert_eq!(back.as_ref().map(JobResponse::encode), Ok(bytes));
        if r == r {
            prop_assert_eq!(back, Ok(r));
        }
    }
}
