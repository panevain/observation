//! The crate of each symbol, as `xtask::core::demangle::crate_of_symbol` finds it, against
//! `crate_of_symbol` of scripts/wasm-size-treemap.py, imported by python3 from the tree (the
//! treemap job runs that script until the port replaces it).
//! `python3` is whichever the machine has on its PATH: on CI, the runner's system Python.
//!
//! `tests/fixtures/treemap/symbols.jsonl` holds `[EXPECT, SYMBOL]` lines: the function names of
//! four small modules (built with v0 and legacy mangling, raw and as wasm-bindgen demangles
//! them) and hand-written edge cases. Each `same` or `recursion` symbol of the file that is
//! mangled (`_R`, `_ZN`) and at most 300 bytes long is also tried cut at each length. EXPECT says what the script does, and so what the port must do:
//! - `same`: the script gives a crate (or none), and the port gives the same;
//! - `recursion`: the script's v0 parser recurses until Python's recursion limit and the script
//!   crashes (a back-reference that leads back to itself); the port gives what the script's
//!   fallback for an undecodable symbol gives;
//! - `deep`: nested more than `demangle::MAX_NESTING` steps; the port gives the script's fallback
//!   (a readable path: it fails), and the script either decodes it or crashes on Python's
//!   recursion limit, whichever it reaches first on this interpreter, or runs for more than half
//!   a second (the longest back-reference chains take the script up to a third of that before
//!   they crash, so a slow runner may stop them first);
//! - `budget`: more than `demangle::V0_STEPS` steps; the port gives the script's fallback, and the
//!   script decodes it, runs for more than half a second, or crashes on the recursion limit;
//! - `hang`: the script loops forever (stopped after half a second); the port fails;
//! - `crash`: the script raises anything else; the port fails;
//! - `crash-3.12`: as `crash` on Python 3.12 (the runners'), which cannot pass a number above
//!   0x7FFFFFFF to `chr`; Python 3.13 gives a crate, and the port still fails;
//! - `stricter`: the script gives a crate (or none), and the port fails: the differences
//!   `xtask::core::demangle` declares.
//!
//! The cut symbols have no EXPECT: each must be `same`, `recursion`, `hang` or `crash`.
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use xtask::core::demangle::crate_of_symbol;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

/// Imports the script, then prints for each JSON string on stdin what its `crate_of_symbol`
/// did: `["ok", CRATE]`, `["recursion", FALLBACK]` (or `["crash", …]` if the fallback raises),
/// `["hang", null]` or `["crash", EXCEPTION]`, followed for a `_R` symbol by its fallback (if that
/// does not raise). The first line is Python's version.
const HARNESS: &str = r#"
import importlib.util, json, signal, sys
spec = importlib.util.spec_from_file_location("treemap", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
class Hang(Exception):
    pass
def alarm(*_):
    raise Hang()
signal.signal(signal.SIGALRM, alarm)
def undecodable(self):
    raise ValueError
def fallback(sym):
    path, m._V0.path = m._V0.path, undecodable
    try:
        return m.v0_crate(sym)
    finally:
        m._V0.path = path
print(json.dumps(list(sys.version_info[:2])))
for line in sys.stdin:
    sym = json.loads(line)
    signal.setitimer(signal.ITIMER_REAL, 0.5)
    try:
        try:
            out = ["ok", m.crate_of_symbol(sym)]
        except RecursionError:
            if not sym.startswith("_R"):
                raise
            out = ["recursion", fallback(sym)]
    except Hang:
        out = ["hang", None]
    except Exception as e:
        out = ["crash", type(e).__name__]
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    if sym.startswith("_R"):
        try:
            out.append(fallback(sym))
        except Exception:
            pass
    print(json.dumps(out), flush=True)
"#;

fn script_results(symbols: &[String]) -> (Vec<u64>, Vec<Vec<serde_json::Value>>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut child = ok(Command::new("python3")
        .args(["-I", "-c", HARNESS])
        .arg(root.join("scripts/wasm-size-treemap.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn());
    let mut stdin = child.stdin.take().expect("a pipe");
    let input: String = symbols
        .iter()
        .map(|s| format!("{}\n", ok(serde_json::to_string(s))))
        .collect();
    let writer = std::thread::spawn(move || ok(stdin.write_all(input.as_bytes())));
    let out = ok(child.wait_with_output());
    ok(writer.join().map_err(|_| "writing to python3 failed"));
    assert!(out.status.success(), "the harness failed: {:?}", out.status);
    let text = ok(String::from_utf8(out.stdout));
    let mut lines = text.lines();
    let version: Vec<u64> = ok(serde_json::from_str(lines.next().expect("the version")));
    let results: Vec<Vec<serde_json::Value>> = lines.map(|l| ok(serde_json::from_str(l))).collect();
    assert_eq!(results.len(), symbols.len(), "one result per symbol");
    (version, results)
}

#[test]
fn every_symbol_gets_the_scripts_crate() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/treemap/symbols.jsonl");
    let mut cases: Vec<(String, String)> = ok(std::fs::read_to_string(fixture))
        .lines()
        .map(|l| ok(serde_json::from_str::<(String, String)>(l)))
        .collect();
    let mut seen: std::collections::BTreeSet<String> = cases.iter().map(|c| c.1.clone()).collect();
    let mut cut = Vec::new();
    for (expect, sym) in &cases {
        let mangled = sym.starts_with("_R") || sym.starts_with("_ZN");
        if mangled
            && sym.len() <= 300
            && sym.is_ascii()
            && ["same", "recursion"].contains(&expect.as_str())
        {
            for n in 2..sym.len() {
                if seen.insert(sym[..n].to_owned()) {
                    cut.push(("cut".to_owned(), sym[..n].to_owned()));
                }
            }
        }
    }
    cases.extend(cut);
    let symbols: Vec<String> = cases.iter().map(|c| c.1.clone()).collect();
    let (version, results) = script_results(&symbols);
    let before_3_13 = version < vec![3, 13];

    let mut counts = BTreeMap::<String, usize>::new();
    let mut failures = Vec::new();
    for ((expect, sym), script) in cases.iter().zip(&results) {
        let kind = script[0].as_str().unwrap_or_default();
        let value = script.get(1).and_then(|v| v.as_str()).map(str::to_owned);
        let port = crate_of_symbol(sym);
        let fallback = script.get(2).map(|v| v.as_str().map(str::to_owned));
        // What the port gives where it gives up: the fallback, or for a readable path a failure.
        let gave_up = if sym.starts_with("_R") {
            fallback.is_some() && port.as_ref().ok() == fallback.as_ref()
        } else {
            port.is_err()
        };
        let recursion_error = kind == "recursion"
            || (kind == "crash"
                && script.get(1).and_then(|v| v.as_str()) == Some("RecursionError"));
        let script_ok = kind == "ok";
        let verdict = match expect.as_str() {
            "cut" => match kind {
                "ok" => "same",
                "recursion" => "recursion",
                "hang" => "hang",
                _ => "crash",
            },
            "crash-3.12" if before_3_13 => "crash",
            "crash-3.12" => "stricter",
            other => other,
        };
        let good = match verdict {
            "same" => script_ok && port.as_ref().ok() == Some(&value),
            "recursion" => kind == "recursion" && port.as_ref().ok() == Some(&value),
            "hang" => kind == "hang" && port.is_err(),
            "crash" => kind == "crash" && port.is_err(),
            "stricter" => script_ok && port.is_err(),
            "deep" | "budget" => gave_up && (script_ok || recursion_error || kind == "hang"),
            _ => false,
        };
        if !good {
            failures.push(format!(
                "{expect} {sym:?}: script {script:?}, port {port:?}"
            ));
        }
        // For `deep` and `budget`, which of the script's outcomes this interpreter gave.
        let outcome = if ["deep", "budget"].contains(&verdict) {
            kind
        } else {
            verdict
        };
        *counts.entry(format!("{expect}/{outcome}")).or_default() += 1;
    }
    println!("{} symbols: {counts:?}", cases.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
