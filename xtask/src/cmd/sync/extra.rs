//! Rules the script never had: names and numbers that several files repeat by hand agree.
//! Like the script's rules they read text and change nothing; each finding names the file and
//! the copy that differs.
//!
//! - Packages: every package that cargo is told to build, test or leave out (`-p`, `--package`,
//!   `--exclude`) in the mise files, the Dockerfile and the workflows is a crate of the
//!   workspace; mise.toml's `server_package` is the one the Dockerfile's `server-build` stage
//!   builds; every `crates/NAME` path a workflow names is a crate.
//! - Images: release.yml's image matrix, prune-images.yml's package matrix, the release notes'
//!   image loop, and the image tables of scripts/security-patch.py and `security-patch check`
//!   name the same images, at the same registry path and Dockerfile stages. Every `ghcr.io/`
//!   reference in release.yml, compose.yaml and mise.toml is a released image (or the release
//!   prefix and a variable); a compose.yaml service that builds names its image's release stage
//!   as its target; every `build-image` target is a stage; a layer cache release.yml reads is
//!   written somewhere. Stage names are compared exactly, as the Dockerfile spells them.
//! - Cooldown: .config/cargo-cooldown.toml's `days` is every Dependabot `*-days` cooldown (and
//!   every update has a `default-days`, but those in `NO_COOLDOWN`), mise's
//!   `minimum_release_age` and scripts/bump-pins.py's `COOLDOWN_DAYS`.
//! - `testing`: a crate's `testing` feature is enabled only from `[dev-dependencies]` (fuzz/ is
//!   outside the workspace and may enable it).
//!
//! YAML files are read with a YAML parser, so quoting, comments and flow style do not matter.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use toml::{Table, Value};
use yaml_serde::Value as YValue;

use super::{Finding, Snapshot, lines, re};
use crate::cmd::security_patch::IMAGES;

regex!(
    CARGO_COMMAND,
    r#"(?:^|[\s;&|(`'"\[])cargo(?:\s+\+\S+)?\s+(?:build|check|clippy|test|nextest|run|doc|llvm-cov|chef|auditable|bench|fmt|rustc)(?:\s|$)"#
);
// The separator is optional only for `-p` (`-pNAME`), so that `--exclude-from-report` names
// nothing.
regex!(
    PACKAGE_FLAG,
    r#"(?:^|[\s"'])(?:(-p)(?:\s+|=)?|(--package|--exclude)(?:\s+|=))["']?([^\s"';\\]+)"#
);
// A command on xtask's or fuzz's workspace, whose packages are not the root's.
regex!(
    OTHER_WORKSPACE,
    r"--manifest-path[ =](?:\./)?(?:xtask|fuzz)/|\{\{\s*vars\.xtask_manifest\s*\}\}"
);
regex!(STAGE, r"(?i)^\s*FROM\s.*\sAS\s+([A-Za-z0-9_.-]+)\s*$");
regex!(FROM, r"(?i)^\s*FROM\s");
// `crates/NAME` at the start of a path or after a `/` (`./` allowed); `packages` leaves out
// those inside a URL.
regex!(
    CRATE_PATH,
    r"(?:^|[\s\x22'!\[,/])(?:\./)?crates/([A-Za-z0-9_-]+)"
);
regex!(NOTES_LOOP, r"^\s*for name in ([^;]*); do\s*$");
// An image reference `ghcr.io/OWNER/NAME`, or one whose name is a variable (`$x`, printf's `%s`).
regex!(
    GHCR,
    r"ghcr\.io/((?:[A-Za-z0-9_.-]+/)*)([A-Za-z0-9_.-]*)([$%]?)"
);
regex!(SCRIPT_IMAGES_START, r"^IMAGES = \{\s*$");
regex!(
    SCRIPT_IMAGE,
    r#"^\s+"([^"]+)": \("([^"]+)", "([^"]+)", (True|False)\),\s*$"#
);
regex!(BUMP_PINS_DAYS, r"^COOLDOWN_DAYS = (\S+?)\s*(?:#.*)?$");

/// The Dependabot updates that have no cooldown on purpose, as (ecosystem, directories, why).
/// Every other update must have a `default-days`; an entry here that names no update, or one
/// that has a cooldown, is a finding.
pub const NO_COOLDOWN: &[(&str, &str, &str)] = &[
    (
        "docker",
        "/",
        "base images get no Dependabot security updates, so their daily digest refreshes are \
         how Debian fixes arrive",
    ),
    (
        "rust-toolchain",
        "/fuzz",
        "the fuzz targets' dated nightly, which builds only the fuzz runs, nothing that ships",
    ),
];

pub const RELEASE: &str = ".github/workflows/release.yml";
pub const PRUNE: &str = ".github/workflows/prune-images.yml";
pub const COOLDOWN: &str = ".config/cargo-cooldown.toml";
pub const DEPENDABOT: &str = ".github/dependabot.yml";
pub const COMPOSE: &str = "compose.yaml";
pub const BUMP_PINS: &str = "scripts/bump-pins.py";
pub const SECURITY_PATCH: &str = "scripts/security-patch.py";

pub(super) fn finding(text: String) -> Finding {
    Finding::Rule(text)
}

/// Lines with each `\`-continued line joined to the next (as a shell or Dockerfile reads a
/// command), a final CR removed, comment lines left out (inside a continued command too, as
/// Docker does), and an empty line inside a continued command skipped.
fn commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in lines(text) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim_start().starts_with('#') || (!cur.is_empty() && line.trim().is_empty()) {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(head) => {
                cur.push_str(head);
                cur.push(' ');
            }
            None => {
                cur.push_str(line);
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The packages named by `-p`, `--package` and `--exclude` in `command` (a template or a
/// variable, which the rule cannot resolve, is left out).
fn package_flags(command: &str) -> Vec<(String, String)> {
    PACKAGE_FLAG
        .captures_iter(command)
        .filter(|c| !c[3].contains(['{', '$']))
        .filter_map(|c| Some((c.get(1).or(c.get(2))?.as_str().to_owned(), c[3].to_owned())))
        .collect()
}

/// The packages cargo is given in `text`: in commands that run a cargo subcommand which takes
/// workspace packages, and, in a mise file, anywhere in `[vars]` (the tasks splice those in).
fn cargo_packages(text: &str, mise: bool) -> Vec<(String, String)> {
    let mut vars = false;
    let mut out = Vec::new();
    for command in commands(text) {
        if mise && command.starts_with('[') {
            vars = command.trim_end() == "[vars]";
        }
        if mise && vars {
            out.extend(package_flags(&command));
            continue;
        }
        // Each simple command on its own, so that `mkdir -p DIR; cargo build` names no DIR. A
        // command on another workspace (`--manifest-path`) names that workspace's packages.
        for simple in command.split([';', '&', '|']) {
            if CARGO_COMMAND.is_match(simple) && !OTHER_WORKSPACE.is_match(simple) {
                out.extend(package_flags(simple));
            }
        }
    }
    out
}

pub(super) fn parse(path: &str, text: &str) -> Result<Table, Finding> {
    toml::from_str(text).map_err(|e| finding(format!("{path}: not TOML: {}", e.message())))
}

/// The workspace's crates: the package names of crates/*/Cargo.toml, which is what the root
/// manifest's `members = ["crates/*"]` makes members.
fn members(s: &Snapshot) -> Result<BTreeSet<String>, Finding> {
    let root = s
        .manifests
        .iter()
        .find(|(p, _)| p == "Cargo.toml")
        .ok_or_else(|| finding("Cargo.toml is missing".into()))?;
    let workspace = parse("Cargo.toml", &root.1)?.get("workspace").cloned();
    let members = workspace.as_ref().and_then(|w| w.get("members"));
    let excluded = workspace
        .as_ref()
        .and_then(|w| w.get("exclude"))
        .and_then(Value::as_array)
        .is_some_and(|e| {
            e.iter().any(|x| {
                x.as_str()
                    .is_some_and(|x| x.trim_start_matches("./").starts_with("crates/"))
            })
        });
    if excluded || members != Some(&Value::Array(vec![Value::String("crates/*".into())])) {
        return Err(finding(
            "Cargo.toml's workspace members are not [\"crates/*\"] (or it excludes a path under \
             crates/), which sync's package rule reads as the crates under crates/; update the \
             rule"
                .into(),
        ));
    }
    let mut names = BTreeSet::new();
    for (path, text) in &s.manifests {
        if path.starts_with("crates/") {
            let table = parse(path, text)?;
            let name = table
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .ok_or_else(|| finding(format!("{path}: no [package] name")))?;
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

/// Dockerfile stage names, in order.
fn stages(dockerfile: &str) -> Vec<String> {
    commands(dockerfile)
        .iter()
        .filter_map(|c| STAGE.captures(c).map(|m| m[1].to_owned()))
        .collect()
}

/// mise.toml's `[vars] server_package`.
fn server_package(mise_toml: &str) -> Result<String, Finding> {
    parse("mise.toml", mise_toml)?
        .get("vars")
        .and_then(|v| v.get("server_package"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| finding("mise.toml has no [vars] server_package".into()))
}

pub fn packages(s: &Snapshot) -> Vec<Finding> {
    let members = match members(s) {
        Ok(m) => m,
        Err(f) => return vec![f],
    };
    let mut out = Vec::new();
    let mut named = Vec::new();
    for (file, text) in [
        ("mise.toml", &s.mise_toml),
        ("mise.ci.toml", &s.mise_ci_toml),
        ("mise.dev.toml", &s.mise_dev_toml),
    ] {
        named.extend(cargo_packages(text, true).into_iter().map(|p| (file, p)));
    }
    named.extend(
        cargo_packages(&s.dockerfile, false)
            .into_iter()
            .map(|p| ("Dockerfile", p)),
    );
    for (file, text) in &s.workflows {
        named.extend(
            cargo_packages(text, false)
                .into_iter()
                .map(|p| (file.as_str(), p)),
        );
    }
    for (file, (flag, name)) in named {
        if !members.contains(&name) {
            out.push(finding(format!(
                "{file}: cargo {flag} {name}: not a crate of the workspace (crates/*)"
            )));
        }
    }

    match server_package(&s.mise_toml) {
        Err(f) => out.push(f),
        Ok(server) => {
            if !members.contains(&server) {
                out.push(finding(format!(
                    "mise.toml [vars] server_package is '{server}', not a crate of the workspace"
                )));
            }
            let mut stage = None;
            let mut built = BTreeSet::new();
            for command in commands(&s.dockerfile) {
                if FROM.is_match(&command) {
                    stage = STAGE.captures(&command).map(|c| c[1].to_owned());
                    if stage.as_deref() == Some("server-build") {
                        built.insert(String::new());
                    }
                } else if stage.as_deref() == Some("server-build") {
                    built.extend(cargo_packages(&command, false).into_iter().map(|(_, n)| n));
                }
            }
            // The empty name marks that the stage exists.
            if !built.remove("") {
                out.push(finding(
                    "Dockerfile has no server-build stage (sync compares its -p with mise.toml's \
                     server_package)"
                        .into(),
                ));
            } else if built != BTreeSet::from([server.clone()]) {
                out.push(finding(format!(
                    "mismatch: Dockerfile's server-build stage builds -p '{}', expected '{server}' \
                     (mise.toml [vars] server_package)",
                    built.into_iter().collect::<Vec<_>>().join(" ")
                )));
            }
        }
    }

    let dirs: BTreeSet<&str> = s
        .manifests
        .iter()
        .filter_map(|(p, _)| p.strip_prefix("crates/")?.strip_suffix("/Cargo.toml"))
        .collect();
    for (file, text) in &s.workflows {
        let mut seen = BTreeSet::new();
        for command in commands(text) {
            for c in CRATE_PATH.captures_iter(&command) {
                // Not a path inside a URL (`https://crates.io/crates/NAME`).
                let start = c.get(1).map_or(0, |m| m.start() - "crates/".len());
                let word = command[..start].rsplit(char::is_whitespace).next();
                if word.is_some_and(|w| w.contains("://") || w.contains("crates.io")) {
                    continue;
                }
                let dir = c[1].to_owned();
                if !dirs.contains(dir.as_str()) && seen.insert(dir.clone()) {
                    out.push(finding(format!(
                        "{file}: crates/{dir} is not a crate (no crates/{dir}/Cargo.toml)"
                    )));
                }
            }
        }
    }
    out
}

/// A YAML file with its merge keys (`<<: *anchor`) applied, or why it is not one.
pub(super) fn yaml(path: &str, text: &str) -> Result<YValue, String> {
    let mut doc: YValue =
        yaml_serde::from_str(text).map_err(|e| format!("{path}: not YAML: {e}"))?;
    doc.apply_merge()
        .map_err(|e| format!("{path}: not YAML: {e}"))?;
    Ok(doc)
}

/// A YAML scalar as text (`None` for a mapping, a sequence or null).
pub(super) fn scalar(v: &YValue) -> Option<String> {
    match v {
        YValue::String(s) => Some(s.clone()),
        YValue::Number(n) => Some(n.to_string()),
        YValue::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// The entries of the mapping under `key` (none when there is no mapping there).
pub(super) fn entries<'a>(
    v: &'a YValue,
    key: &str,
) -> impl Iterator<Item = (&'a YValue, &'a YValue)> {
    v.get(key)
        .and_then(YValue::as_mapping)
        .into_iter()
        .flatten()
}

fn workflow<'a>(s: &'a Snapshot, path: &str) -> Option<&'a str> {
    s.workflows
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, t)| t.as_str())
}

/// One image of release.yml's matrix.
struct Released {
    name: String,
    /// The registry path: IMAGE_NAME's prefix and the name.
    path: String,
    target: String,
    /// The layer-cache scope it reads, and whether release.yml writes it too.
    cache: Option<String>,
    saves: bool,
}

/// release.yml's image matrix (`strategy.matrix.include` entries with an `image`, in the one
/// job that has them) and the prefix of that job's `IMAGE_NAME: <prefix>${{ matrix.image }}`.
fn release_images(release: &str) -> Result<(String, Vec<Released>), String> {
    let doc = yaml(RELEASE, release)?;
    let mut jobs = Vec::new();
    for (job, def) in entries(&doc, "jobs") {
        let include = def
            .get("strategy")
            .and_then(|s| s.get("matrix"))
            .and_then(|m| m.get("include"))
            .and_then(YValue::as_sequence);
        let images: Vec<&YValue> = include
            .into_iter()
            .flatten()
            .filter(|e| e.get("image").is_some())
            .collect();
        if !images.is_empty() {
            jobs.push((scalar(job).unwrap_or_default(), def, images));
        }
    }
    let [(job, def, images)] = &jobs[..] else {
        return Err(format!(
            "{RELEASE} has {} jobs with an image matrix (include: - image: ...), expected one",
            jobs.len()
        ));
    };
    let image_name = def
        .get("env")
        .and_then(|e| e.get("IMAGE_NAME"))
        .and_then(scalar);
    let prefix = image_name
        .as_deref()
        .and_then(|n| n.strip_suffix("${{ matrix.image }}"))
        .ok_or_else(|| {
            format!("{RELEASE}: job {job}'s env has no IMAGE_NAME: <path>${{{{ matrix.image }}}}")
        })?
        .to_owned();
    let mut out = Vec::new();
    for entry in images {
        let field = |key: &str| entry.get(key).and_then(scalar);
        let name = field("image").ok_or_else(|| format!("{RELEASE}: an image that is not text"))?;
        let target =
            field("target").ok_or_else(|| format!("{RELEASE}: image {name} has no target"))?;
        out.push(Released {
            path: format!("{prefix}{name}"),
            name,
            target,
            cache: field("cache"),
            saves: entry.get("save-cache").and_then(YValue::as_bool) == Some(true),
        });
    }
    Ok((prefix, out))
}

/// prune-images.yml's `strategy.matrix.package` list, in the one job that has one.
fn prune_packages(prune: &str) -> Result<Vec<String>, String> {
    let doc = yaml(PRUNE, prune)?;
    let lists: Vec<&YValue> = entries(&doc, "jobs")
        .filter_map(|(_, def)| def.get("strategy")?.get("matrix")?.get("package"))
        .collect();
    let [list] = &lists[..] else {
        return Err(format!(
            "{PRUNE} has {} jobs with a package matrix, expected one",
            lists.len()
        ));
    };
    list.as_sequence()
        .into_iter()
        .flatten()
        .map(|p| scalar(p).ok_or_else(|| format!("{PRUNE}: a package that is not text")))
        .collect()
}

/// The one line of `text` that `re` matches, split into names by `split`.
fn one_list(
    text: &str,
    re: &Regex,
    file: &str,
    what: &str,
    split: &[char],
) -> Result<Vec<String>, String> {
    let found: Vec<String> = lines(text)
        .filter_map(|l| re.captures(l).map(|c| c[1].to_owned()))
        .collect();
    let [list] = &found[..] else {
        return Err(format!("{file} has {} {what}, expected one", found.len()));
    };
    Ok(list
        .split(split)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect())
}

/// scripts/security-patch.py's `IMAGES`: (name, path, stage, rebuild fixes all).
fn script_images(script: &str) -> Result<Vec<(String, String, String, bool)>, String> {
    let mut rows = lines(script).skip_while(|l| !SCRIPT_IMAGES_START.is_match(l));
    if rows.next().is_none() {
        return Err(format!("{SECURITY_PATCH} has no `IMAGES = {{` line"));
    }
    let mut out = Vec::new();
    for line in rows {
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with('}') {
            return Ok(out);
        }
        let c = SCRIPT_IMAGE.captures(line).ok_or_else(|| {
            format!("{SECURITY_PATCH}: IMAGES has a line sync cannot read: {line}")
        })?;
        out.push((
            c[1].to_owned(),
            c[2].to_owned(),
            c[3].to_owned(),
            &c[4] == "True",
        ));
    }
    Err(format!("{SECURITY_PATCH}: IMAGES is not closed"))
}

/// The local `build-image` steps of every workflow: (workflow, job, its `with:`).
fn build_image_steps(s: &Snapshot) -> Result<Vec<(String, String, YValue)>, String> {
    let mut out = Vec::new();
    for (path, text) in &s.workflows {
        if !path.starts_with(".github/workflows/") {
            continue;
        }
        let doc = yaml(path, text)?;
        for (job, def) in entries(&doc, "jobs") {
            for step in def
                .get("steps")
                .and_then(YValue::as_sequence)
                .into_iter()
                .flatten()
            {
                let uses = step.get("uses").and_then(scalar).unwrap_or_default();
                if uses
                    .trim_end_matches('/')
                    .ends_with("/.github/actions/build-image")
                {
                    let with = step.get("with").cloned().unwrap_or(YValue::Null);
                    out.push((path.clone(), scalar(job).unwrap_or_default(), with));
                }
            }
        }
    }
    Ok(out)
}

pub fn images(s: &Snapshot) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(release) = workflow(s, RELEASE) else {
        return vec![finding(format!("{RELEASE} is missing"))];
    };
    let (prefix, released) = match release_images(release) {
        Ok(i) => i,
        Err(why) => return vec![finding(why)],
    };
    let names = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v.join(" ")
    };
    let expected: Vec<String> = released.iter().map(|i| i.name.clone()).collect();
    let mut compare = |label: String, got: Result<Vec<String>, String>| match got {
        Err(why) => out.push(finding(why)),
        Ok(got) if names(&got) != names(&expected) => out.push(Finding::Mismatch {
            label,
            actual: names(&got),
            expected: format!("{} ({RELEASE}'s image matrix)", names(&expected)),
        }),
        Ok(_) => {}
    };
    compare(
        format!("{PRUNE} package matrix"),
        workflow(s, PRUNE)
            .ok_or_else(|| format!("{PRUNE} is missing"))
            .and_then(prune_packages),
    );
    compare(
        format!("{RELEASE} release notes' image loop"),
        one_list(
            release,
            &NOTES_LOOP,
            RELEASE,
            "`for name in ...; do` lines",
            &[' ', '\t'],
        ),
    );
    let script = if s.security_patch.is_empty() {
        Err(format!("{SECURITY_PATCH} is missing"))
    } else {
        script_images(&s.security_patch)
    };
    compare(
        format!("{SECURITY_PATCH} IMAGES"),
        script.clone().map(|v| v.into_iter().map(|i| i.0).collect()),
    );
    let stages = stages(&s.dockerfile);
    for r in &released {
        if !stages.contains(&r.target) {
            out.push(finding(format!(
                "{RELEASE}: image {} targets {}, not a Dockerfile stage",
                r.name, r.target
            )));
        }
    }
    if let Ok(script) = script {
        for (name, path, stage, _) in &script {
            if let Some(r) = released.iter().find(|r| &r.name == name)
                && path != &r.path
            {
                out.push(Finding::Mismatch {
                    label: format!("{SECURITY_PATCH} IMAGES path of {name}"),
                    actual: path.clone(),
                    expected: format!("{} ({RELEASE}'s IMAGE_NAME)", r.path),
                });
            }
            if !stages.contains(stage) {
                out.push(finding(format!(
                    "{SECURITY_PATCH}: image {name} reads stage {stage}, not a Dockerfile stage"
                )));
            }
        }
        let ported: Vec<_> = IMAGES
            .iter()
            .map(|&(n, p, st, r)| (n.to_owned(), p.to_owned(), st.to_owned(), r))
            .collect();
        if ported != script {
            out.push(finding(format!(
                "mismatch: xtask's security_patch::IMAGES differs from {SECURITY_PATCH}'s IMAGES \
                 (names, paths, stages, rebuild flags, in order)"
            )));
        }
    }

    // Every image reference in these files is a released image, or the prefix and a variable.
    for (file, text) in [
        (RELEASE, release),
        (COMPOSE, s.compose.as_str()),
        ("mise.toml", s.mise_toml.as_str()),
    ] {
        let mut seen = BTreeSet::new();
        for c in GHCR.captures_iter(text) {
            let owner = format!("ghcr.io/{}", &c[1]);
            let path = format!("{owner}{}", c[2].trim_end_matches('.'));
            let known = if !c[3].is_empty() {
                owner == prefix && c[2].is_empty()
            } else {
                released.iter().any(|r| r.path == path)
            };
            if !known && seen.insert(path.clone()) {
                out.push(finding(format!(
                    "{file}: {path}{} is not an image {RELEASE} publishes ({prefix} and its image \
                     matrix)",
                    &c[3]
                )));
            }
        }
    }

    // compose.yaml builds each service from the stage its image is released from.
    if s.compose.is_empty() {
        out.push(finding(format!("{COMPOSE} is missing")));
    } else {
        match yaml(COMPOSE, &s.compose) {
            Err(why) => out.push(finding(why)),
            Ok(doc) => {
                for (service, def) in entries(&doc, "services") {
                    let service = scalar(service).unwrap_or_default();
                    let Some(build) = def.get("build") else {
                        continue;
                    };
                    // Without a target, compose builds the Dockerfile's last stage.
                    let Some(target) = build.get("target").and_then(scalar) else {
                        out.push(finding(format!(
                            "{COMPOSE}: service {service} builds with no build.target (the \
                             Dockerfile's last stage): name its image's release stage"
                        )));
                        continue;
                    };
                    if !stages.contains(&target) {
                        out.push(finding(format!(
                            "{COMPOSE}: service {service} builds target {target}, not a Dockerfile \
                             stage"
                        )));
                    }
                    let image = def.get("image").and_then(scalar).unwrap_or_default();
                    let path = GHCR
                        .captures(&image)
                        .filter(|c| c[3].is_empty())
                        .map(|c| format!("ghcr.io/{}{}", &c[1], &c[2]));
                    match released.iter().find(|r| Some(&r.path) == path.as_ref()) {
                        Some(r) if r.target != target => out.push(finding(format!(
                            "{COMPOSE}: service {service} builds target {target}, but its image \
                             {} is released from target {} ({RELEASE})",
                            r.path, r.target
                        ))),
                        Some(_) => {}
                        None => out.push(finding(format!(
                            "{COMPOSE}: service {service} builds target {target} but names no image \
                             {RELEASE} publishes"
                        ))),
                    }
                }
            }
        }
    }

    // build-image targets are stages; a layer cache release.yml reads but does not write is
    // written by some build-image step.
    match build_image_steps(s) {
        Err(why) => out.push(finding(why)),
        Ok(steps) => {
            let mut saved = BTreeSet::new();
            for (path, job, with) in &steps {
                let input = |key: &str| with.get(key).and_then(scalar);
                if let Some(target) = input("target")
                    && !target.contains("${{")
                    && !stages.contains(&target)
                {
                    out.push(finding(format!(
                        "{path}: job {job} builds target {target}, not a Dockerfile stage"
                    )));
                }
                if input("cache-save").as_deref() == Some("true")
                    && let Some(scope) = input("cache-scope")
                {
                    saved.insert(scope);
                }
            }
            for r in &released {
                if let Some(cache) = &r.cache
                    && !r.saves
                    && !saved.contains(cache)
                {
                    out.push(finding(format!(
                        "{RELEASE}: image {} reads the layer cache {cache}, which no build-image \
                         step with cache-save: true writes",
                        r.name
                    )));
                }
            }
        }
    }
    out
}

/// A Dependabot value as the text compared with the days (a string keeps its quotes, so that it
/// never equals a number).
fn days_text(v: &YValue) -> String {
    match v {
        YValue::Number(n) => n.to_string(),
        YValue::String(s) => format!("{s:?}"),
        _ => "<not a number>".into(),
    }
}

pub fn cooldown(s: &Snapshot) -> Vec<Finding> {
    let days = if s.cooldown_toml.is_empty() {
        Err(finding(format!("{COOLDOWN} is missing")))
    } else {
        parse(COOLDOWN, &s.cooldown_toml).and_then(|t| {
            t.get("days")
                .and_then(Value::as_integer)
                .ok_or_else(|| finding(format!("{COOLDOWN} has no integer days")))
        })
    };
    let days = match days {
        Ok(d) => d.to_string(),
        Err(f) => return vec![f],
    };
    let mut out = Vec::new();
    // (label, value) for each copy.
    let mut copies = Vec::new();
    if s.dependabot.is_empty() {
        out.push(finding(format!("{DEPENDABOT} is missing")));
    } else {
        match yaml(DEPENDABOT, &s.dependabot) {
            Err(why) => out.push(finding(why)),
            Ok(doc) => {
                let mut exempted = BTreeSet::new();
                let updates = doc.get("updates").and_then(YValue::as_sequence);
                for update in updates.into_iter().flatten() {
                    let eco = update
                        .get("package-ecosystem")
                        .and_then(scalar)
                        .unwrap_or_default();
                    let dirs = match update.get("directories").and_then(YValue::as_sequence) {
                        Some(dirs) => dirs.iter().filter_map(scalar).collect::<Vec<_>>(),
                        None => update
                            .get("directory")
                            .and_then(scalar)
                            .into_iter()
                            .collect(),
                    }
                    .join(", ");
                    let mut has = false;
                    for (key, value) in entries(update, "cooldown") {
                        let key = scalar(key).unwrap_or_default();
                        if key.ends_with("-days") {
                            has |= key == "default-days";
                            copies.push((
                                format!("{DEPENDABOT} {eco} ({dirs}) cooldown {key}"),
                                days_text(value),
                            ));
                        }
                    }
                    let exempt = NO_COOLDOWN
                        .iter()
                        .position(|&(e, d, _)| e == eco && d == dirs);
                    match (has, exempt) {
                        (false, None) => out.push(finding(format!(
                            "{DEPENDABOT}: update {eco} ({dirs}) has no cooldown default-days \
                             (sync requires one of every update but those in NO_COOLDOWN)"
                        ))),
                        (true, Some(_)) => out.push(finding(format!(
                            "{DEPENDABOT}: update {eco} ({dirs}) has a cooldown: remove it from \
                             sync's NO_COOLDOWN"
                        ))),
                        _ => {}
                    }
                    exempted.extend(exempt);
                }
                for (i, (eco, dirs, _)) in NO_COOLDOWN.iter().enumerate() {
                    if !exempted.contains(&i) {
                        out.push(finding(format!(
                            "{DEPENDABOT}: no update {eco} ({dirs}), which sync's NO_COOLDOWN \
                             exempts from a cooldown"
                        )));
                    }
                }
            }
        }
    }
    let age = parse("mise.toml", &s.mise_toml)
        .ok()
        .and_then(|t| {
            t.get("settings")?
                .get("minimum_release_age")?
                .as_str()?
                .strip_suffix('d')
                .map(str::to_owned)
        })
        .unwrap_or_default();
    copies.push((
        "mise.toml [settings] minimum_release_age (days)".into(),
        age,
    ));
    let pins: Vec<_> = lines(&s.bump_pins)
        .filter_map(|l| BUMP_PINS_DAYS.captures(l).map(|c| c[1].to_owned()))
        .collect();
    let pins = match &pins[..] {
        [one] => one.clone(),
        _ => String::new(),
    };
    copies.push((format!("{BUMP_PINS} COOLDOWN_DAYS"), pins));
    for (label, actual) in copies {
        if actual != days {
            out.push(Finding::Mismatch {
                label,
                actual,
                expected: format!("{days} ({COOLDOWN} days)"),
            });
        }
    }
    out
}

/// The dependency tables of a manifest that build into the crate (not `[dev-dependencies]`),
/// with their names as the finding gives them.
fn build_tables(manifest: &Table) -> Vec<(String, &Table)> {
    let mut found = vec![
        ("dependencies".to_owned(), manifest.get("dependencies")),
        (
            "build-dependencies".to_owned(),
            manifest.get("build-dependencies"),
        ),
        (
            "workspace.dependencies".to_owned(),
            manifest
                .get("workspace")
                .and_then(|w| w.get("dependencies")),
        ),
    ];
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (cfg, t) in targets {
            for kind in ["dependencies", "build-dependencies"] {
                found.push((format!("target.{cfg}.{kind}"), t.get(kind)));
            }
        }
    }
    found
        .into_iter()
        .filter_map(|(name, t)| Some((name, t?.as_table()?)))
        .collect()
}

/// The package a dependency entry names: its `package =`, else, for `workspace = true`, the
/// root's `[workspace.dependencies]` entry's (`workspace`: key to package), else its key.
fn package_of(key: &str, dep: &Value, workspace: &BTreeMap<String, String>) -> String {
    match dep.get("package").and_then(Value::as_str) {
        Some(p) => p.to_owned(),
        None if dep.get("workspace") == Some(&Value::Boolean(true)) => workspace
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_owned()),
        None => key.to_owned(),
    }
}

pub fn testing(s: &Snapshot) -> Vec<Finding> {
    // `packages` reports why the members cannot be read.
    let Ok(members) = members(s) else {
        return vec![];
    };
    // members() has parsed every manifest read here, so none fails to parse below.
    let parsed = |text: &str| toml::from_str::<Table>(text).unwrap_or_default();
    let root = s
        .manifests
        .iter()
        .find(|(p, _)| p == "Cargo.toml")
        .map(|(_, t)| parsed(t))
        .unwrap_or_default();
    let workspace: BTreeMap<String, String> = root
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(Value::as_table)
        .into_iter()
        .flatten()
        .map(|(key, dep)| (key.clone(), package_of(key, dep, &BTreeMap::new())))
        .collect();
    let mut out = Vec::new();
    for (path, text) in &s.manifests {
        if path != "Cargo.toml" && !path.starts_with("crates/") {
            continue;
        }
        let manifest = parsed(text);
        // Each dependency key and the package it names, for the features below.
        let mut packages = BTreeMap::new();
        let mut tables = build_tables(&manifest);
        if let Some(dev) = manifest.get("dev-dependencies").and_then(Value::as_table) {
            tables.push(("dev-dependencies".into(), dev));
        }
        for (_, table) in &tables {
            for (key, dep) in *table {
                packages.insert(key.clone(), package_of(key, dep, &workspace));
            }
        }
        for (name, table) in build_tables(&manifest) {
            for (key, dep) in table {
                let enables = dep
                    .get("features")
                    .and_then(Value::as_array)
                    .is_some_and(|f| f.iter().any(|f| f.as_str() == Some("testing")));
                if enables && members.contains(&package_of(key, dep, &workspace)) {
                    out.push(finding(format!(
                        "{path}: [{name}] {key} enables its testing feature; enable it only \
                         from [dev-dependencies]"
                    )));
                }
            }
        }
        let Some(features) = manifest.get("features").and_then(Value::as_table) else {
            continue;
        };
        for (feature, list) in features {
            if feature == "testing" {
                continue;
            }
            for item in list
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                let enabled = match item.split_once('/') {
                    None => item == "testing",
                    Some((dep, f)) => {
                        let dep = dep.trim_end_matches('?');
                        f == "testing" && packages.get(dep).is_some_and(|p| members.contains(p))
                    }
                };
                if enabled {
                    out.push(finding(format!(
                        "{path}: feature {feature} enables {item}; only the testing feature \
                         may, so that it stays enabled only from [dev-dependencies]"
                    )));
                }
            }
        }
    }
    out
}

/// Every rule of this module, in order.
pub fn check(s: &Snapshot) -> Vec<Finding> {
    let mut out = packages(s);
    out.extend(images(s));
    out.extend(cooldown(s));
    out.extend(testing(s));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCKERFILE: &str = "FROM rust AS chef\n\
        FROM chef AS server-build\n\
        RUN cargo chef cook --release -p app --recipe-path r.json\n\
        RUN cargo build --release -p app; \\\n    mkdir -p /out/cache\n\
        FROM chef AS client-build\n\
        RUN cargo chef cook --release --target wasm32-unknown-unknown \\\n      -p app-web --recipe-path r.json\n\
        # docker build --target regions -p nothing\n\
        FROM debian AS regions\n\
        FROM scratch AS runtime-base\n\
        FROM runtime-base AS runtime\n";
    const RELEASE_YML: &str = "jobs:\n  image:\n    strategy:\n      matrix:\n        include:\n\
        \x20         - image: observation\n            target: runtime\n            cache: image\n\
        \x20           save-cache: true\n\
        \x20         - image: observation-regions\n            target: regions\n            cache: image-regions\n\
        \x20   env:\n      IMAGE_NAME: ghcr.io/panevain/${{ matrix.image }}\n\
        \x20 notes:\n    steps:\n      - run: |\n          for name in observation observation-regions; do\n            echo ghcr.io/panevain/${name}\n          done\n";
    const PRUNE_YML: &str = "jobs:\n  prune:\n    strategy:\n      matrix:\n        package: [observation, observation-regions]\n";
    const SCRIPT: &str = "IMAGES = {\n    # name: (path, stage, rebuild fixes all)\n\
        \x20   \"observation\": (\"ghcr.io/panevain/observation\", \"runtime-base\", False),\n\
        \x20   \"observation-regions\": (\"ghcr.io/panevain/observation-regions\", \"regions\", True),\n}\n";
    const DEPENDABOT_YML: &str = "updates:\n  - package-ecosystem: cargo\n    directories: [\"/\", \"/fuzz\"]\n\
        \x20   cooldown:\n      default-days: 7\n\
        \x20 - package-ecosystem: github-actions\n    directory: /\n    cooldown:\n      default-days: 7\n\
        \x20 - package-ecosystem: rust-toolchain\n    directory: /fuzz\n\
        \x20 - package-ecosystem: docker\n    directory: /\n\
        \x20 - package-ecosystem: npm\n    directory: /web\n    cooldown:\n      # semver-major-days: 30\n      default-days: 7\n";
    const COMPOSE_YAML: &str = "services:\n  observation:\n\
        \x20   image: ${OBS_IMAGE:-ghcr.io/panevain/observation:${OBS_VERSION:-latest}}\n\
        \x20   build:\n      context: .\n      target: runtime\n  regions:\n\
        \x20   image: ghcr.io/panevain/observation-regions:latest\n\
        \x20   build:\n      target: regions\n  other:\n    image: postgres\n";

    fn base() -> Snapshot {
        let manifest = |name: &str| format!("[package]\nname = \"{name}\"\n");
        Snapshot {
            rust_toolchain: String::new(),
            setup_mise: String::new(),
            mise_lock: String::new(),
            manifests: vec![
                (
                    "Cargo.toml".into(),
                    "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\n\
                     l2 = { package = \"lib\", path = \"crates/lib\" }\n"
                        .into(),
                ),
                ("crates/app/Cargo.toml".into(), manifest("app")),
                ("crates/app-web/Cargo.toml".into(), manifest("app-web")),
                ("crates/lib/Cargo.toml".into(), manifest("lib")),
                (
                    "fuzz/Cargo.toml".into(),
                    "[dependencies]\nlib = { path = \"../crates/lib\", features = [\"testing\"] }\n"
                        .into(),
                ),
            ],
            dockerfile: DOCKERFILE.into(),
            mise_toml: "[settings]\nminimum_release_age = \"7d\"\n\n[vars]\n\
                native_excludes = \"--exclude app-web\"\nwasm = \"-p app-web -p lib\"\n\
                server_package = \"app\"\n\n[tasks.serve]\n\
                run = \"cargo run --locked -p {{vars.server_package}}\"\n\
                [tasks.dirs]\nrun = \"mkdir -p corpus\"\n"
                .into(),
            mise_ci_toml: "[tasks.audit]\nrun = \"cargo update -p regex && cargo audit\"\n".into(),
            mise_dev_toml: String::new(),
            workflows: vec![
                (RELEASE.into(), RELEASE_YML.into()),
                (PRUNE.into(), PRUNE_YML.into()),
                (
                    ".github/workflows/ci.yml".into(),
                    "on:\n  push:\n    paths: [crates/**, \"!crates/app-web/**\"]\n\
                     jobs:\n  test:\n    steps:\n      - run: cargo nextest run --exclude app-web\n\
                     \x20 docker:\n    steps:\n      - uses: $/.github/actions/build-image\n\
                     \x20       with:\n          target: regions\n          cache-scope: image-regions\n\
                     \x20         cache-save: true\n"
                        .into(),
                ),
            ],
            deny_toml: String::new(),
            audit_toml: String::new(),
            osv_scanner_toml: String::new(),
            release_toml: String::new(),
            cooldown_toml: "days = 7\nexempt = []\n".into(),
            dependabot: DEPENDABOT_YML.into(),
            bump_pins: "import os\n\nCOOLDOWN_DAYS = 7\n".into(),
            security_patch: SCRIPT.into(),
            compose: COMPOSE_YAML.into(),
            mise_ci_lock: String::new(),
            doc_pins: Ok(String::new()),
        }
    }

    /// The text a test row names: a field of the snapshot, or one file in it.
    fn field<'a>(s: &'a mut Snapshot, name: &str) -> &'a mut String {
        let file = |files: &'a mut Vec<(String, String)>, path: &str| {
            let found = files.iter_mut().find(|(p, _)| p == path);
            found.map(|(_, t)| t).unwrap_or_else(|| panic!("{path}"))
        };
        match name {
            "mise" => &mut s.mise_toml,
            "mise.dev" => &mut s.mise_dev_toml,
            "mise.ci" => &mut s.mise_ci_toml,
            "docker" => &mut s.dockerfile,
            "cooldown" => &mut s.cooldown_toml,
            "dependabot" => &mut s.dependabot,
            "bump-pins" => &mut s.bump_pins,
            "security-patch" => &mut s.security_patch,
            "compose" => &mut s.compose,
            "ci" => file(&mut s.workflows, ".github/workflows/ci.yml"),
            "release" => file(&mut s.workflows, RELEASE),
            "prune" => file(&mut s.workflows, PRUNE),
            "root" => file(&mut s.manifests, "Cargo.toml"),
            "app" => file(&mut s.manifests, "crates/app/Cargo.toml"),
            _ => panic!("no field {name}"),
        }
    }

    /// Runs each row `FIELD | FIND | REPLACE | WANT && WANT ...` (`\n` is a newline; an empty
    /// FIND appends REPLACE, `*` replaces the whole text): the base with that one edit must give exactly those findings,
    /// each containing its WANT, in order (no WANT: none).
    fn rows(table: &str) {
        for row in table.lines().map(str::trim).filter(|r| !r.is_empty()) {
            let cols: Vec<String> = row
                .split('|')
                .map(|c| c.trim().replace("\\n", "\n"))
                .collect();
            let [name, find, replace, want] = &cols[..] else {
                panic!("row {row:?}")
            };
            let mut s = base();
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
            let got: Vec<String> = check(&s).iter().map(ToString::to_string).collect();
            let want: Vec<&str> = want.split(" && ").filter(|w| !w.is_empty()).collect();
            assert_eq!(got.len(), want.len(), "{row}: {got:#?}");
            for (g, w) in got.iter().zip(&want) {
                assert!(g.contains(w), "{row}: {g:?} lacks {w:?}");
            }
        }
    }

    #[test]
    fn the_base_passes() {
        rows("mise | | | ");
    }

    #[test]
    fn packages_cargo_is_given_are_crates() {
        rows(
            r#"
            mise | -p lib | -p libb | mise.toml: cargo -p libb: not a crate
            mise | --exclude app-web | --exclude=web | mise.toml: cargo --exclude web: not a crate
            mise.ci | cargo audit | cargo clippy --package nope | mise.ci.toml: cargo --package nope: not a crate
            docker | -p app-web | -p app-www | Dockerfile: cargo -p app-www: not a crate
            ci | --exclude app-web | --exclude app-web -p gone | ci.yml: cargo -p gone: not a crate
            ci | "!crates/app-web/**" | "!crates/gone/**", crates/gone/x | ci.yml: crates/gone is not a crate
            mise | server_package = "app" | server_package = "lib" | server-build stage builds -p 'app', expected 'lib'
            mise | server_package = "app" | server_package = "ap" | server_package is 'ap', not a crate && expected 'ap'
            docker | build --release -p app; | build --release -p lib; | server-build stage builds -p 'app lib', expected 'app'
            docker | AS server-build | AS server | Dockerfile has no server-build stage
            mise | server_package | server | mise.toml has no [vars] server_package
            root | "crates/*" | "crates/*", "tools" | members are not ["crates/*"]
            mise | run = "mkdir -p corpus" | run = "mkdir -p a; cargo build -p app" | 
            docker | FROM debian AS regions | # cargo build -p nope\nFROM debian AS regions | 
            mise.ci | cargo audit | cargo update -p tokio@1.0.0 && cargo install -p x cargo-fuzz | 
            mise | run = "mkdir -p corpus" | run = "cargo build -p nope" | mise.toml: cargo -p nope: not a crate
            mise | run = "mkdir -p corpus" | run = ["true", "cargo nextest run --exclude nope"] | mise.toml: cargo --exclude nope: not a crate
            mise | -p lib | -p x -p y | mise.toml: cargo -p x: && mise.toml: cargo -p y:
            mise.dev | | [tasks.x]\nrun = "cargo build -p nope"\n | mise.dev.toml: cargo -p nope: not a crate
            docker | -p app-web | -p "app-www" | Dockerfile: cargo -p app-www: not a crate
            docker | -p app-web | -papp-www | Dockerfile: cargo -p app-www: not a crate
            docker | -p app-web | \\n    # a comment\n\n      -p app-www | Dockerfile: cargo -p app-www: not a crate
            ci | --exclude app-web | --manifest-path xtask/Cargo.toml -p xtask | 
            ci | --exclude app-web | --exclude app-web\n      - run: echo https://crates.io/crates/cargo-auditable ./crates/nope | ci.yml: crates/nope is not a crate
            root | "crates/*"] | "crates/*"]\nexclude = ["crates/lib"] | excludes a path under crates/
            root | "crates/*"] | "crates/*"]\nexclude = ["./crates/lib"] | excludes a path under crates/
            ci | --exclude app-web | --exclude app-web --exclude-from-report app | 
            ci | --exclude app-web | --manifest-path Cargo.toml -p bogus | ci.yml: cargo -p bogus: not a crate
            ci | --exclude app-web | --exclude app-web\n      - run: ls "${GITHUB_WORKSPACE}/crates/bogus" | ci.yml: crates/bogus is not a crate
        "#,
        );
    }

    /// `run`'s rule set has the four rules of this module, after the script's.
    #[test]
    fn all_runs_these_rules() {
        let mut s = base();
        s.mise_toml = s.mise_toml.replacen("-p lib", "-p nope", 1);
        field(&mut s, "app")
            .push_str("[dependencies]\nlib = { path = \"../lib\", features = [\"testing\"] }\n");
        s.compose = s.compose.replacen("target: runtime", "target: runtim", 1);
        s.bump_pins = s.bump_pins.replacen("= 7", "= 8", 1);
        let found: Vec<String> = super::super::all(&s)
            .findings
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            found.iter().any(|f| f.contains("cargo -p nope")),
            "{found:?}"
        );
        for want in [
            "[dependencies] lib enables",
            "builds target runtim, not a Dockerfile stage",
            "COOLDOWN_DAYS has '8'",
        ] {
            assert!(found.iter().any(|f| f.contains(want)), "{want}: {found:?}");
        }
    }

    /// A workflow that is not YAML is reported once, though several rules read it.
    #[test]
    fn a_workflow_that_is_not_yaml_is_reported_once() {
        let mut s = base();
        field(&mut s, "ci").push_str("  x: [\n");
        let found: Vec<String> = super::super::all(&s)
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|f| f.contains("not YAML"))
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn image_lists_agree() {
        rows(
            r#"
            prune | , observation-regions] | ] | package matrix has 'observation', expected 'observation observation-regions
            release | for name in observation observation-regions | for name in observation-x observation-regions | image loop has 'observation-regions observation-x'
            release | target: regions | target: region | image observation-regions targets region, not a Dockerfile stage && service regions builds target regions, but its image ghcr.io/panevain/observation-regions is released from target region
            release | target: regions | target: regions\n            target: runtime | release.yml: not YAML
            release | IMAGE_NAME | NAME | job image's env has no IMAGE_NAME
            release | IMAGE_NAME: ghcr.io/panevain/${{ matrix.image }} | IMAGE_NAME: ghcr.io/panevain/${{ matrix.image }}\n      IMAGE_NAME: ghcr.io/panevain/x-${{ matrix.image }} | release.yml: not YAML
            release | notes: | again:\n    strategy:\n      matrix:\n        include:\n          - image: observation\n            target: runtime\n  notes: | release.yml has 2 jobs with an image matrix
            release | - image: observation-regions | - image: "observation-regions" # the agent | 
            release | echo ghcr | echo target: ${{matrix.target}} ghcr | 
            prune | [observation, observation-regions] | ["observation", "observation-regions"] # both | 
            prune | [observation, observation-regions] | \n          - observation\n          - observation-regions | 
            release | ghcr.io/panevain/${name} | ghcr.io/other/${name} | release.yml: ghcr.io/other/$ is not an image
            compose | ghcr.io/panevain/observation-regions | ghcr.io/panevain/observation-agent | compose.yaml: ghcr.io/panevain/observation-agent is not an image && service regions builds target regions but names no image
            compose | target: regions | target: regionz | service regions builds target regionz, not a Dockerfile stage && but its image ghcr.io/panevain/observation-regions is released from target regions
            compose | target: runtime | target: regions | service observation builds target regions, but its image ghcr.io/panevain/observation is released from target runtime
            compose | * |  | compose.yaml is missing
            mise | [vars] | [vars]\nup = "verify ghcr.io/panevain/observation-agent:1"\n | mise.toml: ghcr.io/panevain/observation-agent is not an image
            ci | target: regions | target: regionz | ci.yml: job docker builds target regionz, not a Dockerfile stage
            ci | cache-scope: image-regions | cache-scope: image-regionz | image observation-regions reads the layer cache image-regions
            ci | cache-save: true | cache-save: false | image observation-regions reads the layer cache image-regions
            docker | FROM debian AS regions | FROM debian AS regionz | image observation-regions targets regions, not a Dockerfile stage && image observation-regions reads stage regions, not && service regions builds target regions, not && job docker builds target regions, not
            security-patch | "ghcr.io/panevain/observation" | "ghcr.io/other/observation" | path of observation has 'ghcr.io/other/observation' && security_patch::IMAGES differs
            security-patch | "regions", True | "regions", False | security_patch::IMAGES differs
            security-patch | "runtime-base" | "runtime-bas" | reads stage runtime-bas, not a Dockerfile stage && security_patch::IMAGES differs
            security-patch | "observation-regions": ( | "regions": ( | IMAGES has 'observation regions' && security_patch::IMAGES differs
            security-patch | True),\n | True),\n    "x": 1,\n | IMAGES has a line sync cannot read
            security-patch | IMAGES = { | IMAGES = dict( | no `IMAGES = {` line
            security-patch | IMAGES = { | IMAGES = {\n}\nOTHER = { | IMAGES has '<nothing>' && security_patch::IMAGES differs
            prune | package: | packages: | prune-images.yml has 0 jobs with a package matrix
            compose | build:\n      target: regions | build: . | service regions builds with no build.target
            compose | build:\n      target: regions | build:\n      context: . | service regions builds with no build.target
            compose | target: regions | target: Regions | service regions builds target Regions, not a Dockerfile stage && but its image ghcr.io/panevain/observation-regions is released from target regions
            ci | target: regions | target: Regions | ci.yml: job docker builds target Regions, not a Dockerfile stage
            release | ghcr.io/panevain/${name} | ghcr.io/panevain/observation-${name} | release.yml: ghcr.io/panevain/observation-$ is not an image
            mise | [vars] | # The build-tools stage is ghcr.io/jdx/mise.\n[vars] | mise.toml: ghcr.io/jdx/mise is not an image
            security-patch | * |  | security-patch.py is missing
        "#,
        );
    }

    #[test]
    fn cooldowns_agree() {
        rows(
            r#"
            cooldown | days = 7 | days = 5 | cargo (/, /fuzz) cooldown default-days has '7' && github-actions (/) cooldown default-days has '7' && npm (/web) cooldown default-days has '7' && minimum_release_age (days) has '7' && COOLDOWN_DAYS has '7'
            dependabot | default-days: 7\n  - | default-days: 3\n  - | cargo (/, /fuzz) cooldown default-days has '3', expected '7 (.config/cargo-cooldown.toml days)'
            dependabot | default-days: 7\n  - | default-days: 7\n      semver-major-days: 30\n  - | cargo (/, /fuzz) cooldown semver-major-days has '30'
            dependabot | default-days: 7\n  - | default-days: 3 # three\n  - | cargo (/, /fuzz) cooldown default-days has '3'
            dependabot | cooldown:\n      default-days: 7\n  - package-ecosystem: github | cooldown: { default-days: 3 }\n  - package-ecosystem: github | cargo (/, /fuzz) cooldown default-days has '3'
            dependabot | default-days: 7\n  - | default-days: "7"\n  - | cargo (/, /fuzz) cooldown default-days has '"7"'
            dependabot | cooldown:\n      default-days: 7\n  - package-ecosystem: github | \n  - package-ecosystem: github | update cargo (/, /fuzz) has no cooldown default-days
            dependabot | cooldown:\n      default-days: 7\n  - package-ecosystem: github | cooldown:\n      semver-major-days: 7\n  - package-ecosystem: github | update cargo (/, /fuzz) has no cooldown default-days
            dependabot | | \n  - package-ecosystem: cargo\n    directory: /xtask\n | update cargo (/xtask) has no cooldown default-days
            dependabot | | \n  - package-ecosystem: cargo\n    directory: /xtask\n    cooldown:\n      default-days: 3\n | cargo (/xtask) cooldown default-days has '3'
            dependabot | | \n  - package-ecosystem: pip\n    directory: /\n | update pip (/) has no cooldown default-days
            dependabot | | \n  - package-ecosystem: docker\n    directory: /tools\n | update docker (/tools) has no cooldown default-days
            dependabot | directory: /fuzz | directory: /fuzz\n    cooldown:\n      default-days: 7 | update rust-toolchain (/fuzz) has a cooldown: remove it from sync's NO_COOLDOWN
            dependabot | directory: /fuzz | directory: /fuzz2 | update rust-toolchain (/fuzz2) has no cooldown && no update rust-toolchain (/fuzz), which sync's NO_COOLDOWN
            dependabot | * | updates: []\n | no update docker (/) && no update rust-toolchain (/fuzz)
            dependabot | * |  | dependabot.yml is missing
            dependabot | * | updates: [ | dependabot.yml: not YAML
            dependabot | * | x: &cd\n  default-days: 7\nupdates:\n  - package-ecosystem: cargo\n    directory: /\n    cooldown:\n      <<: *cd\n  - package-ecosystem: docker\n    directory: /\n  - package-ecosystem: rust-toolchain\n    directory: /fuzz\n | 
            dependabot | * | x: &cd\n  default-days: 3\nupdates:\n  - package-ecosystem: cargo\n    directory: /\n    cooldown:\n      <<: *cd\n  - package-ecosystem: docker\n    directory: /\n  - package-ecosystem: rust-toolchain\n    directory: /fuzz\n | cargo (/) cooldown default-days has '3'
            bump-pins | COOLDOWN_DAYS = 7 | COOLDOWN_DAYS = 7  # a week | 
            mise | "7d" | "1w" | minimum_release_age (days) has '<nothing>'
            bump-pins | COOLDOWN_DAYS = 7 | COOLDOWN_DAYS = 7\nCOOLDOWN_DAYS = 7 | COOLDOWN_DAYS has '<nothing>'
            cooldown | days = 7 | days = "7" | has no integer days
            cooldown | days = 7 | dais = 7 | has no integer days
            cooldown | * |  | cargo-cooldown.toml is missing
        "#,
        );
    }

    #[test]
    fn testing_is_enabled_only_from_dev_dependencies() {
        rows(
            r#"
            app |  | [dependencies]\nlib = { workspace = true, features = ["testing"] }\n | [dependencies] lib enables its testing feature
            app |  | [build-dependencies]\nlib = { path = "../lib", features = ["testing"] }\n | [build-dependencies] lib enables
            app |  | [target.'cfg(unix)'.dependencies]\nlib = { path = "../lib", features = ["testing"] }\n | [target.cfg(unix).dependencies] lib enables
            app |  | [dependencies]\nl = { package = "lib", path = "../lib", features = ["testing"] }\n | [dependencies] l enables
            app |  | [features]\ndefault = ["testing"]\ntesting = []\n | feature default enables testing
            app |  | [dependencies]\nlib = { path = "../lib" }\n[features]\nextra = ["lib?/testing"]\n | feature extra enables lib?/testing
            app |  | [dependencies]\nlib = { path = "../lib" }\n[features]\ntesting = ["lib/testing"]\n | 
            app |  | [dev-dependencies]\nlib = { path = "../lib", features = ["testing"] }\napp = { path = ".", features = ["testing"] }\n | 
            app |  | [dependencies]\nserde = { version = "1", features = ["testing"] }\n[features]\nx = ["serde/testing"]\n | 
            root | l2 = { package = "lib", path = "crates/lib" } | l2 = { package = "lib", path = "crates/lib", features = ["testing"] } | Cargo.toml: [workspace.dependencies] l2 enables
            app |  | [dependencies]\nlib = { path = "../lib", features = ["testing"] }\napp-web = { path = "../app-web", features = ["testing"] }\n | [dependencies] lib enables && [dependencies] app-web enables
            app |  | [dependencies]\nl2 = { workspace = true, features = ["testing"] }\n | [dependencies] l2 enables
            app |  | [dependencies]\nl2 = { workspace = true }\n[features]\nx = ["l2/testing"]\n | feature x enables l2/testing
            app |  | [dependencies]\nl2 = { workspace = true }\n[dev-dependencies]\nl2 = { workspace = true, features = ["testing"] }\n | 
            app |  | [dependencies]\nc = { package = "lib", path = "../lib", features = ["testing"] }\n[dev-dependencies]\nc = { package = "serde", version = "1" }\n | [dependencies] c enables
        "#,
        );
    }
}
