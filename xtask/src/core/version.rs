//! Crate versions and the Cargo-style requirements RustSec writes for patched and unaffected
//! ranges, ported as is from `scripts/check-cargo-cooldown.py` (since removed): `version_key`
//! and `matches`.
//!
//! This is deliberately not full semver: a pre-release sorts before its release, and two
//! pre-releases compare as plain strings, as the script did. Digits are ASCII only, and
//! numbers are `u64` (Python's are unbounded). Whitespace is Python's: Unicode whitespace plus
//! the separators U+001C to U+001F.

use std::sync::LazyLock;

use regex::Regex;

// Constant patterns, compiled by every test that reaches them: the panics cannot happen.
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9]+)(?:\.([0-9]+))?(?:\.([0-9]+))?(?:-([0-9A-Za-z.-]+))?(?:\+\S*)?$")
        .unwrap_or_else(|e| panic!("VERSION regex: {e}"))
});
static REQUIREMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(>=|<=|>|<|=|\^|~)?[\s\x1C-\x1F]*(.+)$")
        .unwrap_or_else(|e| panic!("REQUIREMENT regex: {e}"))
});

/// A version that [`matches`] or [`version_key`] could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    NotAVersion(String),
    NotARequirement(String),
}

impl std::fmt::Display for VersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAVersion(v) => write!(f, "not a version: {v:?}"),
            Self::NotARequirement(r) => write!(f, "not a requirement: {r:?}"),
        }
    }
}

impl std::error::Error for VersionError {}

/// Python's `str.strip()`: Unicode whitespace and U+001C to U+001F.
fn strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1C'..='\x1F').contains(&c))
}

/// Sort key of a full version: major, minor, patch (missing parts are 0), then 0 for a
/// pre-release and 1 for a release, then the pre-release text.
pub type Key = (u64, u64, u64, u8, String);

/// The sort key of `v` (surrounding whitespace ignored).
pub fn version_key(v: &str) -> Result<Key, VersionError> {
    let not_a_version = || VersionError::NotAVersion(v.to_owned());
    let caps = VERSION.captures(strip(v)).ok_or_else(not_a_version)?;
    let part = |i| {
        caps.get(i)
            .map_or(Ok(0), |m| m.as_str().parse::<u64>())
            .map_err(|_| not_a_version())
    };
    let pre = caps.get(4).map(|m| m.as_str().to_owned());
    Ok((
        part(1)?,
        part(2)?,
        part(3)?,
        u8::from(pre.is_none()),
        pre.unwrap_or_default(),
    ))
}

/// Whether `version` satisfies a requirement such as `">= 1.2.3, < 2"` or `"^0.9.1"`.
/// Comma-separated comparators must all hold; one without an operator is `^`. Partial versions
/// are padded with zeros, except that `^`, `~` and `=` use the parts that are given (`=1.2`
/// means 1.2.x). An empty requirement matches everything.
pub fn matches(version: &str, req: &str) -> Result<bool, VersionError> {
    let v = version_key(version)?;
    let not_a_requirement = || VersionError::NotARequirement(req.to_owned());
    for comp in req.split(',').map(strip).filter(|c| !c.is_empty()) {
        let caps = REQUIREMENT.captures(comp).ok_or_else(not_a_requirement)?;
        let op = caps.get(1).map_or("^", |m| m.as_str());
        let rest = caps.get(2).map_or("", |m| m.as_str());
        let vm = VERSION.captures(rest).ok_or_else(not_a_requirement)?;
        let parts = (1..=3)
            .filter_map(|i| vm.get(i))
            .map(|m| m.as_str().parse::<u64>().map_err(|_| not_a_requirement()))
            .collect::<Result<Vec<_>, _>>()?;
        let lo = version_key(rest)?;
        let ok = match op {
            ">=" => v >= lo,
            ">" => v > lo,
            "<=" => v <= lo,
            "<" => v < lo,
            "=" if parts.len() == 3 => v == lo,
            _ => {
                // The first part that may not change: the last one given for `=`, the minor (or
                // the major alone) for `~`, the first non-zero one (or the last given) for `^`.
                let fixed = match op {
                    "=" => parts.len() - 1,
                    "~" => parts.len().min(2) - 1,
                    _ => parts
                        .iter()
                        .position(|&x| x != 0)
                        .unwrap_or(parts.len() - 1),
                };
                let mut hi = [0; 3];
                hi[..=fixed].copy_from_slice(&parts[..=fixed]);
                hi[fixed] = hi[fixed].checked_add(1).ok_or_else(not_a_requirement)?;
                let hi: Key = (hi[0], hi[1], hi[2], 0, String::new());
                lo <= v && v < hi
            }
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_pre_releases_before_their_release() {
        let k = |v| version_key(v).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(k(" 1.2 "), (1, 2, 0, 1, String::new()));
        assert_eq!(k("1.2.3-rc.1+build"), (1, 2, 3, 0, "rc.1".to_owned()));
        assert!(k("1.0.0-alpha") < k("1.0.0"));
        assert!(k("1.0.0") < k("1.0.1-alpha"));
        // Plain string order, not semver's numeric identifiers.
        assert!(k("1.0.0-rc.10") < k("1.0.0-rc.9"));
        assert_eq!(k("\u{1c}1.0.0\u{1f}"), (1, 0, 0, 1, String::new()));
        assert_eq!(
            version_key("v1"),
            Err(VersionError::NotAVersion("v1".into()))
        );
    }

    /// The truth table the Python `matches` gave for 702 pairs (tests/fixtures/version-matches.tsv,
    /// generated as the cooldown port's pull request describes).
    #[test]
    fn matches_the_python_on_every_recorded_pair() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/version-matches.tsv"
        );
        let table = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{e}"));
        for line in table.lines() {
            let mut f = line.split('\t');
            let (v, r, want) = (
                f.next().unwrap_or(""),
                f.next().unwrap_or(""),
                f.next().unwrap_or(""),
            );
            let got = match matches(v, r) {
                Ok(true) => "true",
                Ok(false) => "false",
                Err(_) => "error",
            };
            assert_eq!(got, want, "{v:?} against {r:?}");
        }
    }

    #[test]
    fn requirements() {
        let table = [
            ("1.2.3", ">= 1.2.3, < 2", true),
            ("2.0.0", ">= 1.2.3, < 2", false),
            ("0.9.5", "^0.9.1", true),
            ("0.10.0", "^0.9.1", false),
            ("1.9.0", "1.2", true),
            ("0.0.4", "^0.0.3", false),
            ("1.2.9", "~1.2.3", true),
            ("1.3.0", "~1.2.3", false),
            ("1.9.0", "~1", true),
            ("1.2.7", "=1.2", true),
            ("1.3.0", "=1.2", false),
            ("1.2.3", "=1.2.3", true),
            ("1.2.3-rc.1", "=1.2.3", false),
            ("2.0.0-rc.1", "^1.0", false),
            ("1.0.0-rc.1", "^1.0", false),
            ("1.0.0-rc.1", "< 1.0.0", true),
            ("5.0.0", "", true),
            ("5.0.0", " , ", true),
            ("1.0.0", ">\u{1c}1", false),
            ("1.0.0", "\u{1c}>=1", true),
        ];
        for (v, req, want) in table {
            assert_eq!(matches(v, req), Ok(want), "{v} against {req:?}");
        }
        assert_eq!(
            matches("1.0.0", ">= x"),
            Err(VersionError::NotARequirement(">= x".into()))
        );
        assert_eq!(
            matches("x", "1"),
            Err(VersionError::NotAVersion("x".into()))
        );
    }
}
