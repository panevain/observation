//! `cargo xtask security-patch check` against `scripts/security-patch.py check`, both run for
//! real on every case of `tests/fixtures/security-patch/cases.toml`: the script as it is in the
//! tree (hotfix.yml still runs it), the port as the built binary, each in its own copy of the
//! case, with the same stub `cargo` and `trivy` first on `PATH`. The stubs answer from the case
//! (recorded `cargo audit --json` reports, the lock a `cargo update -p` leaves, Trivy reports)
//! and log every call.
//!
//! Each case says what it expects: `same` (exit status, stdout, stderr, report.json,
//! summary.md, the Cargo.lock left behind and the calls are identical), `crash` (the script
//! fails with a traceback; the port must fail too) or `stricter` (the script passes and the
//! port fails: the differences the port declares).
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Deserialize;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cases {
    defaults: Defaults,
    case: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    version: String,
    lock: String,
    dockerfile: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    #[serde(default = "same")]
    expect: String,
    version: Option<String>,
    /// The lock (default: the defaults'); "-" for none.
    lock: Option<String>,
    /// The Dockerfile (default: the defaults'); "-" for none.
    dockerfile: Option<String>,
    /// What the stubs answer, handed to them as JSON.
    #[serde(default)]
    audit: Vec<toml::Value>,
    #[serde(default)]
    update: toml::Table,
    #[serde(default)]
    trivy: toml::Table,
}

fn same() -> String {
    "same".into()
}

/// The stub `cargo` and `trivy` (one script under both names).
const STUB: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, sys
stub = pathlib.Path(os.environ["SECURITY_PATCH_STUB"])
case = json.loads((stub / "case.json").read_text())
tool, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
with open(stub / "calls.log", "a") as f:
    f.write(json.dumps([tool, pathlib.Path.cwd().name if tool == "cargo" else ""] + args) + "\n")
def answer(a):
    sys.stdout.buffer.write(a.get("stdout", "").encode() + (b"\xff" if a.get("invalid_utf8") else b""))
    sys.stderr.write(a.get("stderr", ""))
    sys.exit(a.get("code", 0))
if tool == "cargo" and args[:1] == ["audit"]:
    count = stub / "audits"
    n = int(count.read_text()) if count.exists() else 0
    count.write_text(str(n + 1))
    audits = case["audit"] or [{"stdout": '{"vulnerabilities": {"found": false, "count": 0, "list": []}}'}]
    answer(audits[min(n, len(audits) - 1)])
if tool == "cargo" and args[:2] == ["update", "-p"]:
    u = case["update"].get(args[2], {})
    if "lock" in u:
        pathlib.Path("Cargo.lock").write_text(u["lock"])
    answer(u)
if tool == "trivy":
    answer(case["trivy"].get(args[-1], {"code": 1, "stderr": "FATAL\tunable to find the specified image\n"}))
sys.exit(99)
"#;

/// What one side left: exit status, stdout, stderr, report.json, summary.md, Cargo.lock, calls.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    files: Vec<Option<String>>,
}

fn read(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Sets up the case in a fresh directory and runs `command` there (with `repo` and `out`
/// relative to it).
fn run(defaults: &Defaults, case: &Case, port: bool) -> Run {
    let dir = ok(tempfile::tempdir());
    let d = dir.path();
    let (repo, bin, stub) = (d.join("repo"), d.join("bin"), d.join("stub"));
    for p in [&repo, &bin, &stub] {
        ok(fs::create_dir_all(p));
    }
    for (name, text, default) in [
        ("Cargo.lock", &case.lock, &defaults.lock),
        ("Dockerfile", &case.dockerfile, &defaults.dockerfile),
    ] {
        let text = text.as_ref().unwrap_or(default);
        if text != "-" {
            ok(fs::write(repo.join(name), text));
        }
    }
    let answers = serde_json::json!({
        "audit": case.audit, "update": case.update, "trivy": case.trivy,
    });
    ok(fs::write(stub.join("case.json"), answers.to_string()));
    for tool in ["cargo", "trivy"] {
        let path = bin.join(tool);
        ok(fs::write(&path, STUB));
        use std::os::unix::fs::PermissionsExt as _;
        ok(fs::set_permissions(
            &path,
            fs::Permissions::from_mode(0o755),
        ));
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut cmd = if port {
        let mut c = Command::new(env!("CARGO_BIN_EXE_xtask"));
        c.args(["security-patch", "check"]);
        c
    } else {
        let mut c = Command::new("python3");
        c.arg("-I")
            .arg(root.join("scripts/security-patch.py"))
            .arg("check");
        c
    };
    let version = case.version.as_ref().unwrap_or(&defaults.version);
    let path = std::env::join_paths(
        std::iter::once(bin.clone()).chain(std::env::split_paths(&ok(std::env::var("PATH")))),
    );
    let out: Output = ok(cmd
        .args(["--repo", "repo", "--version", version, "--out", "out"])
        .current_dir(d)
        .env("PATH", ok(path))
        .env("SECURITY_PATCH_STUB", &stub)
        .env("LC_ALL", "C.UTF-8")
        .env_remove("PYTHONWARNINGS")
        .output());
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        files: [
            "out/report.json",
            "out/summary.md",
            "repo/Cargo.lock",
            "stub/calls.log",
        ]
        .map(|f| read(&d.join(f)))
        .to_vec(),
    }
}

#[test]
fn port_and_script_agree_on_every_case() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/security-patch/cases.toml");
    let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(path))));
    let mut failures = Vec::new();
    let mut counts = [0; 3];
    for case in &cases.case {
        let script = run(&cases.defaults, case, false);
        let port = run(&cases.defaults, case, true);
        let crashed = script.stderr.contains("Traceback (most recent call last)");
        let verdict = match case.expect.as_str() {
            "same" if !crashed && script == port => Ok(0),
            "crash" if crashed && port.code != Some(0) => Ok(1),
            "stricter" if script.code == Some(0) && port.code != Some(0) => Ok(2),
            "same" | "crash" | "stricter" => Err(()),
            other => panic!("{}: unknown expect {other}", case.name),
        };
        match verdict {
            Ok(i) => counts[i] += 1,
            Err(()) => failures.push(format!(
                "{} (expect {}):\n  script: {script:?}\n  port:   {port:?}",
                case.name, case.expect
            )),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    eprintln!(
        "{} cases: {} same, {} crash, {} stricter",
        cases.case.len(),
        counts[0],
        counts[1],
        counts[2]
    );
}
