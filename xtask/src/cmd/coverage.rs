//! `cargo xtask coverage check`: each crate's line coverage against its floor in
//! `.config/coverage.toml`, from the JSON export of `cargo llvm-cov --json --summary-only`
//! (llvm-cov's `export` format: `data[].files[]` with each file's `summary.lines`).
//!
//! A file belongs to the crate whose directory holds it (`crates/<name>/...` under the
//! repository). A crate below its floor fails. A crate with no floor, and a floor for a crate
//! with no measured lines, only warn: crates come and go in other changes. The report ends
//! with a floors table to paste, from the measured values.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::core::gha::{self, Level};
use crate::core::report::{Align, Table, thousands};
use crate::{Error, Result};

pub const CONFIG: &str = ".config/coverage.toml";

/// Points taken off each measured percentage in the suggested floors, for run-to-run noise.
pub const MARGIN: u64 = 1;

/// Covered and total lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Lines {
    pub covered: u64,
    pub total: u64,
}

impl Lines {
    /// Whether the coverage is at least `floor` percent (equal passes). No lines pass.
    pub fn meets(self, floor: f64) -> bool {
        self.covered as f64 * 100.0 >= floor * self.total as f64
    }

    fn percent(self) -> Option<f64> {
        (self.total > 0).then(|| self.covered as f64 * 100.0 / self.total as f64)
    }
}

#[derive(Debug, Deserialize)]
struct Export {
    data: Vec<ExportData>,
}

#[derive(Debug, Deserialize)]
struct ExportData {
    files: Vec<ExportFile>,
}

#[derive(Debug, Deserialize)]
struct ExportFile {
    filename: String,
    summary: FileSummary,
}

#[derive(Debug, Deserialize)]
struct FileSummary {
    lines: LineCount,
}

#[derive(Debug, Deserialize)]
struct LineCount {
    count: u64,
    covered: u64,
}

/// The crate a file belongs to: `crates/<name>/...` below `root`.
fn crate_of<'a>(filename: &'a str, root: &str) -> Option<&'a str> {
    let rel = filename.strip_prefix(root)?.strip_prefix('/')?;
    let rest = rel.strip_prefix("crates/")?;
    let (name, _) = rest.split_once('/')?;
    Some(name)
}

/// Each crate's lines, summed over its files, from an llvm-cov JSON export whose file names
/// are absolute paths under `root`. Files elsewhere are left out.
pub fn per_crate(json: &str, root: &str) -> Result<BTreeMap<String, Lines>> {
    let export: Export = serde_json::from_str(json)
        .map_err(|e| Error::new(format!("coverage: not an llvm-cov JSON export: {e}")))?;
    let mut out: BTreeMap<String, Lines> = BTreeMap::new();
    for file in export.data.iter().flat_map(|d| &d.files) {
        if let Some(name) = crate_of(&file.filename, root.trim_end_matches('/')) {
            let lines = out.entry(name.to_owned()).or_default();
            lines.covered += file.summary.lines.covered;
            lines.total += file.summary.lines.count;
        }
    }
    Ok(out)
}

/// The verdict on one crate (or on one floor).
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Pass {
        name: String,
        lines: Lines,
        floor: f64,
    },
    Below {
        name: String,
        lines: Lines,
        floor: f64,
    },
    NoFloor {
        name: String,
        lines: Lines,
    },
    StaleFloor {
        name: String,
        floor: f64,
    },
}

impl Verdict {
    pub fn fails(&self) -> bool {
        matches!(self, Self::Below { .. })
    }
}

/// Every measured crate against its floor, then every floor with no measured crate.
pub fn check(per_crate: &BTreeMap<String, Lines>, floors: &BTreeMap<String, f64>) -> Vec<Verdict> {
    let mut out: Vec<Verdict> = per_crate
        .iter()
        .map(|(name, &lines)| {
            let name = name.clone();
            match floors.get(&name) {
                Some(&floor) if lines.meets(floor) => Verdict::Pass { name, lines, floor },
                Some(&floor) => Verdict::Below { name, lines, floor },
                None => Verdict::NoFloor { name, lines },
            }
        })
        .collect();
    out.extend(
        floors
            .iter()
            .filter(|(name, _)| !per_crate.contains_key(*name))
            .map(|(name, &floor)| Verdict::StaleFloor {
                name: name.clone(),
                floor,
            }),
    );
    out
}

fn pct(lines: Lines) -> String {
    lines
        .percent()
        .map_or("-".to_owned(), |p| format!("{p:.2}%"))
}

fn floor_text(floor: f64) -> String {
    format!("{floor}%")
}

/// The warnings and failures, one line each.
pub fn messages(verdicts: &[Verdict]) -> Vec<(Level, String)> {
    verdicts
        .iter()
        .filter_map(|v| match v {
            Verdict::Below { name, lines, floor } => Some((
                Level::Error,
                format!(
                    "{name}: line coverage {} is below its floor of {}",
                    pct(*lines),
                    floor_text(*floor)
                ),
            )),
            Verdict::NoFloor { name, lines } => Some((
                Level::Warning,
                format!(
                    "{name}: line coverage {} has no floor in {CONFIG}",
                    pct(*lines)
                ),
            )),
            Verdict::StaleFloor { name, floor } => Some((
                Level::Warning,
                format!(
                    "{CONFIG}: the floor of {} for {name} matches no measured crate",
                    floor_text(*floor)
                ),
            )),
            Verdict::Pass { .. } => None,
        })
        .collect()
}

/// The floor to suggest for a measured coverage: rounded down to a whole percent, minus
/// [`MARGIN`], never below zero.
pub fn suggested_floor(lines: Lines) -> u64 {
    if lines.total == 0 {
        return 0;
    }
    (lines.covered * 100 / lines.total).saturating_sub(MARGIN)
}

/// The Markdown report: a table of every crate, the messages, and floors to paste.
pub fn render(per_crate: &BTreeMap<String, Lines>, verdicts: &[Verdict]) -> String {
    let mut t = Table::new(&[
        ("crate", Align::Left),
        ("covered", Align::Right),
        ("lines", Align::Right),
        ("coverage", Align::Right),
        ("floor", Align::Right),
        ("result", Align::Left),
    ]);
    for v in verdicts {
        let (name, lines, floor, result) = match v {
            Verdict::Pass { name, lines, floor } => (name, Some(*lines), Some(*floor), "ok"),
            Verdict::Below { name, lines, floor } => (name, Some(*lines), Some(*floor), "BELOW"),
            Verdict::NoFloor { name, lines } => (name, Some(*lines), None, "no floor"),
            Verdict::StaleFloor { name, floor } => (name, None, Some(*floor), "not measured"),
        };
        t.row([
            format!("`{name}`"),
            lines.map_or("-".to_owned(), |l| thousands(l.covered)),
            lines.map_or("-".to_owned(), |l| thousands(l.total)),
            lines.map_or("-".to_owned(), pct),
            floor.map_or("-".to_owned(), floor_text),
            result.to_owned(),
        ]);
    }
    let mut out = vec!["### Line coverage per crate".to_owned(), String::new()];
    out.extend(t.lines());
    let messages = messages(verdicts);
    if !messages.is_empty() {
        out.push(String::new());
        out.extend(messages.iter().map(|(_, m)| format!("- {m}")));
    }
    out.push(String::new());
    out.push(format!(
        "Floors from these measurements (each rounded down to a whole percent, minus {MARGIN} for \
         noise), for `{CONFIG}`:"
    ));
    out.push(String::new());
    out.push("```toml".to_owned());
    out.push("[floors]".to_owned());
    out.extend(
        per_crate
            .iter()
            .map(|(name, &l)| format!("{name} = {}", suggested_floor(l))),
    );
    out.push("```".to_owned());
    out.join("\n") + "\n"
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    floors: BTreeMap<String, f64>,
}

/// What the command prints: the report on stdout, the messages on stderr (as workflow
/// annotations under GitHub Actions); the summary file gets the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub failed: bool,
}

/// Checks the export at `json` (file names under `repo`) against the repository's floors.
pub fn run(repo: &Path, json: &Path, annotate: bool) -> Result<Outcome> {
    let read =
        |p: &Path| fs::read_to_string(p).map_err(|e| Error::new(format!("{}: {e}", p.display())));
    let config: Config = toml::from_str(&read(&repo.join(CONFIG))?)
        .map_err(|e| Error::new(format!("{CONFIG}: {e}")))?;
    let root = fs::canonicalize(repo).unwrap_or_else(|_| repo.to_owned());
    let measured = per_crate(&read(json)?, &root.to_string_lossy())?;
    if measured.is_empty() {
        return Err(Error::new(format!(
            "coverage: no file of {} is under {}/crates/",
            json.display(),
            root.display()
        )));
    }
    let verdicts = check(&measured, &config.floors);
    let stderr: String = messages(&verdicts)
        .into_iter()
        .map(|(level, m)| {
            if annotate {
                format!("{}\n", gha::annotation(level, &m))
            } else {
                let tag = if level == Level::Error {
                    "error"
                } else {
                    "warning"
                };
                format!("{tag}: {m}\n")
            }
        })
        .collect();
    Ok(Outcome {
        stdout: render(&measured, &verdicts),
        stderr,
        failed: verdicts.iter().any(Verdict::fails),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    fn lines(covered: u64, total: u64) -> Lines {
        Lines { covered, total }
    }

    /// A recorded export in llvm-cov's format (trimmed to the fields read), with file names
    /// under /repo.
    #[test]
    fn sums_files_per_crate() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/coverage/llvm-cov-summary.json"
        );
        let measured = ok(per_crate(&ok(fs::read_to_string(path)), "/repo"));
        assert_eq!(
            measured,
            BTreeMap::from([
                ("observation-core".to_owned(), lines(450, 500)),
                ("observation-server".to_owned(), lines(60, 100)),
                ("observation-server-extra".to_owned(), lines(0, 0)),
            ])
        );
        assert!(per_crate("{}", "/repo").is_err());
    }

    #[test]
    fn floors_are_inclusive() {
        // 3 of 4 lines is 75%: a floor of 75 passes, 75.01 fails.
        let table = [
            (lines(3, 4), 74.0, true),
            (lines(3, 4), 75.0, true),
            (lines(3, 4), 75.01, false),
            (lines(1, 3), 33.0, true),
            (lines(1, 3), 34.0, false),
            (lines(0, 0), 90.0, true),
            (lines(0, 10), 0.0, true),
            (lines(10, 10), 100.0, true),
        ];
        for (l, floor, want) in table {
            assert_eq!(l.meets(floor), want, "{l:?} against {floor}");
        }
    }

    #[test]
    fn verdicts_and_messages() {
        let measured = BTreeMap::from([
            ("a".to_owned(), lines(80, 100)),
            ("b".to_owned(), lines(79, 100)),
            ("c".to_owned(), lines(1, 2)),
        ]);
        let floors = BTreeMap::from([
            ("a".to_owned(), 80.0),
            ("b".to_owned(), 80.0),
            ("gone".to_owned(), 50.0),
        ]);
        let verdicts = check(&measured, &floors);
        assert_eq!(verdicts.iter().filter(|v| v.fails()).count(), 1);
        assert_eq!(
            messages(&verdicts),
            [
                (
                    Level::Error,
                    "b: line coverage 79.00% is below its floor of 80%".to_owned()
                ),
                (
                    Level::Warning,
                    "c: line coverage 50.00% has no floor in .config/coverage.toml".to_owned()
                ),
                (
                    Level::Warning,
                    ".config/coverage.toml: the floor of 50% for gone matches no measured crate"
                        .to_owned()
                ),
            ]
        );
        let report = render(&measured, &verdicts);
        assert!(
            report.contains("| `a` | 80 | 100 | 80.00% | 80% | ok |"),
            "{report}"
        );
        assert!(
            report.contains("| `gone` | - | - | - | 50% | not measured |"),
            "{report}"
        );
        assert!(
            report.ends_with("```toml\n[floors]\na = 79\nb = 78\nc = 49\n```\n"),
            "{report}"
        );
    }

    #[test]
    fn suggested_floors_round_down_then_take_the_margin() {
        assert_eq!(suggested_floor(lines(8_399, 10_000)), 82);
        assert_eq!(suggested_floor(lines(1, 2)), 49);
        assert_eq!(suggested_floor(lines(0, 5)), 0);
        assert_eq!(suggested_floor(lines(0, 0)), 0);
    }
}
