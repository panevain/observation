//! `cargo xtask treemap --twiggy-csv FILE` against scripts/wasm-size-treemap.py on the same CSV
//! and arguments: the script's `main()` run by python3 in one process for every case, the port
//! as the built binary. The CSVs are written here: twiggy's own shape, each header and item rule
//! of `parse_twiggy_csv`, each state of `csv.reader` (quotes, doubled quotes, a quote after a
//! quoted field, a quoted field over several lines or open at the end, empty lines, every line
//! boundary `str.splitlines()` knows, the field limit), and each form of size `int()` reads.
//! Each case expects:
//! - `rule`: what the script does: `same` when it succeeds (both succeed with the same exit
//!   status, stdout, stderr and SVG, byte for byte: a CSV has no gzip figure to measure), else
//!   `fails` (the port fails too);
//! - `usage`: both refuse the command line with exit status 2;
//! - `stricter`: the script succeeds and the port fails: the differences the port declares
//!   (sizes whose magnitudes add up to more than 2^53, and digits other than ASCII).
//!
//! Needs Python 3.12 or later as `python3`, as the other treemap harnesses do.
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

/// Runs the script's `main()` for each `[argv...]` JSON line on stdin, printing
/// `{"exit": N, "stdout": …, "stderr": …}` (for a crash, stderr is the exception's name).
const HARNESS: &str = r#"
import contextlib, importlib.util, io, json, sys
if sys.version_info < (3, 12):
    sys.exit("the treemap parity tests need Python 3.12 or later (from 3.12, sum() of floats "
             "is compensated, and the port follows it); python3 here is " + sys.version.split()[0])
spec = importlib.util.spec_from_file_location("treemap", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
for line in sys.stdin:
    out, err = io.StringIO(), io.StringIO()
    sys.argv = ["wasm-size-treemap.py"] + json.loads(line)
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
    print(json.dumps({"exit": code, "stdout": out.getvalue(), "stderr": err.getvalue()}), flush=True)
"#;

#[derive(Debug, Deserialize)]
struct Run {
    exit: i32,
    stdout: String,
    stderr: String,
}

struct Case {
    name: String,
    expect: &'static str,
    /// The file's bytes (`None`: no file).
    csv: Option<Vec<u8>>,
    /// Arguments after `--twiggy-csv FILE --out SVG`.
    args: Vec<String>,
    /// Whether `--twiggy-csv FILE` is given (`usage` cases may leave it out).
    input: bool,
}

const HEADER: &str = "Shallow Bytes,Shallow %,Item\n";

/// Rows as twiggy writes them, with real items of the client's module (wasm-bindgen demangles
/// the names section, so most functions arrive as readable paths).
const ROWS: &str = "\
5120,10.00%,data[0]
4096,8.00%,\"bevy_ecs::world::World::spawn::h0123456789abcdef\"
3000,5.86%,\"<bevy_ecs::query::state::QueryState<D,F> as core::fmt::Debug>::fmt\"
2048,4.00%,code[0]
1500,2.93%,core::fmt::write
1200,2.34%,\"<f32 as bevy_reflect::Reflect>::reflect_ref\"
1000,1.95%,_ZN4core3fmt9Formatter3pad17h0123456789abcdefE
900,1.76%,__wbg_new_abc123
800,1.56%,alloc::raw_vec::RawVec<T>::grow_one
700,1.37%,observation_client::ui::inspector::show
640,1.25%,\"custom section '.debug_info'\"
600,1.17%,\"\"\"name\"\" section\"
500,0.98%,type[12]
400,0.78%,import[3]
300,0.59%,export[\"main\"]
200,0.39%,\"... and 120 more.\"
100,0.20%,wasm magic bytes
90,0.18%,function[4]
80,0.16%,table[0]
70,0.14%,elem[0]
60,0.12%,global[1]
50,0.10%,memory[0]
40,0.08%,.debug_line
30,0.06%,std::panicking::begin_panic_handler
20,0.04%,__rust_alloc
10,0.02%,dlmalloc::dlmalloc::Dlmalloc::malloc
";

fn case(name: &str, expect: &'static str, csv: &str, args: &[&str]) -> Case {
    Case {
        name: name.to_owned(),
        expect,
        csv: Some(csv.as_bytes().to_vec()),
        args: args.iter().map(|&a| a.to_owned()).collect(),
        input: true,
    }
}

fn cases() -> Vec<Case> {
    let full = format!("{HEADER}{ROWS}");
    let mut v = vec![
        case("twiggy's shape", "rule", &full, &["--table", "15"]),
        case("no table", "rule", &full, &[]),
        case(
            "all crates, threshold 0",
            "rule",
            &full,
            &["--table", "40", "--threshold", "0"],
        ),
        case(
            "shipped sizes given",
            "rule",
            &full,
            &[
                "--table",
                "5",
                "--shipped-raw",
                "70000",
                "--shipped-gzip",
                "20000",
            ],
        ),
        case(
            "shipped sizes 0",
            "rule",
            &full,
            &["--table", "5", "--shipped-raw", "0", "--shipped-gzip", "0"],
        ),
        case(
            "subtitle",
            "rule",
            &full,
            &[
                "--commit",
                "abc123456",
                "--date",
                "2026-11-12",
                "--stage",
                "after wasm-bindgen",
            ],
        ),
        // Headers.
        case(
            "columns reordered",
            "rule",
            "Item,Shallow Bytes\nfoo::a,10\ndata[1],5\nbar::b,7\n",
            &["--table", "5"],
        ),
        case(
            "header padded and in capitals",
            "rule",
            " ITEM , SHALLOW BYTES \nfoo::a,10\nbar::b,7\n",
            &["--table", "5"],
        ),
        case(
            "duplicate header: the last wins",
            "rule",
            "Item,Shallow Bytes,Item\nx::a,10,foo::b\ny::c,5,bar::d\n",
            &["--table", "5"],
        ),
        case(
            "no size column: column 0",
            "rule",
            "Bytes,Item\n10,foo::a\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "no item column: the last",
            "rule",
            "Shallow Bytes,What\n10,foo::a\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "one column",
            "rule",
            "Shallow Bytes\n10\n7\n",
            &["--table", "5"],
        ),
        case(
            "empty header: the last field",
            "rule",
            "\n10,foo::a\n7,x,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "byte order mark",
            "rule",
            "\u{feff}Shallow Bytes,Item\n10,foo::a\n7,bar::b\n",
            &["--table", "5"],
        ),
        case("header only", "rule", HEADER, &["--table", "5"]),
        case("empty file", "rule", "", &["--table", "5"]),
        case(
            "short rows skipped",
            "rule",
            "Shallow Bytes,Shallow %,Item\n10\n10,1%\n10,1%,foo::a\n7,1%,bar::b\n",
            &["--table", "5"],
        ),
        // csv.reader.
        case(
            "quoted commas",
            "rule",
            "Shallow Bytes,Item\n10,\"foo::a<T,U>\"\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "doubled quotes",
            "rule",
            "Shallow Bytes,Item\n10,\"foo::\"\"a\"\"\"\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "quote inside a field",
            "rule",
            "Shallow Bytes,Item\n10,foo::a\"b\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "text after a quoted field",
            "rule",
            "Shallow Bytes,Item\n10,\"foo\"::a\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "quoted size",
            "rule",
            "Shallow Bytes,Item\n\"10\",foo::a\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "quoted field over lines",
            "rule",
            "Shallow Bytes,Item\n10,\"foo::\na\"\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "quoted field open at the end",
            "rule",
            "Shallow Bytes,Item\n7,bar::b\n10,\"foo::a",
            &["--table", "5"],
        ),
        case(
            "empty quoted field open at the end",
            "rule",
            "Shallow Bytes,Item\n7,bar::b\n10,\"",
            &["--table", "5"],
        ),
        case(
            "quote open in the size",
            "rule",
            "Shallow Bytes,Item\n7,bar::b\n\"10,foo::a\n",
            &["--table", "5"],
        ),
        case(
            "empty lines",
            "rule",
            "Shallow Bytes,Item\n\n10,foo::a\n\n\n7,bar::b\n\n",
            &["--table", "5"],
        ),
        case(
            "CRLF",
            "rule",
            "Shallow Bytes,Item\r\n10,foo::a\r\n7,bar::b\r\n",
            &["--table", "5"],
        ),
        case(
            "lone CR",
            "rule",
            "Shallow Bytes,Item\r10,foo::a\r7,bar::b\r",
            &["--table", "5"],
        ),
        case(
            "every line boundary",
            "rule",
            "Shallow Bytes,Item\n10,foo::a\x0b7,bar::b\x0c3,baz::c\x1c2,qux::d\x1d1,x::e\x1e4,y::f\u{85}5,z::g\u{2028}6,w::h\u{2029}8,v::i\n",
            &["--table", "15"],
        ),
        case(
            "quoted size over lines",
            "rule",
            "Shallow Bytes,Item\n\"1\n0\",foo::a\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "items next to the rules",
            "rule",
            "Shallow Bytes,Item\n10,wasmtime::a\n9,data_encoding::b\n8,typenum::c\n7,codespan::d\n",
            &["--table", "9"],
        ),
        case(
            "size after U+001F",
            "rule",
            "Shallow Bytes,Item\n\x1f5,a::x\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "size after U+0085",
            "rule",
            "Shallow Bytes,Item\n\u{85}5,a::x\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "NUL in an item",
            "rule",
            "Shallow Bytes,Item\n10,foo::a\0b\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "no newline at the end",
            "rule",
            "Shallow Bytes,Item\n10,foo::a\n7,bar::b",
            &["--table", "5"],
        ),
        case(
            "trailing comma",
            "rule",
            "Shallow Bytes,Item\n10,foo::a,\n7,bar::b,\n",
            &["--table", "5"],
        ),
        // Sizes, as int() reads them.
        case(
            "sizes int() reads",
            "rule",
            "Shallow Bytes,Item\n 10 ,a::x\n+7,b::x\n1_000,c::x\n007,d::x\n\t5\u{a0},e::x\n",
            &["--table", "9"],
        ),
        case(
            "negative size",
            "rule",
            "Shallow Bytes,Item\n100,a::x\n-3,b::x\n",
            &["--table", "5"],
        ),
        case(
            "negative total",
            "rule",
            "Shallow Bytes,Item\n-100,a::x\n3,b::x\n",
            &["--table", "5"],
        ),
        case(
            "zero total",
            "rule",
            "Shallow Bytes,Item\n0,a::x\n0,b::x\n",
            &["--table", "5"],
        ),
        case(
            "zero total, no table",
            "rule",
            "Shallow Bytes,Item\n0,a::x\n",
            &[],
        ),
        case("empty size", "rule", "Shallow Bytes,Item\n,a::x\n", &[]),
        case("float size", "rule", "Shallow Bytes,Item\n1.5,a::x\n", &[]),
        case("text size", "rule", "Shallow Bytes,Item\nten,a::x\n", &[]),
        case(
            "double underscore",
            "rule",
            "Shallow Bytes,Item\n1__0,a::x\n",
            &[],
        ),
        case(
            "size of a skipped item is still read",
            "rule",
            "Shallow Bytes,Item\nx,.debug_info\n7,bar::b\n",
            &["--table", "5"],
        ),
        case(
            "2^53 in all",
            "rule",
            "Shallow Bytes,Item\n9007199254740990,a::x\n2,b::x\n",
            &["--table", "5"],
        ),
        case(
            "negative 2^53 in all",
            "rule",
            "Shallow Bytes,Item\n-9007199254740990,a::x\n-2,b::x\n",
            &["--table", "5"],
        ),
        case(
            "over 2^53 in all",
            "stricter",
            "Shallow Bytes,Item\n9007199254740990,a::x\n3,b::x\n",
            &["--table", "5"],
        ),
        case(
            "over 2^53 by magnitude",
            "stricter",
            "Shallow Bytes,Item\n9007199254740990,a::x\n-3,b::x\n",
            &["--table", "5"],
        ),
        case(
            "one size over 2^64",
            "stricter",
            "Shallow Bytes,Item\n36893488147419103232,a::x\n",
            &["--table", "5"],
        ),
        case(
            "Arabic-Indic digits",
            "stricter",
            "Shallow Bytes,Item\n\u{661}\u{662},a::x\n7,bar::b\n",
            &["--table", "5"],
        ),
        // Arguments.
        case("bare --", "usage", &full, &["--"]),
        case(
            "negative table (declared with --wasm)",
            "stricter",
            &full,
            &["--table", "-1"],
        ),
    ];
    v.push(Case {
        name: "both inputs".into(),
        expect: "usage",
        csv: Some(full.clone().into_bytes()),
        args: vec!["--wasm".into(), "x.wasm".into()],
        input: true,
    });
    v.push(Case {
        name: "no input".into(),
        expect: "usage",
        csv: None,
        args: vec![],
        input: false,
    });
    v.push(Case {
        name: "missing file".into(),
        expect: "rule",
        csv: None,
        args: vec![],
        input: true,
    });
    v.push(Case {
        name: "not UTF-8".into(),
        expect: "rule",
        csv: Some(b"Shallow Bytes,Item\n10,foo::\xff\n".to_vec()),
        args: vec![],
        input: true,
    });
    v.push(Case {
        name: "a 4,301-digit size".into(),
        expect: "rule",
        csv: Some(format!("Shallow Bytes,Item\n{},a::x\n", "1".repeat(4301)).into_bytes()),
        args: vec![],
        input: true,
    });
    let limit = 131_072;
    for (name, n) in [
        ("a field at the limit", limit),
        ("a field over the limit", limit + 1),
    ] {
        v.push(Case {
            name: name.into(),
            expect: "rule",
            csv: Some(
                format!(
                    "Shallow Bytes,Item\n10,a::{}\n7,bar::b\n",
                    "x".repeat(n - 3)
                )
                .into_bytes(),
            ),
            args: vec!["--table".into(), "5".into()],
            input: true,
        });
    }
    v
}

#[test]
fn port_and_script_agree_on_every_csv() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let dir = ok(tempfile::tempdir());
    let cases = cases();
    let mut script_lines = String::new();
    let mut port_args = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let csv = dir.path().join(format!("{i}.csv"));
        if let Some(bytes) = &c.csv {
            ok(std::fs::write(&csv, bytes));
        }
        let csv = csv.display().to_string();
        let input = if c.input {
            vec!["--twiggy-csv".to_owned(), csv]
        } else {
            vec![]
        };
        let out = |side: &str| {
            vec![
                "--out".to_owned(),
                dir.path()
                    .join(format!("{side}{i}.svg"))
                    .display()
                    .to_string(),
            ]
        };
        let script: Vec<String> = [input.clone(), out("s"), c.args.clone()].concat();
        script_lines.push_str(&ok(serde_json::to_string(&script)));
        script_lines.push('\n');
        port_args.push([vec!["treemap".to_owned()], input, out("p"), c.args.clone()].concat());
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

    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    let mut failures = Vec::new();
    for (i, ((c, script), args)) in cases.iter().zip(&runs).zip(&port_args).enumerate() {
        let svg = |side: &str| std::fs::read(dir.path().join(format!("{side}{i}.svg"))).ok();
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
                port_exit == 0
                    && port_out == script.stdout
                    && port_err == script.stderr
                    && svg("s").is_some()
                    && svg("s") == svg("p")
            }
            "fails" => script.exit != 0 && port_exit != 0,
            "usage" => script.exit == 2 && port_exit == 2,
            "stricter" => script.exit == 0 && port_exit != 0,
            other => panic!("unknown expectation {other}"),
        };
        *counts.entry(expect).or_default() += 1;
        if !good {
            failures.push(format!(
                "{} (expected {expect}):\n  script: exit {} {:?} {:?}\n  port:   exit {port_exit} {port_out:?} {port_err:?}",
                c.name, script.exit, script.stdout, script.stderr
            ));
        }
    }
    eprintln!("{counts:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
