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

/// Which kind of special value `bits` is (quiet NaN, signalling NaN,
/// negative NaN, +inf, -inf, -0), given the format's sign, exponent and quiet
/// bits; a negative NaN counts twice.
fn special_kinds(bits: u64, sign: u64, exp: u64, quiet: u64) -> Vec<usize> {
    let nan = bits & exp == exp && bits & !(sign | exp) != 0;
    let mut out = Vec::new();
    if nan {
        out.push(if bits & quiet != 0 { 0 } else { 1 });
    }
    if nan && bits & sign != 0 {
        out.push(2);
    }
    out.extend(
        [exp, sign | exp, sign]
            .iter()
            .position(|&s| s == bits)
            .map(|i| i + 3),
    );
    out
}

/// The number strategies really draw every kind of special value, so the
/// round trips below carry them. (`any::<f32>()` draws no NaN and no
/// infinity, which this test would catch.)
#[test]
fn number_strategies_draw_nan_payloads_and_both_infinities() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::new(config(1));
    let (mut f, mut d) = ([0u32; 6], [0u32; 6]);
    for _ in 0..2000 {
        let x = num().new_tree(&mut runner).unwrap().current();
        for k in special_kinds(u64::from(x.to_bits()), 1 << 31, 0x7f80_0000, 1 << 22) {
            f[k] += 1;
        }
        let y = bits64().new_tree(&mut runner).unwrap().current();
        for k in special_kinds(y.to_bits(), 1 << 63, 0x7ff0_0000_0000_0000, 1 << 51) {
            d[k] += 1;
        }
    }
    assert!(f.iter().all(|&n| n > 0), "f32 specials drawn: {f:?}");
    assert!(d.iter().all(|&n| n > 0), "f64 specials drawn: {d:?}");
}

// ----- bit patterns ----------------------------------------------------------

/// Every float a value holds, as bit patterns in a fixed order. Comparing a
/// decoded value's with the generated value's pins both directions: value
/// equality cannot see a NaN's payload, and re-encoded bytes cannot see an
/// encoder that rewrites it (the first bytes are already rewritten).
#[derive(Debug, Default, PartialEq)]
struct Bits(Vec<u64>);

impl Bits {
    fn f(&mut self, v: &[f32]) -> &mut Self {
        self.0.extend(v.iter().map(|x| u64::from(x.to_bits())));
        self
    }

    fn d(&mut self, v: &[f64]) -> &mut Self {
        self.0.extend(v.iter().map(|x| x.to_bits()));
        self
    }

    fn footprint(&mut self, fp: &Footprint) -> &mut Self {
        for ring in std::iter::once(&fp.outer).chain(&fp.holes) {
            self.f(ring.as_flattened());
        }
        self.f(&[fp.min_height_m, fp.height_m, fp.base_m])
    }

    fn road(&mut self, r: &Road) -> &mut Self {
        self.f(&[r.width_m]).f(r.points.as_flattened())
    }

    fn terrain(&mut self, t: &Terrain) -> &mut Self {
        self.f(&t.min).f(&[t.cell_m, t.datum_m])
    }

    fn bundle(b: &AreaBundle) -> Self {
        let mut out = Self::default();
        let (bb, o) = (&b.bbox, &b.origin);
        out.d(&[bb.south, bb.west, bb.north, bb.east, o.lat, o.lon]);
        for building in &b.buildings {
            out.footprint(&building.footprint);
        }
        for r in &b.roads {
            out.road(r);
        }
        out.f(b.mesh.positions.as_flattened())
            .f(b.mesh.normals.as_flattened());
        for a in &b.addresses {
            out.f(&a.position);
        }
        out.terrain(&b.terrain);
        out
    }

    fn request(r: &JobRequest) -> Self {
        let mut out = Self::default();
        match r {
            JobRequest::LoadArea { inputs, .. } => {
                out.f(&inputs.grid.min.to_array()).f(&[inputs.grid.cell_m]);
                for fp in &inputs.footprints {
                    out.footprint(fp);
                }
                for road in &inputs.roads {
                    out.road(road);
                }
                out.f(&[inputs.facade_cell_m]).terrain(&inputs.terrain);
            }
            JobRequest::Compute(job) => {
                let c = &job.cone;
                for v in [c.eye, c.forward, c.right, c.up] {
                    out.f(&v.to_array());
                }
                out.f(&[c.tan_half_h, c.tan_half_v, c.range, job.subject_height_m]);
            }
        }
        out
    }

    fn response(r: &JobResponse) -> Self {
        let mut out = Self::default();
        if let JobResponse::Done(done) = r {
            let s = &done.sight;
            out.f(&[s.range]).f(&s.ground_d2).f(&s.facade_d2);
        }
        out
    }
}

/// NaN payloads (quiet and signalling, both signs), both infinities, a
/// negative zero and a subnormal, in turn, without end.
fn specials() -> impl Iterator<Item = f32> {
    [
        0x7fc0_0000,
        0x7fc1_2345,
        0x7f80_0001,
        0xffbf_ffff,
        0xffc0_0042,
        0x7f80_0000,
        0xff80_0000,
        0x8000_0000,
        0x0000_0001,
    ]
    .map(f32::from_bits)
    .into_iter()
    .cycle()
}

/// Fixed messages and a bundle with each special value in every float field:
/// each re-encodes byte for byte and decodes to the same bit patterns, so a
/// codec that rewrote a NaN on the way in or out fails here at any case count.
#[test]
fn special_floats_survive_the_codecs_bit_for_bit() {
    // Every field gets every special value in one of the turns.
    for offset in 0..9 {
        let sp = std::cell::RefCell::new(specials().skip(offset));
        let x = || sp.borrow_mut().next().unwrap_or(f32::NAN);
        // The f64 counterparts, also in turn.
        let d = |k: usize| {
            f64::from_bits(
                [
                    0x7ff0_0000_0000_0001,
                    0xfff8_0000_0000_0042,
                    0x7ff8_0000_0000_0000,
                    0x7ff0_0000_0000_0000,
                    0xfff0_0000_0000_0000,
                    0x8000_0000_0000_0000,
                ][(k + offset) % 6],
            )
        };
        let footprint = Footprint {
            outer: vec![[x(), x()], [x(), x()], [x(), x()]],
            holes: vec![vec![[x(), x()]]],
            min_height_m: x(),
            height_m: x(),
            base_m: x(),
        };
        let road = Road {
            osm_id: 1,
            highway: String::new(),
            name: None,
            width_m: x(),
            points: vec![[x(), x()], [x(), x()]],
        };
        let no_address = || Address {
            housenumber: None,
            street: None,
            housename: None,
            unit: None,
        };
        let b = AreaBundle {
            bbox: BBox {
                south: d(0),
                west: d(1),
                north: d(2),
                east: d(3),
            },
            origin: LatLon::new(d(4), d(5)),
            buildings: vec![Building {
                osm_id: 1,
                footprint: footprint.clone(),
                index_range: [0, 0],
                address: None,
            }],
            roads: vec![road.clone()],
            mesh: MeshData {
                positions: vec![[x(), x(), x()], [x(), x(), x()]],
                normals: vec![[x(), x(), x()]],
                indices: Vec::new(),
            },
            addresses: vec![AddressPoint {
                address: no_address(),
                position: [x(), x()],
                building: None,
            }],
            // The decoder validates terrain, which must be finite.
            terrain: Terrain::default(),
            attribution: String::new(),
            generated_unix: 0,
            osm_base_unix: None,
            osm_fingerprint: 0,
        };
        let bytes = b.encode();
        let back = AreaBundle::decode(&bytes).expect("special floats decode");
        assert_eq!(Bits::bundle(&back), Bits::bundle(&b));
        assert_eq!(back.encode(), bytes);

        let v3 = || Vec3::new(x(), x(), x());
        let requests = [
            JobRequest::LoadArea {
                generation: 1,
                inputs: LayerInputs {
                    grid: GroundGrid {
                        min: Vec2::new(x(), x()),
                        cell_m: x(),
                        w: 1,
                        h: 1,
                    },
                    footprints: vec![footprint],
                    roads: vec![road],
                    facade_cell_m: x(),
                    terrain: Terrain::default(),
                },
            },
            JobRequest::Compute(Job {
                generation: 1,
                camera_id: 2,
                key: 3,
                cone: ViewCone {
                    eye: v3(),
                    forward: v3(),
                    right: v3(),
                    up: v3(),
                    tan_half_h: x(),
                    tan_half_v: x(),
                    range: x(),
                },
                subject_height_m: x(),
            }),
        ];
        for r in requests {
            let bytes = r.encode();
            let back = JobRequest::decode(&bytes).expect("special floats decode");
            assert_eq!(Bits::request(&back), Bits::request(&r));
            assert_eq!(back.encode(), bytes);
        }

        let msg = JobResponse::Done(JobResult {
            generation: 1,
            camera_id: 2,
            key: 3,
            sight: Sight {
                range: x(),
                ground: (0..9).collect(),
                ground_d2: (0..9).map(|_| x()).collect(),
                facade: vec![9, 10],
                facade_d2: vec![x(), x()],
            },
        });
        let bytes = msg.encode();
        let back = JobResponse::decode(&bytes).expect("special floats decode");
        assert_eq!(Bits::response(&back), Bits::response(&msg));
        assert_eq!(back.encode(), bytes);
    }
}

proptest! {
    #![proptest_config(config(128))]

    #[test]
    fn prop_bundles_round_trip(b in bundle()) {
        let bytes = b.encode();
        let back = AreaBundle::decode(&bytes);
        prop_assert_eq!(back.as_ref().map(AreaBundle::encode), Ok(bytes));
        // Every float comes back with its bit pattern (NaN payloads
        // included), whichever side of the codec might rewrite it. NaN is
        // not equal to itself, so whole values are compared only without it.
        prop_assert_eq!(back.as_ref().map(Bits::bundle), Ok(Bits::bundle(&b)));
        if b == b {
            prop_assert_eq!(back, Ok(b));
        }
    }

    #[test]
    fn prop_job_requests_round_trip(r in request()) {
        let bytes = r.encode();
        let back = JobRequest::decode(&bytes);
        prop_assert_eq!(back.as_ref().map(JobRequest::encode), Ok(bytes));
        prop_assert_eq!(back.as_ref().map(Bits::request), Ok(Bits::request(&r)));
        if r == r {
            prop_assert_eq!(back, Ok(r));
        }
    }

    #[test]
    fn prop_job_responses_round_trip(r in response()) {
        let bytes = r.encode();
        let back = JobResponse::decode(&bytes);
        prop_assert_eq!(back.as_ref().map(JobResponse::encode), Ok(bytes));
        prop_assert_eq!(back.as_ref().map(Bits::response), Ok(Bits::response(&r)));
        if r == r {
            prop_assert_eq!(back, Ok(r));
        }
    }
}
