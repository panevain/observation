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

/// Any `f32` bit pattern. Uniform bits are NaN only 0.4% of the time and
/// never an infinity in practice, so NaN (quiet or signalling, either sign,
/// any payload), both infinities and both zeros are also drawn on purpose.
/// proptest's `any::<f32>()` would draw none of NaN or the infinities.
fn bits32() -> impl Strategy<Value = f32> {
    prop_oneof![
        3 => any::<u32>().prop_map(f32::from_bits),
        1 => (any::<bool>(), 1..(1u32 << 23)).prop_map(|(negative, payload)| {
            f32::from_bits(u32::from(negative) << 31 | 0x7f80_0000 | payload)
        }),
        1 => prop::sample::select(vec![f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0]),
    ]
}

/// The `f64` counterpart of [`bits32`].
fn bits64() -> impl Strategy<Value = f64> {
    prop_oneof![
        3 => any::<u64>().prop_map(f64::from_bits),
        1 => (any::<bool>(), 1..(1u64 << 52)).prop_map(|(negative, payload)| {
            f64::from_bits(u64::from(negative) << 63 | 0x7ff0_0000_0000_0000 | payload)
        }),
        1 => prop::sample::select(vec![f64::INFINITY, f64::NEG_INFINITY, 0.0, -0.0]),
    ]
}

/// Mostly [`finite`], sometimes any bit pattern ([`bits32`]), which the codecs
/// must carry unchanged.
fn num() -> impl Strategy<Value = f32> {
    prop_oneof![9 => finite(), 1 => bits32()]
}

/// The `f64` counterpart of [`num`].
fn num64() -> impl Strategy<Value = f64> {
    prop_oneof![9 => -180.0..=180.0f64, 1 => bits64()]
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
        ([num64(), num64(), num64(), num64()], [num64(), num64()]),
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

/// The kinds of special value a strategy drew, by bit pattern.
#[derive(Default, Debug)]
struct Specials {
    quiet_nan: u32,
    signalling_nan: u32,
    negative_nan: u32,
    infinity: u32,
    neg_infinity: u32,
    neg_zero: u32,
}

impl Specials {
    fn add(&mut self, bits: u64, sign: u64, exp: u64, quiet: u64) {
        let mantissa = bits & !(sign | exp);
        if bits & exp == exp && mantissa != 0 {
            if mantissa & quiet != 0 {
                self.quiet_nan += 1;
            } else {
                self.signalling_nan += 1;
            }
            if bits & sign != 0 {
                self.negative_nan += 1;
            }
        } else if bits == exp {
            self.infinity += 1;
        } else if bits == sign | exp {
            self.neg_infinity += 1;
        } else if bits == sign {
            self.neg_zero += 1;
        }
    }

    fn all_seen(&self) -> bool {
        [
            self.quiet_nan,
            self.signalling_nan,
            self.negative_nan,
            self.infinity,
            self.neg_infinity,
            self.neg_zero,
        ]
        .iter()
        .all(|&n| n > 0)
    }
}

/// The number strategies really draw every kind of special value, so the
/// round trips below carry them. (`any::<f32>()` draws no NaN and no
/// infinity, which this test would catch.)
#[test]
fn number_strategies_draw_nan_payloads_and_both_infinities() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::new(config(1));
    let (mut f, mut d) = (Specials::default(), Specials::default());
    for _ in 0..2000 {
        let x = num().new_tree(&mut runner).unwrap().current();
        f.add(u64::from(x.to_bits()), 1 << 31, 0x7f80_0000, 1 << 22);
        let y = bits64().new_tree(&mut runner).unwrap().current();
        d.add(y.to_bits(), 1 << 63, 0x7ff0_0000_0000_0000, 1 << 51);
    }
    assert!(f.all_seen(), "f32 specials drawn: {f:?}");
    assert!(d.all_seen(), "f64 specials drawn: {d:?}");
}

/// A fixed message and bundle holding NaN payloads (quiet and signalling,
/// both signs), both infinities and a negative zero re-encode byte for byte
/// and decode to the same bit patterns.
#[test]
fn special_floats_survive_the_codecs_bit_for_bit() {
    let f32s = [
        f32::from_bits(0x7fc0_0000),
        f32::from_bits(0x7fc1_2345),
        f32::from_bits(0x7f80_0001),
        f32::from_bits(0xffbf_ffff),
        f32::INFINITY,
        f32::NEG_INFINITY,
        -0.0,
        f32::from_bits(1),
    ];
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    let msg = JobResponse::Done(JobResult {
        generation: 1,
        camera_id: 2,
        key: 3,
        sight: Sight {
            range: f32::NAN,
            ground: (0..8).collect(),
            ground_d2: f32s.to_vec(),
            facade: vec![9],
            facade_d2: vec![f32::NEG_INFINITY],
        },
    });
    let bytes = msg.encode();
    let Ok(JobResponse::Done(back)) = JobResponse::decode(&bytes) else {
        panic!("special floats did not decode");
    };
    assert_eq!(bits(&back.sight.ground_d2), bits(&f32s));
    assert_eq!(back.sight.range.to_bits(), f32::NAN.to_bits());
    assert_eq!(JobResponse::Done(back).encode(), bytes);

    let f64s = [
        f64::from_bits(0x7ff8_0000_0000_0001),
        f64::from_bits(0xfff0_0000_0000_0001),
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    let b = AreaBundle {
        bbox: BBox {
            south: f64s[0],
            west: f64s[1],
            north: f64s[2],
            east: f64s[3],
        },
        origin: LatLon::new(-0.0, f64::NAN),
        buildings: Vec::new(),
        roads: Vec::new(),
        mesh: MeshData {
            positions: vec![[f32::NAN, f32::INFINITY, f32::NEG_INFINITY]],
            normals: Vec::new(),
            indices: Vec::new(),
        },
        addresses: Vec::new(),
        terrain: Terrain::default(),
        attribution: String::new(),
        generated_unix: 0,
        osm_base_unix: None,
        osm_fingerprint: 0,
    };
    let bytes = b.encode();
    let back = AreaBundle::decode(&bytes).expect("special floats decode");
    let got = [
        back.bbox.south,
        back.bbox.west,
        back.bbox.north,
        back.bbox.east,
    ];
    assert_eq!(got.map(f64::to_bits), f64s.map(f64::to_bits));
    assert_eq!(back.origin.lat.to_bits(), (-0.0f64).to_bits());
    assert_eq!(back.encode(), bytes);
}

proptest! {
    #![proptest_config(config(128))]

    #[test]
    fn prop_bundles_round_trip(b in bundle()) {
        let bytes = b.encode();
        let back = AreaBundle::decode(&bytes);
        prop_assert_eq!(back.as_ref().map(AreaBundle::encode), Ok(bytes));
        // The bytes are compared above, so every bit pattern (NaN payloads
        // included) is checked. NaN is not equal to itself, so values are
        // compared as well only when there is none.
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
