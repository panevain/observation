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
use xtask::ports::Brotli;
use xtask::ports::process::{BrotliCli, GitCli};

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
    /// Fail when a pinned version differs where it is repeated, is repeated where it must not
    /// be, or the advisory ignore lists disagree.
    Sync,
    /// Fail on colour literals outside the client's theme (.config/design-tokens.toml).
    Tokens,
    /// Fail when a WebAssembly module in dist/ is over its size budget
    /// (.config/wasm-size-budget.toml).
    WasmBudget {
        /// The directory holding the modules (default: dist/ in the repository).
        #[arg(long, value_name = "DIR")]
        dist: Option<PathBuf>,
        /// Append the Markdown report to FILE (e.g. $GITHUB_STEP_SUMMARY).
        #[arg(long, value_name = "FILE")]
        summary: Option<PathBuf>,
        /// Also report brotli -q 11 sizes (needs the brotli CLI).
        #[arg(long)]
        brotli: bool,
    },
    /// Print xtask's version.
    #[command(hide = true)]
    Version,
}

/// Writes a command's output. A closed stdout or stderr (`| head`) leaves nothing to report
/// to, so a failed write is dropped rather than a panic, as `print!` would.
fn emit(stdout: &[u8], stderr: &[u8]) {
    let _ = std::io::stdout()
        .write_all(stdout)
        .and_then(|()| std::io::stdout().flush());
    let _ = std::io::stderr().write_all(stderr);
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let repo = cli
        .repo
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/..")));
    let result = match cli.command {
        Command::Sync => cmd::sync::run(&repo, &GitCli::new(&repo)).map(|out| {
            emit(out.stdout.as_bytes(), out.stderr.as_bytes());
            out.failed
        }),
        Command::Tokens => cmd::tokens::run(&repo).map(|out| {
            emit(&out.stdout, out.stderr.as_bytes());
            out.failed()
        }),
        Command::WasmBudget {
            dist,
            summary,
            brotli,
        } => {
            // The default as the script printed it: an absolute path.
            let dist = dist.unwrap_or_else(|| {
                std::fs::canonicalize(&repo)
                    .unwrap_or_else(|_| repo.clone())
                    .join("dist")
            });
            let brotli_cli = BrotliCli::default();
            let brotli = brotli.then_some(&brotli_cli as &dyn Brotli);
            cmd::wasm_budget::run(&repo, &dist, brotli).and_then(|out| {
                // The verdict first, on stderr, as the script printed it.
                emit(b"", out.stderr.as_bytes());
                emit(out.stdout.as_bytes(), b"");
                if let Some(path) = summary {
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .map_err(|e| xtask::Error::new(format!("{}: {e}", path.display())))?;
                    file.write_all(out.report.as_bytes())
                        .map_err(|e| xtask::Error::new(format!("{}: {e}", path.display())))?;
                }
                Ok(out.failed)
            })
        }
        Command::Version => {
            emit(
                format!("xtask {}\n", env!("CARGO_PKG_VERSION")).as_bytes(),
                b"",
            );
            Ok(false)
        }
    };
    match result {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::FAILURE,
        Err(e) => {
            emit(b"", format!("{e}\n").as_bytes());
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
