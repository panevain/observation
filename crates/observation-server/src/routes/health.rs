//! `/healthz` and `/api/config`.

use axum::Json;
use axum::extract::State;
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
    /// The place search (`/api/geocode`), or `null` when it is off: the
    /// client shows its search box only when this is set.
    pub geocoder: Option<GeocoderInfo>,
    pub version: &'static str,
}

/// What the client needs to know about the place search.
#[derive(Debug, Serialize)]
pub struct GeocoderInfo {
    /// Credit to show with results (also sent with each answer).
    pub attribution: String,
    /// Longest query accepted, in characters.
    pub max_query_chars: usize,
}

/// Public client configuration.
pub async fn config(State(state): State<AppState>) -> Json<ClientConfig> {
    Json(ClientConfig {
        tile_url: "/api/tiles/{z}/{x}/{y}.png",
        attribution: state.config().attribution.clone(),
        min_bbox_side_m: MIN_BBOX_SIDE_M,
        max_bbox_side_m: MAX_BBOX_SIDE_M,
        demo_available: state.demo().is_some(),
        regions: state.regions().is_some(),
        geocoder: state.geocoder().map(|g| GeocoderInfo {
            attribution: g.attribution().to_owned(),
            max_query_chars: crate::geocoder::MAX_QUERY_CHARS,
        }),
        version: env!("CARGO_PKG_VERSION"),
    })
}
