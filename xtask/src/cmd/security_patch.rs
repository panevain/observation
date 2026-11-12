//! `security-patch check`: whether the latest release needs a security patch release, worked
//! out as `scripts/security-patch.py check` does. That script stays: hotfix.yml writes the
//! hotfix pull request's files with it, in a job that builds nothing. The port is for Release
//! watch's `scan`, which only looks. This part reads what the scanners report and writes the
//! report; running them comes with the command line.
//!
//! The repository's `Cargo.lock` is audited (cargo-audit, honouring its `.cargo/audit.toml`),
//! `cargo update -p` is run for each vulnerable crate, and the lock is audited again: the
//! advisories gone are the ones a patch release fixes. The published images of the version are
//! scanned with Trivy (fixable MEDIUM and above, OS packages); a finding that the Dockerfile's
//! base image still has (scanned at its tag) waits for that base to be rebuilt.
//!
//! Output, `report.json`, `summary.md` and the commands run are the script's. Where the script
//! failed, this fails too. It also fails, where the script went on, on a field it reads that is
//! not a string (or, for `patched`, not a list of strings), on JSON that Python reads and
//! serde_json does not (NaN, a lone surrogate, deep nesting), and on a crate name or image
//! reference that starts with `-` (it would be read as an option).

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::core::py::json_truthy;
use crate::{Error, Result};

/// The published images: name, registry path, the Dockerfile stage whose base gives the image
/// its OS packages, and whether a rebuild picks up every Debian fix (the region agent runs
/// `apt-get upgrade`).
pub const IMAGES: [(&str, &str, &str, bool); 2] = [
    (
        "observation",
        "ghcr.io/panevain/observation",
        "runtime-base",
        false,
    ),
    (
        "observation-regions",
        "ghcr.io/panevain/observation-regions",
        "regions",
        true,
    ),
];
/// The severities that call for a patch release.
const TRIGGER: [&str; 2] = ["CRITICAL", "HIGH"];

/// Python's `\s` and `\S` in a `str` pattern (`str.isspace()`: White_Space plus U+001C..U+001F).
const S: &str = r"[\s\x1c-\x1f]";
const NS: &str = r"[^\s\x1c-\x1f]";

/// The script's `FROM_RE` (`re.M | re.I`).
static FROM_RE: LazyLock<Regex> = LazyLock::new(|| {
    let re = format!(
        r"(?mi)^FROM{S}+({NS}+?):({NS}+?)@(sha256:[0-9a-f]{{64}})({S}+AS{S}+({NS}+))?{S}*$"
    );
    Regex::new(&re).unwrap_or_else(|e| panic!("FROM_RE: {e}"))
});
/// The script's `lock_versions` pattern (`re.M`; `[^"]` takes a newline too, as in Python).
static LOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^name = "([^"]+)"\nversion = "([^"]+)""#)
        .unwrap_or_else(|e| panic!("LOCK_RE: {e}"))
});

/// A RustSec advisory cargo-audit reported for a crate in the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Advisory {
    pub id: String,
    pub title: String,
    pub url: String,
    #[serde(rename = "crate")]
    pub krate: String,
    pub version: String,
    pub patched: Vec<String>,
}

/// An advisory gone after `cargo update`, with the crate versions it went to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fixed {
    #[serde(flatten)]
    pub advisory: Advisory,
    pub to: String,
}

/// A fixable OS-package vulnerability Trivy found in an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub id: String,
    pub pkg: String,
    pub installed: String,
    pub fixed: String,
    pub severity: String,
    pub title: String,
    /// The base image (at its tag) still has it: a rebuild would not clear it yet.
    pub waiting: bool,
}

/// One published image, and its findings unless it is `missing` (not published).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Image {
    pub name: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub missing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<Finding>>,
}

/// The crate advisories: those a patch release fixes, and those it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rust {
    pub fixed: Vec<Fixed>,
    pub unfixable: Vec<Advisory>,
}

/// What `report.json` holds, in its order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub version: String,
    pub rust: Rust,
    pub images: Vec<Image>,
    pub action: &'static str,
    pub attention: bool,
}

fn object<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>> {
    v.as_object()
        .ok_or_else(|| Error::new(format!("{what} is not an object: {v}")))
}

fn field<'a>(o: &'a Map<String, Value>, key: &str, what: &str) -> Result<&'a Value> {
    o.get(key)
        .ok_or_else(|| Error::new(format!("{what} has no `{key}`")))
}

fn string(v: &Value, what: &str) -> Result<String> {
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::new(format!("{what} is not a string: {v}")))
}

/// What the script's `for x in value` went through: a list's items. An empty object or string
/// gives nothing, as it did there; anything else made the script fail.
fn items<'a>(v: &'a Value, what: &str) -> Result<&'a [Value]> {
    match v {
        Value::Array(a) => Ok(a),
        Value::Object(o) if o.is_empty() => Ok(&[]),
        Value::String(s) if s.is_empty() => Ok(&[]),
        _ => Err(Error::new(format!("{what} is not a list: {v}"))),
    }
}

/// The advisories in cargo-audit's `--json` report (`vulnerabilities.list`).
pub fn advisories(report: &Value) -> Result<Vec<Advisory>> {
    let what = "cargo audit's report";
    let Some(vulnerabilities) = object(report, what)?.get("vulnerabilities") else {
        return Ok(Vec::new());
    };
    let list = match object(vulnerabilities, "`vulnerabilities`")?.get("list") {
        Some(list) => items(list, "`vulnerabilities.list`")?,
        None => &[],
    };
    let mut out = Vec::new();
    for v in list {
        let v = object(v, "a vulnerability")?;
        let a = object(field(v, "advisory", "a vulnerability")?, "`advisory`")?;
        let pkg = object(field(v, "package", "a vulnerability")?, "`package`")?;
        let id = string(field(a, "id", "an advisory")?, "an advisory's `id`")?;
        let title = match a.get("title") {
            Some(t) => string(t, "an advisory's `title`")?,
            None => String::new(),
        };
        let url = match a.get("url") {
            Some(u) if json_truthy(u) => string(u, "an advisory's `url`")?,
            _ => format!("https://rustsec.org/advisories/{id}"),
        };
        let patched = match v.get("versions") {
            Some(versions) => match object(versions, "`versions`")?.get("patched") {
                Some(Value::Array(p)) => p
                    .iter()
                    .map(|p| string(p, "a patched version"))
                    .collect::<Result<_>>()?,
                Some(p) => return Err(Error::new(format!("`patched` is not a list: {p}"))),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        out.push(Advisory {
            id,
            title,
            url,
            krate: string(field(pkg, "name", "a package")?, "a package's `name`")?,
            version: string(field(pkg, "version", "a package")?, "a package's `version`")?,
            patched,
        });
    }
    Ok(out)
}

/// The (name, version) of every package in a `Cargo.lock`.
pub fn lock_versions(text: &str) -> BTreeSet<(String, String)> {
    LOCK_RE
        .captures_iter(text)
        .map(|c| (c[1].to_owned(), c[2].to_owned()))
        .collect()
}

/// The fixable OS-package findings in Trivy's `--format json` report.
pub fn findings(report: &Value) -> Result<Vec<Finding>> {
    let results = match object(report, "Trivy's report")?.get("Results") {
        Some(r) => items(r, "`Results`")?,
        None => &[],
    };
    let mut out = Vec::new();
    for r in results {
        let r = object(r, "a result")?;
        if r.get("Class").and_then(Value::as_str) != Some("os-pkgs") {
            continue;
        }
        let vulnerabilities = match r.get("Vulnerabilities") {
            Some(Value::Array(v)) => v.as_slice(),
            Some(v) if json_truthy(v) => {
                return Err(Error::new(format!("`Vulnerabilities` is not a list: {v}")));
            }
            _ => &[],
        };
        for v in vulnerabilities {
            let v = object(v, "a vulnerability")?;
            let get = |key: &str| string(field(v, key, "a vulnerability")?, key);
            let or = |key: &str, default: &str| match v.get(key) {
                Some(s) => string(s, key),
                None => Ok(default.to_owned()),
            };
            let title = match v.get("Title") {
                Some(t) if json_truthy(t) => string(t, "Title")?.chars().take(120).collect(),
                _ => String::new(),
            };
            out.push(Finding {
                id: get("VulnerabilityID")?,
                pkg: get("PkgName")?,
                installed: get("InstalledVersion")?,
                fixed: or("FixedVersion", "")?,
                severity: or("Severity", "UNKNOWN")?,
                title,
                waiting: false,
            });
        }
    }
    Ok(out)
}

/// {stage: "image:tag"} for every digest-pinned `FROM … AS stage` line (the last one wins).
pub fn dockerfile_bases(text: &str) -> HashMap<String, String> {
    FROM_RE
        .captures_iter(text)
        .filter_map(|c| {
            Some((
                c.get(5)?.as_str().to_owned(),
                format!("{}:{}", &c[1], &c[2]),
            ))
        })
        .collect()
}

/// The report from what was found.
pub fn assess(
    version: &str,
    fixed: Vec<Fixed>,
    unfixable: Vec<Advisory>,
    images: Vec<Image>,
) -> Report {
    let scanned = || images.iter().filter_map(|i| i.findings.as_ref());
    let trigger = scanned()
        .flatten()
        .any(|f| TRIGGER.contains(&f.severity.as_str()) && !f.waiting);
    let attention = !unfixable.is_empty() || scanned().any(|f| !f.is_empty()) || !fixed.is_empty();
    Report {
        version: version.to_owned(),
        action: if !fixed.is_empty() || trigger {
            "hotfix"
        } else {
            "none"
        },
        attention,
        rust: Rust { fixed, unfixable },
        images,
    }
}

/// `summary.md`.
pub fn summarize(report: &Report) -> String {
    let mut lines = vec![
        format!("Latest release: **v{}**", report.version),
        String::new(),
    ];
    if !report.rust.fixed.is_empty() {
        lines.push("Crate advisories that a patch release fixes:".into());
        lines.push(String::new());
        for Fixed { advisory: v, to } in &report.rust.fixed {
            lines.push(format!(
                "- [{}]({}): `{}` {} -> {to} ({})",
                v.id, v.url, v.krate, v.version, v.title
            ));
        }
        lines.push(String::new());
    }
    if !report.rust.unfixable.is_empty() {
        lines.push(
            "Crate advisories with no semver-compatible fix (they need a code change or an \
             upstream release; add an ignore with a reason only if the code is unreachable):"
                .into(),
        );
        lines.push(String::new());
        for v in &report.rust.unfixable {
            let patched = if v.patched.is_empty() {
                "nothing yet".to_owned()
            } else {
                v.patched.join(", ")
            };
            lines.push(format!(
                "- [{}]({}): `{}` {}, patched in {patched} ({})",
                v.id, v.url, v.krate, v.version, v.title
            ));
        }
        lines.push(String::new());
    }
    for img in &report.images {
        let Some(fs) = &img.findings else {
            lines.push(format!("`{}`: not published.", img.reference));
            lines.push(String::new());
            continue;
        };
        if fs.is_empty() {
            lines.push(format!(
                "`{}`: no fixable OS vulnerabilities (MEDIUM and above).",
                img.reference
            ));
            lines.push(String::new());
            continue;
        }
        lines.push(format!("`{}`: fixable OS vulnerabilities:", img.reference));
        lines.push(String::new());
        lines.push("| Severity | ID | Package | Installed | Fixed | Rebuild fixes it |".into());
        lines.push("| --- | --- | --- | --- | --- | --- |".into());
        let order = |s: &str| match s {
            "CRITICAL" => 0,
            "HIGH" => 1,
            "MEDIUM" => 2,
            _ => 9,
        };
        let mut sorted: Vec<&Finding> = fs.iter().collect();
        sorted.sort_by(|a, b| (order(&a.severity), &a.id).cmp(&(order(&b.severity), &b.id)));
        for f in sorted {
            let rebuild = if f.waiting {
                "no, the base image has not been rebuilt yet"
            } else {
                "yes"
            };
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {rebuild} |",
                f.severity, f.id, f.pkg, f.installed, f.fixed
            ));
        }
        lines.push(String::new());
    }
    lines.push(
        if report.action == "hotfix" {
            "**Action:** a patch release (hotfix.yml) fixes the items marked above."
        } else {
            "**Action:** nothing a patch release could fix right now."
        }
        .into(),
    );
    lines.join("\n") + "\n"
}

/// `report.json`: `json.dumps(report, indent=2)`, whose `ensure_ascii` writes every character
/// outside space to `~` as `\\uXXXX`. Outside strings serde_json writes only those and newlines;
/// inside them it has escaped everything else below space as Python does.
pub fn report_json(report: &Report) -> Result<String> {
    let text = serde_json::to_string_pretty(report).map_err(|e| Error::new(e.to_string()))?;
    let mut out = String::new();
    for c in text.chars() {
        if c == '\n' || (' '..='~').contains(&c) {
            out.push(c);
        } else {
            for u in c.encode_utf16(&mut [0; 2]) {
                let _ = write!(out, "\\u{u:04x}");
            }
        }
    }
    Ok(out + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn json(text: &str) -> Value {
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/security-patch")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The script's `for x in value`: a list, or an empty object or string; nothing else.
    #[test]
    fn lists_as_the_script_iterated_them() {
        for (list, passes) in [
            ("[]", true),
            ("{}", true),
            (r#""""#, true),
            ("null", false),
            (r#"{"a": 1}"#, false),
            (r#""a""#, false),
            ("0", false),
        ] {
            let report = json(&format!(r#"{{"vulnerabilities": {{"list": {list}}}}}"#));
            assert_eq!(advisories(&report).is_ok(), passes, "list {list}");
            let report = json(&format!(r#"{{"Results": {list}}}"#));
            assert_eq!(findings(&report).is_ok(), passes, "Results {list}");
        }
        assert_eq!(advisories(&json("{}")), Ok(Vec::new()));
        assert!(advisories(&json(r#"{"vulnerabilities": null}"#)).is_err());
    }

    #[test]
    fn advisory_defaults() {
        let report = json(
            r#"{"vulnerabilities": {"list": [
                {"advisory": {"id": "A", "url": null}, "package": {"name": "n", "version": "1"}},
                {"advisory": {"id": "B", "title": "t", "url": "https://x"},
                 "package": {"name": "m", "version": "2"}, "versions": {"patched": [">=3"]}}
            ]}}"#,
        );
        let [a, b] = <[Advisory; 2]>::try_from(advisories(&report).unwrap_or_default())
            .unwrap_or_else(|v| panic!("{v:?}"));
        assert_eq!(
            (a.title.as_str(), a.url.as_str(), a.patched.len()),
            ("", "https://rustsec.org/advisories/A", 0)
        );
        assert_eq!(
            (b.url.as_str(), b.patched),
            ("https://x", vec![">=3".to_owned()])
        );
        // Stricter than the script, which carried any JSON value along.
        let bad = r#"{"vulnerabilities": {"list": [{"advisory": {"id": 5}, "package": {}}]}}"#;
        assert!(advisories(&json(bad)).is_err());
    }

    #[test]
    fn os_package_findings_only_with_defaults() {
        let report = json(&format!(
            r#"{{"Results": [
                {{"Class": "lang-pkgs", "Vulnerabilities": [1]}},
                {{"Class": "os-pkgs", "Vulnerabilities": null}},
                {{"Class": "os-pkgs", "Vulnerabilities": [
                    {{"VulnerabilityID": "CVE-1", "PkgName": "p", "InstalledVersion": "1",
                      "Title": "{}"}}]}}
            ]}}"#,
            "é".repeat(130)
        ));
        let found = findings(&report).unwrap_or_default();
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].fixed.as_str(), found[0].severity.as_str()),
            ("", "UNKNOWN")
        );
        assert_eq!(found[0].title, "é".repeat(120));
    }

    /// Both read with the script's patterns (see the outputs Python gave in each comment).
    #[test]
    fn lock_and_dockerfile_patterns() {
        let lock = "x\nname = \"a\"\nversion = \"1\"\n\nname = \"b\"\nsource = \"x\"\n\
                    version = \"2\"\nname = \"c\"\nversion = \"3\"\n";
        // [('a', '1'), ('c', '3')]
        let want = [("a", "1"), ("c", "3")].map(|(n, v)| (n.to_owned(), v.to_owned()));
        assert_eq!(lock_versions(lock), BTreeSet::from(want));
        let d = |c: char| c.to_string().repeat(64);
        let dockerfile = format!(
            "FROM a:b@sha256:{} AS one\nfrom c:d@SHA256:{} as two\nFROM e:f AS three\n\
             FROM g:h@sha256:{}\nFROM i:j@sha256:{} AS one\n",
            d('0'),
            d('A'),
            d('1'),
            d('2')
        );
        // {'one': 'i:j', 'two': 'c:d'}
        let want = [("one", "i:j"), ("two", "c:d")].map(|(s, b)| (s.to_owned(), b.to_owned()));
        assert_eq!(dockerfile_bases(&dockerfile), HashMap::from(want));
    }

    /// report.json and summary.md as the script's `json.dumps` and `summarize` wrote them for
    /// the same report (the fixtures).
    #[test]
    fn report_and_summary_as_the_script_wrote_them() {
        let finding = |id: &str, pkg: &str, severity: &str, title: &str, waiting| Finding {
            id: id.into(),
            pkg: pkg.into(),
            installed: "1".into(),
            fixed: "2".into(),
            severity: severity.into(),
            title: title.into(),
            waiting,
        };
        let advisory = |id: &str, title: &str, url: &str, krate: &str, version: &str| Advisory {
            id: id.into(),
            title: title.into(),
            url: url.into(),
            krate: krate.into(),
            version: version.into(),
            patched: Vec::new(),
        };
        let mut regex = advisory(
            "RUSTSEC-2022-0013",
            "Regexes with large repetitions",
            "https://rustsec.org/advisories/RUSTSEC-2022-0013",
            "regex",
            "1.5.4",
        );
        regex.patched = vec![">=1.5.5".into()];
        let images = vec![
            Image {
                name: "observation".into(),
                reference: "ghcr.io/panevain/observation:1.2.3".into(),
                missing: false,
                findings: Some(vec![
                    finding("CVE-2", "libc6", "MEDIUM", "é\u{7f}", false),
                    finding("CVE-1", "libssl3", "CRITICAL", "🦀", true),
                    finding("CVE-0", "zlib", "LOW", "", false),
                ]),
            },
            Image {
                name: "observation-regions".into(),
                reference: "ghcr.io/panevain/observation-regions:1.2.3".into(),
                missing: true,
                findings: None,
            },
        ];
        let fixed = vec![Fixed {
            advisory: regex,
            to: "1.5.5".into(),
        }];
        let time = advisory(
            "RUSTSEC-2020-0071",
            "Potential segfault",
            "https://example.com/a",
            "time",
            "0.1.43",
        );
        let report = assess("1.2.3", fixed, vec![time], images);
        assert_eq!((report.action, report.attention), ("hotfix", true));
        assert_eq!(report_json(&report), Ok(fixture("report.json")));
        assert_eq!(summarize(&report), fixture("summary.md"));
    }

    /// The action: a fixed advisory, or a CRITICAL/HIGH finding a rebuild clears.
    #[test]
    fn action_and_attention() {
        let image = |severity: &str, waiting| Image {
            name: "i".into(),
            reference: "r".into(),
            missing: false,
            findings: Some(vec![Finding {
                id: "C".into(),
                pkg: "p".into(),
                installed: "1".into(),
                fixed: "2".into(),
                severity: severity.into(),
                title: String::new(),
                waiting,
            }]),
        };
        let verdict = |images| {
            let r = assess("1.0.0", Vec::new(), Vec::new(), images);
            (r.action, r.attention)
        };
        assert_eq!(verdict(vec![]), ("none", false));
        assert_eq!(verdict(vec![image("HIGH", false)]), ("hotfix", true));
        assert_eq!(verdict(vec![image("HIGH", true)]), ("none", true));
        assert_eq!(verdict(vec![image("MEDIUM", false)]), ("none", true));
    }
}
