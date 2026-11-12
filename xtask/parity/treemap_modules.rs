//! `cargo xtask treemap --wasm FILE --table N` against scripts/wasm-size-treemap.py on the same
//! module and arguments: the script's `main()` run by python3 in one process for every case (its
//! stdout and stderr captured, `--out` pointed at a scratch file), the port as the built binary.
//!
//! The modules are built here: one with every kind of section, import, data segment and name
//! subsection; variants of it with a name of each kind the port treats differently; hand-written
//! malformed ones; and a small module cut at every length and with each byte set to 0x00 and to
//! 0xFF. Each case expects:
//! - `same`: both succeed with the same stdout and stderr, except the gzip figure, which the
//!   port measures with another deflate implementation (both zlib at level 9; the unit tests of
//!   `cmd::treemap` check the port's framing, level and input): it must be within 4% + 32 bytes
//!   of the script's (exact when `--shipped-gzip` gives it). miniz_oxide's level 9 is up to 3.3%
//!   larger than zlib's on a real rustc release module, and as much as 469 B (1.9%) larger here;
//! - `fails`: the script fails (a traceback, or "not a wasm module"), and so does the port;
//! - `usage`: both refuse the command line with exit status 2;
//! - `recursion`: the script crashes in its v0 demangler (RecursionError) and the port succeeds
//!   (it takes the script's fallback for that symbol: see `xtask::core::demangle`);
//! - `stricter`: the script succeeds and the port fails: the differences the port declares.
//!
//! The cut and changed modules expect whatever the script does: `same` or `fails`.
//!
//! Needs Python 3.12 or later as `python3`, as `treemap_math` does (the SVG's layout sums floats
//! as 3.12 does); the harness fails with that message under an older Python.
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

/// Runs the script's `main()` for each `[argv...]` JSON line on stdin, printing
/// `{"exit": N, "stdout": …, "stderr": …}` (for a crash, stderr is the exception's name).
const HARNESS: &str = r#"
import contextlib, importlib.util, io, json, signal, sys
if sys.version_info < (3, 12):
    sys.exit("the treemap parity tests need Python 3.12 or later (from 3.12, sum() of floats "
             "is compensated, and the port follows it); python3 here is " + sys.version.split()[0])
spec = importlib.util.spec_from_file_location("treemap", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
class Hang(Exception):
    pass
def alarm(*_):
    raise Hang()
signal.signal(signal.SIGALRM, alarm)
for line in sys.stdin:
    out, err = io.StringIO(), io.StringIO()
    sys.argv = ["wasm-size-treemap.py"] + json.loads(line)
    signal.setitimer(signal.ITIMER_REAL, 5)
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            m.main()
        code = 0
    except SystemExit as e:
        if isinstance(e.code, str):
            err.write(e.code + "\n")
            code = 1
        else:
            code = e.code or 0
    except BaseException as e:
        code, err = 1, io.StringIO("crash: " + type(e).__name__)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    print(json.dumps({"exit": code, "stdout": out.getvalue(), "stderr": err.getvalue()}), flush=True)
"#;

#[derive(Debug, Deserialize)]
struct Run {
    exit: i32,
    stdout: String,
    stderr: String,
}

fn leb(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn bytes(s: &str) -> Vec<u8> {
    [leb(s.len() as u64), s.as_bytes().to_vec()].concat()
}

fn section(id: u8, body: &[u8]) -> Vec<u8> {
    [vec![id], leb(body.len() as u64), body.to_vec()].concat()
}

fn vec_of(items: &[Vec<u8>]) -> Vec<u8> {
    [leb(items.len() as u64), items.concat()].concat()
}

/// A name section's function names: index 0 is the imported function.
fn names(names: &[(u64, &[u8])]) -> Vec<u8> {
    let entries: Vec<Vec<u8>> = names
        .iter()
        .map(|(i, n)| [leb(*i), leb(n.len() as u64), n.to_vec()].concat())
        .collect();
    let funcs = vec_of(&entries);
    let module_name = [leb(3), b"app".to_vec()].concat();
    let locals = vec_of(&[]);
    section(
        0,
        &[
            bytes("name"),
            section(0, &module_name),
            section(1, &funcs),
            section(2, &locals),
        ]
        .concat(),
    )
}

/// A module with every kind of section: one imported function (and a table, a memory, a
/// global and a tag), one function per name with bodies of distinct sizes, three data segments.
fn module(func_names: &[&str]) -> Vec<u8> {
    let sizes: Vec<usize> = (0..func_names.len()).map(|i| (i * 37) % 61 + 2).collect();
    sized_module(func_names, &sizes)
}

/// [`module`] with these body sizes (each at least 2).
fn sized_module(func_names: &[&str], sizes: &[usize]) -> Vec<u8> {
    let imports = vec_of(&[
        [bytes("env"), bytes("f"), vec![0], leb(0)].concat(),
        [bytes("env"), bytes("t"), vec![1, 0x70, 1, 1, 2]].concat(),
        [bytes("env"), bytes("m"), vec![2, 0, 1]].concat(),
        [bytes("env"), bytes("g"), vec![3, 0x7F, 0]].concat(),
        [bytes("env"), bytes("e"), vec![4, 0, 0]].concat(),
    ]);
    let n = func_names.len();
    let bodies: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let size = sizes[i];
            [leb(size as u64), vec![0; size - 1], vec![0x0B]].concat()
        })
        .collect();
    let data = vec_of(&[
        [vec![0, 0x41, 0, 0x0B], bytes("hello")].concat(),
        [vec![1], bytes("passive data")].concat(),
        [vec![2, 0, 0x41, 8, 0x0B], bytes("x")].concat(),
    ]);
    let mut named: Vec<(u64, &[u8])> = vec![(0, b"__wbg_log_0".as_slice())];
    named.extend(
        func_names
            .iter()
            .enumerate()
            .map(|(i, s)| (i as u64 + 1, s.as_bytes())),
    );
    [
        b"\0asm\x01\0\0\0".to_vec(),
        section(1, &[1, 0x60, 0, 0]),
        section(2, &imports),
        section(3, &vec_of(&vec![leb(0); n])),
        section(4, &[1, 0x70, 0, 1]),
        section(5, &[1, 0, 1]),
        section(6, &[1, 0x7F, 0, 0x41, 0, 0x0B]),
        section(7, &vec_of(&[[bytes("main"), vec![0, 1]].concat()])),
        section(8, &[1]),
        section(9, &[0]),
        section(12, &[3]),
        section(13, &[0]),
        section(14, &[1, 2, 3]),
        section(10, &vec_of(&bodies)),
        section(11, &data),
        section(0, &[bytes("producers"), vec![0]].concat()),
        names(&named),
    ]
    .concat()
}

/// Function names of each kind, all attributed the same by both.
const NAMES: &[&str] = &[
    "<bevy_ecs::world::World as core::fmt::Debug>::fmt",
    "bevy_ecs[0123456789abcdef]::system::run",
    "<f32 as bevy_reflect::Reflect>::apply",
    "egui::context::Context::run",
    "<str>::trim",
    "core::fmt::write",
    "alloc::raw_vec::finish_grow",
    "wgpu::backend::Context::new",
    "naga::front::wgsl::parse",
    "observation_client::app::run",
    "observation_core::scene::Scene::load",
    "memcpy",
    "__multi3",
    "dlmalloc",
    "__rust_alloc",
    "wasm_bindgen::convert::closures::invoke",
    "_ZN4core3fmt5write17h0123456789abcdefE",
    "_ZN5serde2de5Error6custom17h0123456789abcdefE",
    "_RNvCs5JHfeWeQK0x_5alloc3foo",
    "_RNvMs1_Csgsc1MD8BB4B_14treemap_sampleINtB5_4GridKj3_Kj2_E3sumB5_",
    "main",
    "a\u{1c}",
    "",
];

struct Case {
    name: String,
    expect: &'static str,
    args: Vec<String>,
    module: Option<Vec<u8>>,
    /// Whether the script is given `--out` (it requires it; the port takes none yet).
    out: bool,
}

fn case(name: &str, expect: &'static str, args: &[&str], module: Option<Vec<u8>>) -> Case {
    Case {
        name: name.to_owned(),
        expect,
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        module,
        out: true,
    }
}

/// A module of tens of KB whose bodies compress about as much as code does: the gzip figures
/// are compared at a size where a change of level or input would show.
fn large_module() -> Vec<u8> {
    let n = 400;
    let mut seed = 12_345u32;
    let bodies: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let size = 40 + (i * 53) % 120;
            let code: Vec<u8> = (0..size)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    [0x20u8, 0x21, 0x28, 0x36, 0x41, 0x6A, 0x10, 0x0B, 0x02, 0x00]
                        [(seed >> 16) as usize % 10]
                        .wrapping_add((seed >> 28) as u8 & 1)
                })
                .collect();
            [leb(size as u64), code].concat()
        })
        .collect();
    let named: Vec<(u64, &[u8])> = (0..n)
        .map(|i| (i as u64, NAMES[i % 16].as_bytes()))
        .collect();
    [
        b"\0asm\x01\0\0\0".to_vec(),
        section(1, &[1, 0x60, 0, 0]),
        section(3, &vec_of(&vec![leb(0); n])),
        section(10, &vec_of(&bodies)),
        names(&named),
    ]
    .concat()
}

fn cases() -> Vec<Case> {
    let full = module(NAMES);
    let small = module(&NAMES[..3]);
    let t = ["--table", "50"];
    let mut cases = vec![
        case("every kind of section", "same", &t, Some(full.clone())),
        case("no table", "same", &["--table", "0"], Some(full.clone())),
        case("table of 1", "same", &["--table", "1"], Some(full.clone())),
        case(
            "table of 10^30",
            "same",
            &["--table", "1000000000000000000000000000000"],
            Some(full.clone()),
        ),
        case(
            "table ' 1_0 '",
            "same",
            &["--table", " 1_0 "],
            Some(full.clone()),
        ),
        case("table +5", "same", &["--table", "+5"], Some(full.clone())),
        case("table =-3", "stricter", &["--table=-3"], Some(full.clone())),
        case(
            "table -3",
            "stricter",
            &["--table", "-3"],
            Some(full.clone()),
        ),
        case(
            "table Arabic-Indic 3",
            "stricter",
            &["--table", "\u{663}"],
            Some(full.clone()),
        ),
        case(
            "table abc",
            "usage",
            &["--table", "abc"],
            Some(full.clone()),
        ),
        case(
            "table 1__0",
            "usage",
            &["--table", "1__0"],
            Some(full.clone()),
        ),
        case(
            "table abbreviated",
            "stricter",
            &["--tab", "3"],
            Some(full.clone()),
        ),
        case(
            "table twice",
            "stricter",
            &["--table", "1", "--table", "3"],
            Some(full.clone()),
        ),
        case(
            "raw 0 is measured",
            "same",
            &["--table", "3", "--shipped-raw", "0"],
            Some(full.clone()),
        ),
        case(
            "raw and gzip given",
            "same",
            &[
                "--table",
                "3",
                "--shipped-raw",
                "26716479",
                "--shipped-gzip",
                "8024364",
            ],
            Some(full.clone()),
        ),
        case(
            "raw negative",
            "same",
            &["--table", "3", "--shipped-raw", "-1234567"],
            Some(full.clone()),
        ),
        case(
            "raw beyond i128",
            "stricter",
            &["--table", "3", "--shipped-raw", &"9".repeat(40)],
            Some(full.clone()),
        ),
        case(
            "gzip 5000 digits",
            "usage",
            &["--table", "3", "--shipped-gzip", &"9".repeat(5000)],
            Some(full.clone()),
        ),
        case("table -0", "same", &["--table", "-0"], Some(full.clone())),
        case("table _1", "usage", &["--table", "_1"], Some(full.clone())),
        case("table 1_", "usage", &["--table", "1_"], Some(full.clone())),
        case(
            "a trailing --",
            "usage",
            &["--table", "3", "--"],
            Some(full.clone()),
        ),
        case(
            "-- before the options",
            "usage",
            &["--", "--table", "3"],
            Some(full.clone()),
        ),
        case(
            "-- as a value",
            "usage",
            &["--table", "--", "3"],
            Some(full.clone()),
        ),
        case(
            "gzip 0 is measured",
            "same",
            &["--table", "3", "--shipped-gzip", "0"],
            Some(full.clone()),
        ),
        case(
            "raw i128 min",
            "same",
            &[
                "--table",
                "3",
                "--shipped-raw",
                "-170141183460469231731687303715884105728",
            ],
            Some(full.clone()),
        ),
        case(
            "a module of tens of KB",
            "same",
            &["--table", "20"],
            Some(large_module()),
        ),
        Case {
            out: false,
            ..case("no --out", "usage", &t, Some(full.clone()))
        },
        case("no such file", "fails", &t, None),
        case("empty file", "fails", &t, Some(vec![])),
        case("magic only", "same", &t, Some(b"\0asm".to_vec())),
        case("header only", "same", &t, Some(b"\0asm\x01\0\0\0".to_vec())),
        case("not wasm", "fails", &t, Some(b"\0wasm\x01\0\0".to_vec())),
    ];
    let head = b"\0asm\x01\0\0\0".to_vec();
    let with = |name: &str, expect, body: Vec<u8>| {
        case(name, expect, &t, Some([head.clone(), body].concat()))
    };
    cases.extend([
        with(
            "padded section size",
            "same",
            [vec![1, 0x83, 0x80, 0x80, 0], vec![1, 0x60, 0]].concat(),
        ),
        with(
            "section size above 2^64",
            "stricter",
            vec![
                1, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02,
            ],
        ),
        with(
            "section size 2^64 - 1",
            "same",
            vec![
                1, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01, 7,
            ],
        ),
        with(
            "code count beyond the bodies",
            "fails",
            section(10, &[5, 2, 0, 0x0B]),
        ),
        with(
            // The body is read as a section too: an empty type section.
            "a code body past the section's end",
            "same",
            vec![10, 1, 1, 1, 0],
        ),
        with("data flag 3", "same", section(11, &[1, 3, 2, 9, 9])),
        with("data without end", "fails", section(11, &[1, 0, 0x41, 0])),
        with(
            "two import sections",
            "same",
            [
                section(2, &vec_of(&[[bytes("a"), bytes("b"), vec![0, 0]].concat()])),
                section(
                    2,
                    &vec_of(&[
                        [bytes("a"), bytes("b"), vec![0, 0]].concat(),
                        [bytes("a"), bytes("c"), vec![0, 0]].concat(),
                    ]),
                ),
                section(10, &vec_of(&[vec![2, 0, 0x0B], vec![3, 0, 0, 0x0B]])),
                names(&[(1, b"egui::a"), (2, b"bevy::b"), (3, b"wgpu::c")]),
            ]
            .concat(),
        ),
        with(
            "a name given twice, the last wins",
            "same",
            [
                section(10, &vec_of(&[vec![2, 0, 0x0B]])),
                names(&[(0, b"egui::a"), (0, b"bevy::a")]),
            ]
            .concat(),
        ),
        with(
            "names twice, the last wins",
            "same",
            [
                section(10, &vec_of(&[vec![2, 0, 0x0B], vec![3, 0, 0, 0x0B]])),
                names(&[(0, b"egui::a"), (1, b"egui::b"), (0, b"bevy::a")]),
                names(&[(1, b"wgpu::b"), (7, b"naga::x")]),
            ]
            .concat(),
        ),
        with(
            "names section named otherwise",
            "same",
            [
                section(10, &vec_of(&[vec![2, 0, 0x0B]])),
                section(
                    0,
                    &[
                        bytes("names"),
                        section(1, &vec_of(&[[leb(0), bytes("egui::a")].concat()])),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        ),
        with(
            "names not UTF-8",
            "same",
            [
                section(10, &vec_of(&[vec![2, 0, 0x0B], vec![3, 0, 0, 0x0B]])),
                names(&[(0, b"egui\xff::a"), (1, b"\xc3(bevy::x")]),
            ]
            .concat(),
        ),
        with(
            "a name past the end of the file",
            "same",
            [
                section(10, &vec_of(&[vec![2, 0, 0x0B]])),
                section(0, &[bytes("name"), vec![1, 40, 1, 0, 7]].concat()),
            ]
            .concat(),
        ),
        with("custom section name past the end", "same", vec![0, 1, 9]),
    ]);
    // A cut-off last section whose declared size is 2^e: the SVG layout divides by zero at some
    // sizes (not all: the float rounding decides), next to one function or one segment, and both
    // fail before the table.
    let func = [
        section(1, &[1, 0x60, 0, 0]),
        section(3, &[1, 0]),
        section(10, &[1, 2, 0, 0x0B]),
        names(&[(0, b"core::fmt::write")]),
    ]
    .concat();
    let data = [
        section(5, &[1, 0, 1]),
        section(11, &[1, 1, 3, b'a', b'b', b'c']),
    ]
    .concat();
    for (body, e, expect) in [
        (&func, 50, "same"),
        (&func, 54, "fails"),
        (&func, 56, "same"),
        (&func, 60, "fails"),
        (&func, 63, "fails"),
        (&data, 54, "same"),
        (&data, 56, "fails"),
    ] {
        let name = format!("a last section of 2^{e} bytes, cut off");
        cases.push(with(
            &name,
            expect,
            [body.clone(), vec![1], leb(1u64 << e)].concat(),
        ));
    }
    // Many crates of every family, sizes from 2 bytes to 30 KB, some names too long for their
    // tiles: what the treemap's grouping, tiling and labels do.
    let prefixes = [
        "bevy_ecs",
        "bevy_render",
        "encase",
        "egui",
        "epaint",
        "wgpu_core",
        "naga",
        "glow",
        "core",
        "alloc",
        "std",
        "observation_client",
        "observation_core",
        "serde",
        "smallvec",
        "a_crate_with_a_name_much_too_long_for_its_tile",
    ];
    let mut many_names = Vec::new();
    let mut many_sizes = Vec::new();
    for k in 0..64 {
        many_names.push(format!(
            "{}{}::f",
            prefixes[k % prefixes.len()],
            k / prefixes.len()
        ));
        many_sizes.push(2 + (k * k * k * 7919) % 30_000);
    }
    let names: Vec<&str> = many_names.iter().map(String::as_str).collect();
    let many = sized_module(&names, &many_sizes);
    let many_case = |name: &str, expect, extra: &[&str]| {
        case(
            name,
            expect,
            &[&["--table", "100"], extra].concat(),
            Some(many.clone()),
        )
    };
    cases.extend([
        many_case("many crates", "same", &[]),
        many_case("threshold 0", "same", &["--threshold", "0"]),
        many_case("threshold 0.02", "same", &["--threshold", "0.02"]),
        many_case("threshold 1", "same", &["--threshold", "1"]),
        many_case("threshold -1", "same", &["--threshold", "-1"]),
        many_case("threshold nan", "fails", &["--threshold", "nan"]),
        many_case("threshold -inf", "usage", &["--threshold", "-Infinity"]),
        many_case("threshold =-inf", "stricter", &["--threshold=-Infinity"]),
        many_case("threshold inf", "same", &["--threshold", "iNf"]),
        many_case("threshold -.5", "same", &["--threshold", "-.5"]),
        many_case("threshold 1e-2", "same", &["--threshold", "1e-2"]),
        many_case("commit -5", "same", &["--commit", "-5"]),
        many_case("commit -x", "usage", &["--commit", "-x"]),
        many_case("commit -1e5", "usage", &["--commit", "-1e5"]),
        many_case("commit =-x", "stricter", &["--commit=-x"]),
        many_case("raw -1_0", "usage", &["--shipped-raw", "-1_0"]),
        many_case("threshold ' 1_0e-3 '", "same", &["--threshold", " 1_0e-3 "]),
        many_case("threshold 1__0", "usage", &["--threshold", "1__0"]),
        many_case(
            "threshold Arabic-Indic 3",
            "stricter",
            &["--threshold", "\u{663}"],
        ),
        many_case(
            "commit, date and stage to escape",
            "same",
            &[
                "--commit",
                "<a & \"b\">",
                "--date",
                "2026-11-12 \u{e9}",
                "--stage",
                "x > y",
            ],
        ),
        many_case("commit -", "same", &["--commit", "-"]),
        many_case("commit '-5 '", "same", &["--commit", "-5 "]),
        many_case("commit '-x y'", "same", &["--commit", "-x y"]),
        many_case("commit '-Os build'", "same", &["--commit", "-Os build"]),
        many_case("date -", "same", &["--date", "-"]),
        many_case("date '-5 '", "same", &["--date", "-5 "]),
        many_case("date '-x y'", "same", &["--date", "-x y"]),
        many_case("date '-Os build'", "same", &["--date", "-Os build"]),
        many_case("stage -", "same", &["--stage", "-"]),
        many_case("stage '-5 '", "same", &["--stage", "-5 "]),
        many_case("stage '-x y'", "same", &["--stage", "-x y"]),
        many_case("stage '-Os build'", "same", &["--stage", "-Os build"]),
        many_case("commit '-5\\n'", "same", &["--commit", "-5\n"]),
        many_case("commit '--x y'", "same", &["--commit", "--x y"]),
        many_case("commit '--da=x y'", "usage", &["--commit", "--da=x y"]),
        many_case("commit '-h x'", "usage", &["--commit", "-h x"]),
        many_case("commit --date", "usage", &["--commit", "--date"]),
        many_case("commit -5.", "usage", &["--commit", "-5."]),
        many_case(
            "commit Arabic-Indic -3",
            "stricter",
            &["--commit", "-\u{663}"],
        ),
        many_case("threshold '-1 '", "same", &["--threshold", "-1 "]),
        many_case("raw '-5 '", "same", &["--shipped-raw", "-5 "]),
        many_case(
            "gzip and raw given",
            "same",
            &["--shipped-raw", "26716479", "--shipped-gzip", "8024364"],
        ),
    ]);
    // Names the port treats differently from the script, each in an otherwise valid module.
    for (name, expect, sym) in [
        (
            "a v0 back-reference loop",
            "recursion",
            "_RNvXNtCs4NIexwV1FGX_4core3anyReNtB2_3Any7type_idCsiuSwmoQr08c_3std",
        ),
        ("a v0 constant without end", "fails", "_RIC3fooKj"),
        ("a non-ASCII v0 symbol", "stricter", "_RC3f\u{e9}"),
    ] {
        cases.push(case(name, expect, &t, Some(module(&[NAMES[0], sym]))));
    }
    for n in 0..small.len() {
        cases.push(case(
            &format!("cut at {n}"),
            "rule",
            &t,
            Some(small[..n].to_vec()),
        ));
    }
    for i in 8..small.len() {
        for b in [0x00, 0xFF] {
            let mut m = small.clone();
            m[i] = b;
            cases.push(case(&format!("byte {i} = {b:#04x}"), "rule", &t, Some(m)));
        }
    }
    cases
}

/// The gzip figure of the totals line, and the stdout without it.
fn split_gzip(stdout: &str) -> (String, Option<u64>) {
    match stdout.rsplit_once("; gzip -9 ") {
        Some((rest, tail)) => {
            let n = tail.trim_end().trim_end_matches(" B").replace(',', "");
            (rest.to_owned(), n.parse().ok())
        }
        None => (stdout.to_owned(), None),
    }
}

/// The SVG with its `N.NN MB gzip` figures taken out, and the figures.
fn split_svg_gzip(svg: &str) -> (String, Vec<f64>) {
    let re = ok(regex::Regex::new(r"(-?[0-9]+\.[0-9]{2}) MB gzip"));
    let figures = re
        .captures_iter(svg)
        .map(|c| ok(c[1].parse::<f64>()))
        .collect();
    (re.replace_all(svg, "N MB gzip").into_owned(), figures)
}

#[test]
fn port_and_script_agree_on_every_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let dir = ok(tempfile::tempdir());
    let cases = cases();
    let mut script_lines = String::new();
    let mut port_args = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let wasm = dir.path().join(format!("{i}.wasm"));
        if let Some(m) = &c.module {
            ok(std::fs::write(&wasm, m));
        }
        let wasm = wasm.display().to_string();
        let out = dir.path().join(format!("s{i}.svg")).display().to_string();
        let port_svg = dir.path().join(format!("p{i}.svg")).display().to_string();
        let (out, port_svg) = if c.out {
            (
                vec!["--out".to_owned(), out],
                vec!["--out".to_owned(), port_svg],
            )
        } else {
            (vec![], vec![])
        };
        let script: Vec<String> =
            [vec!["--wasm".to_owned(), wasm.clone()], out, c.args.clone()].concat();
        script_lines.push_str(&ok(serde_json::to_string(&script)));
        script_lines.push('\n');
        port_args.push(
            [
                vec!["treemap".to_owned(), "--wasm".to_owned(), wasm],
                port_svg,
                c.args.clone(),
            ]
            .concat(),
        );
    }
    let input = dir.path().join("cases.jsonl");
    ok(std::fs::write(&input, script_lines));
    let out = ok(Command::new("python3")
        .args(["-I", "-c", HARNESS])
        .arg(root.join("scripts/wasm-size-treemap.py"))
        .stdin(ok(std::fs::File::open(&input)))
        .output());
    assert!(
        out.status.success(),
        "the harness failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs: Vec<Run> = ok(String::from_utf8(out.stdout))
        .lines()
        .map(|l| ok(serde_json::from_str(l)))
        .collect();
    assert_eq!(runs.len(), cases.len());

    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut failures = Vec::new();
    let (mut gzip_diff, mut gzip_diff_min) = (0i64, 0i64);
    for (i, ((c, script), args)) in cases.iter().zip(&runs).zip(&port_args).enumerate() {
        let svg =
            |side: &str| std::fs::read_to_string(dir.path().join(format!("{side}{i}.svg"))).ok();
        let port = ok(Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(args)
            .output());
        let port_exit = port.status.code().unwrap_or(-1);
        let port_out = String::from_utf8_lossy(&port.stdout).into_owned();
        let port_err = String::from_utf8_lossy(&port.stderr).into_owned();
        let expect = match c.expect {
            "rule" if script.exit == 0 => "same",
            "rule" => "fails",
            e => e,
        };
        let good = match expect {
            "same" => {
                let (s, sg) = split_gzip(&script.stdout);
                let (p, pg) = split_gzip(&port_out);
                // `--shipped-gzip 0` is not given: the figure is measured.
                let given = c
                    .args
                    .windows(2)
                    .any(|w| w[0] == "--shipped-gzip" && w[1] != "0");
                let gzip_ok = match (sg, pg) {
                    (Some(s), Some(p)) if !given => {
                        gzip_diff = gzip_diff.max(p as i64 - s as i64);
                        gzip_diff_min = gzip_diff_min.min(p as i64 - s as i64);
                        s.abs_diff(p) <= s / 25 + 32
                    }
                    (s, p) => s == p,
                };
                let (s_svg, s_mb) = split_svg_gzip(&svg("s").unwrap_or_default());
                let (p_svg, p_mb) = split_svg_gzip(&svg("p").unwrap_or_default());
                let svg_ok = !s_svg.is_empty()
                    && s_svg == p_svg
                    && s_mb.len() == p_mb.len()
                    && s_mb.iter().zip(&p_mb).all(|(s, p)| {
                        if given {
                            s == p
                        } else {
                            (s - p).abs() <= 0.01 + s.abs() / 25.0
                        }
                    });
                script.exit == 0
                    && port_exit == 0
                    && s == p
                    && gzip_ok
                    && svg_ok
                    && script.stderr == port_err
            }
            // The script's own message is kept; a traceback is not compared.
            "fails" => {
                script.exit == 1
                    && port_exit == 1
                    && (script.stderr.starts_with("crash: ") || script.stderr == port_err)
            }
            "usage" => script.exit == 2 && port_exit == 2,
            "recursion" => script.stderr == "crash: RecursionError" && port_exit == 0,
            "stricter" => script.exit == 0 && port_exit != 0,
            _ => false,
        };
        if !good {
            failures.push(format!(
                "{} ({expect}): script {script:?}; port exit {port_exit}, stdout {port_out:?}, stderr {port_err:?}",
                c.name
            ));
        }
        *counts.entry(format!("{}/{expect}", c.expect)).or_default() += 1;
    }
    println!(
        "{} cases: {counts:?}; port's gzip minus the script's: {gzip_diff_min} to {gzip_diff} bytes",
        cases.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
