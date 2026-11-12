//! `cargo xtask treemap --wasm FILE --table N` against scripts/wasm-size-treemap.py on the same
//! module and arguments: the script's `main()` run by python3 in one process for every case (its
//! stdout and stderr captured, `--out` pointed at a scratch file), the port as the built binary.
//!
//! The modules are built here: one with every kind of section, import, data segment and name
//! subsection; variants of it with a name of each kind the port treats differently; hand-written
//! malformed ones; and a small module cut at every length and with each byte set to 0x00 and to
//! 0xFF. Each case expects:
//! - `same`: both succeed with the same stdout and stderr, except the gzip figure, which the
//!   port measures with another deflate implementation: it must be within 2% + 64 bytes of the
//!   script's (exact when `--shipped-gzip` gives it);
//! - `fails`: the script fails (a traceback, or "not a wasm module"), and so does the port;
//! - `usage`: both refuse the command line with exit status 2;
//! - `recursion`: the script crashes in its v0 demangler (RecursionError) and the port succeeds
//!   (it takes the script's fallback for that symbol: see `xtask::core::demangle`);
//! - `stricter`: the script succeeds and the port fails: the differences the port declares.
//!
//! The cut and changed modules expect whatever the script does: `same` or `fails`.
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
            let size = (i * 37) % 61 + 2;
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
}

fn case(name: &str, expect: &'static str, args: &[&str], module: Option<Vec<u8>>) -> Case {
    Case {
        name: name.to_owned(),
        expect,
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        module,
    }
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
        let out = dir.path().join("out.svg").display().to_string();
        let script: Vec<String> = [
            vec!["--wasm".to_owned(), wasm.clone(), "--out".to_owned(), out],
            c.args.clone(),
        ]
        .concat();
        script_lines.push_str(&ok(serde_json::to_string(&script)));
        script_lines.push('\n');
        port_args.push(
            [
                vec!["treemap".to_owned(), "--wasm".to_owned(), wasm],
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
    for ((c, script), args) in cases.iter().zip(&runs).zip(&port_args) {
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
                let given = c.args.iter().any(|a| a == "--shipped-gzip");
                let gzip_ok = match (sg, pg) {
                    (Some(s), Some(p)) if !given => {
                        gzip_diff = gzip_diff.max(p as i64 - s as i64);
                        gzip_diff_min = gzip_diff_min.min(p as i64 - s as i64);
                        s.abs_diff(p) <= s / 50 + 64
                    }
                    (s, p) => s == p,
                };
                script.exit == 0 && port_exit == 0 && s == p && gzip_ok && script.stderr == port_err
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
