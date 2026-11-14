//! CHANGELOG.md's `## [Unreleased]` section and release versions, as scripts/release-plan.py
//! read them: the section's place and its entry count per `###` heading, dating it as a release,
//! and the `X.Y.Z[-pre]` form of versions and tags.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::core::py::{is_space, splitlines};
use crate::core::time::Date;
use crate::{Error, Result};

static ASCII_SEMVER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9]+)\.([0-9]+)\.([0-9]+)(-[0-9A-Za-z.]+)?$").unwrap_or_else(|e| panic!("{e}"))
});
// What the script's `\d` matched: any Unicode decimal digit, and `$` also before a final LF.
static PY_SEMVER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d+)\.(\d+)\.(\d+)(-[0-9A-Za-z.]+)?\n?$").unwrap_or_else(|e| panic!("{e}"))
});

/// A version's `X.Y.Z` as numbers.
pub type Core = (u64, u64, u64);

/// Whether `v` has the script's `SEMVER` form: `None` if not; an error where the script took
/// it but this port does not (digits other than ASCII, a final newline, a number beyond u64).
pub fn semver(v: &str) -> Option<Result<Core>> {
    if let Some(c) = ASCII_SEMVER.captures(v) {
        let num = |i: usize| c.get(i).map_or("", |m| m.as_str()).parse::<u64>();
        return Some(match (num(1), num(2), num(3)) {
            (Ok(x), Ok(y), Ok(z)) => Ok((x, y, z)),
            _ => Err(Error::new(format!("version too large: {v}"))),
        });
    }
    PY_SEMVER
        .is_match(v)
        .then(|| Err(Error::new(format!("not an ASCII semver version: {v:?}"))))
}

/// The script's `core()`: `X.Y.Z` of a version, or `not a semver version: V`.
pub fn core(v: &str) -> Result<Core> {
    semver(v).unwrap_or_else(|| Err(Error::new(format!("not a semver version: {v}"))))
}

/// The `[Unreleased]` section: its body runs from `start` to `end` (byte offsets), and
/// `counts` holds the number of `- ` entries under each `### ` heading (entries before any
/// heading count as Changed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreleased {
    pub start: usize,
    pub end: usize,
    pub counts: BTreeMap<String, usize>,
}

/// Byte offsets of each line start in `text` (`^` in a MULTILINE `re` pattern: the start, and
/// after each LF).
fn line_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1))
}

/// Finds the section in `text` (already read with text-mode newlines).
pub fn unreleased(text: &str) -> Result<Unreleased> {
    const HEAD: &str = "## [Unreleased]";
    let start = line_starts(text)
        .filter(|&i| text[i..].starts_with(HEAD))
        .find_map(|i| text[i..].find('\n').map(|n| i + n + 1))
        .ok_or_else(|| Error::new("CHANGELOG.md has no ## [Unreleased] heading"))?;
    let end = line_starts(&text[start..])
        .map(|i| start + i)
        .find(|&i| text[i..].starts_with("## ["))
        .unwrap_or(text.len());
    let mut counts = BTreeMap::new();
    let mut heading = "Changed".to_owned();
    for line in splitlines(&text[start..end]) {
        if let Some(rest) = line.strip_prefix("### ") {
            heading = rest.trim_matches(is_space).to_owned();
        } else if line.starts_with("- ") {
            *counts.entry(heading.clone()).or_insert(0) += 1;
        }
    }
    Ok(Unreleased { start, end, counts })
}

/// `text` with the section's body moved under `## [VERSION] - TODAY`. Without entries the body
/// becomes a Changed entry listing `subjects`, the commits since the latest release.
pub fn cut(
    text: &str,
    section: &Unreleased,
    version: &str,
    today: Date,
    subjects: &[String],
) -> String {
    let body = if section.counts.is_empty() {
        let mut body = format!(
            "### Changed\n\n- Dependency and maintenance updates{}",
            if subjects.is_empty() { "." } else { ":" }
        );
        for s in subjects {
            body.push_str("\n  - ");
            body.push_str(s);
        }
        body
    } else {
        text[section.start..section.end]
            .trim_matches('\n')
            .to_owned()
    };
    format!(
        "{}\n## [{version}] - {today}\n\n{body}\n\n{}",
        &text[..section.start],
        &text[section.end..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn versions_as_the_script_read_them() {
        assert_eq!(ok(core("1.2.3")), (1, 2, 3));
        assert_eq!(ok(core("01.002.3-rc.1")), (1, 2, 3));
        assert!(semver("1.2").is_none());
        assert!(semver("v1.2.3").is_none());
        assert!(semver("1.2.3-").is_none());
        // The script took these; the port refuses them.
        for v in ["١.2.3", "1.2.3\n", "99999999999999999999.0.0"] {
            assert!(semver(v).is_some_and(|r| r.is_err()), "{v:?}");
        }
        assert_eq!(
            core("x").map_err(|e| e.to_string()),
            Err("not a semver version: x".to_owned())
        );
    }

    #[test]
    fn finds_the_section_and_counts_its_entries() {
        let text = "# C\n\n## [Unreleased]\n\n- loose\n### Added\n- a\n- b\n###  Fixed \n- f\n\n## [0.1.0] - 2026-01-01\n\n- old\n";
        let s = ok(unreleased(text));
        assert_eq!(&text[..s.start], "# C\n\n## [Unreleased]\n");
        assert!(text[s.end..].starts_with("## [0.1.0]"));
        assert_eq!(
            s.counts.into_iter().collect::<Vec<_>>(),
            [
                ("Added".to_owned(), 2),
                ("Changed".to_owned(), 1),
                ("Fixed".to_owned(), 1)
            ]
        );
        // The heading needs its line end; a later "## [" anywhere at a line start ends it.
        assert!(unreleased("## [Unreleased]").is_err());
        assert!(unreleased(" ## [Unreleased]\n").is_err());
        let s = ok(unreleased("## [Unreleased] - soon\n- a\n## [x\n"));
        assert_eq!(s.counts.len(), 1);
    }
}
