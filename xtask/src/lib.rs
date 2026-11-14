//! The library behind `cargo xtask`: everything but the command line.
//!
//! - [`ports`]: the outside world (git, HTTP, cargo-audit, brotli, the clock) as traits, with
//!   adapters that run the real programs. `main.rs` is the only place that builds the adapters;
//!   tests use the in-memory fakes instead.
//! - [`cmd`]: the commands, the only layer that reads and writes files.
//! - [`core`]: pure helpers over parsed inputs (versions, times, Markdown tables, GitHub Actions
//!   annotations). Nothing in it reads or writes files or runs a program.

pub mod cmd;
pub mod core;
#[cfg(test)]
pub mod fakes;
pub mod ports;

use std::fmt;

/// What went wrong, as a message for the person reading the job log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;
