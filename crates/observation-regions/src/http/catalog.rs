//! `/catalog` and `/catalog/sizes`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use observation_core::regions::SizesRequest;

use super::{AgentError, err};
use crate::agent::Agent;

pub(super) async fn catalog_list(State(agent): State<Arc<Agent>>) -> Result<Response, AgentError> {
    let list = agent
        .catalog
        .extracts()
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let entries: Vec<_> = list.iter().map(|x| &x.entry).collect();
    Ok(Json(entries).into_response())
}

pub(super) async fn catalog_sizes(
    State(agent): State<Arc<Agent>>,
    Json(req): Json<SizesRequest>,
) -> Result<Response, AgentError> {
    if !req.is_valid() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "expected {\"ids\": [...]} with at most 40 region ids",
        ));
    }
    let sizes = agent
        .catalog
        .sizes(&req.ids)
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(sizes).into_response())
}
