//! `cargo xtask tokens`: the design-token check (`.config/design-tokens.toml`). Every colour the
//! client uses must come from the theme file; any other `.rs` file under the scanned roots with
//! a colour literal fails.
//!
//! Two differences from the script it replaced (scripts/check-design-tokens.sh): a file with a
//! NUL byte is scanned line by line (grep reported only "Binary file ... matches"), and a scan
//! root that is a symbolic link to a directory is followed (the script's `find` scanned nothing
//! there). Symbolic links inside a root are still skipped, as `find -type f` skipped them.

use std::fs;
use std::path::Path;

use regex::bytes::Regex;
use serde::Deserialize;

use crate::{Error, Result};

pub const CONFIG: &str = ".config/design-tokens.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Directories scanned for `.rs` files, relative to the repository.
    pub roots: Vec<String>,
    /// The one file allowed to hold colour literals.
    pub theme: String,
    /// What counts as a colour literal, each a regex matched against one line.
    pub patterns: Vec<String>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).map_err(|e| Error::new(format!("{CONFIG}: {e}")))
    }

    /// The patterns as one alternation.
    pub fn regex(&self) -> Result<Regex> {
        Regex::new(&self.patterns.join("|"))
            .map_err(|e| Error::new(format!("{CONFIG}: patterns: {e}")))
    }
}

/// A `.rs` file under a root: its path relative to the repository, and its bytes.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: String,
    pub text: Vec<u8>,
}

/// The check's output: offending lines for stdout, the explanation for stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Outcome {
    pub fn failed(&self) -> bool {
        !self.stdout.is_empty()
    }
}

/// Every line of `sources` (in the order given) that `regex` matches, except in the theme file,
/// as `path:line:text` lines.
pub fn scan(sources: &[Source], theme: &str, regex: &Regex) -> Outcome {
    let mut stdout = Vec::new();
    for source in sources.iter().filter(|s| s.path != theme) {
        let mut lines = source.text.split(|&b| b == b'\n').collect::<Vec<_>>();
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        for (i, line) in lines.into_iter().enumerate() {
            if regex.is_match(line) {
                stdout.extend_from_slice(format!("{}:{}:", source.path, i + 1).as_bytes());
                stdout.extend_from_slice(line);
                stdout.push(b'\n');
            }
        }
    }
    let stderr = if stdout.is_empty() {
        String::new()
    } else {
        format!(
            "\ndesign tokens: hard-coded colour literals found above.\n\
             Define the colour in {theme} and reference it from there.\n"
        )
    };
    Outcome { stdout, stderr }
}

/// The `.rs` files under the configured roots of the repository at `repo`, sorted by path
/// (byte order). Symbolic links are neither followed nor read. Fails when the theme file or a
/// root is missing.
pub fn sources(repo: &Path, config: &Config) -> Result<Vec<Source>> {
    if config.roots.is_empty() {
        return Err(Error::new(format!(
            "design tokens: {CONFIG} lists no roots, so nothing would be checked"
        )));
    }
    if !repo.join(&config.theme).is_file() {
        return Err(Error::new(format!(
            "design tokens: {} not found",
            config.theme
        )));
    }
    let mut paths = Vec::new();
    for root in &config.roots {
        if !repo.join(root).is_dir() {
            return Err(Error::new(format!("design tokens: {root} not found")));
        }
        walk(repo, root, &mut paths)?;
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text =
                fs::read(repo.join(&path)).map_err(|e| Error::new(format!("{path}: {e}")))?;
            Ok(Source { path, text })
        })
        .collect()
}

fn walk(repo: &Path, dir: &str, out: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(repo.join(dir)).map_err(|e| Error::new(format!("{dir}: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::new(format!("{dir}: {e}")))?;
        let kind = entry
            .file_type()
            .map_err(|e| Error::new(format!("{dir}: {e}")))?;
        let name = entry.file_name();
        let path = format!("{dir}/{}", name.to_string_lossy());
        if kind.is_dir() {
            walk(repo, &path, out)?;
        } else if kind.is_file() && path.ends_with(".rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Runs the check on the repository at `repo`.
pub fn run(repo: &Path) -> Result<Outcome> {
    let path = repo.join(CONFIG);
    let text = fs::read_to_string(&path)
        .map_err(|e| Error::new(format!("design tokens: {}: {e}", path.display())))?;
    let config = Config::parse(&text)?;
    let regex = config.regex()?;
    Ok(scan(&sources(repo, &config)?, &config.theme, &regex))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ok<T>(r: Result<T>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    fn manifest_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// The repository's own configuration, checking the fixture tree.
    fn check(tree: &Path) -> Result<Outcome> {
        let text = ok(
            fs::read_to_string(manifest_dir().join("..").join(CONFIG).as_path())
                .map_err(|e| Error::new(e.to_string())),
        );
        let config = ok(Config::parse(&text));
        Ok(scan(
            &sources(tree, &config)?,
            &config.theme,
            &ok(config.regex()),
        ))
    }

    /// The fixture tree's output is the golden captured from scripts/check-design-tokens.sh
    /// (see the pull request that ported it); OBS_BLESS=1 rewrites it.
    #[test]
    fn fixture_tree_matches_the_golden() {
        let fixtures = manifest_dir().join("tests/fixtures");
        let out = ok(check(&fixtures.join("tokens")));
        let golden = fixtures.join("tokens.stdout");
        if std::env::var_os("OBS_BLESS").is_some() {
            ok(fs::write(&golden, &out.stdout).map_err(|e| Error::new(e.to_string())));
        }
        let want = ok(fs::read(&golden).map_err(|e| Error::new(e.to_string())));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&want)
        );
        assert!(out.failed());
        assert_eq!(
            out.stderr,
            "\ndesign tokens: hard-coded colour literals found above.\n\
             Define the colour in crates/observation-client/src/theme.rs and reference it from there.\n"
        );
    }

    #[test]
    fn missing_theme_or_root_fails() {
        let tmp = ok(tempfile::tempdir().map_err(|e| Error::new(e.to_string())));
        let tree = tmp.path();
        let mkdir =
            |p: &str| ok(fs::create_dir_all(tree.join(p)).map_err(|e| Error::new(e.to_string())));
        mkdir("crates/observation-client/src");
        assert_eq!(
            check(tree).map(|o| o.stdout),
            Err(Error::new(
                "design tokens: crates/observation-client/src/theme.rs not found"
            ))
        );
        ok(
            fs::write(tree.join("crates/observation-client/src/theme.rs"), "")
                .map_err(|e| Error::new(e.to_string())),
        );
        assert_eq!(
            check(tree).map(|o| o.stdout),
            Err(Error::new(
                "design tokens: crates/observation-client-model/src not found"
            ))
        );
        mkdir("crates/observation-client-model/src");
        let clean = ok(check(tree));
        assert!(!clean.failed());
        assert_eq!(clean.stderr, "");
        let no_roots = Config {
            roots: Vec::new(),
            theme: "t".into(),
            patterns: Vec::new(),
        };
        assert_eq!(
            sources(tree, &no_roots).map(|s| s.len()),
            Err(Error::new(
                "design tokens: .config/design-tokens.toml lists no roots, so nothing would be checked"
            ))
        );
        let missing_config = run(&tree.join("nowhere")).map(|o| o.stdout);
        assert!(
            missing_config
                .as_ref()
                .is_err_and(|e| e.to_string().contains("nowhere/.config/design-tokens.toml")),
            "{missing_config:?}"
        );
    }
}
