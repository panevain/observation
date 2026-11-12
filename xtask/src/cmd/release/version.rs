//! `release version get` and `release version set`: the workspace version in the root
//! `Cargo.toml`'s `[workspace.package]`, as `scripts/workspace-version.sh` and
//! `scripts/set-version.sh` read and write it (see [`crate::core::manifest`] for which lines).
//!
//! Messages, output and the files written are the scripts'. Where the scripts failed, these
//! fail too; the differences, all failures, are listed in `release-version/cases.toml`
//! (`differs`).

use std::io::Read;
use std::path::Path;

use crate::core::manifest::{is_version, set_workspace_version, workspace_version};
use crate::ports::Cargo;
use crate::{Error, Result};

fn read_utf8(path: &Path, shown: &str) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| Error::new(format!("cannot read {shown}: {e}")))?;
    String::from_utf8(bytes).map_err(|_| Error::new(format!("{shown} is not UTF-8")))
}

/// The workspace version of FILE (default `Cargo.toml` in `repo`; `-` reads `stdin`), as the
/// line to print. NAME is what the error calls the file (default FILE). An empty FILE or NAME
/// is the default, as in the script. `annotate` prefixes the error with `::error::` (the
/// script did so whenever `GITHUB_ACTIONS` was set and not empty).
pub fn get(
    repo: &Path,
    file: Option<&str>,
    name: Option<&str>,
    stdin: &mut dyn Read,
    annotate: bool,
) -> Result<String> {
    let file = file.filter(|f| !f.is_empty()).unwrap_or("Cargo.toml");
    let name = name.filter(|n| !n.is_empty()).unwrap_or(file);
    let text = if file == "-" {
        let mut bytes = Vec::new();
        stdin
            .read_to_end(&mut bytes)
            .map_err(|e| Error::new(format!("cannot read stdin: {e}")))?;
        String::from_utf8(bytes).map_err(|_| Error::new("stdin is not UTF-8"))?
    } else {
        read_utf8(&repo.join(file), file)?
    };
    let version = workspace_version(&text);
    if !is_version(&version) {
        let prefix = if annotate { "::error::" } else { "" };
        return Err(Error::new(format!(
            "{prefix}bad workspace version in {name}: '{version}'"
        )));
    }
    Ok(format!("{version}\n"))
}

/// Sets the workspace version in `repo`'s `Cargo.toml`, checks that it reads back, then runs
/// `cargo update -w` in `repo` and again for `fuzz/Cargo.toml`, as the script did. Returns
/// whether it failed without a message of its own (a cargo step failed; cargo said why).
/// What the read-back check reports goes to `diagnostics` first.
pub fn set(
    repo: &Path,
    version: &str,
    cargo: &dyn Cargo,
    annotate: bool,
    diagnostics: &mut String,
) -> Result<bool> {
    if !is_version(version) {
        return Err(Error::new(format!("not a semver version: {version}")));
    }
    let path = repo.join("Cargo.toml");
    let text = read_utf8(&path, "Cargo.toml")?;
    std::fs::write(&path, set_workspace_version(&text, version))
        .map_err(|e| Error::new(format!("cannot write Cargo.toml: {e}")))?;
    let back = match get(repo, None, None, &mut std::io::empty(), annotate) {
        Ok(line) => line.trim_end_matches('\n').to_owned(),
        Err(e) => {
            diagnostics.push_str(&format!("{e}\n"));
            String::new()
        }
    };
    if back != version {
        return Err(Error::new(
            "could not set the version in Cargo.toml [workspace.package]",
        ));
    }
    for manifest in [None, Some("fuzz/Cargo.toml")] {
        if !cargo.update_workspace(repo, manifest)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::fakes::FakeCargo;
    use serde::Deserialize;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    /// release-version/cases.toml.
    #[derive(Deserialize)]
    pub(crate) struct Cases {
        pub template: BTreeMap<String, BTreeMap<String, String>>,
        pub case: Vec<Case>,
    }

    #[derive(Deserialize)]
    pub(crate) struct Case {
        pub name: String,
        /// `get` or `set`.
        pub command: String,
        #[serde(default)]
        pub args: Vec<String>,
        /// A `[template.NAME]` whose files the case starts from.
        pub template: Option<String>,
        /// Files on top of the template; with `raw`, each character is the byte of its code
        /// point (so that a case can hold bytes that are not UTF-8).
        #[serde(default)]
        pub files: BTreeMap<String, String>,
        #[serde(default)]
        pub raw: bool,
        pub stdin: Option<String>,
        /// The value of `GITHUB_ACTIONS` (unset when missing).
        pub github_actions: Option<String>,
        /// Which cargo call fails (1 for the first; recorded runs only).
        pub cargo_fails: Option<usize>,
        /// Why the port's result differs from the script's, on purpose: `stricter` (the
        /// script passed and the port fails) or `message` (both fail, with different output or
        /// exit status).
        pub differs: Option<String>,
    }

    pub(crate) fn load() -> (Cases, String) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/release-version");
        let cases = ok(toml::from_str(&ok(fs::read_to_string(
            dir.join("cases.toml"),
        ))));
        (cases, ok(fs::read_to_string(dir.join("golden"))))
    }

    /// The bytes of a case file.
    pub(crate) fn bytes(text: &str, raw: bool) -> Vec<u8> {
        if raw {
            text.chars()
                .map(|c| u8::try_from(u32::from(c)).unwrap_or_else(|e| panic!("{e}")))
                .collect()
        } else {
            text.as_bytes().to_vec()
        }
    }

    /// Writes the case's files into `dir`.
    pub(crate) fn populate(cases: &Cases, case: &Case, dir: &Path) {
        let template = case.template.as_ref().map(|t| &cases.template[t]);
        for (path, text) in template.into_iter().flatten() {
            let path = dir.join(path);
            ok(fs::create_dir_all(ok(path.parent().ok_or("no parent"))));
            ok(fs::write(path, text));
        }
        for (path, text) in &case.files {
            let path = dir.join(path);
            ok(fs::create_dir_all(ok(path.parent().ok_or("no parent"))));
            ok(fs::write(path, bytes(text, case.raw)));
        }
    }

    /// The golden's sections by case name: `exit N`, stdout, stderr, cargo calls, Cargo.toml.
    pub(crate) fn sections(golden: &str) -> BTreeMap<String, [String; 5]> {
        let mut out = BTreeMap::new();
        for block in golden.split("\n== ") {
            let block = format!(
                "{}\n",
                block.trim_start_matches("== ").trim_end_matches('\n')
            );
            let (name, rest) = block.split_once('\n').unwrap_or((&block, ""));
            let (code, rest) = rest.split_once('\n').unwrap_or((rest, ""));
            let rest = rest.trim_prefix("-- stdout\n");
            let (stdout, rest) = rest.split_once("-- stderr\n").unwrap_or((rest, ""));
            let (stderr, rest) = rest.split_once("-- cargo\n").unwrap_or((rest, ""));
            let (cargo, manifest) = rest.split_once("-- Cargo.toml\n").unwrap_or((rest, ""));
            let manifest = manifest.trim_suffix('\n');
            let owned = [code, stdout, stderr, cargo, manifest].map(str::to_owned);
            out.insert(name.to_owned(), owned);
        }
        out
    }

    /// One case on the fake cargo: exit status, stdout, stderr, cargo calls, Cargo.toml after.
    fn run_case(cases: &Cases, case: &Case) -> (u8, String, String, String, String) {
        let tmp = ok(tempfile::tempdir());
        populate(cases, case, tmp.path());
        let annotate = case
            .github_actions
            .as_deref()
            .is_some_and(|v| !v.is_empty());
        let cargo = FakeCargo {
            update_fails: case.cargo_fails,
            ..FakeCargo::default()
        };
        let mut stdin = case.stdin.as_deref().unwrap_or("").as_bytes();
        let mut diagnostics = String::new();
        let arg = |i: usize| case.args.get(i).map(String::as_str);
        let result = match case.command.as_str() {
            "get" => get(tmp.path(), arg(0), arg(1), &mut stdin, annotate).map(|out| (out, false)),
            "set" => {
                let version = arg(0).unwrap_or("");
                set(tmp.path(), version, &cargo, annotate, &mut diagnostics)
                    .map(|failed| (String::new(), failed))
            }
            other => panic!("unknown command {other}"),
        };
        let (code, stdout, stderr) = match result {
            Ok((stdout, failed)) => (u8::from(failed), stdout, diagnostics),
            Err(e) => (1, String::new(), format!("{diagnostics}{e}\n")),
        };
        let calls: String = cargo
            .calls
            .take()
            .iter()
            .map(|c| format!("{c}\n"))
            .collect();
        let manifest = match case.command.as_str() {
            "set" => fs::read(tmp.path().join("Cargo.toml"))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        (code, stdout, stderr, calls, manifest)
    }

    /// Every case of release-version/cases.toml against what the scripts did with it
    /// (release-version/golden, captured from them with a stand-in cargo that records its
    /// calls): where a script failed, the port fails, with the same exit status and output
    /// unless the case is listed as `differs = "message"`; where it passed, the port gives the
    /// same stdout, stderr, cargo calls and Cargo.toml, unless listed as `differs = "stricter"`,
    /// where it fails.
    #[test]
    fn matches_the_scripts_and_never_passes_where_they_failed() {
        let (cases, golden) = load();
        let want = sections(&golden);
        assert_eq!(
            want.len(),
            cases.case.len(),
            "cases and golden differ in length"
        );
        let mut problems = Vec::new();
        for case in &cases.case {
            let (code, stdout, stderr, calls, manifest) = run_case(&cases, case);
            let Some([sh_code, sh_stdout, sh_stderr, sh_calls, sh_manifest]) = want.get(&case.name)
            else {
                problems.push(format!("{}: not in the golden", case.name));
                continue;
            };
            let manifest = manifest.trim_suffix('\n');
            let same = *sh_code == format!("exit {code}")
                && stdout == *sh_stdout
                && stderr == *sh_stderr
                && calls == *sh_calls
                && manifest == sh_manifest;
            let problem = match (sh_code.as_str(), code, case.differs.as_deref()) {
                ("exit 0", 0, Some("stricter")) => "passes, but is listed as stricter",
                ("exit 0", 0, _) if !same => "passes with different output",
                ("exit 0", 0, _) => "",
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
                    "{}: {problem}\n  script: {sh_code} {sh_stdout:?} {sh_stderr:?} {sh_calls:?}\n  \
                     port: exit {code} {stdout:?} {stderr:?} {calls:?}",
                    case.name
                ));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
