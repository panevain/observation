//! `cargo xtask cooldown`: a change brings in no crate version published less than a cooldown
//! ago. Ported from scripts/check-cargo-cooldown.py, whose report, messages and exit status it
//! keeps.
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
//! It fails closed: an index answer other than success or 404, or a failed request, stops the
//! check with an error. A crate the index does not know (404) whose API entry is missing too
//! is reported as NOT FOUND, which fails.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::time::{Date, Timestamp};
use crate::core::version::matches;
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

/// (name, version) of every crates.io package in a Cargo.lock's text; nothing for an empty
/// text. Packages from other registries, git and paths are not counted.
pub fn packages(text: &str) -> Result<BTreeSet<Package>> {
    if text.is_empty() {
        return Ok(BTreeSet::new());
    }
    let lock: toml::Table =
        toml::from_str(text).map_err(|e| Error::new(format!("Cargo.lock: {e}")))?;
    let mut out = BTreeSet::new();
    let Some(list) = lock.get("package").and_then(toml::Value::as_array) else {
        return Ok(out);
    };
    for p in list {
        let source = p.get("source").and_then(toml::Value::as_str);
        if source.is_some_and(|s| CRATES_IO.contains(&s)) {
            let field = |k: &str| {
                p.get(k)
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| Error::new(format!("Cargo.lock: a package has no `{k}`")))
            };
            out.insert((field("name")?, field("version")?));
        }
    }
    Ok(out)
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

/// A crate's index file: each version's `pubtime`, if it has one. A later line for the same
/// version wins.
pub fn index_entries(body: &str) -> Result<BTreeMap<String, Option<String>>> {
    let mut out = BTreeMap::new();
    for line in body.split('\n').filter(|l| !l.is_empty()) {
        let entry: Value = serde_json::from_str(line)
            .map_err(|e| Error::new(format!("index line: {e}: {line}")))?;
        let vers = entry
            .get("vers")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new(format!("index line without `vers`: {line}")))?;
        let pubtime = entry
            .get("pubtime")
            .and_then(Value::as_str)
            .map(str::to_owned);
        out.insert(vers.to_owned(), pubtime);
    }
    Ok(out)
}

/// The crates.io API's `version.created_at`, if the answer has one.
pub fn api_created_at(body: &str) -> Result<Option<String>> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| Error::new(format!("crates.io API: {e}")))?;
    Ok(v.get("version")
        .and_then(|v| v.get("created_at"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

/// A timestamp from the index or the API, as the script read it (`Z` meaning +00:00).
pub fn parse_stamp(stamp: &str) -> Result<Timestamp> {
    Timestamp::parse(stamp).ok_or_else(|| Error::new(format!("not a timestamp: {stamp:?}")))
}

/// RustSec vulnerabilities in a cargo-audit JSON report: (crate, advisory id, the ranges a
/// version must be in to be fixed: patched, then unaffected).
pub fn vulnerabilities(json: &str) -> Result<Vec<(String, String, Vec<String>)>> {
    let data: Value = serde_json::from_str(json).map_err(|e| Error::new(e.to_string()))?;
    let list = data
        .get("vulnerabilities")
        .and_then(|v| v.get("list"))
        .and_then(Value::as_array);
    let mut out = Vec::new();
    for v in list.into_iter().flatten() {
        let ranges = |k: &str| -> Vec<String> {
            v.get("versions")
                .and_then(|r| r.get(k))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let name = v
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str);
        let id = v
            .get("advisory")
            .and_then(|a| a.get("id"))
            .and_then(Value::as_str);
        let (Some(name), Some(id)) = (name, id) else {
            return Err(Error::new(
                "cargo-audit: a vulnerability without package name or advisory id",
            ));
        };
        let mut fixed = ranges("patched");
        fixed.extend(ranges("unaffected"));
        out.push((name.to_owned(), id.to_owned(), fixed));
    }
    Ok(out)
}

/// RustSec vulnerabilities at BASE, by crate: (advisory id, fixed ranges).
pub type Vulnerable = BTreeMap<String, Vec<(String, Vec<String>)>>;

/// The advisory IDs that `name@version` fixes, if it fixes every one its crate has at BASE
/// (a range that cannot be read counts as not fixed).
pub fn security_fix(name: &str, version: &str, vulnerable: &Vulnerable) -> Option<Vec<String>> {
    let advisories = vulnerable.get(name).filter(|a| !a.is_empty())?;
    for (_, fixed) in advisories {
        let mut any = false;
        for r in fixed {
            match matches(version, r) {
                Ok(true) => {
                    any = true;
                    break;
                }
                Ok(false) => {}
                Err(_) => return None,
            }
        }
        if !any {
            return None;
        }
    }
    let ids: BTreeSet<&str> = advisories.iter().map(|(id, _)| id.as_str()).collect();
    Some(ids.into_iter().map(str::to_owned).collect())
}

/// An exemption from .config/cargo-cooldown.toml that has not expired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exemption {
    pub name: String,
    /// `None`: every version of the crate.
    pub version: Option<String>,
    pub reason: String,
}

/// The configuration: the cooldown in days, the exemptions in force, and notes about expired
/// ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub days: u64,
    pub exempt: Vec<Exemption>,
    pub notes: Vec<String>,
}

/// A TOML value as Python's `repr` writes what tomllib made of it, for the script's messages.
fn py_repr(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => {
            let quote = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::from(quote);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => if *b { "True" } else { "False" }.to_owned(),
        toml::Value::Datetime(d) => match (d.date, d.time) {
            (Some(date), None) => {
                format!("datetime.date({}, {}, {})", date.year, date.month, date.day)
            }
            _ => d.to_string(),
        },
        toml::Value::Array(a) => {
            format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", "))
        }
        toml::Value::Table(t) => format!(
            "{{{}}}",
            t.iter()
                .map(|(k, v)| format!(
                    "{}: {}",
                    py_repr(&toml::Value::String(k.clone())),
                    py_repr(v)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Reads the configuration; exemptions whose `until` is before `today` become notes.
pub fn load_config(text: &str, today: Date) -> Result<Config> {
    let cfg: toml::Table =
        toml::from_str(text).map_err(|e| Error::new(format!("{CONFIG}: {e}")))?;
    let days = cfg
        .get("days")
        .and_then(toml::Value::as_integer)
        .and_then(|d| u64::try_from(d).ok())
        .ok_or_else(|| Error::new(format!("{CONFIG}: `days` must be a whole number of days")))?;
    let mut exempt = Vec::new();
    let mut notes = Vec::new();
    let empty = Vec::new();
    let list = cfg
        .get("exempt")
        .and_then(toml::Value::as_array)
        .unwrap_or(&empty);
    for e in list {
        let s = |k: &str| {
            e.get(k)
                .and_then(toml::Value::as_str)
                .filter(|v| !v.is_empty())
        };
        let (Some(name), Some(reason)) = (s("name"), s("reason")) else {
            return Err(Error::new(format!(
                "{CONFIG}: every exemption needs a name and a reason: {}",
                py_repr(e)
            )));
        };
        // An empty version matches no version, as in the script (but names none in notes).
        let version = e.get("version").and_then(toml::Value::as_str);
        let until = match e.get("until") {
            None => None,
            Some(toml::Value::Datetime(d)) if d.time.is_none() && d.offset.is_none() => d
                .date
                .and_then(|d| Date::new(i32::from(d.year), u32::from(d.month), u32::from(d.day))),
            Some(_) => {
                return Err(Error::new(format!(
                    "{CONFIG}: `until` must be a date (YYYY-MM-DD): {}",
                    py_repr(e)
                )));
            }
        };
        if let Some(until) = until.filter(|u| *u < today) {
            let what = version
                .filter(|v| !v.is_empty())
                .map_or(name.to_owned(), |v| format!("{name} {v}"));
            notes.push(format!(
                "the exemption for {what} expired on {until}; remove it"
            ));
            continue;
        }
        exempt.push(Exemption {
            name: name.to_owned(),
            version: version.map(str::to_owned),
            reason: reason.to_owned(),
        });
    }
    Ok(Config {
        days,
        exempt,
        notes,
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
    now: Timestamp,
) -> Vec<Row> {
    let cutoff = now.minus_days(i64::try_from(config.days).unwrap_or(i64::MAX));
    new.iter()
        .map(|p| {
            let (name, version) = p;
            let when = published.get(p).copied().flatten();
            let rule = config
                .exempt
                .iter()
                .find(|e| &e.name == name && e.version.as_ref().is_none_or(|v| v == version));
            let fixes = security_fix(name, version, vulnerable);
            let (verdict, failed) = if when.is_some_and(|w| w <= cutoff) {
                ("ok".to_owned(), false)
            } else if let Some(rule) = rule {
                (format!("exempt: {}", rule.reason), false)
            } else if let Some(ids) = fixes {
                (format!("security fix ({})", ids.join(", ")), false)
            } else if when.is_some() {
                ("TOO NEW".to_owned(), true)
            } else {
                ("NOT FOUND on crates.io".to_owned(), true)
            };
            Row {
                name: name.clone(),
                version: version.clone(),
                published: when,
                verdict,
                failed,
            }
        })
        .collect()
}

/// The Markdown report, as the script wrote it.
pub fn render(days: u64, base: &str, rows: &[Row], notes: &[String], now: Timestamp) -> String {
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
        let list = match vulnerabilities(&out.stdout) {
            Ok(list) => list,
            Err(_) => {
                diagnostics.push_str(&out.stdout);
                diagnostics.push_str(&out.stderr);
                return Err(Error::new("cargo-audit produced no JSON"));
            }
        };
        for (name, id, fixed) in list {
            found.entry(name).or_default().push((id, fixed));
        }
    }
    Ok(Some(found))
}

/// When crates.io published `name@version`: the index's `pubtime`, else the API's
/// `created_at`, else `None`. Fails on any index answer but success or 404, and on a failed
/// request.
fn published(
    name: &str,
    version: &str,
    http: &dyn Http,
    index: &mut BTreeMap<String, BTreeMap<String, Option<String>>>,
) -> Result<Option<Timestamp>> {
    if !index.contains_key(name) {
        let url = format!("https://index.crates.io/{}", index_path(name));
        let r = http.get(&url)?;
        let entries = match r.status {
            404 => BTreeMap::new(),
            s if s >= 400 => return Err(Error::new(format!("{url}: HTTP {s}"))),
            _ => index_entries(&r.body)?,
        };
        index.insert(name.to_owned(), entries);
    }
    let stamp = index
        .get(name)
        .and_then(|e| e.get(version))
        .cloned()
        .flatten();
    let stamp = match stamp {
        Some(s) => s,
        None => {
            let r = http.get(&format!("https://crates.io/api/v1/crates/{name}/{version}"))?;
            if r.status >= 400 {
                return Ok(None);
            }
            match api_created_at(&r.body)? {
                Some(s) => s,
                None => return Ok(None),
            }
        }
    };
    parse_stamp(&stamp).map(Some)
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
    if !ports.git.is_commit(&args.base)? {
        return Err(Error::new(format!(
            "--base {}: not a commit in this repository (fetch it first?)",
            args.base
        )));
    }
    let mut base_texts = Vec::new();
    let mut new = BTreeSet::new();
    for path in &args.lockfiles {
        let base = ports.git.show(&args.base, path)?.unwrap_or_default();
        let head_file = repo.join(path);
        let head = if head_file.exists() {
            fs::read_to_string(&head_file).map_err(|e| Error::new(format!("{path}: {e}")))?
        } else {
            String::new()
        };
        let before = packages(&base)?;
        new.extend(packages(&head)?.difference(&before).cloned());
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
    for p in &new {
        times.insert(p.clone(), published(&p.0, &p.1, ports.http, &mut index)?);
    }
    let rows = verdicts(&new, &times, &config, &vulnerable, now);
    Ok(Outcome {
        report: render(config.days, &args.base, &rows, &config.notes, now),
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

    /// Every scenario of cooldown/cases.toml gives what scripts/check-cargo-cooldown.py
    /// printed for it (cooldown/golden; see the pull request that ported it). Where the
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
