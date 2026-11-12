//! `release verify-tag`: release.yml's check that a tag may be published, as its `verify` job
//! wrote it inline. The tag must name the workspace version. A stable version's commit must be
//! on `origin/main`. A pre-release's parent must be on `origin/develop`, and the commit may change
//! over its parent only the files on `.config/release.toml`'s `prerelease` list, read from the
//! parent, never from the commit being checked (which could otherwise allow its own changes).
//!
//! The git commands and their arguments are the step's, in the same order, with git's stderr
//! passed through; the messages are the step's, on stdout. Where the step failed, this fails.
//! Stricter: the commit id must be a full hexadecimal object id (the step took any revision;
//! release.yml passes `GITHUB_SHA`).

use std::path::Path;

use crate::cmd::release::{files, version};
use crate::ports::Git;
use crate::{Error, Result};

/// What to print, and whether the check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub stdout: String,
    pub failed: bool,
}

fn refuse(message: String) -> Verdict {
    Verdict {
        stdout: format!("::error::{message}\n"),
        failed: true,
    }
}

/// A git command's failure, whose reason git printed: the step stopped there (`set -e`).
fn stopped() -> Verdict {
    Verdict {
        stdout: String::new(),
        failed: true,
    }
}

fn is_object_id(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Checks `tag` at commit `sha` in `repo` (see the module documentation). `annotate` is
/// `release version get`'s (`GITHUB_ACTIONS` set).
pub fn verify(repo: &Path, tag: &str, sha: &str, git: &dyn Git, annotate: bool) -> Result<Verdict> {
    if !is_object_id(sha) {
        return Err(Error::new(format!("not a commit id: '{sha}'")));
    }
    let line = version::get(repo, None, None, &mut std::io::empty(), annotate)?;
    let version = line.trim_end_matches('\n');
    if format!("v{version}") != tag {
        return Ok(refuse(format!(
            "tag {tag} does not match the workspace version {version}"
        )));
    }
    let parent = format!("{sha}^");
    if !version.contains('-') {
        if git
            .run(&["merge-base", "--is-ancestor", sha, "origin/main"])?
            .code
            != 0
        {
            return Ok(refuse(format!("{tag} is not on main")));
        }
        return Ok(Verdict {
            stdout: format!("{tag}: version {version}, on main\n"),
            failed: false,
        });
    }
    if git
        .run(&["merge-base", "--is-ancestor", &parent, "origin/develop"])?
        .code
        != 0
    {
        return Ok(refuse(format!("{tag}'s parent is not on develop")));
    }
    let shown = format!("{parent}:{}", files::CONFIG);
    let config = git.run(&["show", &shown])?;
    if config.code != 0 {
        return Ok(stopped());
    }
    let text = String::from_utf8(config.stdout)
        .map_err(|_| Error::new(format!("cannot read {shown}: not UTF-8")))?;
    let config = files::Config::parse(&text, &shown)?;
    let diff = git.run(&["diff", "--name-only", &parent, sha])?;
    if diff.code != 0 {
        return Ok(stopped());
    }
    let other = files::unexpected(&diff.stdout, config.list(files::Kind::Prerelease))?;
    // The step read the list through `$(...)`, which drops trailing newlines.
    let other = other.trim_end_matches('\n');
    if !other.is_empty() {
        return Ok(refuse(format!(
            "{tag} changes more than the version: {other}"
        )));
    }
    Ok(Verdict {
        stdout: format!("{tag}: version {version}, a version bump on top of develop\n"),
        failed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::FakeGit;
    use crate::ports::GitRun;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const LIST: &str = "[allowed-files]\nhotfix = [\"Cargo.toml\", \"README.md\"]\n\
        prerelease = [\"Cargo.toml\", \"Cargo.lock\"]\n";

    fn repo(version: &str) -> tempfile::TempDir {
        let dir = ok(tempfile::tempdir());
        let manifest = format!("[workspace.package]\nversion = \"{version}\"\n");
        ok(std::fs::write(dir.path().join("Cargo.toml"), manifest));
        dir
    }

    fn answer(git: &mut FakeGit, args: &str, code: i32, stdout: &str) {
        git.runs.insert(
            args.to_owned(),
            GitRun {
                code,
                stdout: stdout.as_bytes().to_vec(),
            },
        );
    }

    fn pre(diff: &str) -> FakeGit {
        let mut git = FakeGit::default();
        answer(
            &mut git,
            &format!("merge-base --is-ancestor {SHA}^ origin/develop"),
            0,
            "",
        );
        answer(
            &mut git,
            &format!("show {SHA}^:.config/release.toml"),
            0,
            LIST,
        );
        answer(&mut git, &format!("diff --name-only {SHA}^ {SHA}"), 0, diff);
        git
    }

    #[test]
    fn a_stable_tag_must_be_on_main() {
        let dir = repo("1.2.0");
        let mut git = FakeGit::default();
        answer(
            &mut git,
            &format!("merge-base --is-ancestor {SHA} origin/main"),
            0,
            "",
        );
        let v = ok(verify(dir.path(), "v1.2.0", SHA, &git, false));
        assert_eq!(v.stdout, "v1.2.0: version 1.2.0, on main\n");
        assert!(!v.failed);
        for code in [1, 128] {
            answer(
                &mut git,
                &format!("merge-base --is-ancestor {SHA} origin/main"),
                code,
                "",
            );
            let v = ok(verify(dir.path(), "v1.2.0", SHA, &git, false));
            assert_eq!(v.stdout, "::error::v1.2.0 is not on main\n");
            assert!(v.failed);
        }
    }

    #[test]
    fn the_tag_must_name_the_version() {
        let dir = repo("1.2.0");
        for tag in ["v1.2.1", "1.2.0", "v1.2.0-dev.1", "vv1.2.0"] {
            let v = ok(verify(dir.path(), tag, SHA, &FakeGit::default(), false));
            let want = format!("::error::tag {tag} does not match the workspace version 1.2.0\n");
            assert_eq!((v.stdout, v.failed), (want, true));
        }
        let e = verify(repo("1.2").path(), "v1.2", SHA, &FakeGit::default(), true);
        assert_eq!(
            e.map_err(|e| e.to_string()),
            Err("::error::bad workspace version in Cargo.toml: '1.2'".to_owned())
        );
        for sha in ["HEAD", "0123abc", &SHA.to_uppercase(), &format!("{SHA}^")] {
            assert!(verify(dir.path(), "v1.2.0", sha, &FakeGit::default(), false).is_err());
        }
    }

    #[test]
    fn a_pre_release_changes_only_the_parents_list() {
        let dir = repo("1.3.0-dev.1");
        let v = ok(verify(
            dir.path(),
            "v1.3.0-dev.1",
            SHA,
            &pre("Cargo.toml\nCargo.lock\n"),
            false,
        ));
        let want = "v1.3.0-dev.1: version 1.3.0-dev.1, a version bump on top of develop\n";
        assert_eq!((v.stdout.as_str(), v.failed), (want, false));
        let v = ok(verify(dir.path(), "v1.3.0-dev.1", SHA, &pre(""), false));
        assert!(!v.failed);
        // README.md is on the hotfix list only; .config/release.toml is on neither.
        let v = ok(verify(
            dir.path(),
            "v1.3.0-dev.1",
            SHA,
            &pre("Cargo.toml\nREADME.md\n.config/release.toml\nCargo.toml.bak\n"),
            false,
        ));
        let want = "::error::v1.3.0-dev.1 changes more than the version: README.md\n\
            .config/release.toml\nCargo.toml.bak\n";
        assert_eq!((v.stdout.as_str(), v.failed), (want, true));
    }

    #[test]
    fn a_pre_release_fails_where_the_step_stopped() {
        let dir = repo("1.3.0-dev.1");
        let tag = "v1.3.0-dev.1";
        for code in [1, 128] {
            let mut git = pre("");
            answer(
                &mut git,
                &format!("merge-base --is-ancestor {SHA}^ origin/develop"),
                code,
                "",
            );
            let v = ok(verify(dir.path(), tag, SHA, &git, false));
            let want = "::error::v1.3.0-dev.1's parent is not on develop\n";
            assert_eq!((v.stdout.as_str(), v.failed), (want, true));
        }
        let mut git = pre("");
        answer(
            &mut git,
            &format!("show {SHA}^:.config/release.toml"),
            128,
            "",
        );
        assert_eq!(ok(verify(dir.path(), tag, SHA, &git, false)), stopped());
        let mut git = pre("");
        answer(
            &mut git,
            &format!("diff --name-only {SHA}^ {SHA}"),
            128,
            "Cargo.toml\n",
        );
        assert_eq!(ok(verify(dir.path(), tag, SHA, &git, false)), stopped());
        let mut git = pre("");
        answer(
            &mut git,
            &format!("show {SHA}^:.config/release.toml"),
            0,
            "[allowed-files]\n",
        );
        assert!(verify(dir.path(), tag, SHA, &git, false).is_err());
        assert!(verify(dir.path(), tag, SHA, &pre("Cargo.toml\0\n"), false).is_err());
    }
}
