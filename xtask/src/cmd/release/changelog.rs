//! `release changelog --version X.Y.Z`: dates CHANGELOG.md's `[Unreleased]` section as the
//! release, as `scripts/release-plan.py changelog` did. With no entries it lists the commits
//! since the latest release that changed shipped files. A pre-release leaves the file alone.
//!
//! Output, messages and the file written are the script's. Where the script failed, this fails
//! too; the differences, all failures, are listed in `release-plan/cases.toml` (`differs`).

use std::path::Path;

use crate::core::changelog::{core, cut, semver, unreleased};
use crate::core::py::{is_space, splitlines, universal_newlines};
use crate::ports::{Clock, Git};
use crate::{Error, Result};

/// The paths that never reach a published image, as `git` pathspecs: a release is not for
/// changes to these alone (the script's `NOT_SHIPPED`, with its reasons).
pub const NOT_SHIPPED: &[&str] = &[
    ":(exclude).github",
    ":(exclude)docs",
    ":(exclude)fuzz",
    ":(exclude)xtask",
    ":(exclude).config",
    ":(exclude).typos.toml",
    ":(exclude)tests/browser",
    ":(exclude)supply-chain",
    ":(exclude).vscode",
    ":(exclude)scripts/browser-smoke-test.sh",
    ":(exclude)scripts/bump-pins.py",
    ":(exclude)scripts/ci-git.sh",
    ":(exclude)scripts/prune-images.sh",
    ":(exclude)scripts/release-plan.py",
    ":(exclude)scripts/release-state.sh",
    ":(exclude)scripts/rulesets.sh",
    ":(exclude)scripts/security-patch.py",
    ":(exclude)scripts/set-version.sh",
    ":(exclude)scripts/smoke-test-image.sh",
    ":(exclude)scripts/smoke-test-regions.sh",
    ":(exclude)scripts/subset-fonts.sh",
    ":(exclude)scripts/wait-healthz.sh",
    ":(exclude)scripts/wasm-size-report.sh",
    ":(exclude)scripts/wasm-size-treemap.py",
    ":(exclude)scripts/workspace-version.sh",
    ":(exclude,glob)*.md",
];

/// `git ARGS`'s stdout as text, failing as the script did on a non-zero exit (git's stderr
/// has already passed through).
fn git(git: &dyn Git, args: &[&str]) -> Result<String> {
    let run = git.run(args)?;
    if run.code != 0 {
        return Err(Error::new(format!(
            "git {} failed ({})",
            args.join(" "),
            run.code
        )));
    }
    String::from_utf8(run.stdout)
        .map(|s| universal_newlines(&s))
        .map_err(|_| {
            Error::new(format!(
                "git {} printed text that is not UTF-8",
                args.join(" ")
            ))
        })
}

/// The newest stable `vX.Y.Z` tag (without the v), or "" when there is none. Equal versions
/// keep the first in git's order, as Python's `max` did.
pub fn latest_stable(g: &dyn Git) -> Result<String> {
    let mut best: Option<(crate::core::changelog::Core, String)> = None;
    for tag in git(g, &["tag", "-l", "v*"])?
        .split(is_space)
        .filter(|t| !t.is_empty())
    {
        let name: String = tag.chars().skip(1).collect();
        let Some(parsed) = semver(&name) else {
            continue;
        };
        let parsed = parsed?;
        if name.contains('-') {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| parsed > *b) {
            best = Some((parsed, name));
        }
    }
    Ok(best.map(|(_, n)| n).unwrap_or_default())
}

/// Runs the command in `repo`; returns what to print on stdout.
pub fn run(repo: &Path, version: &str, g: &dyn Git, clock: &dyn Clock) -> Result<String> {
    core(version)?;
    if version.contains('-') {
        return Ok(format!(
            "{version} is a pre-release; CHANGELOG.md is left as it is.\n"
        ));
    }
    let path = repo.join("CHANGELOG.md");
    let bytes = std::fs::read(&path).map_err(|e| Error::new(format!("CHANGELOG.md: {e}")))?;
    let text = String::from_utf8(bytes).map_err(|_| Error::new("CHANGELOG.md is not UTF-8"))?;
    let text = universal_newlines(&text);
    let section = unreleased(&text)?;
    let mut subjects = Vec::new();
    if section.counts.is_empty() {
        let latest = latest_stable(g)?;
        let range = if latest.is_empty() {
            "-50".to_owned()
        } else {
            format!("v{latest}..HEAD")
        };
        let mut args = vec!["log", "--no-merges", "--format=%s", &range, "--", "."];
        args.extend(NOT_SHIPPED);
        let log = git(g, &args)?;
        subjects = splitlines(&log)
            .into_iter()
            .filter(|s| !s.trim_matches(is_space).is_empty())
            .map(String::from)
            .collect();
    }
    let today = clock.now().date();
    std::fs::write(&path, cut(&text, &section, version, today, &subjects))
        .map_err(|e| Error::new(format!("CHANGELOG.md: {e}")))?;
    Ok(String::new())
}
