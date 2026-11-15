//! Runs the Observation MCP server over stdio, or over Streamable HTTP when
//! `--http <address>` or `OBS_MCP_LISTEN` gives an address to listen on
//! (see [`observation_mcp::http`] for its settings).
//!
//! `--profile` or `OBS_MCP_PROFILE` and `--toolsets` or `OBS_MCP_TOOLSETS`
//! choose the tools offered (see [`observation_mcp::toolsets`]).
//!
//! Logs go to stderr (`RUST_LOG`, default `info`); over stdio, stdout
//! carries the protocol.

use observation_mcp::ObservationMcp;
use observation_mcp::http::{self, config::HttpConfig};
use observation_mcp::toolsets::ToolSelection;
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: observation-mcp [--http <address>] [--profile <profile>] [--toolsets <groups>]

Serves MCP over stdio, or over Streamable HTTP at http://<address>/mcp when
--http or OBS_MCP_LISTEN gives an address (e.g. 127.0.0.1:8090). HTTP needs
OBS_MCP_TOKENS or OBS_MCP_TOKENS_FILE (name:secret pairs); see docs/design.md.

--profile (OBS_MCP_PROFILE) is full (the default), small or small-app: the
small profiles offer fewer tools, with plain schemas and brief results, for
small local models. --toolsets (OBS_MCP_TOOLSETS) offers only the groups it
names, separated by commas: guide, scenes, cameras, models, places, analysis,
describe, map, goals, app, proposals, view, or all.";

#[tokio::main]
async fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let (listen, profile, toolsets) = match parse_args(std::env::args().skip(1)) {
        Ok(Args::Run {
            listen,
            profile,
            toolsets,
        }) => (listen, profile, toolsets),
        Ok(Args::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let tools = match ToolSelection::from_env(profile, toolsets) {
        Ok(tools) => tools,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(2);
        }
    };
    tracing::info!(tools = %tools.describe(), "tools offered");
    let result = match HttpConfig::from_env(listen) {
        Ok(Some(config)) => http::serve(config, tools).await.map_err(Into::into),
        Ok(None) => serve_stdio(tools).await,
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

async fn serve_stdio(tools: ToolSelection) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "observation-mcp serving on stdio"
    );
    let running = ObservationMcp::new()
        .with_tools(tools)
        .serve(stdio())
        .await?;
    running.waiting().await?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Args {
    Run {
        listen: Option<String>,
        profile: Option<String>,
        toolsets: Option<String>,
    },
    Help,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut listen, mut profile, mut toolsets) = (None, None, None);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg.clone(), None),
        };
        let slot = match flag.as_str() {
            "-h" | "--help" if inline.is_none() => return Ok(Args::Help),
            "--http" => &mut listen,
            "--profile" => &mut profile,
            "--toolsets" => &mut toolsets,
            _ => return Err(format!("unknown argument `{arg}`")),
        };
        let value = match inline {
            Some(v) => v,
            None => args.next().ok_or_else(|| format!("{flag} needs a value"))?,
        };
        *slot = Some(value);
    }
    Ok(Args::Run {
        listen,
        profile,
        toolsets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|&a| a.to_owned()))
    }

    fn run(listen: Option<&str>, profile: Option<&str>, toolsets: Option<&str>) -> Args {
        Args::Run {
            listen: listen.map(Into::into),
            profile: profile.map(Into::into),
            toolsets: toolsets.map(Into::into),
        }
    }

    #[test]
    fn arguments() {
        assert_eq!(parse(&[]), Ok(run(None, None, None)));
        let at = Ok(run(Some("127.0.0.1:8090"), None, None));
        assert_eq!(parse(&["--http", "127.0.0.1:8090"]), at);
        assert_eq!(parse(&["--http=127.0.0.1:8090"]), at);
        assert_eq!(parse(&["--help"]), Ok(Args::Help));
        assert!(parse(&["--http"]).is_err());
        assert!(parse(&["--stdio"]).is_err());
        assert_eq!(
            parse(&["--profile", "small", "--toolsets=scenes,cameras"]),
            Ok(run(None, Some("small"), Some("scenes,cameras")))
        );
        assert!(parse(&["--profile"]).is_err());
    }
}
