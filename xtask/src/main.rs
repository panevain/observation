//! Observation's repository tooling, run as `cargo xtask <command>` (the alias is in
//! `.cargo/config.toml`).
//!
//! A workspace of its own (like `fuzz/`), so its dependencies stay out of the root
//! `Cargo.lock`. The commands that replace the scripts in `scripts/` are added here one at a time.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "xtask",
    bin_name = "cargo xtask",
    about = "Observation's repository tooling"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print xtask's version.
    #[command(hide = true)]
    Version,
}

fn main() {
    match Cli::parse().command {
        Command::Version => println!("xtask {}", env!("CARGO_PKG_VERSION")),
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
