//! `cargo xtask release version get` against scripts/workspace-version.sh, both run for real:
//! the script as it is in the tree, the port as the built binary, each in its own temporary
//! repository built from the same case. release-tag.yml, which compiles nothing, and
//! release-prep's bot jobs read the version with workspace-version.sh for good, so the two must
//! not drift: same exit status, stdout and stderr (paths aside), and where the script fails the
//! port fails. (`version set` was compared with set-version.sh here until that script's last
//! caller moved to the port; the golden in release-version/ still holds what it did.) Also here:
//! the same two readers on this repository's Cargo.toml, and release-tag's tag kind (a shell
//! `case` in the workflow), run by bash.
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

#[derive(Deserialize)]
struct Cases {
    template: BTreeMap<String, BTreeMap<String, String>>,
    case: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    template: Option<String>,
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    raw: bool,
    stdin: Option<String>,
    github_actions: Option<String>,
    cargo_fails: Option<usize>,
    differs: Option<String>,
    /// The port gets `--` before the arguments, as the workflows pass a value (the script got
    /// the arguments alone).
    #[serde(default)]
    separator: bool,
}

fn write(path: &Path, bytes: &[u8]) {
    ok(fs::create_dir_all(ok(path.parent().ok_or("no parent"))));
    ok(fs::write(path, bytes));
}

fn populate(cases: &Cases, case: &Case, dir: &Path) {
    for (path, text) in case.template.iter().flat_map(|t| &cases.template[t]) {
        write(&dir.join(path), text.as_bytes());
    }
    for (path, text) in &case.files {
        let bytes: Vec<u8> = if case.raw {
            text.chars()
                .map(|c| ok(u8::try_from(u32::from(c))))
                .collect()
        } else {
            text.as_bytes().to_vec()
        };
        write(&dir.join(path), &bytes);
    }
}

/// Exit status, stdout, stderr (the repository's path as `$REPO`), and the three files.
fn run(mut cmd: Command, case: &Case, dir: &Path) -> (i32, String, String, Vec<Option<Vec<u8>>>) {
    cmd.current_dir(dir)
        .env_remove("GITHUB_ACTIONS")
        .env("LC_ALL", "C.UTF-8")
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(v) = &case.github_actions {
        cmd.env("GITHUB_ACTIONS", v);
    }
    let mut child = ok(cmd.spawn());
    let input = case.stdin.clone().unwrap_or_default();
    let mut stdin = ok(child.stdin.take().ok_or("no stdin"));
    ok(stdin.write_all(input.as_bytes()));
    drop(stdin);
    let out = ok(child.wait_with_output());
    let real = ok(fs::canonicalize(dir));
    // Without cargo's note that another cargo holds the package cache, which only one side
    // may see.
    let clean = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .replace(&real.display().to_string(), "$REPO")
            .replace(&dir.display().to_string(), "$REPO")
            .split_inclusive('\n')
            .filter(|line| !line.contains("Blocking waiting for file lock"))
            .collect::<String>()
    };
    let files = ["Cargo.toml", "Cargo.lock", "fuzz/Cargo.lock"]
        .map(|f| fs::read(dir.join(f)).ok())
        .to_vec();
    (
        out.status.code().unwrap_or(-1),
        clean(&out.stdout),
        clean(&out.stderr),
        files,
    )
}

#[test]
fn port_and_scripts_agree_on_every_case() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures = manifest.join("tests/fixtures/release-version/cases.toml");
    let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(fixtures))));
    let scripts = manifest.join("../scripts");
    let mut problems = Vec::new();
    let mut compared = 0;
    // A stand-in cargo that fails cannot be had with the real one; the golden covers those.
    let gets = cases.case.iter().filter(|c| c.command == "get");
    for case in gets.filter(|c| c.cargo_fails.is_none()) {
        let (a, b) = (ok(tempfile::tempdir()), ok(tempfile::tempdir()));
        populate(&cases, case, a.path());
        populate(&cases, case, b.path());
        let to = a.path().join("scripts/workspace-version.sh");
        write(&to, b"");
        ok(fs::copy(scripts.join("workspace-version.sh"), &to));
        let mut sh = Command::new("scripts/workspace-version.sh");
        sh.args(&case.args);
        let mut port = Command::new(env!("CARGO_BIN_EXE_xtask"));
        port.arg("--repo")
            .arg(b.path())
            .args(["release", "version", case.command.as_str()]);
        if case.separator {
            port.arg("--");
        }
        port.args(&case.args);
        let (sc, so, se, sf) = run(sh, case, a.path());
        let (pc, po, pe, pf) = run(port, case, b.path());
        let same = sc == pc && so == po && se == pe && sf == pf;
        let differs = case.differs.as_deref();
        let problem = match (sc == 0, pc == 0, differs) {
            (true, true, Some("stricter")) => "passes, but is listed as stricter",
            (true, true, _) if !same => "passes with different output",
            (true, false, Some("stricter")) => "",
            (true, false, _) => "fails where the script passed",
            (false, true, _) => "PASSES WHERE THE SCRIPT FAILED",
            (false, false, Some(_)) => "",
            (false, false, None) if !same => "fails with output other than the script's",
            _ => "",
        };
        compared += 1;
        if !problem.is_empty() {
            problems.push(format!(
                "{}: {problem}\n  script: exit {sc} {so:?} {se:?}\n  port: exit {pc} {po:?} {pe:?}",
                case.name
            ));
        }
    }
    assert!(compared > 30, "only {compared} cases ran");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// release-tag.yml reads the version it tags with workspace-version.sh; the other workflows read
/// it with the port. Both must give the same answer on this repository.
#[test]
fn this_repository_reads_the_same_version_with_both() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let read = |mut cmd: Command| {
        let out = ok(cmd.current_dir(&repo).env_remove("GITHUB_ACTIONS").output());
        (out.status.code(), out.stdout, out.stderr)
    };
    let script = read(Command::new("scripts/workspace-version.sh"));
    let mut port = Command::new(env!("CARGO_BIN_EXE_xtask"));
    port.arg("--repo")
        .arg(&repo)
        .args(["release", "version", "get"]);
    let port = read(port);
    assert_eq!(
        script.0,
        Some(0),
        "{:?}",
        String::from_utf8_lossy(&script.2)
    );
    assert_eq!(script, port);
}

/// The tag kind in release-tag.yml's plan job (a `case` on the merged pull request's head
/// branch), cut from the workflow file and run by bash: hotfix/* waits for an approval,
/// release/* is tagged at once, anything else fails the job.
#[test]
fn release_tag_kind_reads_the_head_branch() {
    let wf = ok(fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/release-tag.yml"),
    ));
    let lines: Vec<&str> = wf.lines().map(str::trim).collect();
    let start = ok(lines
        .iter()
        .position(|l| *l == r#"case "${head}" in"#)
        .ok_or("no case on the head branch in release-tag.yml"));
    let end = ok(lines[start..]
        .iter()
        .position(|l| *l == "esac")
        .ok_or("no esac"));
    let block = lines[start..=start + end].join("\n");
    let script = format!("set -euo pipefail\nhead=\"$1\"\n{block}\necho \"kind=${{kind}}\"\n");
    for (head, want) in [
        ("hotfix/1.2.1", Some("hotfix")),
        ("hotfix/", Some("hotfix")),
        ("release/1.3.0", Some("release")),
        ("release/x/y", Some("release")),
        ("", None),
        ("hotfix", None),
        ("release", None),
        ("release-1.3.0", None),
        ("xhotfix/1.2.1", None),
        ("feature/release/1.3.0", None),
        ("main", None),
        ("Release/1.3.0", None),
    ] {
        let out = ok(Command::new("bash")
            .args(["--noprofile", "--norc", "-c", &script, "_", head])
            .env("GITHUB_SHA", "0123abc")
            .output());
        let stdout = String::from_utf8_lossy(&out.stdout);
        match want {
            Some(kind) => {
                assert!(out.status.success(), "{head:?}: {stdout}");
                assert_eq!(stdout, format!("kind={kind}\n"), "{head:?}");
            }
            None => {
                assert_eq!(out.status.code(), Some(1), "{head:?}: {stdout}");
                assert_eq!(
                    stdout,
                    format!(
                        "::error::0123abc is not a merge of a release/* or hotfix/* PR (head \
                         '{head}'); tag it by hand if it should be released.\n"
                    ),
                    "{head:?}"
                );
            }
        }
    }
}
