//! `/healthz` and `/api/config`.

use axum::Json;
use axum::extract::State;
use axum::http::request::Parts;
use observation_core::geo::{MAX_BBOX_SIDE_M, MIN_BBOX_SIDE_M};
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
    /// Side limits of an area request, metres (`/api/area` enforces them).
    pub min_bbox_side_m: f64,
    pub max_bbox_side_m: f64,
    pub demo_available: bool,
    /// A region agent is configured: the client shows the Regions dialog.
    pub regions: bool,
    /// The agent bridge is on and this request came from this computer, so
    /// the client offers Connect agent.
    pub agent_bridge: bool,
    pub version: &'static str,
}

/// Public client configuration.
pub async fn config(State(state): State<AppState>, parts: Parts) -> Json<ClientConfig> {
    let agent_bridge = state.agent().is_some()
        && super::agent::from_this_computer(&parts.extensions, &parts.headers).is_ok();
    Json(ClientConfig {
        tile_url: "/api/tiles/{z}/{x}/{y}.png",
        attribution: state.config().attribution.clone(),
        min_bbox_side_m: MIN_BBOX_SIDE_M,
        max_bbox_side_m: MAX_BBOX_SIDE_M,
        demo_available: state.demo().is_some(),
        regions: state.regions().is_some(),
        agent_bridge,
        version: env!("CARGO_PKG_VERSION"),
    })
}
