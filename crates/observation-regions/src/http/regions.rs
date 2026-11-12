//! `/regions` and the region actions.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use observation_core::regions::{RegionAction, RegionsSnapshot, is_valid_region_id};
use serde::Deserialize;

use super::{AgentError, err};
use crate::agent::Agent;
use crate::registry::ActionError;

/// Longest a `GET /regions?since=` waits for a change.
const LONG_POLL: Duration = Duration::from_secs(25);

#[derive(Debug, Deserialize)]
pub(super) struct Since {
    since: Option<u64>,
}

pub(super) async fn regions(
    State(agent): State<Arc<Agent>>,
    Query(q): Query<Since>,
) -> Json<RegionsSnapshot> {
    let mut rx = agent.registry.subscribe();
    let mut shutdown = agent.shutdown.subscribe();
    if let Some(since) = q.since
        && rx.borrow_and_update().version == since
        && !*shutdown.borrow()
    {
        tokio::select! {
            _ = tokio::time::timeout(LONG_POLL, rx.changed()) => {}
            _ = shutdown.changed() => {}
        }
    }
    Json(rx.borrow().clone())
}

fn action_id(a: &RegionAction) -> Result<&str, AgentError> {
    if is_valid_region_id(&a.id) {
        Ok(&a.id)
    } else {
        Err(err(StatusCode::BAD_REQUEST, "not a region id"))
    }
}

pub(super) async fn import_region(
    State(agent): State<Arc<Agent>>,
    Json(a): Json<RegionAction>,
) -> Result<Json<RegionsSnapshot>, AgentError> {
    let id = action_id(&a)?;
    let extract = agent
        .catalog
        .find(id)
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no extract called `{id}`")))?;
    match agent.registry.enqueue(id, &extract.entry.name) {
        Ok(()) => Ok(Json(agent.registry.snapshot())),
        Err(_) => Err(err(
            StatusCode::CONFLICT,
            format!("{} is already being imported", extract.entry.name),
        )),
    }
}

pub(super) async fn cancel_region(
    State(agent): State<Arc<Agent>>,
    Json(a): Json<RegionAction>,
) -> Result<Json<RegionsSnapshot>, AgentError> {
    match agent.registry.cancel(action_id(&a)?) {
        Ok(()) => Ok(Json(agent.registry.snapshot())),
        Err(ActionError::NotFound | ActionError::Busy) => Err(err(
            StatusCode::NOT_FOUND,
            "no import of that region is running",
        )),
    }
}

pub(super) async fn delete_region(
    State(agent): State<Arc<Agent>>,
    Json(a): Json<RegionAction>,
) -> Result<Json<RegionsSnapshot>, AgentError> {
    match agent.registry.delete(action_id(&a)?) {
        Ok(()) => Ok(Json(agent.registry.snapshot())),
        Err(ActionError::NotFound | ActionError::Busy) => {
            Err(err(StatusCode::NOT_FOUND, "no such region"))
        }
    }
}

pub(super) async fn region_settings(
    State(agent): State<Arc<Agent>>,
    Json(a): Json<RegionAction>,
) -> Result<Json<RegionsSnapshot>, AgentError> {
    let id = action_id(&a)?;
    let range = agent.registry.refresh_range();
    if !a.has_settings()
        || a.refresh_days
            .flatten()
            .is_some_and(|d| !range.contains(&d))
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!(
                "expected `auto_refresh` (true or false), `refresh_days` ({} to {}), or both",
                range.start(),
                range.end()
            ),
        ));
    }
    match agent
        .registry
        .change_settings(id, a.auto_refresh, a.refresh_days)
    {
        Ok(()) => Ok(Json(agent.registry.snapshot())),
        Err(ActionError::NotFound | ActionError::Busy) => {
            Err(err(StatusCode::NOT_FOUND, "no such region"))
        }
    }
}
