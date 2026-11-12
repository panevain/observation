//! `release plan [--version V] [--prerelease dev]`: the next release version, as
//! `scripts/release-plan.py plan` picked it. Prints `version=`, `kind=`, `latest=` and `reason=`
//! lines (version empty when there is nothing to release). Without `--version`, CHANGELOG.md's
//! `[Unreleased]` section decides:
//!
//! - Added, Changed, Deprecated or Removed entries: minor (Removed is major from 1.0.0);
//! - only Fixed or Security entries: patch;
//! - no entries, but shipped files changed since the latest release: patch;
//! - otherwise nothing, unless a pre-release is asked for (then patch).
//!
//! The bump applies to the latest stable tag, or develop's version when that is higher; with no
//! stable tag, develop's version. `--prerelease dev` gives `X.Y.Z-dev.N`, N one more than the
//! highest such tag. The reason texts are the script's, including "since v" with no release.
//!
//! Where the script failed, this fails too; the differences, all failures, are listed in
//! `release-plan/cases.toml` (`differs`).

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use super::changelog::{NOT_SHIPPED, latest_stable};
use crate::core::changelog::{Core, core, semver, unreleased};
use crate::core::py::{is_space, universal_newlines};
use crate::ports::Git;
use crate::{Error, Result};

static WORKSPACE_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\[workspace\.package\]\n(?:.+\n)*?version = "([^"]+)""#)
        .unwrap_or_else(|e| panic!("{e}"))
});

const MINOR: [&str; 4] = ["Added", "Changed", "Deprecated", "Removed"];
const PATCH: [&str; 2] = ["Fixed", "Security"];

/// What the plan prints, in this order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub version: String,
    pub kind: String,
    pub latest: String,
    pub reason: String,
}

impl Plan {
    pub fn lines(&self) -> String {
        format!(
            "version={}\nkind={}\nlatest={}\nreason={}\n",
            self.version, self.kind, self.latest, self.reason
        )
    }
}

fn read_text(repo: &Path, name: &str) -> Result<String> {
    let bytes = std::fs::read(repo.join(name)).map_err(|e| Error::new(format!("{name}: {e}")))?;
    String::from_utf8(bytes)
        .map(|t| universal_newlines(&t))
        .map_err(|_| Error::new(format!("{name} is not UTF-8")))
}

/// Every `vX.Y.Z[-pre]` tag, without the v.
fn tags(g: &dyn Git) -> Result<Vec<String>> {
    let run = g.run(&["tag", "-l", "v*"])?;
    if run.code != 0 {
        return Err(Error::new(format!("git tag -l v* failed ({})", run.code)));
    }
    let text = String::from_utf8(run.stdout)
        .map_err(|_| Error::new("git tag -l v* printed text that is not UTF-8"))?;
    let mut out = Vec::new();
    for tag in text.split(is_space).filter(|t| !t.is_empty()) {
        let name: String = tag.chars().skip(1).collect();
        if let Some(parsed) = semver(&name) {
            parsed?;
            out.push(name);
        }
    }
    Ok(out)
}

fn fmt((x, y, z): Core) -> String {
    format!("{x}.{y}.{z}")
}

/// Whether files that reach the image changed since `v<latest>`: `git diff --quiet` exits 1.
fn shipped_changes(g: &dyn Git, latest: &str) -> Result<bool> {
    if latest.is_empty() {
        return Ok(true);
    }
    let from = format!("v{latest}");
    let mut args = vec!["diff", "--quiet", &from, "HEAD", "--", "."];
    args.extend(NOT_SHIPPED);
    match g.run(&args)?.code {
        0 => Ok(false),
        1 => Ok(true),
        code => Err(Error::new(format!(
            "git {} failed ({code})",
            args.join(" ")
        ))),
    }
}

/// The plan for the repository at `repo`. `version` and `prerelease` are the flags (empty:
/// not given).
pub fn plan(repo: &Path, version: &str, prerelease: &str, g: &dyn Git) -> Result<Plan> {
    let latest = latest_stable(g)?;
    let taken = tags(g)?;
    let mut out = Plan {
        latest: latest.clone(),
        ..Plan::default()
    };
    if !version.is_empty() {
        let given = core(version)?;
        if taken.iter().any(|t| t == version) {
            return Err(Error::new(format!("v{version} already exists")));
        }
        if !latest.is_empty() && given <= core(&latest)? {
            return Err(Error::new(format!(
                "{version} is not newer than the latest release v{latest}"
            )));
        }
        out.version = version.to_owned();
        out.kind = "explicit".into();
        out.reason = "version given".into();
        return Ok(out);
    }

    let counts = unreleased(&read_text(repo, "CHANGELOG.md")?)?.counts;
    let manifest = read_text(repo, "Cargo.toml")?;
    let develop = WORKSPACE_VERSION
        .captures(&manifest)
        .and_then(|c| c.get(1))
        .ok_or_else(|| Error::new("no version in Cargo.toml [workspace.package]"))?;
    let develop = core(develop.as_str())?;
    let has = |set: &[&str]| -> Vec<&str> {
        counts
            .keys()
            .map(String::as_str)
            .filter(|k| set.contains(k))
            .collect()
    };
    let (minor, patch) = (has(&MINOR), has(&PATCH));
    let (kind, mut reason) = if !minor.is_empty() {
        let major = !latest.is_empty() && core(&latest)?.0 >= 1 && counts.contains_key("Removed");
        let kind = if major { "major" } else { "minor" };
        (kind, format!("CHANGELOG has {} entries", minor.join(", ")))
    } else if !patch.is_empty() {
        (
            "patch",
            format!("CHANGELOG has only {} entries", patch.join(", ")),
        )
    } else if shipped_changes(g, &latest)? {
        (
            "patch",
            format!("no CHANGELOG entries, but shipped files changed since v{latest}"),
        )
    } else if !prerelease.is_empty() {
        (
            "patch",
            "nothing changed (pre-release requested anyway)".to_owned(),
        )
    } else {
        out.reason = format!(
            "nothing to release: no CHANGELOG entries and no shipped changes since v{latest}"
        );
        return Ok(out);
    };

    let target = if latest.is_empty() {
        reason.push_str("; first release, from Cargo.toml");
        develop
    } else {
        let (x, y, z) = core(&latest)?;
        let bump = |n: u64| {
            n.checked_add(1)
                .ok_or_else(|| Error::new(format!("no release after {latest}")))
        };
        let bumped = match kind {
            "major" => (bump(x)?, 0, 0),
            "minor" => (x, bump(y)?, 0),
            _ => (x, y, bump(z)?),
        };
        bumped.max(develop)
    };
    let mut version = fmt(target);
    let mut kind = kind.to_owned();
    if !prerelease.is_empty() {
        let prefix = format!("{version}-{prerelease}.");
        let mut highest = 0u64;
        for t in &taken {
            let Some(n) = t.strip_prefix(&prefix) else {
                continue;
            };
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let n = n
                .parse::<u64>()
                .map_err(|_| Error::new(format!("pre-release number too large: v{t}")))?;
            highest = highest.max(n);
        }
        let next = highest
            .checked_add(1)
            .ok_or_else(|| Error::new(format!("no pre-release after {prefix}{highest}")))?;
        version = format!("{prefix}{next}");
        kind = prerelease.to_owned();
    }
    if taken.contains(&version) {
        return Err(Error::new(format!(
            "v{version} already exists; pass a version"
        )));
    }
    out.version = version;
    out.kind = kind;
    out.reason = reason;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::time::Timestamp;
    use crate::fakes::FixedClock;
    use crate::ports::process::GitCli;
    use serde::Deserialize;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
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

    /// release-plan/cases.toml: one git history per case (see the generator in the PR).
    #[derive(Deserialize)]
    struct Cases {
        case: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        /// `plan` or `changelog`.
        command: String,
        #[serde(default)]
        args: Vec<String>,
        cargo: String,
        /// CHANGELOG.md in the first commit (none when missing).
        changelog: Option<String>,
        #[serde(default)]
        init_tags: Vec<String>,
        #[serde(default)]
        commit: Vec<Commit>,
        /// `stricter` (the script passed, the port fails) or `message` (both fail, with other
        /// output or exit status).
        differs: Option<String>,
    }

    #[derive(Deserialize)]
    struct Commit {
        subject: String,
        files: BTreeMap<String, String>,
        #[serde(default)]
        delete: Vec<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        merge: bool,
        /// With `merge`: a commit on main before the merge, so that it differs from both parents.
        #[serde(default)]
        main_files: BTreeMap<String, String>,
    }

    struct Repo {
        dir: PathBuf,
        commits: u32,
    }

    impl Repo {
        /// `git ARGS`, each commit a second after the last (as the capture did).
        fn git(&mut self, args: &[&str]) {
            self.commits += 1;
            let date = format!("@{} +0000", 1_700_000_000 + self.commits);
            let out = ok(Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(args)
                .envs(ENV)
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .output());
            assert!(out.status.success(), "git {args:?}: {out:?}");
        }

        fn write(&self, files: &BTreeMap<String, String>) {
            for (path, text) in files {
                let path = self.dir.join(path);
                ok(fs::create_dir_all(ok(path.parent().ok_or("no parent"))));
                ok(fs::write(path, text));
            }
        }

        fn commit(&mut self, subject: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "--allow-empty-message", "-m", subject]);
        }
    }

    fn build(dir: &Path, case: &Case) {
        let mut repo = Repo {
            dir: dir.to_owned(),
            commits: 0,
        };
        repo.git(&["init", "-q", "-b", "main"]);
        let mut files = BTreeMap::from([
            ("Cargo.toml".to_owned(), case.cargo.clone()),
            ("src/lib.rs".to_owned(), "// init\n".to_owned()),
        ]);
        if let Some(text) = &case.changelog {
            files.insert("CHANGELOG.md".into(), text.clone());
        }
        repo.write(&files);
        repo.commit("init");
        for t in &case.init_tags {
            repo.git(&["tag", t]);
        }
        for c in &case.commit {
            if c.merge {
                repo.git(&["switch", "-q", "-c", "side"]);
            }
            repo.write(&c.files);
            for p in &c.delete {
                ok(fs::remove_file(dir.join(p)));
            }
            repo.commit(&c.subject);
            if c.merge {
                repo.git(&["switch", "-q", "main"]);
                if !c.main_files.is_empty() {
                    repo.write(&c.main_files);
                    repo.commit("Meanwhile on main");
                }
                repo.git(&["merge", "-q", "--no-ff", "-m", "Merge side", "side"]);
                repo.git(&["branch", "-q", "-D", "side"]);
            }
            for t in &c.tags {
                repo.git(&["tag", t]);
            }
        }
    }

    /// The golden's sections by case name: `exit N`, stdout, stderr, CHANGELOG.md after (up to
    /// the `-- end` line, byte for byte).
    fn sections(golden: &str) -> BTreeMap<String, [String; 4]> {
        let mut out = BTreeMap::new();
        for block in golden.split("-- end\n").filter(|b| !b.is_empty()) {
            let block = block.strip_prefix("== ").unwrap_or(block);
            let (name, rest) = block.split_once('\n').unwrap_or((block, ""));
            let (code, rest) = rest.split_once('\n').unwrap_or((rest, ""));
            let rest = rest.strip_prefix("-- stdout\n").unwrap_or(rest);
            let (stdout, rest) = rest.split_once("-- stderr\n").unwrap_or((rest, ""));
            let (stderr, after) = rest.split_once("-- CHANGELOG.md\n").unwrap_or((rest, ""));
            out.insert(
                name.to_owned(),
                [code, stdout, stderr, after].map(str::to_owned),
            );
        }
        out
    }

    /// One case, in-process on its repository: exit status, stdout, stderr, CHANGELOG.md after
    /// (changelog only). The flags are read as the CLI reads them; a usage error is exit 2.
    fn run_case(case: &Case) -> (u8, String, String, String) {
        let tmp = ok(tempfile::tempdir());
        let dir = tmp.path().join("repo");
        ok(fs::create_dir(&dir));
        build(&dir, case);
        let mut git = GitCli::new(&dir);
        git.env = ENV.iter().map(|&(k, v)| (k.into(), v.into())).collect();
        let flag = |name: &str| {
            case.args
                .iter()
                .position(|a| a == name)
                .map(|i| case.args[i + 1].clone())
        };
        let clock = FixedClock(ok(Timestamp::parse("2026-01-05T23:30:00Z").ok_or("time")));
        let prerelease = flag("--prerelease").unwrap_or_default();
        let result = match (case.command.as_str(), flag("--version")) {
            ("plan", _) if !["", "dev"].contains(&prerelease.as_str()) => None,
            ("plan", version) => {
                Some(plan(&dir, &version.unwrap_or_default(), &prerelease, &git).map(|p| p.lines()))
            }
            ("changelog", Some(version)) => {
                Some(super::super::changelog::run(&dir, &version, &git, &clock))
            }
            ("changelog", None) => None,
            (other, _) => panic!("unknown command {other}"),
        };
        let (code, stdout, stderr) = match result {
            None => (2, String::new(), "usage\n".to_owned()),
            Some(Ok(out)) => (0, out, String::new()),
            Some(Err(e)) => (1, String::new(), format!("{e}\n")),
        };
        let after = match case.command.as_str() {
            "changelog" => fs::read_to_string(dir.join("CHANGELOG.md")).unwrap_or_default(),
            _ => String::new(),
        };
        (code, stdout, stderr, after)
    }

    /// Every case of release-plan/cases.toml against what scripts/release-plan.py did with it
    /// (release-plan/golden): where the script failed, the port fails, with the same exit
    /// status and output unless listed as `differs = "message"`; where it passed, the port gives
    /// the same stdout, stderr and CHANGELOG.md, unless listed as `differs = "stricter"`, where it
    /// fails.
    #[test]
    fn matches_the_script_and_never_passes_where_it_failed() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/release-plan");
        let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(
            dir.join("cases.toml"),
        ))));
        let want = sections(&ok(fs::read_to_string(dir.join("golden"))));
        assert_eq!(
            want.len(),
            cases.case.len(),
            "cases and golden differ in length"
        );
        let mut problems = Vec::new();
        for case in &cases.case {
            let (code, stdout, stderr, after) = run_case(case);
            let Some([sh_code, sh_stdout, sh_stderr, sh_after]) = want.get(&case.name) else {
                problems.push(format!("{}: not in the golden", case.name));
                continue;
            };
            let same = *sh_code == format!("exit {code}")
                && stdout == *sh_stdout
                && stderr == *sh_stderr
                && after == *sh_after;
            let problem = match (sh_code.as_str(), code, case.differs.as_deref()) {
                ("exit 0", 0, Some("stricter")) => "passes, but is listed as stricter",
                ("exit 0", 0, _) if !same => "passes with different output",
                ("exit 0", 0, _) => "",
                ("exit 0", _, Some("stricter"))
                    if case.command == "changelog"
                        && after != case.changelog.clone().unwrap_or_default() =>
                {
                    "fails, but changed CHANGELOG.md"
                }
                ("exit 0", _, Some("stricter")) => "",
                ("exit 0", _, _) => "fails where the script passed (not listed as stricter)",
                (_, 0, _) => "PASSES WHERE THE SCRIPT FAILED",
                (_, _, Some("message")) if same => "same output, but listed as differing",
                (_, _, Some("message")) => "",
                (_, _, _) if !same => "fails with output other than the script's",
                _ => "",
            };
            if !problem.is_empty() {
                problems.push(format!(
                    "{}: {problem}\n  script: {sh_code} {sh_stdout:?} {sh_stderr:?} {sh_after:?}\n  \
                     port: exit {code} {stdout:?} {stderr:?} {after:?}",
                    case.name
                ));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
