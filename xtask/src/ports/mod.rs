//! The outside world as traits, so that commands are tested against fakes (`fakes.rs`) and run
//! against the real programs ([`process`]). Each port has only what a command needs; a command
//! that needs more adds it with the command.

pub mod process;

use std::path::Path;

use crate::Result;
use crate::core::time::Timestamp;

/// What a `git` command printed on stdout, and its exit status (negative: the signal that
/// ended it). Its stderr went straight to this process's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRun {
    pub code: i32,
    pub stdout: Vec<u8>,
}

/// The repository, through `git`.
pub trait Git {
    /// Whether `rev` names a commit.
    fn is_commit(&self, rev: &str) -> Result<bool>;
    /// The text of `path` at `rev`, or `None` if git cannot show it (no such file or revision).
    fn show(&self, rev: &str, path: &str) -> Result<Option<String>>;
    /// `git grep FLAGS -e PATTERN -- PATHSPECS`: its output, or `None` when nothing matched.
    /// `flags` are options; the pattern and pathspecs are never read as one.
    fn grep(&self, flags: &[&str], pattern: &str, pathspecs: &[&str]) -> Result<Option<String>>;
    /// `git ARGS` as a script ran it: stderr passed through, any exit status returned. The
    /// caller makes sure no value in `args` can be read as an option.
    fn run(&self, args: &[&str]) -> Result<GitRun>;
}

/// An HTTP response; any status, so that callers decide what a 404 means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// HTTPS GET requests.
pub trait Http {
    fn get(&self, url: &str) -> Result<Response>;
}

/// What cargo-audit printed, whatever its exit status (non-zero when it finds something).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditOutput {
    /// The JSON report, or nothing when cargo-audit could not run.
    pub stdout: String,
    pub stderr: String,
}

/// cargo's companion tools.
pub trait Cargo {
    /// `cargo-audit audit --json` on a lockfile, run beside it (so no repository's
    /// `.cargo/audit.toml` applies), or `None` when cargo-audit is not installed.
    fn audit_json(&self, lockfile: &Path) -> Result<Option<AuditOutput>>;
    /// `cargo update -w`, with `--manifest-path MANIFEST` when given, run in `dir` with its
    /// output passed through to this process's. Whether it succeeded.
    fn update_workspace(&self, dir: &Path, manifest: Option<&str>) -> Result<bool>;
}

/// The brotli command-line compressor.
pub trait Brotli {
    /// Whether the `brotli` program is installed.
    fn available(&self) -> bool;
    /// The size of `data` compressed with `brotli -q 11`.
    fn compressed_len(&self, data: &[u8]) -> Result<usize>;
}

/// What a `gh` command printed, and its exit status (negative: the signal that ended it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// GitHub through the `gh` CLI, which reads its token from the environment (`GH_TOKEN`).
pub trait GitHub {
    /// Runs `gh` with `args` and returns its output whatever its exit status.
    fn gh(&self, args: &[String]) -> Result<GhOutput>;
}

/// What a program printed, read as `subprocess.run(text=True)` read it (strict UTF-8,
/// universal newlines), and its exit status (negative: the signal that ended it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The programs `security-patch` runs, whatever their exit status. The caller makes sure no
/// value in `args` can be read as an option.
pub trait Scanners {
    /// `cargo ARGS`, run in `dir`.
    fn cargo(&self, dir: &Path, args: &[&str]) -> Result<ProgramOutput>;
    /// `trivy ARGS`.
    fn trivy(&self, args: &[&str]) -> Result<ProgramOutput>;
}

/// The current time.
pub trait Clock {
    fn now(&self) -> Timestamp;
}
