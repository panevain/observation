//! The agent's routes.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use observation_core::regions::{ActionKind, bearer_matches};
use observation_regions_api::paths;
use serde_json::json;

use crate::agent::Agent;

mod catalog;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod format_tests;
mod interpreter;
mod regions;

use self::catalog::{catalog_list, catalog_sizes};
use self::interpreter::interpreter;
use self::regions::{cancel_region, delete_region, import_region, region_settings, regions};

/// An error answer: status and a sentence for the UI.
#[derive(Debug)]
pub struct AgentError(StatusCode, String);

impl IntoResponse for AgentError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn err(status: StatusCode, msg: impl Into<String>) -> AgentError {
    AgentError(status, msg.into())
}

/// The agent's routes.
pub fn app(agent: Arc<Agent>) -> Router {
    let api = Router::new()
        .route(paths::REGIONS, get(regions))
        .route(paths::CATALOG, get(catalog_list))
        .route(paths::CATALOG_SIZES, post(catalog_sizes))
        .route(paths::action(ActionKind::Import), post(import_region))
        .route(paths::action(ActionKind::Cancel), post(cancel_region))
        .route(paths::action(ActionKind::Delete), post(delete_region))
        .route(paths::action(ActionKind::Settings), post(region_settings))
        .route(paths::INTERPRETER, post(interpreter))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&agent),
            require_token,
        ));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(api)
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(agent)
}

async fn require_token(
    State(agent): State<Arc<Agent>>,
    req: Request,
    next: Next,
) -> Result<Response, AgentError> {
    if let Some(token) = &agent.token {
        let given = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        if !bearer_matches(given, token) {
            return Err(err(
                StatusCode::UNAUTHORIZED,
                "missing or wrong agent token",
            ));
        }
    }
    Ok(next.run(req).await)
}
