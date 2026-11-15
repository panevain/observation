//! Runs the Observation MCP server over stdio, or over Streamable HTTP when
//! `--http <address>` or `OBS_MCP_LISTEN` gives an address to listen on
//! (see [`observation_mcp::http`] for its settings).
//!
//! Logs go to stderr (`RUST_LOG`, default `info`); over stdio, stdout
//! carries the protocol.

use observation_mcp::ObservationMcp;
use observation_mcp::http::{self, config::HttpConfig};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: observation-mcp [--http <address>]

Serves MCP over stdio, or over Streamable HTTP at http://<address>/mcp when
--http or OBS_MCP_LISTEN gives an address (e.g. 127.0.0.1:8090). HTTP needs
OBS_MCP_TOKENS or OBS_MCP_TOKENS_FILE (name:secret pairs); see docs/design.md.";

#[tokio::main]
async fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let listen = match parse_args(std::env::args().skip(1)) {
        Ok(Args::Run { listen }) => listen,
        Ok(Args::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match HttpConfig::from_env(listen) {
        Ok(Some(config)) => http::serve(config).await.map_err(Into::into),
        Ok(None) => serve_stdio().await,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

async fn serve_stdio() -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "observation-mcp serving on stdio"
    );
    let running = ObservationMcp::new().serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Args {
    Run { listen: Option<String> },
    Help,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut listen = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Args::Help),
            "--http" => {
                let addr = args.next().ok_or("--http needs an address")?;
                listen = Some(addr);
            }
            other => match other.strip_prefix("--http=") {
                Some(addr) => listen = Some(addr.to_owned()),
                None => return Err(format!("unknown argument `{other}`")),
            },
        }
    }
    Ok(Args::Run { listen })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|&a| a.to_owned()))
    }

    #[test]
    fn arguments() {
        assert_eq!(parse(&[]), Ok(Args::Run { listen: None }));
        let at = Ok(Args::Run {
            listen: Some("127.0.0.1:8090".into()),
        });
        assert_eq!(parse(&["--http", "127.0.0.1:8090"]), at);
        assert_eq!(parse(&["--http=127.0.0.1:8090"]), at);
        assert_eq!(parse(&["--help"]), Ok(Args::Help));
        assert!(parse(&["--http"]).is_err());
        assert!(parse(&["--stdio"]).is_err());
    }
}
