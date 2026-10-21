//! Observation server binary: configuration, tracing, serve with graceful shutdown.

use observation_server::config::{Config, LogFormat};
use observation_server::{AppState, app};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match config.log_format {
        LogFormat::Text => tracing_subscriber::fmt().with_env_filter(filter).init(),
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init(),
    }
    if let Some(addr) = config.metrics_bind {
        let handle = observation_server::metrics::install()?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("could not bind OBS_METRICS_BIND {addr}: {e}"))?;
        tokio::spawn(async move {
            if let Err(e) = observation_server::metrics::serve(listener, handle).await {
                tracing::error!(error = %e, "metrics server stopped");
            }
        });
    }
    if !config.static_dir.join("index.html").is_file() {
        tracing::warn!(dir = %config.static_dir.display(), "no index.html in static dir; the client will not load");
    }
    let bind = config.bind;
    let state = AppState::new(config)?;
    if state.regions().is_some() {
        tracing::info!(
            manageable = state.config().admin_token.is_some(),
            "local regions enabled"
        );
    }

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("could not bind OBS_BIND {bind}: {e}"))?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    let streams = state.clone();
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            streams.begin_shutdown();
        })
        .await?;
    tracing::info!("shut down");
    Ok(())
}

/// Resolves on SIGINT (Ctrl-C) or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "could not listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "could not listen for SIGTERM");
                std::future::pending::<()>().await;
            }
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
