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

    /// How release-plan/golden was captured, run here on scripts/release-plan.py as it is in the
    /// tree: in-process, with the clock fixed at the golden's instant, an exception written as
    /// its last line. Arguments: the script, the repository, then the script's own arguments.
    const PY_RUN: &str = r#"
import contextlib, datetime as real_dt, importlib.util, io, sys
class FakeDatetime(real_dt.datetime):
    @classmethod
    def now(cls, tz=None):
        return real_dt.datetime(2026, 1, 5, 23, 30, 0, tzinfo=real_dt.UTC)
class FakeModule:
    datetime = FakeDatetime
    UTC = real_dt.UTC
script, repo, args = sys.argv[1], sys.argv[2], sys.argv[3:]
spec = importlib.util.spec_from_file_location("release_plan", script)
mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
mod.datetime = FakeModule
sys.argv = ["release-plan.py", args[0], "--repo", repo, *args[1:]]
so, se, code = io.StringIO(), io.StringIO(), 0
try:
    with contextlib.redirect_stdout(so), contextlib.redirect_stderr(se):
        mod.main()
except SystemExit as e:
    if e.code is None: code = 0
    elif isinstance(e.code, int): code = e.code
    else: se.write(f"{e.code}\n"); code = 1
except Exception as e:
    se.write(f"{type(e).__name__}: {e}\n"); code = 1
sys.stdout.write(f"exit {code}\n-- stdout\n{so.getvalue()}-- stderr\n{se.getvalue().replace(repo, '$REPO')}")
"#;

    /// scripts/release-plan.py still gives release-plan/golden on every case. prepare runs its
    /// `changelog` on the token path, and the port is tested against that golden, so this keeps
    /// the script in production and the tested port in step.
    #[test]
    fn the_script_in_the_tree_still_gives_the_golden() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/release-plan");
        let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(
            dir.join("cases.toml"),
        ))));
        let want = sections(&ok(fs::read_to_string(dir.join("golden"))));
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/release-plan.py");
        let mut problems = Vec::new();
        for case in &cases.case {
            let tmp = ok(tempfile::tempdir());
            let repo = tmp.path().join("repo");
            ok(fs::create_dir(&repo));
            build(&repo, case);
            let out = ok(Command::new("python3")
                .args(["-I", "-c", PY_RUN])
                .arg(&script)
                .arg(&repo)
                .arg(&case.command)
                .args(&case.args)
                .envs(ENV)
                .output());
            let mut got = String::from_utf8_lossy(&out.stdout).into_owned();
            got.push_str("-- CHANGELOG.md\n");
            if case.command == "changelog" {
                got.push_str(&fs::read_to_string(repo.join("CHANGELOG.md")).unwrap_or_default());
            }
            let [code, stdout, stderr, after] = &want[&case.name];
            let expected =
                format!("{code}\n-- stdout\n{stdout}-- stderr\n{stderr}-- CHANGELOG.md\n{after}");
            if got != expected {
                problems.push(format!(
                    "{}:\n  script now: {got:?}\n  golden:     {expected:?}",
                    case.name
                ));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// The steps of release-prep.yml's bot jobs (prepare, then pretag) that check plan's outputs
    /// in plain shell before using them: each `run` text named `name`, with the KIND it sets.
    fn bot_job_steps(name: &str) -> Vec<(String, String)> {
        let wf = ok(fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/release-prep.yml"),
        ));
        let lines: Vec<&str> = wf.lines().collect();
        let indent = |l: &str| l.len() - l.trim_start().len();
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line.trim() != format!("- name: {name}") {
                continue;
            }
            let mut kind = String::new();
            let mut j = i + 1;
            let run = loop {
                let l = lines[j].trim();
                assert!(!l.starts_with("- "), "no run in the step at line {}", i + 1);
                if let Some(k) = l.strip_prefix("KIND: ") {
                    k.clone_into(&mut kind);
                }
                if l == "run: |" {
                    let body: Vec<&str> = lines[j + 1..]
                        .iter()
                        .take_while(|b| b.trim().is_empty() || indent(b) > indent(lines[j]))
                        .map(|b| b.get(indent(lines[j]) + 2..).unwrap_or(""))
                        .collect();
                    break body.join("\n") + "\n";
                }
                if let Some(quoted) = l.strip_prefix("run: '").and_then(|r| r.strip_suffix('\'')) {
                    break quoted.replace("''", "'");
                }
                j += 1;
            };
            out.push((kind, run));
        }
        out
    }
    /// Runs a check in `dir` as the job would: exit status and stdout.
    fn run_check(script: &str, dir: &Path, vars: &[(&str, &str)]) -> (i32, String) {
        let out = ok(Command::new("bash")
            .args(["--noprofile", "--norc", "-c", script])
            .current_dir(dir)
            .envs(ENV)
            .envs(vars.iter().copied())
            .env("RUNNER_TEMP", dir.join(".git"))
            .env_remove("GITHUB_ACTIONS")
            .output());
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        (out.status.code().unwrap_or(-1), stdout)
    }

    /// What the bot jobs' checkout gives the check: develop as refs/remotes/origin/develop, and
    /// the scripts it runs (copied in, untracked).
    fn as_checked_out(dir: &Path, develop: &str) {
        let scripts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts");
        ok(fs::create_dir_all(dir.join("scripts")));
        for name in ["workspace-version.sh", "release-state.sh"] {
            ok(fs::copy(scripts.join(name), dir.join("scripts").join(name)));
        }
        git_out(dir, &["update-ref", "refs/remotes/origin/develop", develop]);
        // Never committed by a scenario's later commits.
        ok(fs::write(dir.join(".git/info/exclude"), "scripts/\n"));
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = ok(Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .envs(ENV)
            .output());
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// The request a plan answers: the event, then `inputs.version` and `inputs.prerelease`.
    fn request(given: &str, prerelease: &str) -> [(&'static str, String); 3] {
        let event = if given.is_empty() && prerelease.is_empty() {
            "schedule"
        } else {
            "workflow_dispatch"
        };
        [
            ("EVENT", event.to_owned()),
            ("GIVEN", given.to_owned()),
            ("REQUESTED", prerelease.to_owned()),
        ]
    }

    /// prepare and pretag run the same check, and it accepts every version the port plans in the
    /// scenario repositories of release-plan/cases.toml, from the commit it planned on, for the
    /// request it planned for.
    #[test]
    fn the_bot_jobs_accept_every_plan_the_port_makes() {
        let checks = bot_job_steps("Check the plan");
        let kinds: Vec<&str> = checks.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["stable", "prerelease"]);
        assert_eq!(
            checks[0].1, checks[1].1,
            "prepare and pretag check differently"
        );
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/release-plan");
        let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(
            dir.join("cases.toml"),
        ))));
        let mut checked = 0;
        let mut problems = Vec::new();
        for case in cases.case.iter().filter(|c| c.command == "plan") {
            let flag = |name: &str| {
                case.args
                    .iter()
                    .position(|a| a == name)
                    .map(|i| case.args[i + 1].clone())
                    .unwrap_or_default()
            };
            let (given, prerelease) = (flag("--version"), flag("--prerelease"));
            let tmp = ok(tempfile::tempdir());
            let dir = tmp.path().join("repo");
            ok(fs::create_dir(&dir));
            build(&dir, case);
            let mut git = GitCli::new(&dir);
            git.env = ENV.iter().map(|&(k, v)| (k.into(), v.into())).collect();
            let Ok(planned) = plan(&dir, &given, &prerelease, &git) else {
                continue;
            };
            if planned.version.is_empty() || !["", "dev"].contains(&prerelease.as_str()) {
                continue;
            }
            as_checked_out(&dir, "HEAD");
            let sha = git_out(&dir, &["rev-parse", "HEAD"]);
            let kind = match planned.version.contains('-') {
                true => "prerelease",
                false => "stable",
            };
            let req = request(&given, &prerelease);
            let mut vars = vec![("SHA", sha.as_str()), ("VERSION", &planned.version)];
            vars.push(("KIND", kind));
            vars.extend(req.iter().map(|(k, v)| (*k, v.as_str())));
            let (code, stdout) = run_check(&checks[0].1, &dir, &vars);
            if code != 0 {
                problems.push(format!(
                    "{}: {} refused: {stdout}",
                    case.name, planned.version
                ));
            }
            checked += 1;
        }
        assert!(checked > 40, "only {checked} plans checked");
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// Runs each row of `table` (`commit | sha | version | kind | given | prerelease input |
    /// event | refusal`, `{name}` replaced from `names`, `''` for an empty field, `\n` for a
    /// newline; an empty event is the request's own, an empty refusal passes) against the
    /// check in `dir`.
    fn run_forged(
        script: &str,
        dir: &Path,
        names: &[(&str, &str)],
        table: &str,
        problems: &mut Vec<String>,
    ) {
        for row in table.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let mut row = row.to_owned();
            for (name, value) in names {
                row = row.replace(&format!("{{{name}}}"), value);
            }
            let f: Vec<String> = row
                .split('|')
                .map(|c| match c.trim() {
                    "''" => String::new(),
                    c => c.replace(r"\n", "\n"),
                })
                .collect();
            assert_eq!(f.len(), 8, "{row}");
            git_out(dir, &["checkout", "-q", "--detach", &f[0]]);
            let mut req = request(&f[4], &f[5]);
            if !f[6].is_empty() {
                f[6].clone_into(&mut req[0].1);
            }
            let mut vars = vec![("SHA", f[1].as_str()), ("VERSION", &f[2]), ("KIND", &f[3])];
            vars.extend(req.iter().map(|(k, v)| (*k, v.as_str())));
            let got = run_check(script, dir, &vars);
            let want = match f[7].as_str() {
                "" => (0, String::new()),
                error => (1, format!("::error::{error}\n")),
            };
            if got != want {
                problems.push(format!("{row}: {got:?}, not {want:?}"));
            }
        }
    }

    fn cargo(v: &str) -> BTreeMap<String, String> {
        let text = format!("[workspace]\n\n[workspace.package]\nversion = \"{v}\"\n");
        BTreeMap::from([("Cargo.toml".to_owned(), text)])
    }

    /// c1 -- c2 (v0.1.0) -- c3 (develop: version 0.4.0, tag v0.2.0-dev.1) -- c4 (not on
    /// develop). From c3 develop's 0.4.0 is above every bump of v0.1.0 (a major bump starts at
    /// 1.0.0); from c2 develop's version is the latest release's.
    const FORGED_A: &str = r"
        {c3} | {c3} | 0.4.0 | stable | '' | '' | '' | ''
        {c3} | {c3} | 0.4.0 | stable | '' | none | '' | ''
        {c3} | {c3} | 0.4.0-dev.1 | prerelease | '' | dev | '' | ''
        {c3} | {c3} | 0.7.0 | stable | 0.7.0 | '' | '' | ''
        {c3} | {c3} | 0.7.0 | stable | 0.7.0 | dev | '' | ''
        {c3} | {c3} | 0.7.0-rc.1 | prerelease | 0.7.0-rc.1 | '' | '' | ''
        {c2} | {c2} | 0.1.1 | stable | '' | '' | '' | ''
        {c2} | {c2} | 0.2.0 | stable | '' | '' | '' | ''
        {c2} | {c2} | 0.1.1-dev.1 | prerelease | '' | dev | '' | ''
        {c2} | {c2} | 0.2.0-dev.2 | prerelease | '' | dev | '' | ''
        {c1} | {c1} | 0.1.1-dev.1 | prerelease | '' | dev | '' | ''
        {c1} | {c1} | 0.2.0 | stable | '' | '' | '' | {c1} does not contain v0.1.0
        {c3} | abc | 0.4.0 | stable | '' | '' | '' | the plan's sha is not a commit id: 'abc'
        {c3} | {C3} | 0.4.0 | stable | '' | '' | '' | the plan's sha is not a commit id: '{C3}'
        {c3} | '' | 0.4.0 | stable | '' | '' | '' | the plan's sha is not a commit id: ''
        {c3} | {c2} | 0.4.0 | stable | '' | '' | '' | HEAD is not the plan's commit {c2}
        {c4} | {c4} | 0.4.0 | stable | '' | '' | '' | {c4} is not on develop
        {c3} | {c3} | 0.4.0 | stable | '' | '' | push | unexpected event 'push'
        {c3} | {c3} | 0.4.0-dev.1 | prerelease | '' | '' | '' | this run asked for a stable release, not a prerelease one
        {c3} | {c3} | 0.4.0-dev.1 | prerelease | '' | none | '' | this run asked for a stable release, not a prerelease one
        {c3} | {c3} | 0.4.0 | stable | '' | dev | '' | this run asked for a prerelease release, not a stable one
        {c3} | {c3} | 0.7.0-rc.1 | stable | 0.7.0-rc.1 | '' | '' | this run asked for a prerelease release, not a stable one
        {c3} | {c3} | 0.7.0 | prerelease | 0.7.0 | dev | '' | this run asked for a stable release, not a prerelease one
        {c3} | {c3} | 0.4.0 | stable | '' | rc | '' | prerelease must be none or dev, not 'rc'
        {c3} | {c3} | 0.4.0 | other | '' | '' | '' | KIND must be stable or prerelease, not 'other'
        {c3} | {c3} | 0.2 | stable | '' | '' | '' | not a semver version: '0.2'
        {c3} | {c3} | v0.2.0 | stable | '' | '' | '' | not a semver version: 'v0.2.0'
        {c3} | {c3} | 0.04.0 | stable | '' | '' | '' | not a semver version: '0.04.0'
        {c3} | {c3} | 0.4.0\n | stable | '' | '' | '' | not a semver version: '0.4.0\n'
        {c3} | {c3} | 0.4.0- | prerelease | '' | dev | '' | not a semver version: '0.4.0-'
        {c3} | {c3} | 0.4.0-a..b | prerelease | '' | dev | '' | not a semver version: '0.4.0-a..b'
        {c3} | {c3} | '' | stable | '' | '' | '' | not a semver version: ''
        {c3} | {c3} | 0.4.0-dev.1 | stable | '' | '' | '' | 0.4.0-dev.1 is a pre-release
        {c3} | {c3} | 0.4.0 | prerelease | '' | dev | '' | 0.4.0 is not a pre-release
        {c3} | {c3} | 0.1.0 | stable | '' | '' | '' | v0.1.0 already exists
        {c3} | {c3} | 0.2.0-dev.1 | prerelease | '' | dev | '' | v0.2.0-dev.1 already exists
        {c3} | {c3} | 0.4.0 | stable | 0.7.0 | '' | '' | 0.4.0 is not the version given to this run (0.7.0)
        {c3} | {c3} | 0.0.9 | stable | 0.0.9 | '' | '' | 0.0.9 is not newer than the latest release v0.1.0
        {c3} | {c3} | 0.1.1 | stable | '' | '' | '' | 0.1.1 is not a version this run may release (0.4.0 0.4.0)
        {c3} | {c3} | 0.2.0 | stable | '' | '' | '' | 0.2.0 is not a version this run may release (0.4.0 0.4.0)
        {c3} | {c3} | 1.0.0 | stable | '' | '' | '' | 1.0.0 is not a version this run may release (0.4.0 0.4.0)
        {c3} | {c3} | 9.0.0 | stable | '' | '' | '' | 9.0.0 is not a version this run may release (0.4.0 0.4.0)
        {c3} | {c3} | 0.5.0 | stable | '' | '' | '' | 0.5.0 is not a version this run may release (0.4.0 0.4.0)
        {c2} | {c2} | 1.0.0 | stable | '' | '' | '' | 1.0.0 is not a version this run may release (0.1.1 0.2.0)
        {c2} | {c2} | 0.1.0-dev.1 | prerelease | '' | dev | '' | 0.1.0-dev.1 is not a version this run may release (0.1.1 0.2.0)
        {c3} | {c3} | 0.1.1-dev.1 | prerelease | '' | dev | '' | 0.1.1-dev.1 is not a version this run may release (0.4.0 0.4.0)
        {c3} | {c3} | 0.2.0-dev.2 | prerelease | '' | dev | '' | 0.2.0-dev.2 is not a version this run may release (0.4.0 0.4.0)
        {c3} | {c3} | 9.0.0-dev.1 | prerelease | '' | dev | '' | 9.0.0-dev.1 is not a version this run may release (0.4.0 0.4.0)
        {c3} | {c3} | 0.4.0-dev.2 | prerelease | '' | dev | '' | 0.4.0-dev.2 is not the next pre-release of 0.4.0 (dev.1)
        {c2} | {c2} | 0.2.0-dev.3 | prerelease | '' | dev | '' | 0.2.0-dev.3 is not the next pre-release of 0.2.0 (dev.2)
        {c3} | {c3} | 0.4.0-rc.1 | prerelease | '' | dev | '' | 0.4.0-rc.1 is not X.Y.Z-dev.N
        {c3} | {c3} | 0.4.0-dev.0 | prerelease | '' | dev | '' | 0.4.0-dev.0 is not X.Y.Z-dev.N
        {c3} | {c3} | 0.4.0-dev.01 | prerelease | '' | dev | '' | 0.4.0-dev.01 is not X.Y.Z-dev.N
        {c3} | {c3} | 0.4.0-dev.1.2 | prerelease | '' | dev | '' | 0.4.0-dev.1.2 is not X.Y.Z-dev.N
    ";

    /// d1 (v1.2.2, v1.2.3-dev.1, v1.2.3-dev.4) -- d2 (develop's version 1.3.0) -- d3 (5.0.0):
    /// the planner gives the bump or develop's version, whichever is higher.
    const FORGED_B: &str = r"
        {d1} | {d1} | 1.2.3 | stable | '' | '' | '' | ''
        {d1} | {d1} | 1.3.0 | stable | '' | '' | '' | ''
        {d1} | {d1} | 2.0.0 | stable | '' | '' | '' | ''
        {d1} | {d1} | 1.2.3-dev.5 | prerelease | '' | dev | '' | ''
        {d1} | {d1} | 1.3.0-dev.1 | prerelease | '' | dev | '' | ''
        {d1} | {d1} | 1.2.3-dev.2 | prerelease | '' | dev | '' | 1.2.3-dev.2 is not the next pre-release of 1.2.3 (dev.5)
        {d1} | {d1} | 1.2.4 | stable | '' | '' | '' | 1.2.4 is not a version this run may release (1.2.3 1.3.0 2.0.0)
        {d1} | {d1} | 3.0.0 | stable | '' | '' | '' | 3.0.0 is not a version this run may release (1.2.3 1.3.0 2.0.0)
        {d2} | {d2} | 1.3.0 | stable | '' | '' | '' | ''
        {d2} | {d2} | 2.0.0 | stable | '' | '' | '' | ''
        {d2} | {d2} | 1.2.3 | stable | '' | '' | '' | 1.2.3 is not a version this run may release (1.3.0 1.3.0 2.0.0)
        {d3} | {d3} | 5.0.0 | stable | '' | '' | '' | ''
        {d3} | {d3} | 5.0.0-dev.1 | prerelease | '' | dev | '' | ''
        {d3} | {d3} | 1.2.3 | stable | '' | '' | '' | 1.2.3 is not a version this run may release (5.0.0 5.0.0 5.0.0)
        {d3} | {d3} | 1.3.0 | stable | '' | '' | '' | 1.3.0 is not a version this run may release (5.0.0 5.0.0 5.0.0)
        {d3} | {d3} | 2.0.0 | stable | '' | '' | '' | 2.0.0 is not a version this run may release (5.0.0 5.0.0 5.0.0)
        {d3} | {d3} | 1.2.3-dev.5 | prerelease | '' | dev | '' | 1.2.3-dev.5 is not a version this run may release (5.0.0 5.0.0 5.0.0)
    ";

    /// What the check refuses, one scenario per rule: a forged sha (not a commit id, not the one
    /// checked out, not on develop, without the latest release), a forged kind (not what the run
    /// asked for) or a forged version (not semver, of the other kind, already tagged, not the
    /// next pre-release, too large, or not one the planner gives for this request).
    #[test]
    fn the_bot_jobs_refuse_a_forged_plan() {
        let checks = bot_job_steps("Check the plan");
        let script = &checks[0].1;
        let forms = bot_job_steps("Check the plan's commit id");
        assert_eq!(forms.len(), 2);
        assert_eq!(
            forms[0].1, forms[1].1,
            "prepare and pretag check differently"
        );
        let mut problems = Vec::new();

        let tmp = ok(tempfile::tempdir());
        let dir = tmp.path().join("repo");
        ok(fs::create_dir(&dir));
        let mut repo = Repo {
            dir: dir.clone(),
            commits: 0,
        };
        repo.git(&["init", "-q", "-b", "develop"]);
        repo.write(&cargo("0.1.0"));
        repo.commit("c1");
        repo.write(&BTreeMap::from([("a".to_owned(), "2\n".to_owned())]));
        repo.commit("c2");
        repo.git(&["tag", "v0.1.0"]);
        repo.write(&cargo("0.4.0"));
        repo.commit("c3");
        repo.git(&["tag", "v0.2.0-dev.1"]);
        repo.git(&["switch", "-q", "-c", "fork"]);
        repo.write(&BTreeMap::from([("a".to_owned(), "4\n".to_owned())]));
        repo.commit("c4");
        let [c1, c2, c3, c4] =
            ["HEAD~3", "HEAD~2", "HEAD~1", "HEAD"].map(|r| git_out(&dir, &["rev-parse", r]));
        as_checked_out(&dir, &c3);
        let upper = c3.to_uppercase();
        let names = [
            ("c1", &*c1),
            ("c2", &c2),
            ("c3", &c3),
            ("c4", &c4),
            ("C3", &upper),
        ];
        run_forged(script, &dir, &names, FORGED_A, &mut problems);
        for (sha, code) in [(c3.as_str(), 0), ("abc", 1), (upper.as_str(), 1), ("", 1)] {
            assert_eq!(
                run_check(&forms[0].1, &dir, &[("SHA", sha)]).0,
                code,
                "{sha:?}"
            );
        }

        let tmp = ok(tempfile::tempdir());
        let dir = tmp.path().join("repo");
        ok(fs::create_dir(&dir));
        let mut repo = Repo {
            dir: dir.clone(),
            commits: 0,
        };
        repo.git(&["init", "-q", "-b", "develop"]);
        repo.write(&cargo("1.2.2"));
        repo.commit("d1");
        for tag in ["v1.2.2", "v1.2.3-dev.1", "v1.2.3-dev.4"] {
            repo.git(&["tag", tag]);
        }
        repo.write(&cargo("1.3.0"));
        repo.commit("d2");
        repo.write(&cargo("5.0.0"));
        repo.commit("d3");
        let [d1, d2, d3] = ["HEAD~2", "HEAD~1", "HEAD"].map(|r| git_out(&dir, &["rev-parse", r]));
        as_checked_out(&dir, &d3);
        let names = [("d1", &*d1), ("d2", &d2), ("d3", &d3)];
        run_forged(script, &dir, &names, FORGED_B, &mut problems);
        // Tag numbers that shell arithmetic would wrap are refused, not wrapped.
        git_out(&dir, &["tag", "v1.2.99999999999999999999", &d1]);
        let big = "{d1} | {d1} | 1.3.0 | stable | '' | '' | '' | version too large: 1.2.99999999999999999999";
        run_forged(script, &dir, &names, big, &mut problems);
        git_out(&dir, &["tag", "-d", "v1.2.99999999999999999999"]);
        git_out(&dir, &["tag", "v1.2.3-dev.0000000000000000001", &d1]);
        let big = "{d1} | {d1} | 1.2.3-dev.5 | prerelease | '' | dev | '' | pre-release number too large: v1.2.3-dev.0000000000000000001";
        run_forged(script, &dir, &names, big, &mut problems);
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// The steps of a job of release-prep.yml, each as its lines (the first starts with `- `).
    fn job_steps(job: &str) -> Vec<Vec<String>> {
        let wf = ok(fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/release-prep.yml"),
        ));
        let mut steps: Vec<Vec<String>> = Vec::new();
        let mut inside = (false, false);
        for line in wf.lines() {
            if line.starts_with("  ") && !line.starts_with("   ") && line.ends_with(':') {
                inside = (line.trim_end_matches(':').trim() == job, false);
                continue;
            }
            if !inside.0 {
                continue;
            }
            if line == "    steps:" {
                inside.1 = true;
                continue;
            }
            if !inside.1 || line.trim_start().starts_with('#') {
                continue;
            }
            if line.starts_with("      - ") {
                steps.push(vec![line.trim().to_owned()]);
            } else if let Some(step) = steps.last_mut() {
                step.push(line.trim().to_owned());
            }
        }
        steps
    }

    /// The checks come first in each bot job, bound to the plan's outputs and the request as
    /// written here: the commit id's form, the checkout of that commit without credentials, the
    /// check (and in prepare the release state), and no earlier step reads another output.
    #[test]
    fn the_bot_jobs_check_first_with_these_bindings() {
        for (job, kind) in [("prepare", "stable"), ("pretag", "prerelease")] {
            let steps = job_steps(job);
            let head = |s: &Vec<String>| {
                s.iter()
                    .take_while(|l| *l != "run: |")
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                steps[0],
                [
                    "- name: Check the plan's commit id",
                    "env:",
                    "SHA: ${{ needs.plan.outputs.sha }}",
                    r#"run: '[[ "${SHA}" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::the plan''s sha is not a commit id"; exit 1; }'"#,
                ],
                "{job}"
            );
            assert_eq!(
                steps[1][..4],
                [
                    "- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1",
                    "with:",
                    "ref: ${{ needs.plan.outputs.sha }}",
                    "persist-credentials: false # the token is added to the push below only",
                ],
                "{job}"
            );
            assert!(steps[1][4].starts_with("fetch-depth: 0 #"), "{job}");
            assert_eq!(steps[1].len(), 5, "{job}");
            assert_eq!(
                head(&steps[2]),
                [
                    "- name: Check the plan",
                    "env:",
                    "SHA: ${{ needs.plan.outputs.sha }}",
                    "VERSION: ${{ needs.plan.outputs.version }}",
                    &format!("KIND: {kind}"),
                    "EVENT: ${{ github.event_name }}",
                    "GIVEN: ${{ inputs.version }}",
                    "REQUESTED: ${{ inputs.prerelease }}",
                ],
                "{job}"
            );
            if job == "prepare" {
                assert_eq!(steps[3][0], "- name: Release state");
                assert!(!steps[3].iter().any(|l| l.contains("${{")), "{job}");
            }
            let token = steps
                .iter()
                .position(|s| s[0] == "- name: Bot token")
                .unwrap_or_else(|| panic!("{job}: no Bot token step"));
            assert!(token > 3, "{job}");
        }
    }

    /// prepare's release state step fails where the plan job skipped: main not tagged yet, the
    /// back-merge open, a hotfix/* or release/* branch; it passes when none is the case.
    #[test]
    fn prepare_checks_the_release_state_again() {
        let step = job_steps("prepare")
            .into_iter()
            .find(|s| s[0] == "- name: Release state")
            .unwrap_or_else(|| panic!("no Release state step"));
        let script: String = step
            .iter()
            .skip_while(|l| *l != "run: |")
            .skip(1)
            .map(|l| format!("{l}\n"))
            .collect();
        // origin: main at 1.1.0 (tagged v1.1.0), develop on top of it.
        let tmp = ok(tempfile::tempdir());
        let origin = tmp.path().join("origin");
        let dir = tmp.path().join("repo");
        ok(fs::create_dir(&origin));
        ok(fs::create_dir(&dir));
        let mut repo = Repo {
            dir: dir.clone(),
            commits: 0,
        };
        git_out(&origin, &["init", "-q", "--bare"]);
        repo.git(&["init", "-q", "-b", "main"]);
        repo.write(&cargo("1.1.0"));
        repo.commit("m1");
        repo.git(&["tag", "v1.1.0"]);
        repo.git(&["switch", "-q", "-c", "develop"]);
        repo.write(&BTreeMap::from([("a".to_owned(), "1\n".to_owned())]));
        repo.commit("d1");
        repo.git(&["remote", "add", "origin", &origin.to_string_lossy()]);
        repo.git(&["push", "-q", "origin", "main", "develop", "v1.1.0"]);
        as_checked_out(&dir, "HEAD");
        let fail = |why: &str| (1, format!("::error::{why}\n"));
        let mut problems = Vec::new();
        let mut expect = |name: &str, want: (i32, String)| {
            let got = run_check(&script, &dir, &[]);
            if got != want {
                problems.push(format!("{name}: {got:?}, not {want:?}"));
            }
        };
        expect("clear", (0, String::new()));
        git_out(&origin, &["branch", "hotfix/1.1.1", "main"]);
        expect(
            "hotfix",
            fail("A hotfix/* branch exists (a hotfix is in progress)."),
        );
        git_out(&origin, &["branch", "-D", "hotfix/1.1.1"]);
        git_out(&origin, &["branch", "release/1.2.0", "main"]);
        expect(
            "release",
            fail("A release/* branch exists (a release is in progress)."),
        );
        git_out(&origin, &["branch", "-D", "release/1.2.0"]);
        // main moves to 1.2.0, untagged: a release waits for its tag
        repo.git(&["switch", "-q", "main"]);
        repo.write(&cargo("1.2.0"));
        repo.commit("m2");
        repo.git(&["push", "-q", "origin", "main"]);
        repo.git(&["switch", "-q", "develop"]);
        expect(
            "pending",
            fail(
                "main is at 1.2.0, which is not tagged yet (a release or hotfix waits for its tag).",
            ),
        );
        // tagged, but develop does not contain it yet
        repo.git(&["tag", "v1.2.0", "main"]);
        repo.git(&["push", "-q", "origin", "v1.2.0"]);
        expect(
            "back-merge",
            fail("develop does not contain v1.2.0 yet (its back-merge is still open)."),
        );
        repo.git(&["merge", "-q", "--no-ff", "-m", "back-merge", "main"]);
        expect("merged", (0, String::new()));
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
