//! Region agent binary: configuration, tracing, worker tasks and the HTTP API.

use observation_regions::config::Config;
use observation_regions::{Agent, app};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if config.json_logs {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    if config.token.is_none() {
        tracing::warn!(
            "OBS_REGIONS_TOKEN is not set: anyone who can reach this port can import regions"
        );
    }
    std::fs::create_dir_all(&config.data_dir)
        .map_err(|e| format!("could not create {}: {e}", config.data_dir.display()))?;
    let agent = Agent::new(&config)?;
    tokio::spawn(Agent::run_worker(agent.clone()));
    tokio::spawn(Agent::run_refresher(agent.clone()));

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .map_err(|e| format!("could not bind OBS_REGIONS_BIND {}: {e}", config.bind))?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    axum::serve(listener, app(agent))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("shut down");
    Ok(())
}

/// Resolves on SIGINT or SIGTERM. Long polls are cut short by the shutdown;
/// a running import is killed and resumes after the restart.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
