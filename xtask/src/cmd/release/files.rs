//! `release unexpected-files`: the files a release change may touch, from
//! `.config/release.toml`'s `[allowed-files]`. It reads paths, one per line (`git diff
//! --name-only`), and prints those not on a list, as the workflows' `grep -vxE 'A|B|C'` did:
//! whole lines compared exactly, no pattern and no prefix, each printed line ending in a
//! newline, nothing printed for empty input.

use std::io::Read;
use std::path::Path;

use serde::Deserialize;

use crate::{Error, Result};

pub const CONFIG: &str = ".config/release.toml";

/// `.config/release.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(rename = "allowed-files")]
    pub allowed_files: AllowedFiles,
}

/// The files each kind of release change may touch.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedFiles {
    /// What hotfix.yml's `prepare` may change in main (versions and pins).
    pub hotfix: Vec<String>,
    /// What a pre-release tag's commit may change over its parent (release.yml's `verify` checks
    /// an inline copy).
    pub prerelease: Vec<String>,
}

/// Which list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hotfix,
    Prerelease,
}

impl Config {
    /// Parses the file's text; `shown` names it in errors. Every list must be non-empty, and
    /// every entry a non-empty path on one line, so that a damaged file cannot allow
    /// everything or nothing by accident.
    pub fn parse(text: &str, shown: &str) -> Result<Self> {
        let config: Self = toml::from_str(text).map_err(|e| Error::new(format!("{shown}: {e}")))?;
        for (name, list) in [
            ("hotfix", &config.allowed_files.hotfix),
            ("prerelease", &config.allowed_files.prerelease),
        ] {
            if list.is_empty() {
                return Err(Error::new(format!(
                    "{shown}: allowed-files.{name} is empty"
                )));
            }
            if let Some(bad) = list
                .iter()
                .find(|p| p.is_empty() || p.chars().any(char::is_control))
            {
                return Err(Error::new(format!(
                    "{shown}: allowed-files.{name} has a bad path {bad:?}"
                )));
            }
        }
        Ok(config)
    }

    pub fn list(&self, kind: Kind) -> &[String] {
        match kind {
            Kind::Hotfix => &self.allowed_files.hotfix,
            Kind::Prerelease => &self.allowed_files.prerelease,
        }
    }
}

/// The lines of `input` that are not exactly one of `allowed`, each followed by a newline.
/// Input that is not UTF-8 or holds a NUL is refused: grep would have called it binary.
pub fn unexpected(input: &[u8], allowed: &[String]) -> Result<String> {
    let text = std::str::from_utf8(input).map_err(|_| Error::new("the paths are not UTF-8"))?;
    if text.contains('\0') {
        return Err(Error::new("the paths hold a NUL byte"));
    }
    let body = text.trim_suffix('\n');
    if text.is_empty() {
        return Ok(String::new());
    }
    Ok(body
        .split('\n')
        .filter(|line| !allowed.iter().any(|a| a == line))
        .map(|line| format!("{line}\n"))
        .collect())
}

/// Reads the list of `kind` from `config` (default [`CONFIG`]; a relative path is taken from
/// `repo`), then the paths from `stdin`, and returns what to print.
pub fn run(repo: &Path, kind: Kind, config: Option<&Path>, stdin: &mut dyn Read) -> Result<String> {
    let path = repo.join(config.unwrap_or(Path::new(CONFIG)));
    let shown = config.map_or_else(|| CONFIG.to_owned(), |p| p.display().to_string());
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::new(format!("cannot read {shown}: {e}")))?;
    let config = Config::parse(&text, &shown)?;
    let mut input = Vec::new();
    stdin
        .read_to_end(&mut input)
        .map_err(|e| Error::new(format!("cannot read stdin: {e}")))?;
    unexpected(&input, config.list(kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    fn hotfix() -> Vec<String> {
        [
            "Cargo.toml",
            "Cargo.lock",
            "fuzz/Cargo.lock",
            "CHANGELOG.md",
            "Dockerfile",
        ]
        .map(String::from)
        .to_vec()
    }

    /// What the workflows' `grep -vxE 'Cargo\.toml|...'` prints for `input` (exit status 0 or
    /// 1, both meaning "ran", as `|| true` took them).
    fn grep(input: &[u8], allowed: &[String]) -> Vec<u8> {
        let pattern = allowed
            .iter()
            .map(|p| p.replace('.', r"\."))
            .collect::<Vec<_>>()
            .join("|");
        let mut child = ok(Command::new("grep")
            .args(["-vxE", "--", &pattern])
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn());
        ok(ok(child.stdin.take().ok_or("no stdin")).write_all(input));
        let out = ok(child.wait_with_output());
        assert!(matches!(out.status.code(), Some(0 | 1)), "{out:?}");
        out.stdout
    }

    #[test]
    fn prints_what_grep_printed() {
        let allowed = hotfix();
        for input in [
            "",
            "\n",
            "\n\n",
            "Cargo.toml\n",
            "Cargo.toml",
            "Cargo.toml\nCargo.lock\nfuzz/Cargo.lock\nCHANGELOG.md\nDockerfile\n",
            "Cargo.toml.bak\n",
            "fuzz/Cargo.lock2\n",
            "xCargo.toml\n",
            "crates/a/Cargo.toml\n",
            "Cargo.lock\nbuild.rs\nDockerfile\nsrc/main.rs",
            "CargoXtoml\n",
            "Cargo.toml \n",
            " Cargo.toml\n",
            "Cargo.toml\r\n",
            "cargo.toml\n",
            "Dockerfile\n\nDockerfile\n",
            "\"m\\303\\274de\"\n",
            "Cargo.toml\nCargo.toml\n",
        ] {
            let ours = ok(unexpected(input.as_bytes(), &allowed));
            assert_eq!(
                ours.as_bytes(),
                grep(input.as_bytes(), &allowed),
                "{input:?}"
            );
        }
        for input in [
            "Cargo.toml\n",
            "Cargo.lock\n",
            "fuzz/Cargo.lock\n",
            "CHANGELOG.md\n",
            "x\n",
        ] {
            let prerelease = &allowed[..3];
            assert_eq!(
                ok(unexpected(input.as_bytes(), prerelease)).as_bytes(),
                grep(input.as_bytes(), prerelease),
                "{input:?}"
            );
        }
    }

    #[test]
    fn prefixes_and_empty_input() {
        let allowed = hotfix();
        assert_eq!(ok(unexpected(b"", &allowed)), "");
        assert_eq!(
            ok(unexpected(b"Cargo.toml.bak\nfuzz/Cargo.lock2\n", &allowed)),
            "Cargo.toml.bak\nfuzz/Cargo.lock2\n"
        );
    }

    #[test]
    fn refuses_input_that_is_not_text() {
        let allowed = hotfix();
        assert!(unexpected(b"Cargo.toml\0\n", &allowed).is_err());
        assert!(unexpected(b"m\xfcde\n", &allowed).is_err());
    }

    #[test]
    fn refuses_a_damaged_config() {
        let good = "[allowed-files]\nhotfix = [\"a\"]\nprerelease = [\"b\"]\n";
        assert!(Config::parse(good, "f").is_ok());
        for bad in [
            "",
            "[allowed-files]\nhotfix = [\"a\"]\n",
            "[allowed-files]\nhotfix = []\nprerelease = [\"b\"]\n",
            "[allowed-files]\nhotfix = [\"\"]\nprerelease = [\"b\"]\n",
            "[allowed-files]\nhotfix = [\"a\\nb\"]\nprerelease = [\"b\"]\n",
            "[allowed-files]\nhotfix = [\"a\"]\nprerelease = [\"b\"]\nother = [\"c\"]\n",
            "x = 1\n[allowed-files]\nhotfix = [\"a\"]\nprerelease = [\"b\"]\n",
        ] {
            assert!(Config::parse(bad, "f").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn this_repository_has_both_lists() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let out = ok(run(
            &repo,
            Kind::Prerelease,
            None,
            &mut &b"Cargo.toml\nCargo.lock\nfuzz/Cargo.lock\nCHANGELOG.md\n"[..],
        ));
        assert_eq!(out, "CHANGELOG.md\n");
        let out = ok(run(
            &repo,
            Kind::Hotfix,
            None,
            &mut &b"Dockerfile\nREADME.md\n"[..],
        ));
        assert_eq!(out, "README.md\n");
    }
}
