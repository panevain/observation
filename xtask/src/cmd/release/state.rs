//! `release state` and `release gate`: the release state of `origin`, as scripts/release-state.sh
//! (since removed) printed it, and the three workflow checks that read it, as release-prep.yml,
//! hotfix.yml and release-watch.yml wrote them inline. Each check is kept as it was: Prepare
//! release skips, Hotfix fails, Release watch skips, each with its own messages.
//!
//! The git commands and their arguments are the script's and the workflows', in the same order,
//! and git's stderr passes through. Where the script failed, this fails too; where a git command
//! failed and the script went on anyway (it read the failure as an answer), this fails instead,
//! except where the exit status is the answer (`ls-remote --exit-code`, `merge-base
//! --is-ancestor`).

use std::cmp::Ordering;
use std::path::Path;

use crate::cmd::release::version;
use crate::ports::{Git, GitRun};
use crate::{Error, Result};

/// What the script printed: `key=value` lines for `GITHUB_OUTPUT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// The newest stable `vX.Y.Z` tag on origin, without the `v` (empty if there is none).
    pub latest: String,
    /// The workspace version on origin's main.
    pub main: String,
    /// main's version is not tagged yet (a release or hotfix waits for its tag).
    pub pending: bool,
    /// A `hotfix/*` branch exists.
    pub hotfix: bool,
    /// The patch release after `latest` (empty if there is none).
    pub next: String,
}

impl State {
    pub fn lines(&self) -> String {
        format!(
            "latest={}\nmain={}\npending={}\nhotfix={}\nnext={}\n",
            self.latest, self.main, self.pending, self.hotfix, self.next
        )
    }
}

/// `git ARGS`, failing with a message unless it exits with one of `ok`.
fn git_expect(git: &dyn Git, args: &[&str], ok: &[i32]) -> Result<GitRun> {
    let run = git.run(args)?;
    if ok.contains(&run.code) {
        Ok(run)
    } else {
        Err(Error::new(format!(
            "`git {}` failed ({})",
            args.join(" "),
            run.code
        )))
    }
}

/// Whether bash's `[[ -z "$(...)" ]]` saw output: `$(...)` drops NULs and trailing newlines.
fn printed(stdout: &[u8]) -> bool {
    stdout.iter().any(|&b| b != b'\n' && b != 0)
}

/// `X.Y.Z` in ASCII digits (the script's `grep -E '^[0-9]+\.[0-9]+\.[0-9]+$'`).
fn is_stable(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// GNU `sort -V` on `X.Y.Z` strings: each number by value, then the bytes (which only leading
/// zeros can tell apart).
fn version_order(a: &str, b: &str) -> Ordering {
    let key = |s: &str| -> Vec<(usize, String)> {
        s.split('.')
            .map(|p| {
                let digits = p.trim_start_matches('0');
                (digits.len(), digits.to_owned())
            })
            .collect()
    };
    key(a)
        .cmp(&key(b))
        .then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

/// The newest stable version in `git ls-remote --tags --refs origin 'v*'`'s output, as
/// `sed 's|.*refs/tags/v||' | grep -E SEMVER | sort -V | tail -n1` found it.
pub fn latest_of(listing: &str) -> String {
    listing
        .split('\n')
        .map(|line| match line.rfind("refs/tags/v") {
            Some(i) => &line[i + "refs/tags/v".len()..],
            None => line,
        })
        .filter(|v| is_stable(v))
        .max_by(|a, b| version_order(a, b))
        .unwrap_or("")
        .to_owned()
}

/// bash's `${major}.${minor}.$((patch + 1))` on a stable `latest`: a patch with a leading zero
/// is octal (an 8 or 9 in it is an error), and the arithmetic wraps at 64 bits.
pub fn next_of(latest: &str) -> Result<String> {
    if latest.is_empty() {
        return Ok(String::new());
    }
    let mut parts = latest.splitn(3, '.');
    let (major, minor, patch) = match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return Err(Error::new(format!("not a stable version: {latest}"))),
    };
    let base = if patch.len() > 1 && patch.starts_with('0') {
        8
    } else {
        10
    };
    let mut value: i64 = 0;
    for b in patch.bytes() {
        let digit = i64::from(b.wrapping_sub(b'0'));
        if !b.is_ascii_digit() || digit >= base {
            return Err(Error::new(format!(
                "no patch release after {latest}: {patch}: value too great for base (error \
                 token is \"{patch}\")"
            )));
        }
        value = value.wrapping_mul(base).wrapping_add(digit);
    }
    Ok(format!("{major}.{minor}.{}", value.wrapping_add(1)))
}

/// UTF-8 text without NULs, or an error (git prints neither in a ref name).
fn text(run: GitRun, what: &str) -> Result<String> {
    String::from_utf8(run.stdout)
        .ok()
        .filter(|t| !t.contains('\0'))
        .ok_or_else(|| Error::new(format!("{what} printed bytes that are not text")))
}

/// The release state, from `origin` through `git` run in the repository. `None` when a git
/// command failed where the script stopped with git's own status: git said why on stderr.
/// `annotate` is the `::error::` prefix on a bad version on main (`GITHUB_ACTIONS` set).
pub fn read(git: &dyn Git, annotate: bool) -> Result<Option<State>> {
    let tags = git.run(&["ls-remote", "--tags", "--refs", "origin", "v*"])?;
    if tags.code != 0 {
        return Ok(None);
    }
    let latest = latest_of(&text(tags, "git ls-remote")?);
    if git
        .run(&["fetch", "--quiet", "--depth=1", "origin", "main"])?
        .code
        != 0
    {
        return Ok(None);
    }
    let show = git.run(&["show", "FETCH_HEAD:Cargo.toml"])?;
    let main = version::get(
        Path::new(""),
        Some("-"),
        Some("Cargo.toml on main"),
        &mut show.stdout.as_slice(),
        annotate,
    )?;
    if show.code != 0 {
        return Ok(None);
    }
    let main = main.trim_end_matches('\n').to_owned();
    // `main` is a checked version: it cannot be read as an option or a pattern.
    let tag = format!("refs/tags/v{main}");
    let tagged = git_expect(
        git,
        &["ls-remote", "--exit-code", "--tags", "origin", &tag],
        &[0, 2],
    )?;
    let heads = ["ls-remote", "--heads", "origin", "refs/heads/hotfix/*"];
    let hotfix = printed(&git_expect(git, &heads, &[0])?.stdout);
    let next = next_of(&latest)?;
    Ok(Some(State {
        latest,
        pending: tagged.code != 0,
        main,
        hotfix,
        next,
    }))
}

/// Which workflow's check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Prepare release: a stable release skips while a release or hotfix is open, waits for its
    /// tag or for its back-merge. A pre-release does not go through main: no check applies.
    Prep { prerelease: bool },
    /// Hotfix: fails unless main is the latest release and nothing else is open.
    Hotfix,
    /// Release watch: skips while there is no release, or one waits for its tag, or a hotfix is
    /// open.
    Watch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Go,
    Skip(String),
    Fail(String),
}

/// The check, in the workflows' order. Prepare release's last two questions go to git only
/// when the earlier ones pass, as the inline checks did.
pub fn gate(kind: Kind, state: &State, git: &dyn Git) -> Result<Verdict> {
    let State {
        latest,
        main,
        pending,
        hotfix,
        ..
    } = state;
    let waits = format!(
        "main is at {main}, which is not tagged yet (a release or hotfix waits for its tag); skipping."
    );
    let hotfix_open = "A hotfix/* branch exists (a hotfix is in progress); skipping.".to_owned();
    Ok(match kind {
        Kind::Prep { prerelease: true } => Verdict::Go,
        Kind::Prep { prerelease: false } => {
            // Before the first release main's initial version was never tagged, so that alone
            // is not a release waiting for its tag.
            if *pending && !latest.is_empty() {
                Verdict::Skip(waits)
            } else if !latest.is_empty()
                && git_expect(
                    git,
                    &["merge-base", "--is-ancestor", &format!("v{latest}"), "HEAD"],
                    &[0, 1],
                )?
                .code
                    != 0
            {
                Verdict::Skip(format!(
                    "develop does not contain v{latest} yet (its back-merge is still open); skipping."
                ))
            } else if *hotfix {
                Verdict::Skip(hotfix_open)
            } else if printed(
                &git_expect(
                    git,
                    &["ls-remote", "--heads", "origin", "refs/heads/release/*"],
                    &[0],
                )?
                .stdout,
            ) {
                Verdict::Skip(
                    "A release/* branch exists (a release is in progress); skipping.".to_owned(),
                )
            } else {
                Verdict::Go
            }
        }
        Kind::Hotfix if latest.is_empty() => {
            Verdict::Fail("no release yet; nothing to patch".to_owned())
        }
        Kind::Hotfix if *pending => Verdict::Fail(format!(
            "main is at {main}, which is not tagged yet; tag or finish that release first"
        )),
        Kind::Hotfix if *hotfix => Verdict::Fail("a hotfix/* branch already exists".to_owned()),
        Kind::Hotfix if main != latest => Verdict::Fail(format!(
            "main is at {main} but the latest release is {latest}"
        )),
        Kind::Hotfix => Verdict::Go,
        Kind::Watch if latest.is_empty() => {
            Verdict::Skip("No release yet; nothing to watch.".to_owned())
        }
        Kind::Watch if *pending => Verdict::Skip(waits),
        Kind::Watch if *hotfix => Verdict::Skip(hotfix_open),
        Kind::Watch => Verdict::Go,
    })
}

/// What a gate's step printed and wrote, after the state lines in `--output`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gated {
    pub stdout: String,
    /// For the job summary.
    pub summary: String,
    /// `go=true|false` for the checks that skip (the state lines precede it).
    pub output: String,
    pub failed: bool,
}

/// The verdict as each workflow's step reported it.
pub fn render(kind: Kind, state: &State, verdict: &Verdict) -> Gated {
    let go = |go: bool| format!("go={go}\n");
    match (kind, verdict) {
        (Kind::Hotfix, Verdict::Fail(why)) => Gated {
            stdout: format!("::error::{why}\n"),
            failed: true,
            ..Gated::default()
        },
        (Kind::Watch, Verdict::Go) => {
            let line = format!("Checking v{}.\n", state.latest);
            Gated {
                stdout: line.clone(),
                summary: line,
                output: go(true),
                failed: false,
            }
        }
        (_, Verdict::Skip(why) | Verdict::Fail(why)) => Gated {
            stdout: format!("{why}\n"),
            summary: format!("{why}\n"),
            output: go(false),
            failed: false,
        },
        (Kind::Hotfix, Verdict::Go) => Gated::default(),
        (_, Verdict::Go) => Gated {
            output: go(true),
            ..Gated::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::FakeGit;
    use serde::Deserialize;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    /// release-state/cases.toml.
    #[derive(Deserialize)]
    struct Cases {
        case: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        /// `state`, `prep`, `prep-pre` (a pre-release), `hotfix`, `watch`.
        targets: Vec<String>,
        /// The value of `GITHUB_ACTIONS` (unset when missing).
        github_actions: Option<String>,
        /// git's answer to each command: arguments joined with spaces -> status and stdout.
        git: BTreeMap<String, Answer>,
        /// Per target, why the port's result differs from the script's, on purpose:
        /// `stricter` (the script passed and the port fails) or `message` (both fail, with
        /// other output or exit status).
        #[serde(default)]
        differs: BTreeMap<String, String>,
    }

    #[derive(Deserialize)]
    struct Answer {
        code: i32,
        stdout: String,
    }

    /// The golden's sections by `name / target`: exit, stdout, stderr, output, summary, git.
    fn sections(golden: &str) -> BTreeMap<String, Vec<String>> {
        let mut out = BTreeMap::new();
        for block in golden.split("\n== ") {
            let block = format!("{}\n", block.trim_start_matches("== "));
            let (name, mut rest) = block.split_once('\n').unwrap_or((&block, ""));
            let mut parts = Vec::new();
            for marker in [
                "-- stdout\n",
                "-- stderr\n",
                "-- output\n",
                "-- summary\n",
                "-- git\n",
            ] {
                let (before, after) = rest.split_once(marker).unwrap_or((rest, ""));
                parts.push(before.trim_end_matches('\n').to_owned());
                rest = after;
            }
            parts.push(rest.trim_end_matches('\n').to_owned());
            out.insert(name.to_owned(), parts);
        }
        out
    }

    /// One target of a case, run through the port with a fake git, in the golden's sections.
    fn run_target(case: &Case, target: &str) -> Vec<String> {
        let git = FakeGit {
            runs: case
                .git
                .iter()
                .map(|(k, a)| {
                    let run = GitRun {
                        code: a.code,
                        stdout: a.stdout.as_bytes().to_vec(),
                    };
                    (k.clone(), run)
                })
                .collect(),
            ..FakeGit::default()
        };
        let annotate = case
            .github_actions
            .as_deref()
            .is_some_and(|v| !v.is_empty());
        let kind = match target {
            "state" => None,
            "prep" => Some(Kind::Prep { prerelease: false }),
            "prep-pre" => Some(Kind::Prep { prerelease: true }),
            "hotfix" => Some(Kind::Hotfix),
            "watch" => Some(Kind::Watch),
            other => panic!("unknown target {other}"),
        };
        let (mut code, mut stdout, mut stderr) = (1, String::new(), String::new());
        let (mut output, mut summary) = (String::new(), String::new());
        match read(&git, annotate) {
            Err(e) => stderr = format!("{e}\n"),
            Ok(None) => {}
            Ok(Some(state)) => match kind {
                None => (code, stdout) = (0, state.lines()),
                Some(kind) => {
                    // The workflows wrote the state lines to GITHUB_OUTPUT (prep: to a file of
                    // its own, of which only the go line counts).
                    let prep = matches!(kind, Kind::Prep { .. });
                    if !prep {
                        output = state.lines();
                    }
                    match gate(kind, &state, &git) {
                        Err(e) => stderr = format!("{e}\n"),
                        Ok(verdict) => {
                            let gated = render(kind, &state, &verdict);
                            code = i32::from(gated.failed);
                            (stdout, summary) = (gated.stdout, gated.summary);
                            output.push_str(&gated.output);
                        }
                    }
                }
            },
        }
        let calls: String = git.calls.take().iter().map(|c| format!("{c}\n")).collect();
        [
            format!("exit {code}"),
            stdout,
            stderr,
            output,
            summary,
            calls,
        ]
        .map(|s| s.trim_end_matches('\n').to_owned())
        .to_vec()
    }

    /// Every target of every case of release-state/cases.toml against what the script and the
    /// workflows' inline checks did with it (release-state/golden, captured with a stand-in git
    /// answering from the case): the same exit status, stdout, stderr, GITHUB_OUTPUT, summary
    /// and git commands, except where a target is listed in `differs`: `stricter` (the script
    /// passed, the port must fail) or `message` (both fail, with other output or status).
    #[test]
    fn matches_the_script_and_checks_and_never_passes_where_they_failed() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/release-state");
        let cases: Cases = ok(toml::from_str(&ok(std::fs::read_to_string(
            dir.join("cases.toml"),
        ))));
        let want = sections(&ok(std::fs::read_to_string(dir.join("golden"))));
        let mut problems = Vec::new();
        let mut ran = 0;
        for case in &cases.case {
            for target in &case.targets {
                ran += 1;
                let key = format!("{} / {target}", case.name);
                let Some(sh) = want.get(&key) else {
                    problems.push(format!("{key}: not in the golden"));
                    continue;
                };
                let port = run_target(case, target);
                let (sh_ok, port_ok) = (sh[0] == "exit 0", port[0] == "exit 0");
                let problem = match (sh_ok, port_ok, case.differs.get(target).map(String::as_str)) {
                    (true, true, Some("stricter")) => "passes, but is listed as stricter",
                    (true, true, _) if port != *sh => "passes with different output",
                    (true, false, Some("stricter")) => "",
                    (true, false, _) => "fails where the script passed (not listed as stricter)",
                    (false, true, _) => "PASSES WHERE THE SCRIPT FAILED",
                    (false, false, Some("message")) if port == *sh => {
                        "same, but listed as differing"
                    }
                    (false, false, Some("message")) => "",
                    (false, false, _) if port != *sh => "fails with output other than the script's",
                    _ => "",
                };
                if !problem.is_empty() {
                    problems.push(format!(
                        "{key}: {problem}\n  script: {sh:?}\n  port:   {port:?}"
                    ));
                }
            }
        }
        assert_eq!(ran, want.len(), "cases and golden differ in length");
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// git prints neither bytes that are not UTF-8 nor NULs in a ref name; the script's sed and
    /// grep would have read them on their own terms, so the port refuses them.
    #[test]
    fn refuses_a_listing_that_is_not_text() {
        for listing in [&b"x\trefs/tags/v1.0.\xff\n"[..], b"x\trefs/tags/v1.0.0\0\n"] {
            let git = FakeGit {
                runs: [(
                    "ls-remote --tags --refs origin v*".to_owned(),
                    GitRun {
                        code: 0,
                        stdout: listing.to_vec(),
                    },
                )]
                .into(),
                ..FakeGit::default()
            };
            let err = read(&git, false).err().map(|e| e.to_string());
            assert_eq!(
                err.as_deref(),
                Some("git ls-remote printed bytes that are not text")
            );
        }
    }
}
