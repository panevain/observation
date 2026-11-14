//! `/api/geocode?q=...`: the optional place search.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::geocoder::Place;
use crate::ratelimit::ClientKey;

#[derive(Debug, Deserialize)]
pub struct GeocodeParams {
    q: Option<String>,
}

/// Body of a successful search.
#[derive(Debug, Serialize)]
struct GeocodeAnswer<'a> {
    results: &'a [Place],
    /// Credit the client shows with the results.
    attribution: &'a str,
}

/// `GET /api/geocode?q=<place>`: at most five places, or 404 when the
/// search is off.
pub async fn geocode(
    State(state): State<AppState>,
    client: ClientKey,
    Query(params): Query<GeocodeParams>,
) -> Result<Response, ApiError> {
    let geocoder = state.geocoder().ok_or(ApiError::NotFound(
        "place search is not turned on on this server",
    ))?;
    let q = params.q.ok_or_else(|| {
        ApiError::BadRequest("missing `q` query parameter (the place to search for)".into())
    })?;
    let places = geocoder.search(&q, client).await?;
    let body = GeocodeAnswer {
        results: &places,
        attribution: geocoder.attribution(),
    };
    // Searches stay out of the browser's cache; the server caches answers.
    Ok((
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(body),
    )
        .into_response())
}
