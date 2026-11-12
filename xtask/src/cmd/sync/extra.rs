//! Rules the script never had: names and numbers that several files repeat by hand agree.
//! Like the script's rules they read text and change nothing; each finding names the file and
//! the copy that differs.
//!
//! - Packages: every package that cargo is told to build, test or leave out (`-p`, `--package`,
//!   `--exclude`) in the mise files, the Dockerfile and the workflows is a crate of the
//!   workspace; mise.toml's `server_package` is the one the Dockerfile's `server-build` stage
//!   builds; every `crates/NAME` path a workflow names is a crate.
//! - `testing`: a crate's `testing` feature is enabled only from `[dev-dependencies]` (fuzz/ is
//!   outside the workspace and may enable it).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use toml::{Table, Value};

use super::{Finding, Snapshot, lines, re};

regex!(
    CARGO_COMMAND,
    r#"(?:^|[\s;&|(`'"\[])cargo(?:\s+\+\S+)?\s+(?:build|check|clippy|test|nextest|run|doc|llvm-cov|chef|auditable|bench|fmt|rustc)(?:\s|$)"#
);
regex!(
    PACKAGE_FLAG,
    r#"(?:^|[\s"'])(-p|--package|--exclude)(?:\s+|=)?["']?([^\s"';\\]+)"#
);
regex!(STAGE, r"(?i)^\s*FROM\s.*\sAS\s+([A-Za-z0-9_.-]+)\s*$");
regex!(FROM, r"(?i)^\s*FROM\s");
// `crates/NAME` at the start of a path (`./` allowed), not inside a URL.
regex!(
    CRATE_PATH,
    r"(?:^|[\s\x22'!\[,])(?:\./)?crates/([A-Za-z0-9_-]+)"
);

fn finding(text: String) -> Finding {
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
        .filter(|c| !c[2].contains(['{', '$']))
        .map(|c| (c[1].to_owned(), c[2].to_owned()))
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
            if CARGO_COMMAND.is_match(simple) && !simple.contains("--manifest-path") {
                out.extend(package_flags(simple));
            }
        }
    }
    out
}

fn parse(path: &str, text: &str) -> Result<Table, Finding> {
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
            e.iter()
                .any(|x| x.as_str().is_some_and(|x| x.starts_with("crates/")))
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
    let members = match members(s) {
        Ok(m) => m,
        Err(f) => return vec![f],
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
            mise_toml: "[vars]\n\
                native_excludes = \"--exclude app-web\"\nwasm = \"-p app-web -p lib\"\n\
                server_package = \"app\"\n\n[tasks.serve]\n\
                run = \"cargo run --locked -p {{vars.server_package}}\"\n\
                [tasks.dirs]\nrun = \"mkdir -p corpus\"\n"
                .into(),
            mise_ci_toml: "[tasks.audit]\nrun = \"cargo update -p regex && cargo audit\"\n".into(),
            mise_dev_toml: String::new(),
            workflows: vec![(
                ".github/workflows/ci.yml".into(),
                "on:\n  push:\n    paths: [crates/**, \"!crates/app-web/**\"]\n\
                     jobs:\n  test:\n    steps:\n      - run: cargo nextest run --exclude app-web\n"
                    .into(),
            )],
            deny_toml: String::new(),
            audit_toml: String::new(),
            osv_scanner_toml: String::new(),
            release_toml: String::new(),
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
            "ci" => file(&mut s.workflows, ".github/workflows/ci.yml"),
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
            root | "crates/*" | "crates/*", "tools" | members are not ["crates/*"] && members are not ["crates/*"]
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
            root | "crates/*"] | "crates/*"]\nexclude = ["crates/lib"] | excludes a path under crates/ && excludes a path under crates/
        "#,
        );
    }

    /// `run`'s rule set has both rules of this module, after the script's.
    #[test]
    fn all_runs_these_rules() {
        let mut s = base();
        s.mise_toml = s.mise_toml.replacen("-p lib", "-p nope", 1);
        field(&mut s, "app")
            .push_str("[dependencies]\nlib = { path = \"../lib\", features = [\"testing\"] }\n");
        let found: Vec<String> = super::super::all(&s)
            .findings
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            found.iter().any(|f| f.contains("cargo -p nope")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|f| f.contains("[dependencies] lib enables")),
            "{found:?}"
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
