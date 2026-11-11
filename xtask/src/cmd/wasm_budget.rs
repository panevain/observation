//! `cargo xtask wasm-budget`: every WebAssembly module in dist/ within its size budget
//! (`.config/wasm-size-budget.toml`: raw and gzip -9 limits per module, keyed by the module's
//! name without trunk's content hash, so observation-client-<hash>_bg.wasm is
//! `observation-client`). A module with no budget fails too, so a new one gets a limit when it
//! is added. `--brotli` adds each module's `brotli -q 11` size to the table (it has no budget;
//! about a minute for the client, and it needs the brotli CLI). Ported from
//! scripts/check-wasm-size.py, whose table, messages and exit status it keeps.
//!
//! gzip is measured in process (flate2 at level 9, gzip framing as Python's `gzip.compress`);
//! its compressed sizes can differ slightly from zlib's. Each module's exact sizes and its
//! headroom against the limits also go to stderr.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::sync::LazyLock;

use flate2::Compression;
use flate2::write::GzEncoder;
use regex::Regex;
use serde::Deserialize;

use crate::core::report::thousands;
use crate::ports::Brotli;
use crate::{Error, Result};

pub const CONFIG: &str = ".config/wasm-size-budget.toml";

/// A module's limits, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Budget {
    pub raw: u64,
    pub gzip: u64,
}

/// One module's measurements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// The file name in dist/.
    pub file: String,
    pub raw: u64,
    pub gzip: u64,
}

/// trunk's filehash: <name>-<16 hex digits or fewer>_bg.wasm; worker output has no hash.
static NAME: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern, compiled by the tests: the panic cannot happen.
    Regex::new(r"^(?P<name>.+?)(-[0-9a-f]{8,16})?_bg\.wasm$").unwrap_or_else(|e| panic!("{e}"))
});

/// The budget key of a module file: its name without trunk's hash, or the file's stem.
pub fn module_name(file: &str) -> String {
    match NAME.captures(file) {
        Some(c) => c["name"].to_owned(),
        // pathlib's stem: the name without its last suffix (a leading dot is not a suffix).
        None => match file.rfind('.') {
            Some(i) if i > 0 => file[..i].to_owned(),
            _ => file.to_owned(),
        },
    }
}

fn mib(n: u64) -> String {
    format!("{:.2} MiB", n as f64 / 1_048_576.0)
}

/// The verdict on one module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub raw: u64,
    pub gzip: u64,
    pub budget: Option<Budget>,
    pub verdict: &'static str,
}

/// Every module's verdict, and the failures in the order the script printed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub rows: Vec<Row>,
    pub failures: Vec<String>,
}

/// Checks each module (in the order given) against its budget. A size equal to its limit
/// passes; one byte more fails.
pub fn check(modules: &[Module], budgets: &BTreeMap<String, Budget>) -> Checked {
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for m in modules {
        let name = module_name(&m.file);
        let Some(&budget) = budgets.get(&name) else {
            failures.push(format!("{}: no budget for `{name}` in {CONFIG}", m.file));
            rows.push(Row {
                name,
                raw: m.raw,
                gzip: m.gzip,
                budget: None,
                verdict: "no budget",
            });
            continue;
        };
        let over: Vec<(&str, u64, u64)> =
            [("raw", m.raw, budget.raw), ("gzip", m.gzip, budget.gzip)]
                .into_iter()
                .filter(|&(_, size, limit)| size > limit)
                .collect();
        for &(k, size, limit) in &over {
            failures.push(format!(
                "{name}: {k} {} B ({}) is over its budget of {} B ({})",
                thousands(size),
                mib(size),
                thousands(limit),
                mib(limit)
            ));
        }
        let verdict = if over.is_empty() { "ok" } else { "OVER" };
        rows.push(Row {
            name,
            raw: m.raw,
            gzip: m.gzip,
            budget: Some(budget),
            verdict,
        });
    }
    Checked { rows, failures }
}

/// Each module's exact sizes and what is left under each limit (negative: over).
pub fn headroom(checked: &Checked) -> Vec<String> {
    let left = |size: u64, limit: u64| {
        if size <= limit {
            format!("{} B left", thousands(limit - size))
        } else {
            format!("{} B over", thousands(size - limit))
        }
    };
    checked
        .rows
        .iter()
        .map(|r| match r.budget {
            Some(b) => format!(
                "headroom: {}: raw {} B of {} B ({}); gzip {} B of {} B ({})",
                r.name,
                thousands(r.raw),
                thousands(b.raw),
                left(r.raw, b.raw),
                thousands(r.gzip),
                thousands(b.gzip),
                left(r.gzip, b.gzip)
            ),
            None => format!(
                "headroom: {}: no budget (raw {} B; gzip {} B)",
                r.name,
                thousands(r.raw),
                thousands(r.gzip)
            ),
        })
        .collect()
}

/// The Markdown report, as the script wrote it (its verdict column has an empty header).
/// `brotli` holds each row's brotli size when `--brotli` was given.
pub fn render(checked: &Checked, brotli: Option<&[u64]>) -> String {
    let mut head = "| module | raw | budget | gzip -9 | budget |".to_owned();
    let mut rule = "| --- | ---: | ---: | ---: | ---: |".to_owned();
    if brotli.is_some() {
        head.push_str(" brotli -q 11 |");
        rule.push_str(" ---: |");
    }
    let mut lines = vec![
        "### Wasm size budget".to_owned(),
        String::new(),
        format!("{head} |"),
        format!("{rule} --- |"),
    ];
    for (i, r) in checked.rows.iter().enumerate() {
        let limit = |f: fn(&Budget) -> u64| r.budget.as_ref().map_or("-".to_owned(), |b| mib(f(b)));
        let mut row = format!(
            "| `{}` | {} | {} | {} | {} |",
            r.name,
            mib(r.raw),
            limit(|b| b.raw),
            mib(r.gzip),
            limit(|b| b.gzip)
        );
        if let Some(sizes) = brotli {
            row.push_str(&format!(
                " {} |",
                mib(sizes.get(i).copied().unwrap_or_default())
            ));
        }
        lines.push(format!("{row} {} |", r.verdict));
    }
    if !checked.failures.is_empty() {
        lines.push(String::new());
        lines.extend(checked.failures.iter().map(|f| format!("- {f}")));
        lines.push(String::new());
        lines.push(format!(
            "Shrink the module, or raise its limit in `{CONFIG}` with the reason in the pull request."
        ));
    }
    lines.join("\n") + "\n"
}

/// The size of `data` compressed as Python's `gzip.compress(data, compresslevel=9)` frames it:
/// a 10-byte header, the deflate stream, an 8-byte trailer.
pub fn gzip_len(data: &[u8]) -> Result<u64> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::new(9));
    gz.write_all(data)
        .map_err(|e| Error::new(format!("gzip: {e}")))?;
    let out = gz.finish().map_err(|e| Error::new(format!("gzip: {e}")))?;
    Ok(out.len() as u64)
}

/// What the command prints: stderr first (failures, then headroom), then stdout; the summary
/// file gets the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub stderr: String,
    pub stdout: String,
    pub report: String,
    pub failed: bool,
}

/// Runs the check on the modules in `dist` against the repository's budget.
pub fn run(repo: &Path, dist: &Path, brotli: Option<&dyn Brotli>) -> Result<Outcome> {
    if brotli.is_some_and(|b| !b.available()) {
        return Err(Error::new("--brotli needs the brotli CLI on PATH"));
    }
    let text = fs::read_to_string(repo.join(CONFIG))
        .map_err(|e| Error::new(format!("{}: {e}", repo.join(CONFIG).display())))?;
    let budgets: BTreeMap<String, Budget> =
        toml::from_str(&text).map_err(|e| Error::new(format!("{CONFIG}: {e}")))?;
    let mut files = match fs::read_dir(dist) {
        Ok(entries) => entries
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| Error::new(format!("{}: {e}", dist.display())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Error::new(format!("{}: {e}", dist.display()))),
    };
    files.retain(|f| f.ends_with(".wasm"));
    files.sort();
    if files.is_empty() {
        return Err(Error::new(format!(
            "no .wasm files in {}; build first (mise run build)",
            dist.display()
        )));
    }
    let mut modules = Vec::new();
    let mut contents = Vec::new();
    for file in files {
        let data = fs::read(dist.join(&file)).map_err(|e| Error::new(format!("{file}: {e}")))?;
        modules.push(Module {
            raw: data.len() as u64,
            gzip: gzip_len(&data)?,
            file,
        });
        contents.push(data);
    }
    let checked = check(&modules, &budgets);
    let mut stderr = String::new();
    if !checked.failures.is_empty() {
        stderr.push_str(&checked.failures.join("\n"));
        stderr.push('\n');
    }
    for line in headroom(&checked) {
        stderr.push_str(&line);
        stderr.push('\n');
    }
    let brotli_sizes = match brotli {
        Some(b) => Some(
            contents
                .iter()
                .map(|d| b.compressed_len(d).map(|n| n as u64))
                .collect::<Result<Vec<_>>>()?,
        ),
        None => None,
    };
    let report = render(&checked, brotli_sizes.as_deref());
    Ok(Outcome {
        stderr,
        stdout: format!("{report}\n"),
        report,
        failed: !checked.failures.is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    /// One case of the golden: its sections by name.
    fn sections(case: &str) -> BTreeMap<&str, String> {
        let mut out = BTreeMap::new();
        let mut current = None;
        for line in case.split_inclusive('\n') {
            if let Some(name) = line.strip_prefix("-- ") {
                current = Some(name.trim_end());
                out.insert(name.trim_end(), String::new());
            } else if let Some(name) = current {
                out.entry(name).or_insert_with(String::new).push_str(line);
            }
        }
        out
    }

    /// Every case of wasm-budget.golden (what scripts/check-wasm-size.py printed for the sizes
    /// it measured; see the pull request that ported it), rendered from the same sizes.
    #[test]
    fn report_matches_the_script() {
        let golden =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wasm-budget.golden");
        let golden = ok(fs::read_to_string(golden));
        for case in golden.split("== ").filter(|c| !c.is_empty()) {
            let (name, body) = case.split_once('\n').unwrap_or((case, ""));
            let s = sections(body);
            let mut modules = Vec::new();
            let mut brotli = Vec::new();
            for line in s["modules"].lines() {
                let f: Vec<&str> = line.split(' ').collect();
                let n = |i: usize| ok(f[i].parse::<u64>());
                modules.push(Module {
                    file: f[0].to_owned(),
                    raw: n(1),
                    gzip: n(2),
                });
                if f.len() > 3 {
                    brotli.push(n(3));
                }
            }
            // The command checks dist/ in file-name order.
            let mut order: Vec<usize> = (0..modules.len()).collect();
            order.sort_by(|&a, &b| modules[a].file.cmp(&modules[b].file));
            let modules: Vec<Module> = order.iter().map(|&i| modules[i].clone()).collect();
            let brotli: Vec<u64> = order
                .iter()
                .filter_map(|&i| brotli.get(i).copied())
                .collect();
            let budgets: BTreeMap<String, Budget> = ok(toml::from_str(&s["budget"]));
            let checked = check(&modules, &budgets);
            let with_brotli = s["args"].trim() == "--brotli";
            let report = render(&checked, with_brotli.then_some(brotli.as_slice()));
            let stderr: String = checked.failures.iter().map(|f| format!("{f}\n")).collect();
            let exit = u8::from(!checked.failures.is_empty()).to_string();
            assert_eq!(s["exit"].trim(), exit, "{name}: exit");
            assert_eq!(s["stdout"], format!("{report}\n"), "{name}: stdout");
            assert_eq!(s["summary"], report, "{name}: summary");
            assert_eq!(s["stderr"], stderr, "{name}: stderr");
        }
    }

    #[test]
    fn limits_are_inclusive() {
        let budgets = BTreeMap::from([("m".to_owned(), Budget { raw: 100, gzip: 50 })]);
        let verdict = |raw, gzip| {
            let m = Module {
                file: "m_bg.wasm".into(),
                raw,
                gzip,
            };
            check(&[m], &budgets).rows[0].verdict
        };
        // raw under / at / over, against gzip under / at / over.
        let table = [
            ((99, 49), "ok"),
            ((99, 50), "ok"),
            ((99, 51), "OVER"),
            ((100, 49), "ok"),
            ((100, 50), "ok"),
            ((100, 51), "OVER"),
            ((101, 49), "OVER"),
            ((101, 50), "OVER"),
            ((101, 51), "OVER"),
        ];
        for ((raw, gzip), want) in table {
            assert_eq!(verdict(raw, gzip), want, "raw {raw}, gzip {gzip}");
        }
        let unknown = Module {
            file: "x_bg.wasm".into(),
            raw: 1,
            gzip: 1,
        };
        assert_eq!(check(&[unknown], &budgets).rows[0].verdict, "no budget");
    }

    #[test]
    fn headroom_shows_exact_bytes() {
        let budgets = BTreeMap::from([(
            "m".to_owned(),
            Budget {
                raw: 2_000,
                gzip: 1_000,
            },
        )]);
        let modules = [
            Module {
                file: "m-0123abcd_bg.wasm".into(),
                raw: 1_500,
                gzip: 1_001,
            },
            Module {
                file: "new_bg.wasm".into(),
                raw: 7,
                gzip: 27,
            },
        ];
        assert_eq!(
            headroom(&check(&modules, &budgets)),
            [
                "headroom: m: raw 1,500 B of 2,000 B (500 B left); gzip 1,001 B of 1,000 B (1 B over)",
                "headroom: new: no budget (raw 7 B; gzip 27 B)",
            ]
        );
    }

    /// gzip framing as Python's gzip.compress: 20 bytes for nothing (a 10-byte header, the
    /// 2-byte empty deflate stream, an 8-byte trailer). The deflate stream itself comes from
    /// miniz_oxide, not zlib, so longer inputs can compress to a few bytes more or less.
    #[test]
    fn gzip_framing_matches_python() {
        assert_eq!(ok(gzip_len(b"")), 20);
    }

    #[test]
    fn module_names() {
        let cases = [
            (
                "observation-client-0123456789abcdef_bg.wasm",
                "observation-client",
            ),
            ("coverage-worker_bg.wasm", "coverage-worker"),
            ("a-1234567_bg.wasm", "a-1234567"),
            ("plain.wasm", "plain"),
            (".hidden.wasm", ".hidden"),
            (".wasm", ".wasm"),
        ];
        for (file, name) in cases {
            assert_eq!(module_name(file), name, "{file}");
        }
    }
}
