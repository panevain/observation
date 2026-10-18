//! `/healthz` and `/api/config`.

use axum::Json;
use axum::extract::State;
use observation_core::geo::MAX_BBOX_SIDE_M;
use serde::Serialize;

use crate::AppState;

/// Liveness probe.
pub async fn healthz() -> &'static str {
    "ok"
}

/// Body of `GET /api/config`.
#[derive(Debug, Serialize)]
pub struct ClientConfig {
    /// Tile URL template the client should use (our proxy, not upstream).
    pub tile_url: &'static str,
    pub attribution: String,
    pub max_bbox_side_m: f64,
    pub demo_available: bool,
    pub version: &'static str,
}

/// Public client configuration.
pub async fn config(State(state): State<AppState>) -> Json<ClientConfig> {
    Json(ClientConfig {
        tile_url: "/api/tiles/{z}/{x}/{y}.png",
        attribution: state.config().attribution.clone(),
        max_bbox_side_m: MAX_BBOX_SIDE_M,
        demo_available: state.demo().is_some(),
        version: env!("CARGO_PKG_VERSION"),
    })
}
