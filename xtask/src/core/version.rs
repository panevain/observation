//! Crate versions and the Cargo-style requirements RustSec writes for patched and unaffected
//! ranges, ported as is from `scripts/check-cargo-cooldown.py` (`version_key`, `matches`).
//!
//! This is deliberately not full semver: a pre-release sorts before its release, and two
//! pre-releases compare as plain strings, as the script did. Digits are ASCII only.

use std::sync::LazyLock;

use regex::Regex;

static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9]+)(?:\.([0-9]+))?(?:\.([0-9]+))?(?:-([0-9A-Za-z.-]+))?(?:\+\S*)?$")
        .unwrap_or_else(|e| panic!("VERSION regex: {e}"))
});
static REQUIREMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(>=|<=|>|<|=|\^|~)?\s*(.+)$").unwrap_or_else(|e| panic!("REQUIREMENT regex: {e}"))
});

/// A version that [`matches`] or [`version_key`] could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    NotAVersion(String),
    NotARequirement(String),
}

/// Sort key of a full version: major, minor, patch (missing parts are 0), then 0 for a
/// pre-release and 1 for a release, then the pre-release text.
pub type Key = (u64, u64, u64, u8, String);

/// The sort key of `v` (surrounding whitespace ignored).
pub fn version_key(v: &str) -> Result<Key, VersionError> {
    let not_a_version = || VersionError::NotAVersion(v.to_owned());
    let caps = VERSION.captures(v.trim()).ok_or_else(not_a_version)?;
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
    for comp in req.split(',').map(str::trim).filter(|c| !c.is_empty()) {
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
        assert_eq!(
            version_key("v1"),
            Err(VersionError::NotAVersion("v1".into()))
        );
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
