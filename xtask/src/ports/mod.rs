//! The outside world as traits, so that commands are tested against fakes (`fakes.rs`) and run
//! against the real programs ([`process`]). Each port has only what a command needs; a command
//! that needs more adds it with the command.

pub mod process;

use std::path::Path;

use crate::Result;
use crate::core::time::Timestamp;

/// The repository, through `git`.
pub trait Git {
    /// Whether `rev` names a commit.
    fn is_commit(&self, rev: &str) -> Result<bool>;
    /// The text of `path` at `rev`, or `None` if git cannot show it (no such file or revision).
    fn show(&self, rev: &str, path: &str) -> Result<Option<String>>;
    /// `git grep` with these arguments: its output, or `None` when nothing matched.
    fn grep(&self, args: &[&str]) -> Result<Option<String>>;
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

/// cargo's companion tools.
pub trait Cargo {
    /// `cargo-audit audit --json` on a lockfile, run beside it (so no repository's
    /// `.cargo/audit.toml` applies): its JSON report, or `None` when cargo-audit is not installed.
    fn audit_json(&self, lockfile: &Path) -> Result<Option<String>>;
}

/// The brotli command-line compressor.
pub trait Brotli {
    /// Whether the `brotli` program is installed.
    fn available(&self) -> bool;
    /// The size of `data` compressed with `brotli -q 11`.
    fn compressed_len(&self, data: &[u8]) -> Result<usize>;
}

/// The current time.
pub trait Clock {
    fn now(&self) -> Timestamp;
}
