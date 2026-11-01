//! `/api/area` and `/api/demo`.

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use observation_core::bundle::BUNDLE_CONTENT_TYPE;
use observation_core::geo::BBox;
use serde::Deserialize;

use crate::AppState;
use crate::error::ApiError;
use crate::ratelimit::ClientKey;

/// Grid used to snap requested boxes for caching (≈ 10 m).
const SNAP_DEG: f64 = 0.0001;

#[derive(Debug, Deserialize)]
pub struct AreaParams {
    bbox: Option<String>,
}

fn bundle_response(bytes: Bytes, cache_control: &'static str) -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(BUNDLE_CONTENT_TYPE),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// `GET /api/area?bbox=south,west,north,east`
pub async fn area(
    State(state): State<AppState>,
    client: ClientKey,
    Query(params): Query<AreaParams>,
) -> Result<Response, ApiError> {
    let raw = params.bbox.ok_or_else(|| {
        ApiError::BadRequest("missing `bbox` query parameter (south,west,north,east)".into())
    })?;
    let bbox = BBox::parse(&raw).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    bbox.validate_size()
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let bytes = state.bundles().get(bbox.snapped(SNAP_DEG), client).await?;
    Ok(bundle_response(bytes, "public, max-age=86400"))
}

/// `GET /api/demo`: the fixture bundle built at startup.
pub async fn demo(State(state): State<AppState>) -> Result<Response, ApiError> {
    let bytes = state.demo().cloned().ok_or(ApiError::NotFound(
        "the demo street is not available on this server",
    ))?;
    Ok(bundle_response(bytes, "public, max-age=3600"))
}
