//! Observation's repository tooling, run as `cargo xtask <command>` (the alias is in
//! `.cargo/config.toml`).
//!
//! A workspace of its own (like `fuzz/`), so its dependencies stay out of the root
//! `Cargo.lock`. The commands that replace the scripts in `scripts/` are added here one at a time.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use xtask::cmd;

#[derive(Debug, Parser)]
#[command(
    name = "xtask",
    bin_name = "cargo xtask",
    about = "Observation's repository tooling"
)]
struct Cli {
    /// The repository to work on (default: the one xtask was built from).
    #[arg(long, global = true, value_name = "DIR")]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Fail on colour literals outside the client's theme (.config/design-tokens.toml).
    Tokens,
    /// Print xtask's version.
    #[command(hide = true)]
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let repo = cli
        .repo
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/..")));
    let result = match cli.command {
        Command::Tokens => cmd::tokens::run(&repo).map(|out| {
            // A closed stdout or stderr leaves nothing to report to.
            let _ = std::io::stdout().write_all(&out.stdout);
            eprint!("{}", out.stderr);
            out.failed()
        }),
        Command::Version => {
            println!("xtask {}", env!("CARGO_PKG_VERSION"));
            Ok(false)
        }
    };
    match result {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
