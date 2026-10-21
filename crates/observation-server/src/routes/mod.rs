//! HTTP routes: JSON/binary API plus the static client with SPA fallback.

mod area;
mod health;
mod regions;
mod static_files;
mod tiles;

use axum::Router;
use axum::routing::{get, post};

use crate::AppState;
use crate::error::ApiError;

/// All routes, without the cross-cutting middleware added by [`crate::app`].
pub fn router(state: AppState) -> Router {
    let static_service = static_files::service(&state.config().static_dir);
    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/api/config", get(health::config))
        .route("/api/area", get(area::area))
        .route("/api/demo", get(area::demo))
        .route("/api/tiles/{z}/{x}/{y}", get(tiles::tile))
        .route("/api/regions", get(regions::status))
        .route("/api/regions/catalog", get(regions::catalog))
        .route("/api/regions/{action}", post(regions::act))
        // Unknown API paths must not fall through to the SPA's index.html.
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        .fallback_service(static_service)
        .with_state(state)
}

/// Long-lived routes, kept out of the request timeout by [`crate::app`].
pub fn streams(state: AppState) -> Router {
    Router::new()
        .route("/api/regions/events", get(regions::events))
        .with_state(state)
}

async fn api_not_found() -> ApiError {
    ApiError::NotFound("no such API address on this server")
}
