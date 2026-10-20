//! HTTP routes: JSON/binary API plus the static client with SPA fallback.

mod area;
mod health;
mod static_files;
mod tiles;

use axum::Router;
use axum::routing::get;

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
        // Unknown API paths must not fall through to the SPA's index.html.
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        .fallback_service(static_service)
        .with_state(state)
}

async fn api_not_found() -> ApiError {
    ApiError::NotFound("no such API address on this server")
}
