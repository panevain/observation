//! Runs the Observation MCP server over stdio.
//!
//! stdout carries the protocol, so logs go to stderr (`RUST_LOG`, default
//! `info`).

use observation_mcp::ObservationMcp;
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "observation-mcp serving on stdio"
    );
    let running = ObservationMcp.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
