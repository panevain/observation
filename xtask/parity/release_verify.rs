//! release.yml's `verify` step before `release verify-tag` (kept below as it was) against the
//! step as it is now (cut out of release.yml), both run with bash and the built binary in
//! scenario repositories: the same stdout, stderr and exit status, except where a scenario says
//! how they differ.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

/// The step's `run` text before this command.
const OLD: &str = r#"set -euo pipefail
version="$("${XTASK}" release version get)"
[ "v${version}" = "${TAG}" ] || { echo "::error::tag ${TAG} does not match the workspace version ${version}"; exit 1; }
if [[ "${version}" == *-* ]]; then
  # Pre-releases stay off main, so that main is always the latest stable release.
  git merge-base --is-ancestor "${GITHUB_SHA}^" origin/develop || { echo "::error::${TAG}'s parent is not on develop"; exit 1; }
  # The allowed files (.config/release.toml's prerelease list) come from the parent,
  # never from the tag being checked, which could otherwise allow its own changes.
  config="${RUNNER_TEMP}/release.toml"
  git show "${GITHUB_SHA}^:.config/release.toml" > "${config}"
  other="$(git diff --name-only "${GITHUB_SHA}^" "${GITHUB_SHA}" | "${XTASK}" release unexpected-files prerelease --config "${config}")"
  [ -z "${other}" ] || { echo "::error::${TAG} changes more than the version: ${other}"; exit 1; }
  echo "${TAG}: version ${version}, a version bump on top of develop"
else
  git merge-base --is-ancestor "${GITHUB_SHA}" origin/main || { echo "::error::${TAG} is not on main"; exit 1; }
  echo "${TAG}: version ${version}, on main"
fi
"#;

/// The step's `run` text in release.yml now.
fn new_step() -> String {
    let wf = ok(fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/release.yml"),
    ));
    let mut lines = wf.lines().skip_while(|l| {
        l.trim() != "- name: Tag matches the version and is on main (pre-releases on develop)"
    });
    let run = lines
        .find_map(|l| {
            l.trim()
                .strip_prefix("run: '")
                .and_then(|r| r.strip_suffix('\''))
        })
        .unwrap_or_else(|| panic!("no one-line run in the verify step"));
    run.replace("''", "'")
}

const ENV: [(&str, &str); 7] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("LC_ALL", "C.UTF-8"),
    ("GIT_AUTHOR_NAME", "a"),
    ("GIT_AUTHOR_EMAIL", "a@example.com"),
    ("GIT_COMMITTER_NAME", "a"),
    ("GIT_COMMITTER_EMAIL", "a@example.com"),
];

fn git(dir: &Path, args: &[&str]) -> String {
    let out = ok(Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(ENV)
        .output());
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

const LIST: &str = "[allowed-files]\nhotfix = [\"Cargo.toml\", \"README.md\"]\n\
    prerelease = [\"Cargo.toml\", \"Cargo.lock\", \"fuzz/Cargo.lock\"]\n";

/// Writes `files` (an empty text deletes the file) and commits them; returns the commit id.
fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
    for (path, text) in files {
        let path = dir.join(path);
        if text.is_empty() {
            ok(fs::remove_file(path));
        } else {
            ok(fs::create_dir_all(ok(path.parent().ok_or("no parent"))));
            ok(fs::write(path, text));
        }
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "c"]);
    git(dir, &["rev-parse", "HEAD"])
}

fn manifest(version: &str) -> String {
    format!("[workspace.package]\nversion = \"{version}\"\n")
}

/// A checkout of the tagged commit: main and develop as origin's, the commit at HEAD.
struct Scenario {
    name: &'static str,
    tag: &'static str,
    dir: tempfile::TempDir,
    sha: String,
    /// How the new step differs: None (not at all) or `message` (both fail, otherwise).
    differs: Option<&'static str>,
}

/// develop = base (version `from`, the list), main = a stable release on it; then `tagged` on
/// top of `on` ("develop" or "main" or "root") with the version `version`.
fn scenario(
    name: &'static str,
    tag: &'static str,
    on: &str,
    version: &str,
    extra: &[(&str, &str)],
    differs: Option<&'static str>,
) -> Scenario {
    let dir = ok(tempfile::tempdir());
    let d = dir.path();
    git(d, &["init", "-q", "-b", "develop"]);
    let base = if on == "root" {
        None
    } else {
        Some(commit(
            d,
            &[
                ("Cargo.toml", &manifest("1.2.0")),
                ("Cargo.lock", "lock 1\n"),
                ("README.md", "readme\n"),
                (".config/release.toml", LIST),
            ],
        ))
    };
    if let Some(base) = &base {
        git(d, &["update-ref", "refs/remotes/origin/develop", base]);
        git(d, &["update-ref", "refs/remotes/origin/main", base]);
    }
    let manifest = manifest(version);
    let mut files: Vec<(&str, &str)> = vec![("Cargo.toml", &manifest), ("Cargo.lock", "lock 2\n")];
    files.extend_from_slice(extra);
    let sha = commit(d, &files);
    if on == "main" {
        git(d, &["update-ref", "refs/remotes/origin/main", &sha]);
    }
    Scenario {
        name,
        tag,
        dir,
        sha,
        differs,
    }
}

fn run(script: &str, s: &Scenario) -> (Option<i32>, String, String) {
    let temp = ok(tempfile::tempdir());
    // In CI the binary's default repository is the checkout; here it is the scenario's.
    let xtask = temp.path().join("xtask");
    let wrapper = format!(
        "#!/bin/sh\nexec '{}' --repo \"$PWD\" \"$@\"\n",
        env!("CARGO_BIN_EXE_xtask")
    );
    ok(fs::write(&xtask, wrapper));
    ok(fs::set_permissions(
        &xtask,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    ));
    let out = ok(Command::new("bash")
        .args(["--noprofile", "--norc", "-c", script])
        .current_dir(s.dir.path())
        .envs(ENV)
        .env("XTASK", &xtask)
        .env("TAG", s.tag)
        .env("GITHUB_SHA", &s.sha)
        .env("GITHUB_ACTIONS", "true")
        .env("RUNNER_TEMP", temp.path())
        .output());
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.code(), text(&out.stdout), text(&out.stderr))
}

/// The scenarios that pass; every other one fails.
const PASS: [&str; 4] = [
    "stable on main",
    "pre-release on develop",
    "pre-release also on main",
    "pre-release with fuzz lock",
];

#[test]
fn the_step_checks_as_it_did_and_never_passes_where_it_failed() {
    let pre = "1.3.0-dev.1";
    let own_list = format!("{LIST}# the tag allows its own changes\n").replace(
        "\"fuzz/Cargo.lock\"]",
        "\"fuzz/Cargo.lock\", \"README.md\", \".config/release.toml\"]",
    );
    let scenarios = [
        scenario("stable on main", "v1.3.0", "main", "1.3.0", &[], None),
        scenario(
            "stable on develop only",
            "v1.3.0",
            "develop",
            "1.3.0",
            &[],
            None,
        ),
        scenario(
            "tag is not the version",
            "v1.3.1",
            "main",
            "1.3.0",
            &[],
            None,
        ),
        scenario("bad workspace version", "v1.3", "main", "1.3", &[], None),
        scenario(
            "pre-release on develop",
            "v1.3.0-dev.1",
            "develop",
            pre,
            &[],
            None,
        ),
        scenario(
            "pre-release also on main",
            "v1.3.0-dev.1",
            "main",
            pre,
            &[],
            None,
        ),
        scenario(
            "pre-release with fuzz lock",
            "v1.3.0-dev.1",
            "develop",
            pre,
            &[("fuzz/Cargo.lock", "f\n")],
            None,
        ),
        scenario(
            "pre-release changes README",
            "v1.3.0-dev.1",
            "develop",
            pre,
            &[("README.md", "new\n")],
            None,
        ),
        scenario(
            "pre-release deletes README",
            "v1.3.0-dev.1",
            "develop",
            pre,
            &[("README.md", "")],
            None,
        ),
        scenario(
            "pre-release allows its own changes",
            "v1.3.0-dev.1",
            "develop",
            pre,
            &[("README.md", "new\n"), (".config/release.toml", &own_list)],
            None,
        ),
        scenario(
            "pre-release as a root commit",
            "v1.3.0-dev.1",
            "root",
            pre,
            &[],
            None,
        ),
        scenario(
            "pre-release tag on stable",
            "v1.3.0",
            "develop",
            pre,
            &[],
            None,
        ),
    ];
    let mut problems = Vec::new();
    let mut check = |s: &Scenario| {
        let old = run(OLD, s);
        let new = run(&new_step(), s);
        let same = match s.differs {
            None => old == new,
            Some("message") => old.0 != Some(0) && new.0 != Some(0),
            Some(other) => panic!("unknown differs {other}"),
        };
        let passes = PASS.contains(&s.name);
        if !same || (old.0 != Some(0) && new.0 == Some(0)) || passes != (new.0 == Some(0)) {
            problems.push(format!("{}:\n old {old:?}\n new {new:?}", s.name));
        }
    };
    for s in &scenarios {
        check(s);
    }
    // A parent without .config/release.toml: git show fails (128 in the old step, 1 now).
    let s = scenario(
        "parent without the list",
        "v1.3.0-dev.1",
        "develop",
        pre,
        &[],
        Some("message"),
    );
    let parent = git(s.dir.path(), &["rev-parse", "HEAD^"]);
    git(s.dir.path(), &["checkout", "-q", &parent]);
    let bare = commit(s.dir.path(), &[(".config/release.toml", "")]);
    git(
        s.dir.path(),
        &["update-ref", "refs/remotes/origin/develop", &bare],
    );
    let sha = commit(s.dir.path(), &[("Cargo.toml", &manifest(pre))]);
    let s = Scenario { sha, ..s };
    check(&s);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
