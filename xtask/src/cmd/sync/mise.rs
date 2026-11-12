//! Rules on the mise setup that the script never had (the review of the mise setup, items P3
//! and C3). Like the others they read text and change nothing.
//!
//! - Locks: every tool pinned in mise.toml or mise.ci.toml, in `[tools]` or by a task's
//!   `tools`, has a `[[tools.NAME]]` entry with that `version` in the lockfile beside it
//!   (mise.lock, mise.ci.lock). CI's strict installs check this only for the tools some job
//!   installs; fnox, uv and mise.lock's cosign are installed by no job. Only the `version` is
//!   compared: not the platform checksums, not lock entries that nothing pins, and not
//!   mise.dev.toml's pins (`mise run lock` runs under `MISE_ENV=ci`).
//! - Layering: each job that uses setup-mise (directly or through a composite action) runs
//!   with `MISE_ENV: ci` from its own or the workflow's `env:`, so that mise.ci.toml applies;
//!   a job, step or workflow that sets `MISE_ENV: ci` sets mise up. Workflows are read as YAML.
//! - `check`: every job of a workflow that runs on `pull_request` is covered by mise.toml's
//!   `check` task (each of its `run:` steps is only `mise run TASK` of tasks `check` runs), or is
//!   listed in [`JOBS_NOT_IN_CHECK`] with the reason.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use toml::{Table, Value};
use yaml_serde::Value as YValue;

use super::extra::{entries, finding, parse, scalar, yaml};
use super::{Finding, Snapshot, re};

regex!(MISE_RUN, r"(?:^|[\s;&|(`'\x22])mise\s+run\s+");
regex!(TASK_NAME, r"^[A-Za-z0-9:_-]+$");

/// The options of `mise run` that take a value (the next word, unless written `--x=value`).
const MISE_RUN_VALUES: &[&str] = &[
    "--affected-base",
    "--affected-head",
    "-C",
    "--cd",
    "-j",
    "--jobs",
    "-o",
    "--output",
    "--secrets",
    "-s",
    "--shell",
    "-t",
    "--tool",
    "--allow-env",
    "--allow-net",
    "--allow-read",
    "--allow-write",
    "--task-cache",
    "--timeout",
    "-E",
    "--env",
];

/// The task each `mise run` in `code` names: the first word after `run` that is not an option
/// of mise's (or an option's value).
fn mise_run_tasks(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for m in MISE_RUN.find_iter(code) {
        let mut words = code[m.end()..].split_whitespace();
        while let Some(word) = words.next() {
            if word.starts_with('-') {
                if MISE_RUN_VALUES.contains(&word) {
                    words.next();
                }
                continue;
            }
            let word = word.trim_end_matches([';', ')', '&', '|']);
            if TASK_NAME.is_match(word) {
                out.push(word.to_owned());
            }
            break;
        }
    }
    out
}

/// The jobs of the `pull_request` workflows that `mise run check` does not cover, and why: a job
/// is covered when each of its `run:` steps is only `mise run TASK` of tasks `check` runs. An
/// entry whose job is gone, or is covered, is a finding.
pub const JOBS_NOT_IN_CHECK: &[(&str, &str, &str)] = &[
    (
        CI,
        "coverage",
        "line coverage with cargo-llvm-cov, a CI-only tool (mise.ci.toml); pushes only",
    ),
    (
        CI,
        "coverage-floor",
        "coverage:check needs cargo-llvm-cov, a CI-only tool (mise.ci.toml)",
    ),
    (
        CI,
        "wasm",
        "the trunk release build of the client, its dist/ and size budget; `check` leaves the \
         build out",
    ),
    (
        CI,
        "docker",
        "builds the images with Docker and smoke-tests them",
    ),
    (
        CI,
        "browser",
        "smoke-tests the built client in headless Chrome inside the app image (Docker, and the \
         wasm job's dist/)",
    ),
    (SECURITY, "zizmor", "zizmor, a CI-only tool (mise.ci.toml)"),
    (
        SECURITY,
        "cargo-audit",
        "cargo-audit, a CI-only tool (mise.ci.toml)",
    ),
    (
        SECURITY,
        "cargo-vet",
        "cargo-vet, a CI-only tool (mise.ci.toml)",
    ),
    (
        SECURITY,
        "cargo-cooldown",
        "judges the pull request's new crate versions with the base branch's check, against the \
         crates.io index",
    ),
    (
        SECURITY,
        "trivy",
        "Trivy, a CI-only tool (mise.ci.toml), on the repository and a Docker image",
    ),
    (
        ".github/workflows/fuzz.yml",
        "fuzz",
        "cargo-fuzz (mise.ci.toml) on fuzz/'s nightly toolchain",
    ),
    (
        ".github/workflows/fuzz.yml",
        "report",
        "files the nightly crash issue; scheduled runs only",
    ),
    (
        ".github/workflows/codeql.yml",
        "analyze",
        "CodeQL's analysis, which runs on GitHub",
    ),
    (
        ".github/workflows/branch-policy.yml",
        "policy",
        "checks the pull request's head and base branches, which exist only on GitHub",
    ),
    (
        ".github/workflows/image-full.yml",
        "image",
        "builds the full image with Docker and smoke-tests it",
    ),
];

const CI: &str = ".github/workflows/ci.yml";
const SECURITY: &str = ".github/workflows/security.yml";

/// The versions a pin names: `"1.2"`, `{ version = "1.2", ... }`, or a list of those. `None`
/// when the shape is none of these.
fn versions(pin: &Value) -> Option<Vec<String>> {
    match pin {
        Value::String(v) => Some(vec![v.clone()]),
        Value::Table(t) => Some(vec![t.get("version")?.as_str()?.to_owned()]),
        Value::Array(a) if !a.is_empty() => a
            .iter()
            .map(versions)
            .collect::<Option<Vec<_>>>()
            .map(|v| v.concat()),
        _ => None,
    }
}

/// Every (where, tool, versions) a mise config pins: its `[tools]`, then each task's `tools`.
fn pins(config: &Table) -> Vec<(String, String, Option<Vec<String>>)> {
    let mut out = Vec::new();
    let tables = |v: Option<&Value>| v.and_then(Value::as_table).cloned().unwrap_or_default();
    for (name, pin) in tables(config.get("tools")) {
        out.push(("[tools]".to_owned(), name, versions(&pin)));
    }
    for (task, def) in tables(config.get("tasks")) {
        for (name, pin) in tables(def.get("tools")) {
            out.push((format!("task {task}"), name, versions(&pin)));
        }
    }
    out
}

pub fn locks(s: &Snapshot) -> Vec<Finding> {
    let mut out = Vec::new();
    for (file, text, lockfile, lock) in [
        ("mise.toml", &s.mise_toml, "mise.lock", &s.mise_lock),
        (
            "mise.ci.toml",
            &s.mise_ci_toml,
            "mise.ci.lock",
            &s.mise_ci_lock,
        ),
    ] {
        let (config, lock) = match (parse(file, text), parse(lockfile, lock)) {
            (Ok(c), Ok(l)) => (c, l),
            (Err(f), _) | (_, Err(f)) => {
                out.push(f);
                continue;
            }
        };
        let locked = |name: &str, version: &str| {
            lock.get("tools")
                .and_then(|t| t.get(name))
                .and_then(Value::as_array)
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|e| e.get("version").and_then(Value::as_str) == Some(version))
                })
        };
        for (place, name, versions) in pins(&config) {
            let Some(versions) = versions else {
                out.push(finding(format!(
                    "{file}: {place} pins {name} in a form sync cannot read (a version, \
                     {{ version = ... }} or a list of those)"
                )));
                continue;
            };
            for v in versions.iter().filter(|v| !locked(&name, v)) {
                out.push(finding(format!(
                    "{file}: {place} pins {name} {v}, but {lockfile} has no [[tools.{name}]] with \
                     version = \"{v}\" (run `mise run lock`)"
                )));
            }
        }
    }
    out
}

/// The local composite action a step uses (`$/.github/actions/NAME` or `./.github/actions/NAME`).
fn local_action(step: &YValue) -> Option<String> {
    let uses = step.get("uses").and_then(scalar)?;
    let rest = uses
        .strip_prefix("$/.github/actions/")
        .or_else(|| uses.strip_prefix("./.github/actions/"))?;
    Some(rest.trim_end_matches('/').to_owned())
}

fn steps(def: &YValue) -> impl Iterator<Item = &YValue> {
    def.get("steps")
        .and_then(YValue::as_sequence)
        .into_iter()
        .flatten()
}

fn mise_env(def: &YValue) -> Option<String> {
    def.get("env")
        .and_then(|e| e.get("MISE_ENV"))
        .and_then(scalar)
}

pub fn layering(s: &Snapshot) -> Vec<Finding> {
    let mut out = Vec::new();
    // The composite actions that set mise up: setup-mise, and any action that uses one of them.
    let mut actions = Vec::new();
    for (path, text) in &s.workflows {
        let Some(name) = path
            .strip_prefix(".github/actions/")
            .and_then(|p| p.split('/').next())
        else {
            continue;
        };
        match yaml(path, text) {
            Ok(doc) => {
                let runs = doc.get("runs").cloned().unwrap_or(YValue::Null);
                let used: BTreeSet<String> = steps(&runs).filter_map(local_action).collect();
                actions.push((name.to_owned(), used));
            }
            Err(why) => out.push(finding(why)),
        }
    }
    let mut mise = BTreeSet::from(["setup-mise".to_owned()]);
    loop {
        let before = mise.len();
        for (name, used) in &actions {
            if used.iter().any(|u| mise.contains(u)) {
                mise.insert(name.clone());
            }
        }
        if mise.len() == before {
            break;
        }
    }
    for (path, text) in &s.workflows {
        if !path.starts_with(".github/workflows/") {
            continue;
        }
        let doc = match yaml(path, text) {
            Ok(d) => d,
            Err(why) => {
                out.push(finding(why));
                continue;
            }
        };
        let top = mise_env(&doc);
        let mut any_mise = false;
        for (job, def) in entries(&doc, "jobs") {
            let job = scalar(job).unwrap_or_default();
            let uses_mise = steps(def)
                .filter_map(local_action)
                .any(|a| mise.contains(&a));
            any_mise |= uses_mise;
            let own = mise_env(def);
            let step_ci = steps(def).any(|st| mise_env(st).as_deref() == Some("ci"));
            if uses_mise && own.as_ref().or(top.as_ref()).map(String::as_str) != Some("ci") {
                out.push(finding(format!(
                    "{path}: job {job} sets mise up but its env (or the workflow's) has no \
                     MISE_ENV: ci, so mise.ci.toml would not apply"
                )));
            } else if !uses_mise && (own.as_deref() == Some("ci") || step_ci) {
                out.push(finding(format!(
                    "{path}: job {job} sets MISE_ENV: ci but sets no mise up (setup-mise, or an \
                     action that uses it)"
                )));
            }
        }
        if top.as_deref() == Some("ci") && !any_mise {
            out.push(finding(format!(
                "{path} sets MISE_ENV: ci but no job of it sets mise up (setup-mise, or an action \
                 that uses it)"
            )));
        }
    }
    out
}

/// The tasks mise.toml's `check` runs (`run = [{ task = ... }, { tasks = [...] }]`).
fn check_tasks(mise_toml: &str) -> Result<BTreeSet<String>, Finding> {
    let config = parse("mise.toml", mise_toml)?;
    let steps = config
        .get("tasks")
        .and_then(|t| t.get("check"))
        .and_then(|c| c.get("run"))
        .and_then(Value::as_array)
        .ok_or_else(|| finding("mise.toml has no [tasks.check] run = [...]".into()))?;
    let mut out = BTreeSet::new();
    for step in steps {
        // A shell command step runs no task of the list.
        if step.is_str() {
            continue;
        }
        let names: Vec<&Value> = match (step.get("task"), step.get("tasks")) {
            (Some(one), None) => vec![one],
            (None, Some(Value::Array(many))) => many.iter().collect(),
            _ => vec![],
        };
        if names.is_empty() {
            return Err(finding(format!(
                "mise.toml's check has a step sync cannot read: {step}"
            )));
        }
        for n in names {
            let n = n
                .as_str()
                .ok_or_else(|| finding(format!("mise.toml's check names a task {n}")))?;
            out.insert(n.to_owned());
        }
    }
    Ok(out)
}

/// The events a workflow runs on (`on:` as a name, a list or a mapping).
fn triggers(doc: &YValue) -> Vec<String> {
    // YAML 1.1 reads a bare `on` key as true.
    let on = doc
        .as_mapping()
        .into_iter()
        .flatten()
        .find(|(k, _)| matches!(scalar(k).as_deref(), Some("on" | "true")))
        .map(|(_, v)| v);
    match on {
        Some(YValue::Sequence(list)) => list.iter().filter_map(scalar).collect(),
        Some(YValue::Mapping(m)) => m.keys().filter_map(scalar).collect(),
        Some(v) => scalar(v).into_iter().collect(),
        None => vec![],
    }
}

/// Why a job is not covered by `check`, one entry per step that is not `mise run` of a check
/// task (empty: covered).
fn uncovered(def: &YValue, check: &BTreeSet<String>) -> Vec<String> {
    let mut why = Vec::new();
    let mut runs = 0;
    for step in steps(def) {
        let Some(script) = step.get("run").and_then(scalar) else {
            continue;
        };
        runs += 1;
        for line in script.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let tasks = mise_run_tasks(line);
            let plain = line.starts_with("mise run ")
                && !line.contains([';', '&', '|', '`', '>', '<'])
                && !line.contains("$(");
            match tasks.first() {
                Some(task) if plain && !check.contains(task) => {
                    why.push(format!(
                        "runs `mise run {task}`, which `mise run check` does not"
                    ));
                }
                Some(_) if plain => {}
                _ => why.push(format!(
                    "has a step that is not `mise run` of a check task (`{line}`)"
                )),
            }
        }
    }
    if runs == 0 {
        why.push("has no `run:` step that `mise run check` covers".to_owned());
    }
    why
}

pub fn check_covers_ci(s: &Snapshot, exceptions: &[(&str, &str, &str)]) -> Vec<Finding> {
    let check = match check_tasks(&s.mise_toml) {
        Ok(c) => c,
        Err(f) => return vec![f],
    };
    let mut out = Vec::new();
    // (workflow, job, covered) of every job of a pull_request workflow.
    let mut jobs = Vec::new();
    for (path, text) in &s.workflows {
        if !path.starts_with(".github/workflows/") {
            continue;
        }
        let Ok(doc) = yaml(path, text) else {
            continue; // layering reports it
        };
        if !triggers(&doc).iter().any(|t| t == "pull_request") {
            continue;
        }
        for (job, def) in entries(&doc, "jobs") {
            let job = scalar(job).unwrap_or_default();
            let why = uncovered(def, &check);
            let listed = exceptions.iter().any(|(p, j, _)| p == path && *j == job);
            if !listed {
                for w in &why {
                    out.push(finding(format!(
                        "{path}: job {job} {w}: make it a task of check, or add the job to sync's \
                         JOBS_NOT_IN_CHECK with the reason"
                    )));
                }
            }
            jobs.push((path.clone(), job, why.is_empty()));
        }
    }
    for (path, job, _) in exceptions {
        match jobs.iter().find(|(p, j, _)| p == path && j == job) {
            None => out.push(finding(format!(
                "stale entry in sync's JOBS_NOT_IN_CHECK: {path} has no job {job} in a \
                 pull_request workflow; remove it"
            ))),
            Some((_, _, true)) => out.push(finding(format!(
                "stale entry in sync's JOBS_NOT_IN_CHECK: `mise run check` covers {path} job \
                 {job}; remove it"
            ))),
            Some(_) => {}
        }
    }
    out
}

/// Every rule of this module, in order.
pub fn check(s: &Snapshot) -> Vec<Finding> {
    let mut out = locks(s);
    out.extend(layering(s));
    out.extend(check_covers_ci(s, JOBS_NOT_IN_CHECK));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISE: &str = "[tools]\ntrunk = \"0.22.1\"\n\
        cargo-nextest = { version = \"0.9.150\", version_prefix = \"cargo-nextest-\" }\n\n\
        [tasks.up]\ntools.fnox = \"1.46.0\"\nrun = \"up\"\n\n\
        [tasks.fonts]\ntools = { uv = \"0.13.5\" }\nrun = \"fonts\"\n\n\
        [tasks.check]\nrun = [\n  { tasks = [\"fmt\", \"sync\"] },\n  { task = \"test\" },\n]\n";
    const LOCK: &str = "[[tools.cargo-nextest]]\nversion = \"0.9.150\"\nbackend = \"x\"\n\n\
        [[tools.fnox]]\nversion = \"1.46.0\"\n\n[[tools.trunk]]\nversion = \"0.22.1\"\n\n\
        [[tools.uv]]\nversion = \"0.13.5\"\n";
    const CI_YML: &str = "on: [push, pull_request]\nenv:\n  MISE_ENV: ci\njobs:\n  a:\n    steps:\n\
        \x20     - uses: $/.github/actions/setup-ci\n      - run: mise run fmt\n\
        \x20     - run: mise run test\n  b:\n    steps:\n      - run: |\n\
        \x20         mise run coverage && mise run build\n";
    const SECURITY_YML: &str = "on:\n  pull_request:\nenv:\n  MISE_ENV: \"ci\"\njobs:\n  a:\n    steps:\n\
        \x20     - uses: $/.github/actions/setup-mise\n      - run: mise run sync\n";

    fn base() -> Snapshot {
        let file = |p: &str, t: &str| (p.to_owned(), t.to_owned());
        Snapshot {
            rust_toolchain: String::new(),
            setup_mise: String::new(),
            mise_lock: LOCK.into(),
            manifests: vec![],
            dockerfile: String::new(),
            mise_toml: MISE.into(),
            mise_ci_toml: "[tools]\ntrivy = \"0.76.1\"\n".into(),
            mise_dev_toml: String::new(),
            workflows: vec![
                file(".github/workflows/ci.yml", CI_YML),
                file(
                    ".github/workflows/release-tag.yml",
                    "jobs:\n  a:\n    steps: []\n",
                ),
                file(".github/workflows/security.yml", SECURITY_YML),
                file(
                    ".github/actions/setup-ci/action.yml",
                    "runs:\n  steps:\n    - uses: $/.github/actions/setup-mise\n",
                ),
                file(
                    ".github/actions/setup-mise/action.yml",
                    "runs:\n  steps: []\n",
                ),
            ],
            deny_toml: String::new(),
            audit_toml: String::new(),
            osv_scanner_toml: String::new(),
            release_toml: String::new(),
            cooldown_toml: String::new(),
            dependabot: String::new(),
            bump_pins: String::new(),
            security_patch: String::new(),
            mise_ci_lock: "[[tools.trivy]]\nversion = \"0.76.1\"\n".into(),
            compose: String::new(),
            doc_pins: Ok(String::new()),
        }
    }

    fn field<'a>(s: &'a mut Snapshot, name: &str) -> &'a mut String {
        match name {
            "mise" => &mut s.mise_toml,
            "mise.ci" => &mut s.mise_ci_toml,
            "lock" => &mut s.mise_lock,
            "ci.lock" => &mut s.mise_ci_lock,
            _ => {
                let path = format!(".github/{name}");
                let found = s.workflows.iter_mut().find(|(p, _)| *p == path);
                found.map(|(_, t)| t).unwrap_or_else(|| panic!("{path}"))
            }
        }
    }

    /// Each row `FIELD | FIND | REPLACE | WANT && ...`, as in the tests of `extra` (`\n` is a
    /// newline, an empty FIND appends, `*` replaces the whole text), with [`JOBS_NOT_IN_CHECK`]
    /// replaced by the base's one exception (ci.yml's job b).
    fn rows(table: &str) {
        let exceptions = [(".github/workflows/ci.yml", "b", "CI only")];
        for row in table.lines().map(str::trim).filter(|r| !r.is_empty()) {
            let cols: Vec<String> = row
                .split('|')
                .map(|c| c.trim().replace("\\n", "\n"))
                .collect();
            let [name, find, replace, want] = &cols[..] else {
                panic!("row {row:?}")
            };
            let mut s = base();
            if name == "ci.yml" && find == "*" && replace.is_empty() {
                s.workflows.retain(|(p, _)| p != ".github/workflows/ci.yml");
            } else {
                let text = field(&mut s, name);
                if find.is_empty() {
                    text.push_str(replace);
                } else if find == "*" {
                    replace.clone_into(text);
                } else {
                    assert!(
                        text.contains(find.as_str()),
                        "{row}: {find:?} not in {text:?}"
                    );
                    *text = text.replacen(find.as_str(), replace, 1);
                }
            }
            let mut got = locks(&s);
            got.extend(layering(&s));
            got.extend(check_covers_ci(&s, &exceptions));
            let got: Vec<String> = got.iter().map(ToString::to_string).collect();
            let want: Vec<&str> = want.split(" && ").filter(|w| !w.is_empty()).collect();
            assert_eq!(got.len(), want.len(), "{row}: {got:#?}");
            for (g, w) in got.iter().zip(&want) {
                assert!(g.contains(w), "{row}: {g:?} lacks {w:?}");
            }
        }
    }

    #[test]
    fn locks_hold_every_pin() {
        rows(
            r#"
            mise | | | 
            mise | trunk = "0.22.1" | trunk = "0.22.2" | [tools] pins trunk 0.22.2, but mise.lock has no [[tools.trunk]] with version = "0.22.2"
            mise | tools.fnox = "1.46.0" | tools.fnox = "1.47.0" | task up pins fnox 1.47.0
            mise | { uv = "0.13.5" } | { uv = { version = "0.14.0" } } | task fonts pins uv 0.14.0
            mise | trunk = "0.22.1" | trunk = ["0.22.1", "0.23.0"] | pins trunk 0.23.0
            mise | trunk = "0.22.1" | trunk = 1 | pins trunk in a form sync cannot read
            mise | trunk = "0.22.1" | trunk = [] | pins trunk in a form sync cannot read
            mise.ci | trivy = "0.76.1" | trivy = "0.76.1"\ncosign = "3.1.4" | mise.ci.toml: [tools] pins cosign 3.1.4, but mise.ci.lock has no
            mise.ci | | [tasks.x]\ntools.zizmor = "1.31.1"\n | task x pins zizmor 1.31.1
            ci.lock | * | | mise.ci.toml: [tools] pins trivy 0.76.1
            lock | version = "0.13.5" | version = "0.13.6" | task fonts pins uv 0.13.5
            lock | [[tools.trunk]] | [[tools.trunk-x]] | pins trunk 0.22.1
            lock | * | [tools | mise.lock: not TOML
        "#,
        );
    }

    #[test]
    fn mise_env_ci_goes_with_setup_mise() {
        rows(
            r#"
            workflows/ci.yml | MISE_ENV: ci | MISE_ENV: dev | ci.yml: job a sets mise up but its env (or the workflow's) has no MISE_ENV: ci
            workflows/ci.yml | MISE_ENV: ci | # MISE_ENV: ci | ci.yml: job a sets mise up but
            workflows/security.yml | setup-mise | setup-rust | security.yml sets MISE_ENV: ci but no job of it sets mise up
            workflows/release-tag.yml | steps: [] | steps:\n      - uses: ./.github/actions/setup-ci | release-tag.yml: job a sets mise up but
            workflows/release-tag.yml | | env:\n  MISE_ENV: ci,dev\n | 
            actions/setup-ci/action.yml | setup-mise | setup-rust | ci.yml sets MISE_ENV: ci but no job of it sets mise up
            workflows/ci.yml | env:\n  MISE_ENV: ci\njobs:\n  a:\n    steps:\n | jobs:\n  a:\n    steps:\n      - env:\n          MISE_ENV: ci\n        run: mise run fmt\n | ci.yml: job a sets mise up but
            workflows/ci.yml | env:\n  MISE_ENV: ci\njobs:\n  a:\n | jobs:\n  a:\n    env:\n      MISE_ENV: dev\n | ci.yml: job a sets mise up but
            workflows/ci.yml | jobs:\n  a:\n | jobs:\n  a:\n    env:\n      MISE_ENV: dev\n | ci.yml: job a sets mise up but
            workflows/release-tag.yml | * | jobs:\n  a:\n    env:\n      MISE_ENV: ci\n    steps:\n      - uses: $/.github/actions/setup-mise\n  b:\n    steps:\n      - uses: ./.github/actions/setup-mise/\n | release-tag.yml: job b sets mise up but
            workflows/release-tag.yml | * | jobs:\n  a:\n    steps:\n      - uses: $/.github/actions/setup-mise\n      - run: "export MISE_ENV: ci"\n | release-tag.yml: job a sets mise up but
            workflows/release-tag.yml | * | jobs:\n  a:\n    steps:\n      - uses: "$/.github/actions/setup-mise"\n | release-tag.yml: job a sets mise up but
            workflows/release-tag.yml | * | jobs:\n  a:\n    env:\n      MISE_ENV: ci\n    steps:\n      - uses: "$/.github/actions/setup-mise"\n  b:\n    env:\n      MISE_ENV: "ci"\n    steps: []\n | release-tag.yml: job b sets MISE_ENV: ci but sets no mise up
            workflows/release-tag.yml | * | jobs:\n  b:\n    steps:\n      - env:\n          MISE_ENV: ci\n        run: "true"\n | release-tag.yml: job b sets MISE_ENV: ci but sets no mise up
            workflows/release-tag.yml | * | jobs: [ | release-tag.yml: not YAML
        "#,
        );
    }

    #[test]
    fn check_covers_what_ci_runs() {
        rows(
            r#"
            workflows/ci.yml | mise run fmt | mise run clippy-wasm --x\n      - run: mise run clippy-wasm | ci.yml: job a runs `mise run clippy-wasm`, which `mise run check` does not && job a runs `mise run clippy-wasm`
            workflows/security.yml | mise run sync | mise run audit | security.yml: job a runs `mise run audit`
            workflows/security.yml | mise run sync | true # mise run audit later | security.yml: job a has a step that is not `mise run` of a check task (`true`)
            workflows/ci.yml | mise run fmt | mise run x\n      - run: mise run y | job a runs `mise run x` && job a runs `mise run y`
            workflows/ci.yml | mise run fmt | mise run --output prefix -j 2 fmt | 
            workflows/ci.yml | mise run fmt | mise run -o prefix --force clippy-wasm | job a runs `mise run clippy-wasm`
            workflows/ci.yml | mise run fmt | mise run --output=prefix fmt; true | job a has a step that is not
            workflows/ci.yml | mise run fmt | mise -E ci run fmt | job a has a step that is not
            workflows/ci.yml | mise run fmt | echo mise run fmt | job a has a step that is not
            workflows/ci.yml | mise run fmt | mise run fmt $(cat args) | job a has a step that is not
            workflows/ci.yml | mise run fmt | echo "a #b"; mise run fmt | job a has a step that is not
            workflows/ci.yml | mise run coverage && mise run build | mise run fmt | `mise run check` covers .github/workflows/ci.yml job b; remove it
            workflows/ci.yml | b:\n    steps: | c:\n    steps: | ci.yml: job c has a step that is not && has no job b in a pull_request workflow; remove it
            workflows/ci.yml | jobs: | jobs:\n  c:\n    steps:\n      - uses: $/.github/actions/setup-ci\n | ci.yml: job c has no `run:` step that `mise run check` covers
            workflows/release-tag.yml | * | on: pull_request\njobs:\n  x:\n    steps:\n      - run: mise run fmt\n      - run: cargo xtask cooldown\n | release-tag.yml: job x has a step that is not `mise run` of a check task (`cargo xtask cooldown`)
            workflows/release-tag.yml | * | on:\n  pull_request:\n    paths: [x]\njobs:\n  x:\n    uses: ./.github/workflows/y.yml\n | release-tag.yml: job x has no `run:` step
            workflows/release-tag.yml | * | on: [push]\njobs:\n  x:\n    steps:\n      - run: cargo xtask cooldown\n | 
            mise | { task = "test" }, | { run = "x" }, | check has a step sync cannot read
            mise | { task = "test" }, | { task = "test" },\n  "echo done", | 
            mise | [tasks.check] | [tasks.checks] | mise.toml has no [tasks.check] run
            ci.yml | * | | has no job b in a pull_request workflow; remove it
        "#,
        );
    }

    /// Comment and empty lines of a script run nothing; any other line must be `mise run TASK`.
    #[test]
    fn scripts_are_read_line_by_line() {
        let check = BTreeSet::from(["fmt".to_owned(), "test".to_owned()]);
        let job = |script: &str| {
            let def: YValue = yaml_serde::from_str("steps: [{run: x}]").unwrap_or(YValue::Null);
            let mut def = def;
            if let Some(step) = def
                .get_mut("steps")
                .and_then(|s| s.get_mut(0))
                .and_then(|s| s.get_mut("run"))
            {
                *step = YValue::String(script.to_owned());
            }
            uncovered(&def, &check)
        };
        assert!(job("# first\n\nmise run fmt\nmise run test\n").is_empty());
        assert_eq!(job("mise run fmt\nmise run x\nls").len(), 2);
        assert_eq!(
            mise_run_tasks("mise run -C dir --timeout 5 test && mise run fmt"),
            ["test", "fmt"]
        );
    }

    /// `run`'s rule set has each rule of this module.
    #[test]
    fn all_runs_these_rules() {
        let mut s = base();
        s.mise_ci_lock.clear();
        let ci = field(&mut s, "workflows/ci.yml");
        *ci = ci.replacen("MISE_ENV: ci", "MISE_ENV: dev", 1).replacen(
            "mise run fmt",
            "mise run treemap",
            1,
        );
        let found: Vec<String> = super::super::all(&s)
            .findings
            .iter()
            .map(ToString::to_string)
            .collect();
        for want in [
            "pins trivy 0.76.1",
            "ci.yml: job a sets mise up but",
            "runs `mise run treemap`",
        ] {
            assert!(found.iter().any(|f| f.contains(want)), "{want}: {found:?}");
        }
    }

    /// The real exceptions are checked against the real tree by `the_repository_is_in_sync`;
    /// each names a reason.
    #[test]
    fn every_exception_has_a_reason() {
        assert!(
            JOBS_NOT_IN_CHECK
                .iter()
                .all(|(p, j, why)| p.ends_with(".yml") && !j.is_empty() && !why.is_empty())
        );
    }
}
