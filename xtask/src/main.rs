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
use xtask::ports::process::{BrotliCli, CargoCli, Curl, GhCli, GitCli, ScannersCli, SystemClock};

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
enum ReleaseCommand {
    /// Move CHANGELOG.md's [Unreleased] entries under a dated VERSION heading (with no entries,
    /// list the commits since the latest release that changed shipped files). A pre-release
    /// leaves the file alone.
    Changelog {
        #[arg(long, value_name = "X.Y.Z")]
        version: String,
    },
    /// Print the next release as version=, kind=, latest= and reason= lines (version empty
    /// when there is nothing to release), from CHANGELOG.md's [Unreleased] section and the
    /// shipped files changed since the latest stable tag.
    Plan {
        /// This version, if newer than the latest release and not tagged yet.
        #[arg(long, value_name = "V", default_value = "")]
        version: String,
        /// A pre-release: X.Y.Z-dev.N, N one more than the highest such tag (even when nothing
        /// changed). Empty: none.
        #[arg(long, value_name = "dev", default_value = "", value_parser = ["", "dev"])]
        prerelease: String,
    },
    /// Print origin's release state as key=value lines: latest (the newest stable v* tag),
    /// main (main's workspace version), pending (main's version is not tagged yet), hotfix (a
    /// hotfix/* branch exists), next (the patch release after latest).
    State,
    /// Read the release state and apply a workflow's check to it.
    Gate {
        kind: GateKind,
        /// Prep only: a pre-release, to which no check applies (the state is still read).
        #[arg(long)]
        prerelease: bool,
        /// Append the state lines, then go=true|false (prep, watch), to FILE ($GITHUB_OUTPUT).
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Append the verdict (prep, watch) to FILE ($GITHUB_STEP_SUMMARY).
        #[arg(long, value_name = "FILE")]
        summary: Option<PathBuf>,
    },
    /// Print each path read from stdin (one per line) that is not on KIND's list in
    /// .config/release.toml's [allowed-files]: whole paths, compared exactly.
    UnexpectedFiles {
        kind: FilesKind,
        /// The list's file (default: .config/release.toml in the repository). A relative path
        /// is taken from the repository.
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
    },
    /// The workspace version (`version` in Cargo.toml's [workspace.package]).
    #[command(subcommand)]
    Version(VersionCommand),
}

/// Which list of .config/release.toml `release unexpected-files` reads.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum FilesKind {
    /// What a hotfix may change in main.
    Hotfix,
    /// What a pre-release tag's commit may change over its parent.
    Prerelease,
}

/// The workflow whose check `release gate` applies.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum GateKind {
    /// Prepare release: skips (go=false) while a release or hotfix is open.
    Prep,
    /// Hotfix: fails unless main is the latest release and nothing else is open.
    Hotfix,
    /// Release watch: skips (go=false) without a release to watch or while one is open.
    Watch,
}

#[derive(Debug, Subcommand)]
enum SecurityPatchCommand {
    /// Audit the repository's Cargo.lock and run `cargo update -p` for each vulnerable crate
    /// (so the lock is left with every compatible fix: use a scratch checkout), scan the
    /// published images of VERSION with Trivy, write OUT/report.json and OUT/summary.md, and
    /// print the action: hotfix (a patch release would fix something) or none. Needs --repo.
    Check {
        #[arg(long, value_name = "X.Y.Z")]
        version: String,
        #[arg(long, value_name = "OUT")]
        out: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum VersionCommand {
    /// Print the workspace version; fail unless it is X.Y.Z or X.Y.Z-pre.
    Get {
        /// The manifest (default: Cargo.toml in the repository; - reads stdin). A relative
        /// path is taken from the repository.
        file: Option<String>,
        /// What an error calls it (default: FILE).
        name: Option<String>,
    },
    /// Set the workspace version, then refresh the workspace crates' entries in Cargo.lock and
    /// fuzz/Cargo.lock (`cargo update -w`).
    Set {
        /// X.Y.Z or X.Y.Z-pre, without the leading v.
        version: String,
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
    /// The release automation's steps.
    #[command(subcommand)]
    Release(ReleaseCommand),
    /// Whether the latest release needs a security patch release.
    #[command(subcommand)]
    SecurityPatch(SecurityPatchCommand),
    /// Fail when a pinned version differs where it is repeated, is repeated where it must not
    /// be, the advisory ignore lists disagree, a package named to cargo is not a crate, a
    /// testing feature is enabled outside [dev-dependencies], the image lists, cooldown days,
    /// mise lockfiles or MISE_ENV disagree with what they mirror, or a job of a pull_request
    /// workflow is neither covered by `mise run check` nor listed with the reason.
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

/// Whether the scripts' `${GITHUB_ACTIONS:+::error::}` applied: set and not empty.
fn github_actions() -> bool {
    std::env::var_os("GITHUB_ACTIONS").is_some_and(|v| !v.is_empty())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // The check edits the lock it audits, so it never defaults to this tree: like the script,
    // it needs --repo (a usage error, exit status 2).
    if matches!(cli.command, Command::SecurityPatch(_)) && cli.repo.is_none() {
        use clap::CommandFactory as _;
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "security-patch needs --repo <DIR> (a scratch checkout: the check edits its \
                 Cargo.lock)",
            )
            .exit();
    }
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
        Command::Release(ReleaseCommand::State) => {
            cmd::release::state::read(&GitCli::new(&repo), github_actions()).map(|state| {
                let Some(state) = state else { return true };
                emit(state.lines().as_bytes(), b"");
                false
            })
        }
        Command::Release(ReleaseCommand::Gate {
            kind,
            prerelease,
            output,
            summary,
        }) => {
            use cmd::release::state::{self, Kind};
            let kind = match kind {
                GateKind::Prep => Kind::Prep { prerelease },
                GateKind::Hotfix => Kind::Hotfix,
                GateKind::Watch => Kind::Watch,
            };
            let git = GitCli::new(&repo);
            state::read(&git, github_actions()).and_then(|state| {
                let Some(state) = state else { return Ok(true) };
                if let Some(path) = &output {
                    append(path, &state.lines())?;
                }
                let gated = state::render(kind, &state, &state::gate(kind, &state, &git)?);
                emit(gated.stdout.as_bytes(), b"");
                if let Some(path) = &summary {
                    append(path, &gated.summary)?;
                }
                if let Some(path) = &output {
                    append(path, &gated.output)?;
                }
                Ok(gated.failed)
            })
        }
        Command::Release(ReleaseCommand::Changelog { version }) => {
            cmd::release::changelog::run(&repo, &version, &GitCli::new(&repo), &SystemClock).map(
                |out| {
                    emit(out.as_bytes(), b"");
                    false
                },
            )
        }
        Command::Release(ReleaseCommand::Plan {
            version,
            prerelease,
        }) => cmd::release::plan::plan(&repo, &version, &prerelease, &GitCli::new(&repo)).map(
            |plan| {
                emit(plan.lines().as_bytes(), b"");
                false
            },
        ),
        Command::Release(ReleaseCommand::UnexpectedFiles { kind, config }) => {
            let kind = match kind {
                FilesKind::Hotfix => cmd::release::files::Kind::Hotfix,
                FilesKind::Prerelease => cmd::release::files::Kind::Prerelease,
            };
            cmd::release::files::run(&repo, kind, config.as_deref(), &mut std::io::stdin().lock())
                .map(|out| {
                    emit(out.as_bytes(), b"");
                    false
                })
        }
        Command::Release(ReleaseCommand::Version(VersionCommand::Get { file, name })) => {
            cmd::release::version::get(
                &repo,
                file.as_deref(),
                name.as_deref(),
                &mut std::io::stdin().lock(),
                github_actions(),
            )
            .map(|line| {
                emit(line.as_bytes(), b"");
                false
            })
        }
        Command::Release(ReleaseCommand::Version(VersionCommand::Set { version })) => {
            let mut diagnostics = String::new();
            let result = cmd::release::version::set(
                &repo,
                &version,
                &CargoCli::default(),
                github_actions(),
                &mut diagnostics,
            );
            emit(b"", diagnostics.as_bytes());
            result
        }
        Command::SecurityPatch(SecurityPatchCommand::Check { version, out }) => {
            let mut diagnostics = String::new();
            let result = cmd::security_patch::check(
                &repo,
                &version,
                &out,
                &ScannersCli::default(),
                &mut diagnostics,
            );
            emit(b"", diagnostics.as_bytes());
            result.map(|action| {
                emit(action.as_bytes(), b"");
                false
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

    /// The script's argparse choices: `--prerelease` is "" or "dev"; changelog needs `--version`.
    #[test]
    fn release_plan_flags_as_the_script_took_them() {
        let parse = |args: &[&str]| Cli::try_parse_from([&["xtask", "release"], args].concat());
        assert!(parse(&["plan"]).is_ok());
        assert!(parse(&["plan", "--prerelease", ""]).is_ok());
        assert!(parse(&["plan", "--prerelease", "dev", "--version", "1.0.0"]).is_ok());
        assert!(parse(&["plan", "--prerelease", "rc"]).is_err());
        assert!(parse(&["changelog"]).is_err());
        assert!(parse(&["changelog", "--version", "1.0.0"]).is_ok());
    }
}
