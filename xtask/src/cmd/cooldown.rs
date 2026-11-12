//! `cargo xtask cooldown`: a change brings in no crate version published less than a cooldown
//! ago. Ported from scripts/check-cargo-cooldown.py (removed once the callers switched),
//! whose report, messages and exit status it keeps.
//!
//! Dependabot's cooldown delays only the crate it bumps; the crates that bump pulls in, and
//! anything added by hand, arrive at whatever age they are. This check covers every crates.io
//! package that is in a lockfile now but was not in it at BASE, in every workspace
//! (Cargo.lock, fuzz/Cargo.lock and xtask/Cargo.lock), directly used or not. Publication times
//! come from the crates.io sparse index (`pubtime`), with the crates.io API as a fallback.
//!
//! A new version is accepted when it is at least `days` old (.config/cargo-cooldown.toml),
//! when an exemption there names it, or when it fixes a RustSec vulnerability that the same
//! crate has at BASE: the crate has an advisory at BASE and the new version is in the patched
//! (or unaffected) range of every such advisory. The last needs cargo-audit; without it, such
//! updates need an exemption.
//!
//! It fails closed. Every input on which the script stopped with an error stops this check
//! with an error too: a failed request, an index answer other than success or 404, and any
//! answer, report or configuration of a shape the script could not handle (it raised; here
//! the message says what was wrong). A crate whose publication time neither the index nor the
//! API gives is NOT FOUND, which fails. A new lock entry whose name or version could not be a
//! crates.io crate's is an error before any verdict, and is never put in a URL: the script's
//! request for such an entry raised wherever urllib refused the URL (a space or control
//! character, or a non-ASCII one), so no exemption or security fix may pass it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::py;
use crate::core::time::{Date, Timestamp};
use crate::core::version::{matches, version_key};
use crate::ports::{Cargo, Clock, Git, Http};
use crate::{Error, Result};

pub const CONFIG: &str = ".config/cargo-cooldown.toml";
/// The lockfiles checked when no --lockfile is given.
pub const LOCKFILES: [&str; 3] = ["Cargo.lock", "fuzz/Cargo.lock", "xtask/Cargo.lock"];
const CRATES_IO: [&str; 2] = [
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
];
pub const USER_AGENT: &str = "observation-ci (https://github.com/panevain/observation)";

/// A crate version: (name, version).
pub type Package = (String, String);

/// The tables of `for e in value` where the script then called `e.get`: an array's items, each
/// of which must be a table. Python iterated a table's keys or a string's characters and
/// failed on the first, so only an empty one passes; anything else could not be iterated.
fn tables<'a>(value: Option<&'a toml::Value>, what: &str) -> Result<Vec<&'a toml::Table>> {
    let bad = || Error::new(format!("{what} must be an array of tables"));
    match value {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(items)) => {
            items.iter().map(|v| v.as_table().ok_or_else(bad)).collect()
        }
        Some(toml::Value::Table(t)) if t.is_empty() => Ok(Vec::new()),
        Some(toml::Value::String(s)) if s.is_empty() => Ok(Vec::new()),
        Some(_) => Err(bad()),
    }
}

/// (name, version) of every crates.io package in a Cargo.lock's text; nothing for an empty
/// text. Packages from other registries, git and paths are not counted. `path` names the
/// lockfile in errors.
pub fn packages(path: &str, text: &str) -> Result<BTreeSet<Package>> {
    if text.is_empty() {
        return Ok(BTreeSet::new());
    }
    let lock = py::parse_toml(text).map_err(|e| Error::new(format!("{path}: {e}")))?;
    let mut out = BTreeSet::new();
    for p in tables(lock.get("package"), &format!("{path}: `package`"))? {
        let from_crates_io = match p.get("source") {
            Some(toml::Value::String(s)) => CRATES_IO.contains(&s.as_str()),
            // Python's `in` on a set: an array or table cannot be hashed.
            Some(toml::Value::Array(_) | toml::Value::Table(_)) => {
                return Err(Error::new(format!(
                    "{path}: a package's `source` is not a string: {}",
                    py::repr_table(p)
                )));
            }
            _ => false,
        };
        if from_crates_io {
            let field = |k: &str| match p.get(k) {
                Some(toml::Value::String(s)) => Ok(s.clone()),
                _ => Err(Error::new(format!(
                    "{path}: a crates.io package's `{k}` is missing or not a string: {}",
                    py::repr_table(p)
                ))),
            };
            out.insert((field("name")?, field("version")?));
        }
    }
    Ok(out)
}

/// Whether `name` could be a crates.io crate's name (ASCII letters, digits, `-` and `_`, at
/// most 64), so that it can go into an index or API URL as it is.
pub fn valid_crate_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The sparse index's path for a crate (index.crates.io/<path>).
pub fn index_path(name: &str) -> String {
    let n = name.to_lowercase();
    match n.chars().count() {
        0..=2 => format!("{}/{n}", n.chars().count()),
        3 => format!(
            "3/{}/{n}",
            &n[..n.char_indices().nth(1).map_or(n.len(), |(i, _)| i)]
        ),
        _ => {
            let at = |k: usize| n.char_indices().nth(k).map_or(n.len(), |(i, _)| i);
            format!("{}/{}/{n}", &n[..at(2)], &n[at(2)..at(4)])
        }
    }
}

/// A crate's index file: each version's `pubtime` as the line has it (`None` without one). A
/// later line for the same version wins. Lines are split as `str.splitlines` splits them; each
/// must be a JSON object with a `vers`. A `vers` that is not a string names no version the
/// lockfile can have; an array or object one could not be a dict key in the script.
pub fn index_entries(body: &str) -> Result<BTreeMap<String, Option<Value>>> {
    let mut out = BTreeMap::new();
    for line in py::splitlines(body).into_iter().filter(|l| !l.is_empty()) {
        let entry: Value = serde_json::from_str(line)
            .map_err(|e| Error::new(format!("index line: {e}: {line}")))?;
        let vers = entry
            .as_object()
            .and_then(|o| o.get("vers"))
            .ok_or_else(|| Error::new(format!("index line without `vers`: {line}")))?;
        match vers {
            Value::String(v) => {
                out.insert(v.clone(), entry.get("pubtime").cloned());
            }
            Value::Array(_) | Value::Object(_) => {
                return Err(Error::new(format!(
                    "index line with an unusable `vers`: {line}"
                )));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The crates.io API's `version.created_at` as the answer has it; `None` when either key is
/// missing. Any other shape is an error (the script raised).
pub fn api_created_at(body: &str) -> Result<Option<Value>> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| Error::new(format!("crates.io API: {e}")))?;
    let unexpected = || Error::new(format!("crates.io API: unexpected answer: {body}"));
    let top = v.as_object().ok_or_else(unexpected)?;
    let Some(version) = top.get("version") else {
        return Ok(None);
    };
    Ok(version
        .as_object()
        .ok_or_else(unexpected)?
        .get("created_at")
        .cloned())
}

/// A publication time from the index or the API: a string with a UTC offset (the script
/// compared it with the current time, which raised for one without).
pub fn parse_stamp(stamp: &Value) -> Result<Timestamp> {
    stamp
        .as_str()
        .and_then(Timestamp::parse_aware)
        .ok_or_else(|| Error::new(format!("not a timestamp with a UTC offset: {stamp}")))
}

/// RustSec vulnerabilities at BASE, by crate: (advisory id, fixed ranges), each as the report
/// had it (the script used the ranges and ids as they came; see [`security_fix`]).
pub type Vulnerable = BTreeMap<String, Vec<(Value, Vec<Value>)>>;

/// Python's `list(x)` of a JSON value: an array's items, a string's characters or an object's
/// keys; `None` for what cannot be iterated.
fn py_list(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Array(a) => Some(a.clone()),
        Value::String(s) => Some(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(o) => Some(o.keys().map(|k| Value::String(k.clone())).collect()),
        _ => None,
    }
}

/// The vulnerabilities in a cargo-audit JSON report, added to `found`:
/// `vulnerabilities.list[]` with each one's `package.name`, `advisory.id` and
/// `versions.patched` + `versions.unaffected`. A report of any shape the script could not
/// read is an error. A package name that is not a string names no crate (the script kept it
/// under a key no crate matched).
pub fn add_vulnerabilities(report: &Value, found: &mut Vulnerable) -> Result<()> {
    let bad = |what: &str| Error::new(format!("cargo-audit: unexpected report: {what}"));
    let empty = serde_json::Map::new();
    let top = report.as_object().ok_or_else(|| bad("not an object"))?;
    let vulnerabilities = match top.get("vulnerabilities") {
        None => &empty,
        Some(v) => v.as_object().ok_or_else(|| bad("`vulnerabilities`"))?,
    };
    let list = match vulnerabilities.get("list") {
        None => Vec::new(),
        Some(l) => py_list(l).ok_or_else(|| bad("`vulnerabilities.list`"))?,
    };
    for v in &list {
        let v = v
            .as_object()
            .ok_or_else(|| bad("a vulnerability is not an object"))?;
        let ranges = match v.get("versions") {
            None => &empty,
            Some(r) => r.as_object().ok_or_else(|| bad("`versions`"))?,
        };
        let mut fixed = Vec::new();
        for key in ["patched", "unaffected"] {
            if let Some(r) = ranges.get(key) {
                fixed.extend(py_list(r).ok_or_else(|| bad(&format!("`versions.{key}`")))?);
            }
        }
        let name = v
            .get("package")
            .and_then(Value::as_object)
            .and_then(|p| p.get("name"))
            .ok_or_else(|| bad("a vulnerability without `package.name`"))?;
        let id = v
            .get("advisory")
            .and_then(Value::as_object)
            .and_then(|a| a.get("id"))
            .ok_or_else(|| bad("a vulnerability without `advisory.id`"))?;
        match name {
            Value::String(name) => found
                .entry(name.clone())
                .or_default()
                .push((id.clone(), fixed)),
            Value::Array(_) | Value::Object(_) => return Err(bad("`package.name`")),
            _ => {}
        }
    }
    Ok(())
}

/// The advisory IDs that `name@version` fixes, if it fixes every one its crate has at BASE.
/// A version or range that cannot be read counts as not fixed, as in the script; a range that
/// is not a string, or an id that is not one when the ids are listed, is an error (the script
/// raised).
pub fn security_fix(
    name: &str,
    version: &str,
    vulnerable: &Vulnerable,
) -> Result<Option<Vec<String>>> {
    let Some(advisories) = vulnerable.get(name).filter(|a| !a.is_empty()) else {
        return Ok(None);
    };
    let readable = version_key(version).is_ok();
    for (_, fixed) in advisories {
        let mut any = false;
        for r in fixed {
            // The script read the version before each range.
            if !readable {
                return Ok(None);
            }
            let Value::String(r) = r else {
                return Err(Error::new(format!(
                    "cargo-audit: a range of {name} is not a string: {r}"
                )));
            };
            match matches(version, r) {
                Ok(true) => {
                    any = true;
                    break;
                }
                Ok(false) => {}
                Err(_) => return Ok(None),
            }
        }
        if !any {
            return Ok(None);
        }
    }
    let mut ids = BTreeSet::new();
    for (id, _) in advisories {
        let Value::String(id) = id else {
            return Err(Error::new(format!(
                "cargo-audit: an advisory id of {name} is not a string: {id}"
            )));
        };
        ids.insert(id.clone());
    }
    Ok(Some(ids.into_iter().collect()))
}

/// An exemption from .config/cargo-cooldown.toml that has not expired, with its values as
/// tomllib gave them.
#[derive(Debug, Clone, PartialEq)]
pub struct Exemption {
    pub name: toml::Value,
    /// `None`: every version of the crate. A value that is not a string matches no version.
    pub version: Option<toml::Value>,
    pub reason: toml::Value,
}

impl Exemption {
    fn applies(&self, name: &str, version: &str) -> bool {
        self.name.as_str() == Some(name)
            && self
                .version
                .as_ref()
                .is_none_or(|v| v.as_str() == Some(version))
    }
}

/// The configuration: the cooldown in days (and as the script printed it), the exemptions in
/// force, and notes about expired ones.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub days: i64,
    /// `days` as the report shows it (`True` for `days = true`, a whole number to Python).
    pub days_text: String,
    pub exempt: Vec<Exemption>,
    pub notes: Vec<String>,
}

/// Reads the configuration; exemptions whose `until` is before `today` become notes. The
/// checks are the script's, on Python's terms (truthiness, `isinstance`).
pub fn load_config(text: &str, today: Date) -> Result<Config> {
    let cfg = py::parse_toml(text).map_err(|e| Error::new(format!("{CONFIG}: {e}")))?;
    let (days, days_text) = match cfg.get("days") {
        Some(toml::Value::Integer(d)) if *d >= 0 => (*d, d.to_string()),
        // bool is an int to Python.
        Some(toml::Value::Boolean(b)) => (i64::from(*b), if *b { "True" } else { "False" }.into()),
        _ => {
            return Err(Error::new(format!(
                "{CONFIG}: `days` must be a whole number of days"
            )));
        }
    };
    let mut exempt = Vec::new();
    let mut notes = Vec::new();
    for e in tables(cfg.get("exempt"), &format!("{CONFIG}: `exempt`"))? {
        let truthy = |k: &str| e.get(k).filter(|v| py::toml_truthy(v));
        let (Some(name), Some(reason)) = (truthy("name"), truthy("reason")) else {
            return Err(Error::new(format!(
                "{CONFIG}: every exemption needs a name and a reason: {}",
                py::repr_table(e)
            )));
        };
        let until = match e.get("until") {
            None => None,
            Some(toml::Value::Datetime(d)) if d.time.is_none() => d
                .date
                .and_then(|d| Date::new(i32::from(d.year), u32::from(d.month), u32::from(d.day))),
            // A date-time is a date to `isinstance`; comparing it with a date then raised.
            Some(toml::Value::Datetime(d)) if d.date.is_some() => {
                return Err(Error::new(format!(
                    "{CONFIG}: `until` must be a date (YYYY-MM-DD), not a date and time: {}",
                    py::repr_table(e)
                )));
            }
            Some(_) => {
                return Err(Error::new(format!(
                    "{CONFIG}: `until` must be a date (YYYY-MM-DD): {}",
                    py::repr_table(e)
                )));
            }
        };
        let version = e.get("version");
        if let Some(until) = until.filter(|u| *u < today) {
            let what = match version.filter(|v| py::toml_truthy(v)) {
                Some(v) => format!("{} {}", py::str_toml(name), py::str_toml(v)),
                None => py::str_toml(name),
            };
            notes.push(format!(
                "the exemption for {what} expired on {until}; remove it"
            ));
            continue;
        }
        exempt.push(Exemption {
            name: name.clone(),
            version: version.cloned(),
            reason: reason.clone(),
        });
    }
    Ok(Config {
        days,
        days_text,
        exempt,
        notes,
    })
}

/// The cutoff: `now` less the cooldown. An error where Python's datetime could not subtract
/// it (before year 1), as the script stopped there.
pub fn cutoff(now: Timestamp, days: i64) -> Result<Timestamp> {
    now.checked_minus_days(days).ok_or_else(|| {
        Error::new(format!(
            "{CONFIG}: `days` = {days} reaches back before the year 1"
        ))
    })
}

/// One row of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub version: String,
    pub published: Option<Timestamp>,
    pub verdict: String,
    pub failed: bool,
}

/// The verdict on each new package, in order: old enough, exempt, a security fix, or failed
/// (TOO NEW, or NOT FOUND on crates.io when no publication time is known).
pub fn verdicts(
    new: &BTreeSet<Package>,
    published: &BTreeMap<Package, Option<Timestamp>>,
    config: &Config,
    vulnerable: &Vulnerable,
    cutoff: Timestamp,
) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    for p in new {
        let (name, version) = p;
        let when = published.get(p).copied().flatten();
        let rule = config.exempt.iter().find(|e| e.applies(name, version));
        let fixes = security_fix(name, version, vulnerable)?;
        let (verdict, failed) = if when.is_some_and(|w| w <= cutoff) {
            ("ok".to_owned(), false)
        } else if let Some(rule) = rule {
            (format!("exempt: {}", py::str_toml(&rule.reason)), false)
        } else if let Some(ids) = fixes {
            (format!("security fix ({})", ids.join(", ")), false)
        } else if when.is_some() {
            ("TOO NEW".to_owned(), true)
        } else {
            ("NOT FOUND on crates.io".to_owned(), true)
        };
        rows.push(Row {
            name: name.clone(),
            version: version.clone(),
            published: when,
            verdict,
            failed,
        });
    }
    Ok(rows)
}

/// The Markdown report, as the script wrote it. `days` is the cooldown as the script printed
/// it.
pub fn render(days: &str, base: &str, rows: &[Row], notes: &[String], now: Timestamp) -> String {
    let mut lines = vec![
        format!("### Cargo cooldown ({days} days, against {base})"),
        String::new(),
    ];
    if rows.is_empty() {
        lines.push("No crate versions added.".to_owned());
    } else {
        lines.push("| crate | version | published | age | verdict |".to_owned());
        lines.push("| --- | --- | --- | ---: | --- |".to_owned());
        for r in rows {
            let date = r.published.map_or("?".to_owned(), |w| w.date().to_string());
            let age = r
                .published
                .map_or("unknown".to_owned(), |w| format!("{} d", now.days_since(w)));
            lines.push(format!(
                "| `{}` | {} | {date} | {age} | {} |",
                r.name, r.version, r.verdict
            ));
        }
    }
    lines.push(String::new());
    lines.extend(notes.iter().map(|n| format!("- {n}")));
    let failed = rows.iter().filter(|r| r.failed).count();
    if failed > 0 {
        lines.push(String::new());
        lines.push(format!(
            "**{failed} crate version(s) younger than {days} days**: wait until they are, pin an \
             older version, or add an exemption with a reason to `{CONFIG}`."
        ));
    }
    lines.join("\n").trim_end().to_owned() + "\n"
}

/// The command's inputs besides the ports.
#[derive(Debug, Clone)]
pub struct Args {
    pub base: String,
    pub lockfiles: Vec<String>,
    /// `--now`: the current time (ISO 8601); without an offset, UTC.
    pub now: Option<String>,
}

/// What the command prints: the report on stdout (with the extra newline `print` added) and in
/// the summary file; on cargo-audit's failure, its output on stderr before the error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub report: String,
    pub failed: bool,
}

/// A directory under the system's temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let base = std::env::temp_dir();
        for n in 0..1000 {
            let dir = base.join(format!("xtask-cooldown-{}-{n}", std::process::id()));
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::new(format!("{}: {e}", dir.display()))),
            }
        }
        Err(Error::new("could not create a temporary directory"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The ports the command uses.
pub struct Ports<'a> {
    pub git: &'a dyn Git,
    pub http: &'a dyn Http,
    pub cargo: &'a dyn Cargo,
    pub clock: &'a dyn Clock,
}

impl std::fmt::Debug for Ports<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ports").finish_non_exhaustive()
    }
}

/// RustSec vulnerabilities in the lockfiles at BASE (`None` without cargo-audit). When
/// cargo-audit prints no JSON, its output goes to `diagnostics` and the check stops.
fn vulnerable_at_base(
    base_texts: &[String],
    cargo: &dyn Cargo,
    diagnostics: &mut String,
) -> Result<Option<Vulnerable>> {
    let tmp = TempDir::new()?;
    let mut found = Vulnerable::new();
    for (i, text) in base_texts.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        let lock = tmp.0.join(i.to_string()).join("Cargo.lock");
        fs::create_dir_all(tmp.0.join(i.to_string()))
            .and_then(|()| fs::write(&lock, text))
            .map_err(|e| Error::new(format!("{}: {e}", lock.display())))?;
        let Some(out) = cargo.audit_json(&lock)? else {
            return Ok(None);
        };
        let report: Value = match serde_json::from_str(&py::universal_newlines(&out.stdout)) {
            Ok(report) => report,
            Err(_) => {
                diagnostics.push_str(&out.stdout);
                diagnostics.push_str(&out.stderr);
                return Err(Error::new("cargo-audit produced no JSON"));
            }
        };
        add_vulnerabilities(&report, &mut found)?;
    }
    Ok(Some(found))
}

/// Fails unless a new lock entry (from `lockfile`) could be a crates.io crate: a name of
/// [`valid_crate_name`] and a version `semver` reads. Only such an entry is put in a URL.
///
/// The script requested such an entry's index file and then its API URL, and urllib raised
/// (exit 1) when the URL, its ends stripped and any `#…` cut off, kept a byte in `\x00-\x20`
/// or `\x7f` (`InvalidURL`) or a non-ASCII character (`UnicodeEncodeError`). Every such entry
/// fails here. So do the rest, where the script's request went through (`serde.rs`,
/// `1.0.0#x`, a trailing space): those are listed as stricter.
pub fn check_lock_entry(lockfile: &str, name: &str, version: &str) -> Result<()> {
    let what = if !valid_crate_name(name) {
        "its name is not a crates.io crate name (ASCII letters, digits, `-` and `_`, at most 64)"
    } else if semver::Version::parse(version).is_err() {
        "its version is not a semver version"
    } else {
        return Ok(());
    };
    Err(Error::new(format!(
        "{lockfile}: new crates.io package name = {}, version = {}: {what}",
        py::repr_str(name),
        py::repr_str(version)
    )))
}

/// When crates.io published `name@version`: the index's `pubtime`, else (when the line has
/// none, or a false one to Python) the API's `created_at`, else `None`. Fails on any index
/// answer but success or 404, on a failed request, and on answers of a shape the script could
/// not read. The entry must have passed [`check_lock_entry`]; it is checked again here, so that
/// nothing else is ever put in a URL.
fn published(
    lockfile: &str,
    name: &str,
    version: &str,
    http: &dyn Http,
    index: &mut BTreeMap<String, BTreeMap<String, Option<Value>>>,
) -> Result<Option<Timestamp>> {
    check_lock_entry(lockfile, name, version)?;
    if !index.contains_key(name) {
        let url = format!("https://index.crates.io/{}", index_path(name));
        let r = http.get(&url)?;
        let entries = match r.status {
            404 => BTreeMap::new(),
            s if s >= 400 => return Err(Error::new(format!("{url}: HTTP {s}"))),
            _ => {
                index_entries(&r.body).map_err(|e| Error::new(format!("{name} {version}: {e}")))?
            }
        };
        index.insert(name.to_owned(), entries);
    }
    let pubtime = index
        .get(name)
        .and_then(|e| e.get(version))
        .cloned()
        .flatten()
        .filter(py::json_truthy);
    let stamp = match pubtime {
        Some(s) => s,
        None => {
            let r = http.get(&format!("https://crates.io/api/v1/crates/{name}/{version}"))?;
            if r.status >= 400 {
                return Ok(None);
            }
            match api_created_at(&r.body)
                .map_err(|e| Error::new(format!("{name} {version}: {e}")))?
            {
                Some(s) => s,
                None => return Ok(None),
            }
        }
    };
    parse_stamp(&stamp)
        .map(Some)
        .map_err(|e| Error::new(format!("{name} {version}: {e}")))
}

/// Runs the check on the repository at `repo`. `diagnostics` receives what goes to stderr
/// before an error (cargo-audit's output when it printed no JSON).
pub fn run(
    repo: &Path,
    args: &Args,
    ports: &Ports<'_>,
    diagnostics: &mut String,
) -> Result<Outcome> {
    let now = match &args.now {
        Some(s) => Timestamp::parse(s)
            .ok_or_else(|| Error::new(format!("--now {s}: not an ISO 8601 time")))?,
        None => ports.clock.now(),
    };
    let text = fs::read_to_string(repo.join(CONFIG))
        .map_err(|e| Error::new(format!("{}: {e}", repo.join(CONFIG).display())))?;
    let mut config = load_config(&text, now.date())?;
    let cutoff = cutoff(now, config.days)?;
    if !ports.git.is_commit(&args.base)? {
        return Err(Error::new(format!(
            "--base {}: not a commit in this repository (fetch it first?)",
            args.base
        )));
    }
    let mut base_texts = Vec::new();
    let mut new = BTreeSet::new();
    // The first lockfile each new entry was found in, for errors.
    let mut found_in = BTreeMap::new();
    for path in &args.lockfiles {
        let base = py::universal_newlines(&ports.git.show(&args.base, path)?.unwrap_or_default());
        let head_file = repo.join(path);
        let head = if head_file.exists() {
            fs::read_to_string(&head_file).map_err(|e| Error::new(format!("{path}: {e}")))?
        } else {
            String::new()
        };
        let before = packages(path, &base)?;
        for p in packages(path, &head)?.difference(&before) {
            found_in.entry(p.clone()).or_insert(path.as_str());
            new.insert(p.clone());
        }
        base_texts.push(base);
    }
    let vulnerable = if new.is_empty() {
        Vulnerable::new()
    } else {
        match vulnerable_at_base(&base_texts, ports.cargo, diagnostics)? {
            Some(v) => v,
            None => {
                config.notes.push(
                    "cargo-audit is not on PATH, so security fixes are not recognised; they need an exemption"
                        .to_owned(),
                );
                Vulnerable::new()
            }
        }
    };
    let mut index = BTreeMap::new();
    let mut times = BTreeMap::new();
    // In the script's order: each entry's requests, and so its error, before the next entry's,
    // and every one before any verdict.
    for p in &new {
        let lockfile = found_in.get(p).copied().unwrap_or_default();
        let when = published(lockfile, &p.0, &p.1, ports.http, &mut index)?;
        times.insert(p.clone(), when);
    }
    let rows = verdicts(&new, &times, &config, &vulnerable, cutoff)?;
    Ok(Outcome {
        report: render(&config.days_text, &args.base, &rows, &config.notes, now),
        failed: rows.iter().any(|r| r.failed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{FakeCargo, FakeGit, FakeHttp, FixedClock};
    use crate::ports::{AuditOutput, Response};
    use serde::Deserialize;
    use std::collections::HashMap;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    #[derive(Deserialize)]
    struct Cases {
        case: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        now: String,
        base: Option<String>,
        config: String,
        lockfiles: BTreeMap<String, Sides>,
        #[serde(default)]
        index: BTreeMap<String, Answer>,
        #[serde(default)]
        api: BTreeMap<String, Answer>,
        audit: Option<Audit>,
    }

    #[derive(Deserialize)]
    struct Sides {
        base: Option<String>,
        head: Option<String>,
    }

    #[derive(Deserialize)]
    struct Answer {
        status: Option<u16>,
        #[serde(default)]
        body: String,
        #[serde(default)]
        error: bool,
    }

    #[derive(Deserialize)]
    struct Audit {
        #[serde(default)]
        json: String,
        #[serde(default)]
        stderr: String,
    }

    fn answer(a: &Answer) -> Result<Response> {
        if a.error {
            return Err(Error::new("recorded network failure"));
        }
        Ok(Response {
            status: a.status.unwrap_or(200),
            body: a.body.clone(),
        })
    }

    /// One case, written as the golden writes it. A failure the script left as a traceback
    /// is written as `<error>` and the port's message, which the test checks on its own.
    fn render_case(case: &Case) -> (String, Option<String>) {
        let tmp = ok(tempfile::tempdir());
        let repo = tmp.path();
        ok(fs::create_dir_all(repo.join(".config")));
        ok(fs::write(repo.join(CONFIG), &case.config));
        let base = case.base.clone().unwrap_or_else(|| "base".into());
        let mut git = FakeGit {
            commits: vec!["base".into()],
            ..FakeGit::default()
        };
        for (path, sides) in &case.lockfiles {
            git.files
                .insert(("base".into(), path.clone()), sides.base.clone());
            if let Some(head) = &sides.head {
                ok(fs::create_dir_all(repo.join(path).parent().unwrap_or(repo)));
                ok(fs::write(repo.join(path), head));
            }
        }
        let mut responses = HashMap::new();
        for (name, a) in &case.index {
            responses.insert(
                format!("https://index.crates.io/{}", index_path(name)),
                answer(a),
            );
        }
        for (key, a) in &case.api {
            responses.insert(format!("https://crates.io/api/v1/crates/{key}"), answer(a));
        }
        let http = FakeHttp {
            responses,
            ..FakeHttp::default()
        };
        let mut cargo = FakeCargo {
            audit_installed: case.audit.is_some(),
            ..FakeCargo::default()
        };
        if let Some(audit) = &case.audit {
            for sides in case.lockfiles.values() {
                if let Some(text) = sides.base.clone().filter(|t| !t.is_empty()) {
                    let out = AuditOutput {
                        stdout: audit.json.clone(),
                        stderr: audit.stderr.clone(),
                    };
                    cargo.audits.insert(text, out);
                }
            }
        }
        let clock = FixedClock(Timestamp::from_unix_micros(0));
        let ports = Ports {
            git: &git,
            http: &http,
            cargo: &cargo,
            clock: &clock,
        };
        let args = Args {
            base,
            lockfiles: case.lockfiles.keys().cloned().collect(),
            now: Some(case.now.clone()),
        };
        let mut diagnostics = String::new();
        let result = run(repo, &args, &ports, &mut diagnostics);
        let (code, stdout, stderr, port_error) = match result {
            Ok(out) => (
                u8::from(out.failed),
                format!("{}\n", out.report),
                diagnostics,
                None,
            ),
            Err(e) if diagnostics.is_empty() && e.to_string().starts_with("https://") => (
                1,
                String::new(),
                "<error>\n".to_owned(),
                Some(e.to_string()),
            ),
            Err(e) if e.to_string() == "recorded network failure" => (
                1,
                String::new(),
                "<error>\n".to_owned(),
                Some(e.to_string()),
            ),
            Err(e) => (1, String::new(), format!("{diagnostics}{e}\n"), None),
        };
        (
            format!(
                "== {}\nexit {code}\n-- stdout\n{stdout}-- stderr\n{stderr}",
                case.name
            ),
            port_error,
        )
    }

    /// Every scenario of cooldown/cases.toml gives what scripts/check-cargo-cooldown.py (since
    /// removed) printed for it (cooldown/golden; see the pull request that ported it). Where the
    /// script ended with a traceback, the port fails with its own message: checked here.
    #[test]
    fn scenarios_match_the_script() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cooldown");
        let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(
            dir.join("cases.toml"),
        ))));
        let golden = ok(fs::read_to_string(dir.join("golden")));
        let mut want = golden
            .split("== ")
            .filter(|s| !s.is_empty())
            .map(|s| format!("== {s}"));
        for case in &cases.case {
            let (got, port_error) = render_case(case);
            let mut expected = want.next().unwrap_or_default();
            if let Some(i) = expected.find("<traceback> ") {
                expected.replace_range(i.., "<error>\n");
            }
            assert_eq!(got, expected, "case {:?}", case.name);
            match case.name.as_str() {
                "network error" => {
                    assert_eq!(port_error.as_deref(), Some("recorded network failure"))
                }
                "index server error" => assert_eq!(
                    port_error.as_deref(),
                    Some("https://index.crates.io/fl/ak/flaky: HTTP 503")
                ),
                _ => assert_eq!(port_error, None, "case {:?}", case.name),
            }
        }
        assert_eq!(
            want.next(),
            None,
            "the golden has more cases than cases.toml"
        );
    }

    /// A new lock entry the script's request raised on (urllib refused the URL) fails before
    /// any request and before an exemption or a security fix could pass it (the fakes panic on
    /// a request nobody set up). It names the lockfile and the entry.
    #[test]
    fn an_entry_no_url_can_hold_is_an_error() {
        let lock = |name: &str, version: &str| {
            format!(
                "version = 4\n\n[[package]]\nname = {name:?}\nversion = {version:?}\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
            )
        };
        let advisory = r#"{"vulnerabilities": {"list": [{"advisory": {"id": "RUSTSEC-0000-0001"},
            "package": {"name": "serde"}, "versions": {"patched": [">= 1.0.0"], "unaffected": []}}]}}"#;
        let cases = [
            ("serde", "1.0.0 x", "serde", "its version"),
            ("a b", "1.0.0", "a b", "its name"),
            ("s\u{e9}rde", "1.0.0", "s\u{e9}rde", "its name"),
            ("serde", " 1.0.0", "", "its version"),
        ];
        for (name, version, exempt, what) in cases {
            let tmp = ok(tempfile::tempdir());
            let repo = tmp.path();
            ok(fs::create_dir_all(repo.join("fuzz")));
            ok(fs::create_dir_all(repo.join(".config")));
            let config = if exempt.is_empty() {
                "days = 7\n".to_owned()
            } else {
                format!("days = 7\n[[exempt]]\nname = {exempt:?}\nreason = \"r\"\n")
            };
            ok(fs::write(repo.join(CONFIG), config));
            let base = lock("serde", "0.9.0");
            ok(fs::write(repo.join("fuzz/Cargo.lock"), lock(name, version)));
            let git = FakeGit {
                commits: vec!["base".into()],
                files: HashMap::from([(
                    ("base".into(), "fuzz/Cargo.lock".into()),
                    Some(base.clone()),
                )]),
                ..FakeGit::default()
            };
            let cargo = FakeCargo {
                audit_installed: true,
                audits: HashMap::from([(
                    base,
                    AuditOutput {
                        stdout: advisory.into(),
                        stderr: String::new(),
                    },
                )]),
                ..FakeCargo::default()
            };
            let ports = Ports {
                git: &git,
                http: &FakeHttp::default(),
                cargo: &cargo,
                clock: &FixedClock(Timestamp::from_unix_micros(0)),
            };
            let args = Args {
                base: "base".into(),
                lockfiles: vec!["fuzz/Cargo.lock".into()],
                now: Some("2026-11-12T00:00:00Z".into()),
            };
            let err = match run(repo, &args, &ports, &mut String::new()) {
                Ok(out) => panic!("{name:?} {version:?} passed the check: {}", out.report),
                Err(e) => e.to_string(),
            };
            let want = format!(
                "fuzz/Cargo.lock: new crates.io package name = {}, version = {}: {what}",
                py::repr_str(name),
                py::repr_str(version)
            );
            assert!(err.starts_with(&want), "{err}");
        }
    }

    /// One case run on the fakes: exit status, stdout, stderr (an error as its message).
    /// A panic is a failure of the test.
    fn run_case(case: &Case) -> (u8, String, String) {
        let tmp = ok(tempfile::tempdir());
        let repo = tmp.path();
        ok(fs::create_dir_all(repo.join(".config")));
        ok(fs::write(repo.join(CONFIG), &case.config));
        let mut git = FakeGit {
            commits: vec!["base".into()],
            ..FakeGit::default()
        };
        for (path, sides) in &case.lockfiles {
            git.files
                .insert(("base".into(), path.clone()), sides.base.clone());
            if let Some(head) = &sides.head {
                ok(fs::create_dir_all(repo.join(path).parent().unwrap_or(repo)));
                ok(fs::write(repo.join(path), head));
            }
        }
        let mut responses = HashMap::new();
        for (name, a) in &case.index {
            responses.insert(
                format!("https://index.crates.io/{}", index_path(name)),
                answer(a),
            );
        }
        for (key, a) in &case.api {
            responses.insert(format!("https://crates.io/api/v1/crates/{key}"), answer(a));
        }
        let http = FakeHttp {
            responses,
            ..FakeHttp::default()
        };
        let mut cargo = FakeCargo {
            audit_installed: case.audit.is_some(),
            ..FakeCargo::default()
        };
        if let Some(audit) = &case.audit {
            for sides in case.lockfiles.values() {
                if let Some(text) = sides.base.clone().filter(|t| !t.is_empty()) {
                    let out = AuditOutput {
                        stdout: audit.json.clone(),
                        stderr: audit.stderr.clone(),
                    };
                    cargo.audits.insert(py::universal_newlines(&text), out);
                }
            }
        }
        let clock = FixedClock(Timestamp::from_unix_micros(0));
        let ports = Ports {
            git: &git,
            http: &http,
            cargo: &cargo,
            clock: &clock,
        };
        let args = Args {
            base: case.base.clone().unwrap_or_else(|| "base".into()),
            lockfiles: case.lockfiles.keys().cloned().collect(),
            now: Some(case.now.clone()),
        };
        let mut diagnostics = String::new();
        match run(repo, &args, &ports, &mut diagnostics) {
            Ok(out) => (
                u8::from(out.failed),
                format!("{}\n", out.report),
                diagnostics,
            ),
            Err(e) => (1, String::new(), format!("{diagnostics}{e}\n")),
        }
    }

    /// The cases on which the port fails although the script passed, on purpose (each is
    /// listed under "Behaviour differences" in the pull request that made it so).
    const STRICTER: &[&str] = &[
        // Timestamp forms Python's fromisoformat read and crates.io does not write.
        "pubtime +00 short offset",
        "pubtime offset with secs",
        "pubtime space before offset",
        "pubtime basic format",
        "pubtime lowercase t",
        // A new entry whose name or version could not be a crates.io crate's is an error
        // without a request. The script's requests went through for these: serde's index file
        // for `serde?x`, and NOT FOUND, then an exemption, for the other two.
        "name query string",
        "name glob brackets exempt",
        "version two parts exempt",
        // A crates.io lock entry whose name or version is not a string.
        "lock name int in both",
    ];

    /// The cases on which both fail, the script with its own report (NOT FOUND, exit 1) and the
    /// port with an error, on purpose: the entry's name or version could not be a crates.io
    /// crate's, so the port refuses it before any verdict.
    const ERROR_FOR_A_REPORT: &[&str] = &[
        "name glob brackets",
        "name slash",
        "version glob brackets",
        "version glob braces",
    ];

    /// Every case of cooldown/parity-cases.toml against what the script did with it
    /// (cooldown/parity-golden, captured from the script with the same recorded answers):
    /// - where the script failed, the port fails; where it failed with a message of its own
    ///   (not a Python traceback), with the same output, except for the cases in
    ///   [`ERROR_FOR_A_REPORT`], where the port fails with its own error;
    /// - where the script passed, the port passes with the same output, except for the cases
    ///   in [`STRICTER`], where it fails.
    #[test]
    fn never_passes_where_the_script_failed() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cooldown");
        let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(
            dir.join("parity-cases.toml"),
        ))));
        let golden = ok(fs::read_to_string(dir.join("parity-golden")));
        let mut want = BTreeMap::new();
        for block in golden.split("== ").filter(|b| !b.is_empty()) {
            let (name, rest) = block.split_once('\n').unwrap_or((block, ""));
            let (code, rest) = rest.split_once('\n').unwrap_or((rest, ""));
            let rest = rest.trim_prefix("-- stdout\n");
            let (stdout, stderr) = rest.split_once("-- stderr\n").unwrap_or((rest, ""));
            want.insert(
                name.to_owned(),
                (code.to_owned(), stdout.to_owned(), stderr.to_owned()),
            );
        }
        assert_eq!(
            want.len(),
            cases.case.len(),
            "cases and golden differ in length"
        );
        let mut problems = Vec::new();
        for case in &cases.case {
            let (code, stdout, stderr) = run_case(case);
            let Some((py_code, py_stdout, py_stderr)) = want.get(&case.name) else {
                problems.push(format!("{}: not in the golden", case.name));
                continue;
            };
            let same = stdout == *py_stdout && stderr == *py_stderr;
            let problem = match (py_code.as_str(), code) {
                ("exit 0", 0) if !same => "passes with different output",
                ("exit 0", 0) if STRICTER.contains(&case.name.as_str()) => {
                    "passes, but is listed as stricter"
                }
                ("exit 0", 0) => "",
                ("exit 0", _) if !STRICTER.contains(&case.name.as_str()) => {
                    "fails where the script passed (not listed as stricter)"
                }
                ("exit 0", _) => "",
                (_, 0) => "PASSES WHERE THE SCRIPT FAILED",
                _ if ERROR_FOR_A_REPORT.contains(&case.name.as_str()) => {
                    if !stdout.is_empty() || !stderr.contains(": new crates.io package name = ") {
                        "is listed as an error for the script's report, but is not that error"
                    } else {
                        ""
                    }
                }
                _ if !py_stderr.contains("<traceback>") && !same => {
                    "fails with output other than the script's message"
                }
                _ => "",
            };
            if !problem.is_empty() {
                problems.push(format!(
                    "{}: {problem}\n  script: {py_code} {py_stdout:?} {py_stderr:?}\n  port: exit {code} {stdout:?} {stderr:?}",
                    case.name
                ));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn index_paths() {
        let cases = [
            ("a", "1/a"),
            ("ab", "2/ab"),
            ("abc", "3/a/abc"),
            ("Serde", "se/rd/serde"),
            ("syn", "3/s/syn"),
        ];
        for (name, path) in cases {
            assert_eq!(index_path(name), path);
        }
    }
}
