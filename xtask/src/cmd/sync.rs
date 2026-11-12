//! `cargo xtask sync`: pinned versions agree wherever they are repeated, nothing repeats what
//! is pinned once, and the advisory ignore lists agree. Ported rule by rule from
//! scripts/check-toolchain-sync.sh, matching what its grep, sed and awk lines matched,
//! quirks included (each is noted where it is kept). The rules in [`extra`] and [`mise`] are
//! newer: the script never had them.
//!
//! Sources of truth: rust-toolchain.toml for Rust (mise.lock records it for mise),
//! .github/actions/setup-mise for the mise that the workflows, the Dockerfile and mise.toml's
//! min_version use, and deny.toml for advisory ignores (mirrored in .cargo/audit.toml and
//! osv-scanner.toml). Every other tool is pinned once, in mise.toml or mise.ci.toml.
//!
//! Every finding is reported (on stderr), except that an unreadable pin or a failed docs
//! search stops the check there. With none, one summary line goes to stdout.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::ports::Git;
use crate::{Error, Result};

/// The files the check reads, as text. Paths are relative to the repository.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub rust_toolchain: String,
    /// .github/actions/setup-mise/action.yml
    pub setup_mise: String,
    pub mise_lock: String,
    /// Cargo.toml, crates/*/Cargo.toml, fuzz/Cargo.toml, xtask/Cargo.toml: those that exist.
    pub manifests: Vec<(String, String)>,
    pub dockerfile: String,
    pub mise_toml: String,
    pub mise_ci_toml: String,
    /// mise.dev.toml (empty when there is none).
    pub mise_dev_toml: String,
    /// .github/workflows/*.y*ml, then .github/actions/*/action.y*ml, each in path order.
    pub workflows: Vec<(String, String)>,
    pub deny_toml: String,
    pub audit_toml: String,
    pub osv_scanner_toml: String,
    /// .config/release.toml (empty when there is none).
    pub release_toml: String,
    /// The files only the rules in [`extra`] read, each empty when there is none:
    /// .config/cargo-cooldown.toml, .github/dependabot.yml, scripts/bump-pins.py,
    /// scripts/security-patch.py and compose.yaml.
    pub cooldown_toml: String,
    pub dependabot: String,
    pub bump_pins: String,
    pub security_patch: String,
    pub compose: String,
    /// mise.ci.lock (empty when there is none), which only [`mise`] reads.
    pub mise_ci_lock: String,
    /// What `git grep` found for [`doc_pattern`] in the Markdown files (empty: nothing), or why
    /// it could not search.
    pub doc_pins: Result<String, String>,
}

/// One thing the check found wrong, written to stderr as the script wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// The Rust or the CI mise version could not be read. Stops the check.
    Unreadable,
    Mismatch {
        label: String,
        actual: String,
        expected: String,
    },
    /// release.yml calls docker/build-push-action more than once.
    ReleaseBuilds(usize),
    /// mise.toml's tasks pin one tool at different versions.
    TaskPins { name: String, pins: String },
    /// `git grep` output lines naming a pinned version in the docs.
    DocPins(String),
    /// The docs could not be searched. Stops the check.
    DocSearch(String),
    /// A mise template reads a variable with `get_env` without falling back to mise's own
    /// `[env]` (`default=env.NAME`), so a value set in mise.local.toml would be ignored.
    EnvIgnoresLocal { file: String, line: String },
    /// An entry of [`GET_ENV_EXCEPTIONS`] that matches no such `get_env`.
    StaleEnvException {
        file: String,
        task: String,
        name: String,
    },
    /// hotfix.yml's inline `allowed=` list and .config/release.toml's `hotfix` list differ, or
    /// one of them cannot be read.
    HotfixList(String),
    /// A rule the script did not have (see [`extra`]), written as it says.
    Rule(String),
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable => writeln!(f, "could not read pinned versions"),
            Self::Mismatch {
                label,
                actual,
                expected,
            } => {
                let actual = if actual.is_empty() {
                    "<nothing>"
                } else {
                    actual
                };
                writeln!(f, "mismatch: {label} has '{actual}', expected '{expected}'")
            }
            Self::ReleaseBuilds(n) => writeln!(
                f,
                "mismatch: release.yml uses docker/build-push-action directly {n} times (only its \
                 push step may; build through $/.github/actions/build-image)"
            ),
            Self::TaskPins { name, pins } => {
                writeln!(
                    f,
                    "mismatch: mise.toml's tasks pin {name} at different versions: {pins}"
                )
            }
            Self::DocPins(lines) => {
                writeln!(
                    f,
                    "pinned versions repeated in the docs (name the file that pins them instead):"
                )?;
                // Line by line at LF only: a CR stays, as the script's `sed` kept it.
                lines.split('\n').try_for_each(|l| writeln!(f, "  {l}"))
            }
            Self::DocSearch(why) => {
                writeln!(f, "could not search the docs for pinned versions ({why})")
            }
            Self::EnvIgnoresLocal { file, line } => writeln!(
                f,
                "{file}: get_env without default=env.<the same name>, so the variable set under \
                 [env] in mise.local.toml would be ignored: {line}"
            ),
            Self::StaleEnvException { file, task, name } => writeln!(
                f,
                "stale exception in sync's GET_ENV_EXCEPTIONS: {file} task {task:?} no longer reads \
                 {name} with get_env without default=env.{name}; remove the entry"
            ),
            Self::HotfixList(why) => writeln!(f, "mismatch: {why}"),
            Self::Rule(text) => writeln!(f, "{text}"),
        }
    }
}

/// The check's result: the findings in order, and the summary printed when there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub summary: String,
}

fn re(pattern: &str) -> Regex {
    // Constant patterns, each compiled by the tests: the panic cannot happen.
    Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"))
}

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| re($pattern));
    };
}

regex!(CHANNEL, r#"^channel *= *"[^"]+""#);
regex!(LAST_QUOTED, r#".*"([^"]+)""#);
regex!(SETUP_MISE_VERSION, r"^ +version: *[0-9.]+$");
regex!(UP_TO_COLON_SPACES, r".*: *");
regex!(TOOLS_RUST, r"^\[\[tools\.rust\]\]");
regex!(VERSION_LINE, r"^version = ");
regex!(RUST_VERSION, r"^rust-version");
// The dots in ghcr.io are any character, as in the script.
regex!(DOCKER_MISE, r"^FROM ghcr.io/jdx/mise:[0-9.]+");
regex!(
    MIN_VERSION,
    r#"^min_version = \{ hard = "[0-9.]+", soft = "[0-9.]+" \}"#
);
regex!(LAST_QUOTED_VERSION, r#".*"([0-9.]+)".*"#);
regex!(
    GET_ENV,
    r#"^get_env\(\s*name\s*=\s*['"]([^'"]*)['"]\s*(?:,\s*default\s*=\s*env\.([A-Za-z0-9_]+))?"#
);
regex!(
    TASK_HEADER,
    r#"^\[tasks\.(?:"([^"]+)"|([A-Za-z0-9_:-]+))(?:\.[A-Za-z_]+)*\]"#
);
regex!(MISE_ACTION, r"uses: *jdx/mise-action@");
regex!(BUILD_PUSH, r"uses: *docker/build-push-action@");
regex!(TASK_TOOL, r#"^tools\.([A-Za-z0-9_-]+) *= *"([^"]+)".*"#);
regex!(TASK_TOOLS_TABLE, r"^tools *= *\{(.*)\}.*");
regex!(TASK_TOOLS_ENTRY, r#"^ *([A-Za-z0-9_-]+) *= *"([^"]+)" *$"#);
regex!(TOOLS_KEY, r"^[A-Za-z0-9_-]+ *=");
regex!(PIN_VERSION, r#"version *= *"[^"]+""#);
regex!(
    ADVISORY_ID,
    r"RUSTSEC-[0-9]{4}-[0-9]{4}|GHSA(-[0-9a-z]{4}){3}"
);
regex!(IGNORE_START, r"^ignore *= *\[");
regex!(IGNORE_END_ON_LINE, r"\][^\]]*$");
regex!(IGNORE_END, r"^ *\]");
regex!(COMMENT_LINE, r"^ *#");
regex!(OSV_ID, r"^id *=");
regex!(ALLOWED_LINE, r"^ *allowed='([^']*)'$");
// The word `allowed` as a shell name: what the rule must account for on every line.
regex!(ALLOWED_WORD, r"(^|[^A-Za-z0-9_-])allowed($|[^A-Za-z0-9_-])");
// The one way the job may use it: an exact-line `grep -vxE` whose pattern ends in the group.
regex!(ALLOWED_USE, r#"grep -vxE "[^"]*\(\$\{allowed\}\)" "#);
// One path of that list, with `\.` for each dot and no other pattern syntax.
regex!(ALLOWED_PATH, r"^(?:[A-Za-z0-9_/-]|\\\.)+$");
// The globs `*.y*ml` and `action.y*ml`.
regex!(WORKFLOW_NAME, r"^.*\.y.*ml$");
regex!(ACTION_NAME, r"^action\.y.*ml$");

pub mod extra;
pub mod mise;

/// A file's lines as grep, sed and awk read them: split at LF, the last one even without a
/// final newline. A CR stays part of its line.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .filter(move |_| !text.is_empty())
}

/// `grep -Eom1 RE | head -n1`: the first match in the first line that has one.
fn first<'a>(re: &Regex, text: &'a str) -> &'a str {
    lines(text)
        .find_map(|l| re.find(l))
        .map_or("", |m| m.as_str())
}

/// sed's `s/RE/\1/`: the first match replaced by its first group.
fn sed_group(re: &Regex, text: &str) -> String {
    re.replacen(text, 1, "$1").into_owned()
}

/// awk's default field splitting: runs of blanks and tabs.
fn fields(line: &str) -> Vec<&str> {
    line.split([' ', '\t']).filter(|f| !f.is_empty()).collect()
}

/// The pattern `git grep -IinoE` looks for in the Markdown files: a tool pinned in mise.toml's
/// or mise.ci.toml's [tools] or by a task, Rust, mise or Overpass, followed by a version, or a
/// `rust:X` image tag. A hyphen in a name also matches a space ("cargo nextest").
pub fn doc_pattern(mise_toml: &str, mise_ci_toml: &str) -> String {
    let mut names = BTreeSet::new();
    // One awk run over both files: whether the current section is [tools] carries over from
    // the end of mise.toml into mise.ci.toml (kept).
    let mut in_tools = false;
    for line in lines(mise_toml).chain(lines(mise_ci_toml)) {
        if line.starts_with('[') {
            in_tools = line == "[tools]";
        }
        if in_tools && TOOLS_KEY.is_match(line) {
            names.insert(fields(line)[0].to_owned());
        }
    }
    for pin in task_pins(mise_toml) {
        names.insert(pin.split(' ').next().unwrap_or_default().to_owned());
    }
    names.extend(["rust", "mise", "overpass"].map(String::from));
    names.remove("");
    let names: Vec<String> = names.iter().map(|n| n.replace('-', "[- ]")).collect();
    format!(
        "(^|[^A-Za-z0-9_-])({})`?( api)?[ @]v?[0-9]+\\.[0-9]+|rust:[0-9][^ `]*",
        names.join("|")
    )
}

/// "NAME VERSION" for each distinct task tool pin in mise.toml (`tools.NAME = "VERSION"` or
/// `tools = { NAME = "VERSION", ... }` at the start of a line), sorted.
fn task_pins(mise_toml: &str) -> BTreeSet<String> {
    let mut pins = BTreeSet::new();
    for line in lines(mise_toml) {
        if let Some(c) = TASK_TOOL.captures(line) {
            pins.insert(format!("{} {}", &c[1], &c[2]));
        }
    }
    for line in lines(mise_toml) {
        if let Some(c) = TASK_TOOLS_TABLE.captures(line) {
            for entry in c[1].split(',') {
                if let Some(e) = TASK_TOOLS_ENTRY.captures(entry) {
                    pins.insert(format!("{} {}", &e[1], &e[2]));
                }
            }
        }
    }
    pins
}

/// A file's [tools] pin of `name` (`NAME = "VERSION"` or `NAME = { version = "VERSION", ... }`):
/// the first quoted string of the `version = "..."` part, or of the line; empty if none.
fn tools_pin(text: &str, name: &str) -> String {
    let mut in_tools = false;
    for line in lines(text) {
        if line.starts_with('[') {
            in_tools = line == "[tools]";
        }
        let f = fields(line);
        if in_tools && f.first() == Some(&name) && f.get(1) == Some(&"=") {
            let mut s = PIN_VERSION.find(line).map_or(line, |m| m.as_str());
            if let Some(q) = s.find('"') {
                s = &s[q + 1..];
            }
            if let Some(q) = s.find('"') {
                s = &s[..q];
            }
            return s.to_owned();
        }
    }
    String::new()
}

/// What is inside a file's first `ignore = [ ... ]`, written on one line or over several
/// (closed by a line starting with `]`, so a `]` inside a reason string does not end it).
fn ignores(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open = false;
    for line in lines(text) {
        if IGNORE_START.is_match(line) {
            let rest = IGNORE_START.replace(line, "");
            if IGNORE_END_ON_LINE.is_match(&rest) {
                out.push(IGNORE_END_ON_LINE.replace(&rest, "").into_owned());
                return out;
            }
            open = true;
            out.push(rest.into_owned());
        } else if open && IGNORE_END.is_match(line) {
            return out;
        } else if open {
            out.push(line.to_owned());
        }
    }
    out
}

/// The advisory IDs in `lines`, sorted, distinct, space-separated.
fn ids<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    let found: BTreeSet<&str> = lines
        .into_iter()
        .flat_map(|l| ADVISORY_ID.find_iter(l).map(|m| m.as_str()))
        .collect();
    found.into_iter().collect::<Vec<_>>().join(" ")
}

/// The version mise.lock records for Rust: the first `version = ` line after [[tools.rust]],
/// wherever it is (kept: a [[tools.rust]] without one takes the next tool's), with everything
/// but digits and dots removed.
fn lock_rust(mise_lock: &str) -> String {
    let mut after = false;
    for line in lines(mise_lock) {
        if TOOLS_RUST.is_match(line) {
            after = true;
        } else if after && VERSION_LINE.is_match(line) {
            return line
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect();
        }
    }
    String::new()
}

/// The paths among `files` with a line `re` matches, except `skip`, space-separated.
fn files_matching(files: &[(String, String)], re: &Regex, skip: &[&str]) -> String {
    files
        .iter()
        .filter(|(path, text)| {
            !skip.contains(&path.as_str()) && lines(text).any(|l| re.is_match(l))
        })
        .map(|(path, _)| path.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every rule, in the script's order.
pub fn check(s: &Snapshot) -> Report {
    let mut findings = Vec::new();
    let rust = sed_group(&LAST_QUOTED, first(&CHANNEL, &s.rust_toolchain));
    let mise = UP_TO_COLON_SPACES
        .replacen(first(&SETUP_MISE_VERSION, &s.setup_mise), 1, "")
        .into_owned();
    if rust.is_empty() || mise.is_empty() {
        return Report {
            findings: vec![Finding::Unreadable],
            summary: String::new(),
        };
    }
    let check = |findings: &mut Vec<Finding>, expected: &str, actual: String, label: String| {
        if actual != expected {
            findings.push(Finding::Mismatch {
                label,
                actual,
                expected: expected.to_owned(),
            });
        }
    };

    check(
        &mut findings,
        &rust,
        lock_rust(&s.mise_lock),
        "mise.lock [[tools.rust]]".into(),
    );
    // No other copy of the Rust version.
    check(
        &mut findings,
        "",
        files_matching(&s.manifests, &RUST_VERSION, &[]),
        "manifests with rust-version (rust-toolchain.toml sets the version)".into(),
    );
    let docker = first(&DOCKER_MISE, &s.dockerfile);
    let docker = docker.rsplit_once(':').map_or(docker, |(_, v)| v);
    check(
        &mut findings,
        &mise,
        docker.to_owned(),
        "Dockerfile FROM ghcr.io/jdx/mise:".into(),
    );
    let min = sed_group(&LAST_QUOTED_VERSION, first(&MIN_VERSION, &s.mise_toml));
    check(&mut findings, &mise, min, "mise.toml min_version".into());
    // mise is set up only through setup-mise, and images are built only through build-image
    // (and release.yml's push step).
    check(
        &mut findings,
        "",
        files_matching(
            &s.workflows,
            &MISE_ACTION,
            &[".github/actions/setup-mise/action.yml"],
        ),
        "jdx/mise-action used directly (use $/.github/actions/setup-mise)".into(),
    );
    check(
        &mut findings,
        "",
        files_matching(
            &s.workflows,
            &BUILD_PUSH,
            &[
                ".github/actions/build-image/action.yml",
                ".github/workflows/release.yml",
            ],
        ),
        "docker/build-push-action used directly (use $/.github/actions/build-image)".into(),
    );
    let release = s
        .workflows
        .iter()
        .find(|(p, _)| p == ".github/workflows/release.yml");
    let release_builds = release.map_or(0, |(_, t)| {
        lines(t).filter(|l| BUILD_PUSH.is_match(l)).count()
    });
    if release_builds > 1 {
        findings.push(Finding::ReleaseBuilds(release_builds));
    }

    // Task tools: every task pins one version of a tool, and a [tools] pin of it agrees.
    let pins = task_pins(&s.mise_toml);
    let names: BTreeSet<&str> = pins.iter().filter_map(|p| p.split(' ').next()).collect();
    let mut synced = Vec::new();
    for name in names {
        // awk's `$1 == n { print $2 }`: the second word only (kept).
        let versions: Vec<&str> = pins
            .iter()
            .map(|p| fields(p))
            .filter(|f| f.first() == Some(&name))
            .filter_map(|f| f.get(1).copied())
            .collect();
        let joined = versions.join(" ");
        if joined.contains(' ') {
            findings.push(Finding::TaskPins {
                name: name.into(),
                pins: joined,
            });
            continue;
        }
        for (file, text) in [
            ("mise.toml", &s.mise_toml),
            ("mise.ci.toml", &s.mise_ci_toml),
        ] {
            let pinned = tools_pin(text, name);
            if !pinned.is_empty() && joined != pinned {
                findings.push(Finding::Mismatch {
                    label: format!("mise.toml tools.{name} ({file} [tools] has {pinned})"),
                    actual: joined.clone(),
                    expected: pinned,
                });
            }
        }
        synced.push(format!("{name} {joined}"));
    }

    // A variable read with get_env falls back to mise's [env], or mise.local.toml is ignored.
    findings.extend(env_ignores_local(
        &[
            ("mise.toml", &s.mise_toml),
            ("mise.dev.toml", &s.mise_dev_toml),
            ("mise.ci.toml", &s.mise_ci_toml),
        ],
        GET_ENV_EXCEPTIONS,
    ));

    // hotfix.yml's open-pr job keeps its own copy of .config/release.toml's hotfix list.
    findings.extend(hotfix_list(&s.workflows, &s.release_toml));

    // The docs never repeat a pinned version (CHANGELOG.md records history and is exempt).
    match &s.doc_pins {
        Err(why) => {
            findings.push(Finding::DocSearch(why.clone()));
            return Report {
                findings,
                summary: String::new(),
            };
        }
        Ok(out) if !out.trim_end_matches('\n').is_empty() => {
            findings.push(Finding::DocPins(out.trim_end_matches('\n').to_owned()));
        }
        Ok(_) => {}
    }

    // Advisory ignores: the same IDs in all three files.
    let deny_ignores = ignores(&s.deny_toml);
    let deny_ids = ids(deny_ignores
        .iter()
        .map(String::as_str)
        .filter(|l| !COMMENT_LINE.is_match(l)));
    let audit_ignores = ignores(&s.audit_toml);
    let audit_ids = ids(audit_ignores
        .iter()
        .map(|l| l.split('#').next().unwrap_or_default()));
    let osv_ids = ids(lines(&s.osv_scanner_toml).filter(|l| OSV_ID.is_match(l)));
    for (actual, label) in [
        (audit_ids, ".cargo/audit.toml ignore"),
        (osv_ids, "osv-scanner.toml IgnoredVulns"),
    ] {
        if actual != deny_ids {
            findings.push(Finding::Mismatch {
                label: label.into(),
                actual,
                expected: deny_ids.clone(),
            });
        }
    }

    let none = |s: String| if s.is_empty() { "none".to_owned() } else { s };
    let summary = format!(
        "versions in sync: rust {rust}, mise {mise}; task tools: {}; advisory ignores: {}",
        none(synced.join(", ")),
        none(deny_ids)
    );
    Report { findings, summary }
}

/// The `get_env` reads that may ignore mise.local.toml on purpose: (file, task, variable,
/// why). An entry that no longer matches such a read fails the check.
pub const GET_ENV_EXCEPTIONS: &[(&str, &str, &str, &str)] = &[(
    "mise.dev.toml",
    "dev:server",
    "OBS_ADMIN_TOKEN",
    "dev deliberately ignores mise.local.toml; only an exported variable replaces the dev token",
)];

/// Every `get_env(name='NAME', ...)` outside a comment line must read `default=env.NAME...`:
/// `get_env` sees only the environment mise started with, so without that fallback a value set
/// under `[env]` in mise.local.toml (where machine settings go) is silently ignored. The
/// fallback keeps the order: the shell, then mise's `[env]`, then the default. A read listed in
/// `exceptions` (by file, the task whose table it is in, and variable) is allowed; an exception
/// that matches no such read is a finding.
pub fn env_ignores_local(
    files: &[(&str, &str)],
    exceptions: &[(&str, &str, &str, &str)],
) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut used = vec![false; exceptions.len()];
    for (file, text) in files {
        let mut task = String::new();
        for line in lines(text).filter(|l| !l.trim_start().starts_with('#')) {
            if let Some(c) = TASK_HEADER.captures(line.trim_start()) {
                task = c
                    .get(1)
                    .or_else(|| c.get(2))
                    .map_or("", |m| m.as_str())
                    .to_owned();
            }
            let mut ignored = false;
            for (at, _) in line.match_indices("get_env(") {
                let caps = GET_ENV.captures(&line[at..]);
                let name = caps
                    .as_ref()
                    .map_or("?", |c| c.get(1).map_or("?", |m| m.as_str()));
                let good = caps
                    .as_ref()
                    .and_then(|c| c.get(2))
                    .is_some_and(|d| d.as_str() == name);
                if good {
                    continue;
                }
                match exceptions
                    .iter()
                    .position(|(f, t, n, _)| f == file && *t == task && *n == name)
                {
                    Some(i) => used[i] = true,
                    None => ignored = true,
                }
            }
            if ignored {
                out.push(Finding::EnvIgnoresLocal {
                    file: (*file).to_owned(),
                    line: line.trim().to_owned(),
                });
            }
        }
    }
    for ((file, task, name, _), used) in exceptions.iter().zip(used) {
        if !used {
            out.push(Finding::StaleEnvException {
                file: (*file).to_owned(),
                task: (*task).to_owned(),
                name: (*name).to_owned(),
            });
        }
    }
    out
}

/// hotfix.yml's open-pr job checks the patch with an inline regex (`allowed='A\.b|C'`), since
/// it compiles nothing; its set of paths must be .config/release.toml's `hotfix` list (order
/// aside). Nothing is checked in a tree with neither file. So that nothing else can widen it,
/// every line of hotfix.yml outside a comment that names `allowed` must be that one assignment
/// or a use in `grep -vxE "…(${allowed})"`.
pub fn hotfix_list(workflows: &[(String, String)], release_toml: &str) -> Vec<Finding> {
    const WORKFLOW: &str = ".github/workflows/hotfix.yml";
    let workflow = workflows.iter().find(|(p, _)| p == WORKFLOW);
    if workflow.is_none() && release_toml.is_empty() {
        return Vec::new();
    }
    let inline = workflow
        .ok_or_else(|| format!("{WORKFLOW} is missing"))
        .and_then(|(_, text)| {
            let named = lines(text)
                .filter(|l| !l.trim_start().starts_with('#'))
                .filter(|l| ALLOWED_WORD.is_match(l));
            let mut lists = Vec::new();
            for line in named {
                if let Some(list) = ALLOWED_LINE.captures(line).and_then(|c| c.get(1)) {
                    lists.push(list.as_str());
                } else if !ALLOWED_USE.is_match(line) {
                    return Err(format!(
                        "{WORKFLOW} names allowed other than in its one allowed='...' line and \
                         grep -vxE \"...(${{allowed}})\": {}",
                        line.trim()
                    ));
                }
            }
            let [list] = lists[..] else {
                return Err(format!(
                    "{WORKFLOW} has {} allowed='...' lines, expected one",
                    lists.len()
                ));
            };
            list.split('|')
                .map(|p| {
                    if ALLOWED_PATH.is_match(p) {
                        Ok(p.replace("\\.", "."))
                    } else {
                        Err(format!("{WORKFLOW}'s allowed list has {p:?}, not a path"))
                    }
                })
                .collect::<std::result::Result<BTreeSet<String>, String>>()
        });
    let listed = if release_toml.is_empty() {
        Err(format!("{} is missing", super::release::files::CONFIG))
    } else {
        super::release::files::Config::parse(release_toml, super::release::files::CONFIG)
            .map(|c| c.allowed_files.hotfix.into_iter().collect::<BTreeSet<_>>())
            .map_err(|e| e.to_string())
    };
    match (inline, listed) {
        (Err(why), _) | (_, Err(why)) => vec![Finding::HotfixList(why)],
        (Ok(inline), Ok(listed)) if inline != listed => {
            let show = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(" ");
            vec![Finding::HotfixList(format!(
                "{WORKFLOW}'s allowed list has '{}', expected '{}' ({}'s hotfix list)",
                show(&inline),
                show(&listed),
                super::release::files::CONFIG
            ))]
        }
        _ => Vec::new(),
    }
}

/// The check's output: what goes to stdout and stderr, and whether it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub failed: bool,
}

impl Report {
    pub fn outcome(&self) -> Outcome {
        let stderr: String = self.findings.iter().map(ToString::to_string).collect();
        let failed = !self.findings.is_empty();
        let stdout = if failed {
            String::new()
        } else {
            format!("{}\n", self.summary)
        };
        Outcome {
            stdout,
            stderr,
            failed,
        }
    }
}

fn read(repo: &Path, path: &str) -> Result<String> {
    fs::read_to_string(repo.join(path)).map_err(|e| Error::new(format!("sync: {path}: {e}")))
}

/// The names in `dir` (relative to `repo`) a shell glob's `*` would list: not hidden, sorted
/// by byte (the C locale's order), kept when `keep` says so.
fn glob(repo: &Path, dir: &str, keep: impl Fn(&str) -> bool) -> Result<Vec<String>> {
    let entries = match fs::read_dir(repo.join(dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::new(format!("sync: {dir}: {e}"))),
    };
    let mut names = Vec::new();
    for entry in entries {
        let name = entry
            .map_err(|e| Error::new(format!("sync: {dir}: {e}")))?
            .file_name();
        let name = name.to_string_lossy();
        if !name.starts_with('.') && keep(&name) {
            names.push(format!("{dir}/{name}"));
        }
    }
    names.sort();
    Ok(names)
}

fn files(repo: &Path, paths: Vec<String>) -> Result<Vec<(String, String)>> {
    paths
        .into_iter()
        .filter(|p| repo.join(p).is_file())
        .map(|p| Ok((p.clone(), read(repo, &p)?)))
        .collect()
}

/// The file read at `path` in `repo`, or empty when there is none.
fn optional(repo: &Path, path: &str) -> Result<String> {
    if repo.join(path).exists() {
        read(repo, path)
    } else {
        Ok(String::new())
    }
}

/// Reads the repository at `repo` and runs every rule: the script's ([`check`]), then the
/// newer ones ([`extra`], [`mise`]); `git` searches the docs.
pub fn run(repo: &Path, git: &dyn Git) -> Result<Outcome> {
    Ok(all(&snapshot(repo, git)?).outcome())
}

/// Every rule on `s`: the script's ([`check`]), then the newer ones ([`extra`], [`mise`]).
pub fn all(s: &Snapshot) -> Report {
    let mut report = check(s);
    report.findings.extend(extra::check(s));
    report.findings.extend(mise::check(s));
    // A file several rules read (a workflow that is not YAML) is reported once.
    let mut unique = Vec::new();
    for f in std::mem::take(&mut report.findings) {
        if !unique.contains(&f) {
            unique.push(f);
        }
    }
    report.findings = unique;
    report
}

/// Reads what the rules read from the repository at `repo`; `git` searches its docs.
pub fn snapshot(repo: &Path, git: &dyn Git) -> Result<Snapshot> {
    let mut manifests = vec!["Cargo.toml".to_owned()];
    // Each glob expands to whole paths sorted as the shell sorts them, so
    // crates/observation-client-model/Cargo.toml comes before crates/observation-client/...
    // ('-' sorts before '/').
    let mut crates: Vec<String> = glob(repo, "crates", |_| true)?
        .into_iter()
        .map(|d| format!("{d}/Cargo.toml"))
        .collect();
    crates.sort();
    manifests.extend(crates);
    manifests.extend(["fuzz/Cargo.toml".to_owned(), "xtask/Cargo.toml".to_owned()]);
    let mut workflows = glob(repo, ".github/workflows", |n| WORKFLOW_NAME.is_match(n))?;
    let mut actions = Vec::new();
    for dir in glob(repo, ".github/actions", |_| true)?
        .into_iter()
        .filter(|d| repo.join(d).is_dir())
    {
        actions.extend(glob(repo, &dir, |n| ACTION_NAME.is_match(n))?);
    }
    actions.sort();
    workflows.extend(actions);
    let mut s = Snapshot {
        rust_toolchain: read(repo, "rust-toolchain.toml")?,
        setup_mise: read(repo, ".github/actions/setup-mise/action.yml")?,
        mise_lock: read(repo, "mise.lock")?,
        manifests: files(repo, manifests)?,
        dockerfile: read(repo, "Dockerfile")?,
        mise_toml: read(repo, "mise.toml")?,
        mise_ci_toml: read(repo, "mise.ci.toml")?,
        mise_dev_toml: if repo.join("mise.dev.toml").exists() {
            read(repo, "mise.dev.toml")?
        } else {
            String::new()
        },
        workflows: files(repo, workflows)?,
        deny_toml: read(repo, "deny.toml")?,
        audit_toml: read(repo, ".cargo/audit.toml")?,
        osv_scanner_toml: read(repo, "osv-scanner.toml")?,
        release_toml: optional(repo, super::release::files::CONFIG)?,
        cooldown_toml: optional(repo, extra::COOLDOWN)?,
        dependabot: optional(repo, extra::DEPENDABOT)?,
        bump_pins: optional(repo, extra::BUMP_PINS)?,
        security_patch: optional(repo, extra::SECURITY_PATCH)?,
        compose: optional(repo, extra::COMPOSE)?,
        mise_ci_lock: optional(repo, "mise.ci.lock")?,
        doc_pins: Ok(String::new()),
    };
    let pattern = doc_pattern(&s.mise_toml, &s.mise_ci_toml);
    let flags = ["--untracked", "-I", "-i", "-n", "-o", "-E"];
    s.doc_pins = git
        .grep(&flags, &pattern, &["*.md", ":!CHANGELOG.md"])
        .map(Option::unwrap_or_default)
        .map_err(|e| e.to_string());
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::process::GitCli;
    use serde::Deserialize;
    use std::path::PathBuf;
    use std::process::Command;

    const ISOLATED: [(&str, &str); 3] = [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("LC_ALL", "C.UTF-8"),
    ];

    #[derive(Deserialize)]
    struct Cases {
        case: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        #[serde(default)]
        edit: Vec<Edit>,
    }

    #[derive(Deserialize)]
    struct Edit {
        file: String,
        find: Option<String>,
        replace: String,
    }

    fn ok<T, E: fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    fn manifest_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn copy_tree(from: &Path, to: &Path) {
        ok(fs::create_dir_all(to));
        for entry in ok(fs::read_dir(from)) {
            let entry = ok(entry);
            let target = to.join(entry.file_name());
            if ok(entry.file_type()).is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                ok(fs::copy(entry.path(), target));
            }
        }
    }

    /// One case as the golden writes it.
    fn render(case: &Case) -> String {
        let tmp = ok(tempfile::tempdir());
        let tree = tmp.path().join("repo");
        copy_tree(&manifest_dir().join("tests/fixtures/sync"), &tree);
        for e in &case.edit {
            let path = tree.join(&e.file);
            let mut text = ok(fs::read_to_string(&path));
            match &e.find {
                Some(find) => {
                    assert!(
                        text.contains(find.as_str()),
                        "{}: {find:?} not in {}",
                        case.name,
                        e.file
                    );
                    text = text.replacen(find.as_str(), &e.replace, 1);
                }
                None => text.push_str(&e.replace),
            }
            ok(fs::write(&path, text));
        }
        let init = Command::new("git")
            .args(["init", "-q"])
            .arg(&tree)
            .envs(ISOLATED)
            .status();
        assert!(init.is_ok_and(|s| s.success()));
        let mut git = GitCli::new(&tree);
        git.env = ISOLATED
            .iter()
            .map(|&(k, v)| (k.into(), v.into()))
            .collect();
        // The script's rules only: the golden is what the script printed.
        let out = check(&ok(snapshot(&tree, &git))).outcome();
        format!(
            "== {}\nexit {}\n-- stdout\n{}-- stderr\n{}",
            case.name,
            u8::from(out.failed),
            out.stdout,
            out.stderr
        )
    }

    /// Every case of sync-cases.toml gives what scripts/check-toolchain-sync.sh printed for it
    /// (sync.golden, captured as the pull request that ported it describes); OBS_BLESS=1
    /// rewrites the golden.
    #[test]
    fn each_rule_fires_as_the_script_did() {
        let fixtures = manifest_dir().join("tests/fixtures");
        let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(
            fixtures.join("sync-cases.toml"),
        ))));
        let rendered: Vec<String> = cases.case.iter().map(render).collect();
        let golden_path = fixtures.join("sync.golden");
        if std::env::var_os("OBS_BLESS").is_some() {
            ok(fs::write(&golden_path, rendered.concat()));
        }
        let golden = ok(fs::read_to_string(&golden_path));
        let mut want = golden
            .split("== ")
            .filter(|s| !s.is_empty())
            .map(|s| format!("== {s}"));
        for (case, got) in cases.case.iter().zip(&rendered) {
            assert_eq!(
                Some(got.as_str()),
                want.next().as_deref(),
                "case {:?}",
                case.name
            );
        }
        assert_eq!(
            want.next(),
            None,
            "the golden has more cases than sync-cases.toml"
        );
    }

    /// The pattern the script built for the fixture, printed by its own pipeline.
    #[test]
    fn doc_pattern_is_the_scripts() {
        let fixture = manifest_dir().join("tests/fixtures/sync");
        let read = |f: &str| ok(fs::read_to_string(fixture.join(f)));
        assert_eq!(
            doc_pattern(&read("mise.toml"), &read("mise.ci.toml")),
            "(^|[^A-Za-z0-9_-])(cargo[- ]audit|cargo[- ]nextest|cosign|fnox|mise|overpass|rust|trivy|\
             trunk|typos|uv)`?( api)?[ @]v?[0-9]+\\.[0-9]+|rust:[0-9][^ `]*"
        );
    }

    #[test]
    fn the_repository_is_in_sync() {
        let repo = manifest_dir().join("..");
        // The docs rule needs a git checkout (a worktree's .git is a file).
        if !repo.join(".git").exists() {
            eprintln!("skipped: {} is not a git checkout", repo.display());
            return;
        }
        let out = ok(run(&repo, &GitCli::new(&repo)));
        assert_eq!((out.stderr.as_str(), out.failed), ("", false));
    }

    /// Templates of the forms the mise files used before and after the fix for P1 (a version
    /// pinned in mise.local.toml was ignored by `mise run up`).
    #[test]
    fn get_env_must_fall_back_to_mise_env() {
        let before = "[tasks.up.env]\nOBS_VERSION = \"{{ get_env(name='OBS_VERSION', default='latest') }}\"\n";
        let after = "[tasks.up.env]\nOBS_VERSION = \"{{ get_env(name='OBS_VERSION', default=env.OBS_VERSION | default(value='latest')) }}\"\n";
        let dev_before =
            "env = { A = \"x\", T = \"{{ get_env(name='T', default=vars.dev_token) }}\" }\n";
        let dev_after = "env = { T = \"{{ get_env(name='T', default=env.T | default(value=vars.dev_token)) }}\" }\n";
        let other_name = "X = \"{{ get_env(name='X', default=env.Y) }}\"\n";
        let no_default = "X = \"{{ get_env(name='X') }}\"\n";
        let two_calls = "X = \"{{ get_env(name='X', default=env.X) }}{{ get_env(name='Y') }}\"\n";
        let comment = "# get_env(name='X') sees only the starting environment\n";
        for good in [after, dev_after, comment] {
            assert_eq!(env_ignores_local(&[("mise.toml", good)], &[]), [], "{good}");
        }
        for bad in [before, dev_before, other_name, no_default, two_calls] {
            let found = env_ignores_local(&[("mise.dev.toml", bad)], &[]);
            assert_eq!(found.len(), 1, "{bad}");
            assert!(
                found[0]
                    .to_string()
                    .starts_with("mise.dev.toml: get_env without"),
                "{bad}"
            );
        }
    }

    /// An exception allows one read in one task; the same read in another task, or an
    /// exception with nothing to allow, is a finding.
    #[test]
    fn get_env_exceptions_are_exact_and_never_stale() {
        let dev =
            "[tasks.\"dev:server\"]\nenv = { T = \"{{ get_env(name='T', default=vars.t) }}\" }\n";
        let other = "[tasks.serve]\nenv = { T = \"{{ get_env(name='T', default=vars.t) }}\" }\n";
        let fixed =
            "[tasks.\"dev:server\"]\nenv = { T = \"{{ get_env(name='T', default=env.T) }}\" }\n";
        let exception = [("mise.dev.toml", "dev:server", "T", "why")];
        assert_eq!(env_ignores_local(&[("mise.dev.toml", dev)], &exception), []);
        let found = env_ignores_local(&[("mise.dev.toml", other)], &exception);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(matches!(found[0], Finding::EnvIgnoresLocal { .. }));
        assert!(matches!(found[1], Finding::StaleEnvException { .. }));
        let found = env_ignores_local(&[("mise.dev.toml", fixed)], &exception);
        assert_eq!(
            found.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "stale exception in sync's GET_ENV_EXCEPTIONS: mise.dev.toml task \"dev:server\" no \
              longer reads T with get_env without default=env.T; remove the entry\n"
            ]
        );
        // A table under a task (`[tasks.up.env]`) belongs to that task.
        let up = "[tasks.up.env]\nV = \"{{ get_env(name='V') }}\"\n";
        assert_eq!(
            env_ignores_local(&[("mise.toml", up)], &[("mise.toml", "up", "V", "why")]),
            []
        );
    }

    #[test]
    fn quirks_kept() {
        // [[tools.rust]] without a version takes the next `version = ` line, wherever it is.
        let lock =
            "[[tools.rust]]\nbackend = \"core:rust\"\n\n[[tools.trunk]]\nversion = \"v0.22.1-x\"\n";
        assert_eq!(lock_rust(lock), "0.22.1");
        // A [tools] section at the end of mise.toml carries over into mise.ci.toml.
        let pattern = doc_pattern("[tools]\na = \"1\"\n", "b = \"2\"\n[env]\nc = \"3\"\n");
        assert!(pattern.contains("(a|b|mise|overpass|rust)"), "{pattern}");
        // awk's fields: the version is the first quoted string of `version = "..."`.
        let ci = "[tools]\nx = { version_prefix = \"v\", version = \"2\" }\ny = 3\n";
        assert_eq!(tools_pin(ci, "x"), "2");
        assert_eq!(tools_pin(ci, "y"), "y = 3");
        assert_eq!(ignores("ignore = [\"A\"] # x ]y\n"), ["\"A\"] # x "]);
    }

    #[test]
    fn hotfix_list_matches_the_inline_regex() {
        let wf = |line: &str| {
            vec![(
                ".github/workflows/hotfix.yml".to_owned(),
                format!("jobs:\n  open-pr:\n    steps:\n      - run: |\n          {line}\n"),
            )]
        };
        let toml = "[allowed-files]\nhotfix = [\"Cargo.toml\", \"fuzz/Cargo.lock\", \"Dockerfile\"]\nprerelease = [\"Cargo.toml\"]\n";
        let check = |w: &[(String, String)], t: &str| {
            hotfix_list(w, t)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };
        let good = wf(r"allowed='Cargo\.toml|fuzz/Cargo\.lock|Dockerfile'");
        assert_eq!(check(&good, toml), Vec::<String>::new());
        // The order of the alternation does not matter.
        let reordered = wf(r"allowed='Dockerfile|Cargo\.toml|fuzz/Cargo\.lock'");
        assert_eq!(check(&reordered, toml), Vec::<String>::new());
        // Neither file: nothing to compare.
        assert_eq!(check(&[], ""), Vec::<String>::new());
        assert_eq!(
            check(&wf(r"allowed='Cargo\.toml|Dockerfile'"), toml),
            [
                "mismatch: .github/workflows/hotfix.yml's allowed list has 'Cargo.toml Dockerfile', \
              expected 'Cargo.toml Dockerfile fuzz/Cargo.lock' (.config/release.toml's hotfix \
              list)\n"
            ]
        );
        for (w, t, want) in [
            (good.clone(), "", ".config/release.toml is missing"),
            (vec![], toml, ".github/workflows/hotfix.yml is missing"),
            (wf("x=1"), toml, "has 0 allowed='...' lines"),
            (
                wf(r"allowed='Cargo.toml|fuzz/Cargo\.lock|Dockerfile'"),
                toml,
                "\"Cargo.toml\", not a path",
            ),
            (
                wf(r"allowed='Cargo\.toml|fuzz/Cargo\.lock|Docker.*'"),
                toml,
                "not a path",
            ),
            (
                wf(r"allowed='Cargo\.toml||Dockerfile'"),
                toml,
                "\"\", not a path",
            ),
            (
                good.clone(),
                "[allowed-files]\nhotfix = []\nprerelease = [\"a\"]\n",
                "hotfix is empty",
            ),
        ] {
            let found = check(&w, t);
            assert!(
                found.len() == 1 && found[0].contains(want),
                "{want}: {found:?}"
            );
        }
        // Nothing else may assign or use it; an inline list larger than the toml list fails.
        for extra in [
            "allowed+='|.*'",
            "allowed=\".*\"",
            "allowed='.*'",
            "export allowed",
            "read -r allowed <<<x",
            "bad=\"$(grep -vE \"(${allowed})\" <<<x)\"",
            "bad=\"$(grep -vxE \"${allowed}\" <<<x)\"",
        ] {
            let mut w = good.clone();
            w[0].1.push_str(&format!("          {extra}\n"));
            let found = check(&w, toml);
            assert!(found.len() == 1, "{extra}: {found:?}");
        }
        let mut used = good.clone();
        used[0].1.push_str(
            "          bad=\"$(grep -vxE \"M\t(${allowed})\" <<<x || true)\"\n          # allowed+='|.*' in a comment\n",
        );
        assert_eq!(check(&used, toml), Vec::<String>::new());
        assert_eq!(
            check(
                &wf(r"allowed='Cargo\.toml|fuzz/Cargo\.lock|Dockerfile|README\.md'"),
                toml
            )
            .len(),
            1
        );
        let twice = vec![(
            ".github/workflows/hotfix.yml".to_owned(),
            "  allowed='Cargo\\.toml'\n  allowed='Cargo\\.toml'\n".to_owned(),
        )];
        assert!(check(&twice, toml)[0].contains("has 2 allowed='...' lines"));
    }
}
