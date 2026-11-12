//! The treemap's arithmetic against scripts/wasm-size-treemap.py's own functions, imported by
//! python3 from the tree, on generated inputs: `squarify` (every rectangle bit for bit, on rows
//! with ties, zeros, negative sizes and square containers), `kib` (with the halves `round()`
//! sends to even), and `aggregate` then `render` on random crates of every family (the SVG byte
//! for byte, or both failing), with thresholds including 0, 1 and NaN and sizes beyond 2^53.
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use xtask::core::treemap::{Meta, aggregate, kib, render, squarify};

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

/// Answers each task on stdin (one JSON line) with one JSON line. Floats travel as `repr()`.
const HARNESS: &str = r#"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("treemap", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
for line in sys.stdin:
    t = json.loads(line)
    try:
        if t["kind"] == "squarify":
            x, y, w, h = (float(v) for v in t["rect"])
            r = m.squarify([tuple(s) for s in t["sizes"]], x, y, w, h)
            out = {k: [repr(v) for v in r[k]] for k in r}
        elif t["kind"] == "kib":
            out = [m.kib(n) for n in t["n"]]
        else:
            items = [("func", c + "::f", s) for c, s in t["crates"]]
            families, total, _ = m.aggregate(items, float(t["threshold"]))
            out = m.render(families, total, t["meta"])
    except Exception as e:
        out = {"crash": type(e).__name__}
    print(json.dumps(out), flush=True)
"#;

/// A deterministic generator (64-bit LCG, high bits).
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

fn python(tasks: &[Value]) -> Vec<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let dir = ok(tempfile::tempdir());
    let input = dir.path().join("tasks.jsonl");
    let lines: String = tasks.iter().map(|t| format!("{t}\n")).collect();
    ok(std::fs::write(&input, lines));
    let out = ok(Command::new("python3")
        .args(["-I", "-c", HARNESS])
        .arg(root.join("scripts/wasm-size-treemap.py"))
        .stdin(ok(std::fs::File::open(&input)))
        .output());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let results: Vec<Value> = ok(String::from_utf8(out.stdout))
        .lines()
        .map(|l| ok(serde_json::from_str(l)))
        .collect();
    assert_eq!(results.len(), tasks.len());
    results
}

fn repr(x: f64) -> String {
    // Python's repr and Rust's `{:?}` both print the shortest round-trip digits; only the
    // forms differ (1e+16 against 1e16, inf against inf), so compare parsed values.
    format!("{x:?}")
}

#[test]
fn squarify_gives_the_scripts_rectangles() {
    let mut rng = Rng(7);
    let mut tasks = Vec::new();
    let mut inputs = Vec::new();
    for case in 0..600 {
        let n = 1 + rng.next(12) as usize;
        let mut sizes: Vec<(String, i128)> = (0..n)
            .map(|i| {
                let v = match rng.next(6) {
                    0 => 0,
                    1 => -(rng.next(100) as i128),
                    2 => 1 + rng.next(3) as i128, // ties
                    _ => {
                        let bits = 1 + rng.next(40);
                        1 + rng.next(1 << bits) as i128
                    }
                };
                (format!("k{i}"), v)
            })
            .collect();
        sizes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let w = (1 + rng.next(2000)) as f64 / if case % 3 == 0 { 1.0 } else { 7.0 };
        let h = if case % 4 == 0 {
            w
        } else {
            (1 + rng.next(1000)) as f64 / 3.0
        };
        let rect = [rng.next(50) as f64 / 3.0, rng.next(80) as f64, w, h];
        tasks.push(json!({
            "kind": "squarify",
            "sizes": sizes,
            "rect": rect.iter().map(|v| repr(*v)).collect::<Vec<_>>(),
        }));
        inputs.push((sizes, rect));
    }
    let mut failures = 0;
    for ((sizes, r), want) in inputs.iter().zip(python(&tasks)) {
        let keyed: Vec<(&str, i128)> = sizes.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let got = squarify(&keyed, r[0], r[1], r[2], r[3]);
        let same = match (&got, want.get("crash")) {
            (Err(_), Some(_)) => true,
            (Ok(rects), None) => {
                let want = want.as_object().cloned().unwrap_or_default();
                rects.len() == want.len()
                    && rects.iter().all(|(k, v)| {
                        let w: Vec<f64> = want[k]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|s| ok(s.as_str().unwrap_or("x").parse::<f64>()))
                            .collect();
                        w.len() == 4
                            && [v.0, v.1, v.2, v.3]
                                .iter()
                                .zip(&w)
                                .all(|(a, b)| a.to_bits() == b.to_bits())
                    })
            }
            _ => false,
        };
        if !same {
            failures += 1;
            eprintln!("{sizes:?} in {r:?}: port {got:?}, script {want}");
        }
    }
    assert_eq!(failures, 0, "of {}", tasks.len());
}

#[test]
fn kib_rounds_as_the_script() {
    let n: Vec<i128> = (0..5000)
        .map(|i| i * 512)
        .chain([
            1_048_575, 1_048_576, 1_053_819, 1_053_820, -512, -1536, -1_048_576,
        ])
        .collect();
    let want = python(&[json!({"kind": "kib", "n": n})]).remove(0);
    let got: Vec<Value> = n.iter().map(|&n| json!(kib(n))).collect();
    assert_eq!(Value::Array(got), want);
}

#[test]
fn the_svg_is_the_scripts_for_random_crates() {
    const POOL: &[&str] = &[
        "bevy_ecs",
        "bevy_render",
        "bevy_egui",
        "encase",
        "egui",
        "epaint",
        "ttf_parser",
        "wgpu",
        "wgpu_core",
        "naga",
        "glow",
        "khronos_egl",
        "core",
        "alloc",
        "std",
        "compiler_builtins",
        "observation_client",
        "observation_core",
        "serde",
        "smallvec",
        "hashbrown",
        "a_crate_with_a_name_much_too_long_for_any_tile",
        "x",
        "data segments",
        "module structure",
        "other / unnamed",
    ];
    let mut rng = Rng(11);
    let mut tasks = Vec::new();
    let mut inputs = Vec::new();
    for case in 0..400 {
        let n = 1 + rng.next(POOL.len() as u64) as usize;
        let mut crates: HashMap<String, i128> = HashMap::new();
        for _ in 0..n {
            let c = POOL[rng.next(POOL.len() as u64) as usize];
            let v = match rng.next(10) {
                0 => (1i128 << 53) + 1,
                1 => 1 + rng.next(10) as i128,
                _ => {
                    let bits = 4 + rng.next(24);
                    1 + rng.next(1 << bits) as i128
                }
            };
            crates.insert(c.to_owned(), v);
        }
        let threshold = match case % 7 {
            0 => 0.0,
            1 => 1.0,
            2 => f64::NAN,
            3 => 0.3,
            _ => 0.005,
        };
        let meta = Meta {
            raw: crates.values().sum::<i128>() + rng.next(1000) as i128,
            gzip: rng.next(1 << 25) as i128,
            commit: "abc & <d>".into(),
            date: "2026-11-12".into(),
            stage: "after \"wasm-opt\"".into(),
        };
        let mut list: Vec<_> = crates.iter().map(|(c, s)| json!([c, s])).collect();
        list.sort_by_key(|v| v.to_string());
        tasks.push(json!({
            "kind": "render",
            "crates": list,
            "threshold": repr(threshold).replace("NaN", "nan"),
            "meta": {"raw": meta.raw, "gzip": meta.gzip, "commit": meta.commit,
                     "date": meta.date, "stage": meta.stage},
        }));
        inputs.push((crates, threshold, meta));
    }
    // Sizes where `int >= float` is not `float(int) >= float`: 2^53 + 3 rounds up to the limit.
    // And small families beside a large one, some of whose tiles are a few pixels higher than
    // the family's header (the label needs 4 more).
    let mut extra: Vec<HashMap<String, i128>> = vec![HashMap::from([
        ("serde".to_owned(), (1i128 << 53) + 3),
        ("smallvec".to_owned(), 1),
    ])];
    extra.extend((1..=300).map(|k| {
        HashMap::from([
            ("bevy_ecs".to_owned(), 1_000_000),
            ("serde".to_owned(), rng.next(300_000) as i128 + 1),
            ("core".to_owned(), k * 100),
        ])
    }));
    for crates in extra {
        let meta = Meta {
            raw: 1,
            gzip: 1,
            commit: "c".into(),
            date: "d".into(),
            stage: "s".into(),
        };
        let list: Vec<_> = crates.iter().map(|(c, s)| json!([c, s])).collect();
        tasks.push(json!({
            "kind": "render",
            "crates": list,
            "threshold": "1.0",
            "meta": {"raw": 1, "gzip": 1, "commit": "c", "date": "d", "stage": "s"},
        }));
        inputs.push((crates, 1.0, meta));
    }
    let mut failures = 0;
    let mut crashes = 0;
    for ((crates, threshold, meta), want) in inputs.iter().zip(python(&tasks)) {
        // The harness names each crate `CRATE::f`, which the script credits to CRATE (and the
        // three non-code names to "other / unnamed", as a function).
        let mut per_crate: HashMap<String, i128> = HashMap::new();
        for (c, s) in crates {
            let c = if c.contains(' ') {
                "other / unnamed"
            } else {
                c.as_str()
            };
            *per_crate.entry(c.to_owned()).or_default() += s;
        }
        let total = per_crate.values().sum();
        let got = render(&aggregate(&per_crate, total, *threshold), total, meta);
        let same = match (&got, want.as_str()) {
            (Ok(svg), Some(w)) => svg == w,
            (Err(_), None) => {
                crashes += 1;
                want.get("crash").is_some()
            }
            _ => false,
        };
        if !same {
            failures += 1;
            eprintln!("{crates:?} at {threshold}: port {got:?}, script {want}");
        }
    }
    println!("{} renders, {crashes} failing in both", tasks.len());
    assert_eq!(failures, 0, "of {}", tasks.len());
}
