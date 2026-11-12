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
use xtask::ports::process::{BrotliCli, CargoCli, Curl, GhCli, GitCli, SystemClock};

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
enum AutomergeCommand {
    /// Print {"merge": [...], "hold": [...]}: the open Dependabot pull requests that may merge
    /// themselves, and those (or the Pins pull request) labelled hold with auto-merge on.
    List {
        /// The GitHub repository (OWNER/NAME).
        #[arg(long = "github-repo", value_name = "OWNER/NAME")]
        github_repo: String,
        /// Append a Markdown table of the verdicts to FILE (e.g. $GITHUB_STEP_SUMMARY).
        #[arg(long, value_name = "FILE")]
        summary: Option<PathBuf>,
        /// Word the table as what would happen.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CoverageCommand {
    /// Fail when a crate's line coverage is below its floor in .config/coverage.toml.
    Check {
        /// The export of `cargo llvm-cov --json --summary-only`.
        #[arg(long, value_name = "FILE")]
        json: PathBuf,
        /// Append the Markdown report to FILE (e.g. $GITHUB_STEP_SUMMARY).
        #[arg(long, value_name = "FILE")]
        summary: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Dependabot pull requests whose auto-merge to enable or disable.
    #[command(subcommand)]
    Automerge(AutomergeCommand),
    /// Fail when a crate version added to a lockfile since BASE is younger than the cooldown
    /// (.config/cargo-cooldown.toml), unless exempt or a RustSec fix.
    Cooldown {
        /// The revision to compare the lockfiles against (e.g. HEAD^1 or origin/develop).
        #[arg(long, value_name = "REV")]
        base: String,
        /// A lockfile to check (repeatable; default: Cargo.lock, fuzz/Cargo.lock,
        /// xtask/Cargo.lock).
        #[arg(long = "lockfile", value_name = "PATH")]
        lockfiles: Vec<String>,
        /// Append the Markdown report to FILE (e.g. $GITHUB_STEP_SUMMARY).
        #[arg(long, value_name = "FILE")]
        summary: Option<PathBuf>,
        /// The current time, ISO 8601 (for testing).
        #[arg(long, value_name = "TIME")]
        now: Option<String>,
    },
    /// Line coverage per crate.
    #[command(subcommand)]
    Coverage(CoverageCommand),
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

/// Appends `text` to the file at `path` (a job summary), creating it if needed.
fn append(path: &std::path::Path, text: &str) -> xtask::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| xtask::Error::new(format!("{}: {e}", path.display())))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let repo = cli
        .repo
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/..")));
    let result = match cli.command {
        Command::Automerge(AutomergeCommand::List {
            github_repo,
            summary,
            dry_run,
        }) => {
            let args = cmd::automerge::Args {
                repo: github_repo,
                dry_run,
            };
            let mut diagnostics = String::new();
            let result = cmd::automerge::run(&args, &GhCli::default(), &mut diagnostics);
            emit(b"", diagnostics.as_bytes());
            result.and_then(|out| {
                // As the script: an empty --summary writes nothing.
                if let Some(path) = summary.filter(|p| !p.as_os_str().is_empty()) {
                    append(&path, &out.summary)?;
                }
                emit(out.stdout.as_bytes(), b"");
                Ok(false)
            })
        }
        Command::Cooldown {
            base,
            lockfiles,
            summary,
            now,
        } => {
            let lockfiles = if lockfiles.is_empty() {
                cmd::cooldown::LOCKFILES.map(String::from).to_vec()
            } else {
                lockfiles
            };
            let git = GitCli::new(&repo);
            let http = Curl {
                user_agent: cmd::cooldown::USER_AGENT.to_owned(),
            };
            let ports = cmd::cooldown::Ports {
                git: &git,
                http: &http,
                cargo: &CargoCli::default(),
                clock: &SystemClock,
            };
            let args = cmd::cooldown::Args {
                base,
                lockfiles,
                now,
            };
            let mut diagnostics = String::new();
            let result = cmd::cooldown::run(&repo, &args, &ports, &mut diagnostics);
            emit(b"", diagnostics.as_bytes());
            result.and_then(|out| {
                emit(format!("{}\n", out.report).as_bytes(), b"");
                if let Some(path) = summary {
                    append(&path, &out.report)?;
                }
                Ok(out.failed)
            })
        }
        Command::Coverage(CoverageCommand::Check { json, summary }) => {
            let annotate = std::env::var_os("GITHUB_ACTIONS").is_some_and(|v| v == "true");
            cmd::coverage::run(&repo, &json, annotate).and_then(|out| {
                emit(out.stdout.as_bytes(), out.stderr.as_bytes());
                if let Some(path) = summary {
                    append(&path, &out.stdout)?;
                }
                Ok(out.failed)
            })
        }
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
                    append(&path, &out.report)?;
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
