//! Rules on the mise setup that the script never had (the review of the mise setup, items P3
//! and C3). Like the others they read text and change nothing.
//!
//! - Locks: every tool pinned in mise.toml or mise.ci.toml, in `[tools]` or by a task's
//!   `tools`, has a `[[tools.NAME]]` entry with that `version` in the lockfile beside it
//!   (mise.lock, mise.ci.lock). CI's strict installs check this only for the tools some job
//!   installs; fnox, uv and mise.lock's cosign are installed by no job.
//! - Layering: a workflow that uses setup-mise (directly or through a composite action) sets
//!   `MISE_ENV: ci`, so that mise.ci.toml applies; one that does not use it does not set it.
//! - `check`: every `mise run TASK` in ci.yml and security.yml is a step of mise.toml's `check`
//!   task, or is listed in [`NOT_IN_CHECK`] with the reason.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use toml::{Table, Value};

use super::extra::{finding, parse};
use super::{Finding, Snapshot, lines, re};

regex!(
    USES_ACTION,
    r"^\s*(?:-\s+)?uses:\s*(?:\$|\.)/\.github/actions/([A-Za-z0-9_.-]+)\s*(?:#.*)?$"
);
regex!(
    MISE_ENV_CI,
    r#"^\s*MISE_ENV:\s*(?:ci|"ci"|'ci')\s*(?:#.*)?$"#
);
regex!(
    MISE_RUN,
    r"(?:^|[\s;&|(`'\x22])mise\s+run\s+([A-Za-z0-9:_-]+)"
);

/// The tasks ci.yml and security.yml run that `mise run check` leaves out, and why. An entry
/// that neither file runs any more, or that `check` now runs, is a finding.
pub const NOT_IN_CHECK: &[(&str, &str)] = &[
    (
        "build",
        "the trunk build of the client; `check` leaves the build out",
    ),
    ("build:budget", "measures the build's dist/"),
    (
        "coverage",
        "needs cargo-llvm-cov, a CI-only tool (mise.ci.toml)",
    ),
    ("coverage:check", "reads the coverage job's report"),
    ("zizmor", "a CI-only tool (mise.ci.toml)"),
    ("audit", "cargo-audit, a CI-only tool (mise.ci.toml)"),
    ("vet", "cargo-vet, a CI-only tool (mise.ci.toml)"),
    ("scan:repo", "Trivy, a CI-only tool (mise.ci.toml)"),
    ("scan:image", "Trivy and a Docker image"),
    ("scan:crates", "Trivy and a Docker image"),
];

/// The versions a pin names: `"1.2"`, `{ version = "1.2", ... }`, or a list of those. `None`
/// when the shape is none of these.
fn versions(pin: &Value) -> Option<Vec<String>> {
    match pin {
        Value::String(v) => Some(vec![v.clone()]),
        Value::Table(t) => Some(vec![t.get("version")?.as_str()?.to_owned()]),
        Value::Array(a) => a
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

pub fn layering(s: &Snapshot) -> Vec<Finding> {
    let uses = |text: &str| -> BTreeSet<String> {
        lines(text)
            .filter_map(|l| USES_ACTION.captures(l).map(|c| c[1].to_owned()))
            .collect()
    };
    // The composite actions that set mise up: setup-mise, and any action that uses one of them.
    let actions: Vec<(String, BTreeSet<String>)> = s
        .workflows
        .iter()
        .filter_map(|(p, t)| {
            let name = p.strip_prefix(".github/actions/")?.split('/').next()?;
            Some((name.to_owned(), uses(t)))
        })
        .collect();
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
    let mut out = Vec::new();
    for (path, text) in &s.workflows {
        if !path.starts_with(".github/workflows/") {
            continue;
        }
        let code: Vec<&str> = lines(text)
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        let uses_mise = code.iter().any(|l| {
            USES_ACTION
                .captures(l)
                .is_some_and(|c| mise.contains(&c[1]))
        });
        let sets_ci = code.iter().any(|l| MISE_ENV_CI.is_match(l));
        match (uses_mise, sets_ci) {
            (true, false) => out.push(finding(format!(
                "{path} sets mise up but not MISE_ENV: ci, so mise.ci.toml would not apply"
            ))),
            (false, true) => out.push(finding(format!(
                "{path} sets MISE_ENV: ci but sets no mise up (setup-mise, or an action that \
                 uses it)"
            ))),
            _ => {}
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

pub fn check_covers_ci(s: &Snapshot, exceptions: &[(&str, &str)]) -> Vec<Finding> {
    let check = match check_tasks(&s.mise_toml) {
        Ok(c) => c,
        Err(f) => return vec![f],
    };
    let mut out = Vec::new();
    let mut run = BTreeSet::new();
    for path in [".github/workflows/ci.yml", ".github/workflows/security.yml"] {
        let Some((_, text)) = s.workflows.iter().find(|(p, _)| p == path) else {
            out.push(finding(format!("{path} is missing")));
            continue;
        };
        for line in lines(text).filter(|l| !l.trim_start().starts_with('#')) {
            // A trailing comment (` # ...`) runs nothing.
            let code = line.split(" #").next().unwrap_or(line);
            for c in MISE_RUN.captures_iter(code) {
                let task = c[1].to_owned();
                if !check.contains(&task)
                    && !exceptions.iter().any(|(t, _)| *t == task)
                    && run.insert(task.clone())
                {
                    out.push(finding(format!(
                        "{path} runs `mise run {task}`, which `mise run check` does not: add it to \
                         check, or to sync's NOT_IN_CHECK with the reason"
                    )));
                }
                run.insert(task);
            }
        }
    }
    for (task, _) in exceptions {
        if check.contains(*task) {
            out.push(finding(format!(
                "stale entry in sync's NOT_IN_CHECK: `mise run check` runs {task}; remove it"
            )));
        } else if !run.contains(*task) {
            out.push(finding(format!(
                "stale entry in sync's NOT_IN_CHECK: neither ci.yml nor security.yml runs \
                 `mise run {task}`; remove it"
            )));
        }
    }
    out
}

/// Every rule of this module, in order.
pub fn check(s: &Snapshot) -> Vec<Finding> {
    let mut out = locks(s);
    out.extend(layering(s));
    out.extend(check_covers_ci(s, NOT_IN_CHECK));
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
    const CI_YML: &str = "env:\n  MISE_ENV: ci\njobs:\n  a:\n    steps:\n\
        \x20     - uses: $/.github/actions/setup-ci\n      - run: mise run fmt\n\
        \x20     - run: |\n          mise run test && mise run coverage\n      - run: mise run build\n";
    const SECURITY_YML: &str = "env:\n  MISE_ENV: \"ci\"\njobs:\n  a:\n    steps:\n\
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
    /// newline, an empty FIND appends, `*` replaces the whole text), with [`NOT_IN_CHECK`]
    /// replaced by the base's two exceptions.
    fn rows(table: &str) {
        let exceptions = [("coverage", "CI only"), ("build", "a build")];
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
            workflows/ci.yml | MISE_ENV: ci | MISE_ENV: dev | ci.yml sets mise up but not MISE_ENV: ci
            workflows/ci.yml | MISE_ENV: ci | # MISE_ENV: ci | ci.yml sets mise up but not MISE_ENV: ci
            workflows/security.yml | setup-mise | setup-rust | security.yml sets MISE_ENV: ci but sets no mise up
            workflows/release-tag.yml | steps: [] | steps:\n      - uses: ./.github/actions/setup-ci | release-tag.yml sets mise up but not
            workflows/release-tag.yml | | env:\n  MISE_ENV: ci,dev\n | 
            actions/setup-ci/action.yml | setup-mise | setup-rust | ci.yml sets MISE_ENV: ci but sets no mise up
        "#,
        );
    }

    #[test]
    fn check_covers_what_ci_runs() {
        rows(
            r#"
            workflows/ci.yml | mise run fmt | mise run fmt\n      - run: mise run clippy-wasm --x\n      - run: mise run clippy-wasm | ci.yml runs `mise run clippy-wasm`, which `mise run check` does not
            workflows/security.yml | mise run sync | mise run audit | security.yml runs `mise run audit`
            workflows/security.yml | mise run sync | true # mise run audit later | 
            workflows/ci.yml | mise run coverage | true | neither ci.yml nor security.yml runs `mise run coverage`
            mise | { task = "test" }, | { task = "test" },\n  { task = "coverage" }, | `mise run check` runs coverage; remove it
            mise | { task = "test" }, | { run = "x" }, | check has a step sync cannot read
            mise | [tasks.check] | [tasks.checks] | mise.toml has no [tasks.check] run
            ci.yml | * | | ci.yml is missing && runs `mise run coverage`; remove it && runs `mise run build`; remove it
        "#,
        );
    }

    /// `run`'s rule set has this module's rules.
    #[test]
    fn all_runs_these_rules() {
        let mut s = base();
        s.mise_ci_lock.clear();
        let found: Vec<String> = super::super::all(&s)
            .findings
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            found.iter().any(|f| f.contains("pins trivy 0.76.1")),
            "{found:?}"
        );
    }

    /// The real exceptions are checked against the real tree by `the_repository_is_in_sync`;
    /// each names a reason.
    #[test]
    fn every_exception_has_a_reason() {
        assert!(
            NOT_IN_CHECK
                .iter()
                .all(|(t, why)| !t.is_empty() && !why.is_empty())
        );
    }
}
