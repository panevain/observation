//! `/api/regions...`: local region status, live progress and management.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream};
use observation_core::regions::{
    ActionKind, RegionAction, RegionsStatus, bearer_matches, is_valid_region_id,
};
use tokio::sync::{OwnedSemaphorePermit, watch};

use crate::AppState;
use crate::error::ApiError;
use crate::regions::Regions;

fn regions(state: &AppState) -> Result<&Arc<Regions>, ApiError> {
    state.regions().ok_or(ApiError::NotFound(
        "local regions are not enabled on this server",
    ))
}

fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// `GET /api/regions`
pub async fn status(State(state): State<AppState>) -> Result<Response, ApiError> {
    Ok(no_store(Json(regions(&state)?.status()).into_response()))
}

/// `GET /api/regions/catalog`: extracts that can be imported.
pub async fn catalog(State(state): State<AppState>) -> Result<Response, ApiError> {
    let body = regions(&state)?.catalog().await?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
            ),
        ],
        body,
    )
        .into_response())
}

/// `POST /api/regions/{import|cancel|delete}` with `{"id": "…"}`, or
/// `/api/regions/settings` with `{"id": "…", "auto_refresh": bool}`, and
/// `Authorization: Bearer <OBS_ADMIN_TOKEN>`.
pub async fn act(
    State(state): State<AppState>,
    Path(action): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let regions = regions(&state)?;
    let action = ActionKind::parse(&action).ok_or(ApiError::NotFound("no such region action"))?;
    let Some(admin) = state.config().admin_token.as_deref() else {
        return Err(ApiError::Forbidden(
            "region management is turned off on this server (set OBS_ADMIN_TOKEN)",
        ));
    };
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !bearer_matches(given, admin) {
        return Err(ApiError::Unauthorized(
            "the admin token is missing or wrong",
        ));
    }
    let mut req: RegionAction = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("expected {\"id\": \"<region id>\"}".into()))?;
    if !is_valid_region_id(&req.id) {
        return Err(ApiError::BadRequest("not a region id".into()));
    }
    if action == ActionKind::Settings {
        if req.auto_refresh.is_none() {
            return Err(ApiError::BadRequest(
                "expected {\"id\": \"<region id>\", \"auto_refresh\": true or false}".into(),
            ));
        }
    } else {
        // Only settings take more than the id.
        req.auto_refresh = None;
    }
    tracing::info!(?action, region = %req.id, auto_refresh = ?req.auto_refresh, "region action");
    let status = regions.act(action, &req).await?;
    Ok(no_store(Json(status).into_response()))
}

/// `GET /api/regions/events`: the [`RegionsStatus`] now and after every change,
/// as server-sent events. Ends when the server shuts down.
pub async fn events(State(state): State<AppState>) -> Result<Response, ApiError> {
    let regions = regions(&state)?;
    let permit = Arc::clone(state.event_streams())
        .try_acquire_owned()
        .map_err(|_| ApiError::RateLimited {
            retry_after_secs: 30,
        })?;
    let stream = status_stream(regions.subscribe(), state.shutdown_signal(), permit);
    Ok(no_store(
        Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response(),
    ))
}

fn status_stream(
    rx: watch::Receiver<RegionsStatus>,
    shutdown: watch::Receiver<bool>,
    permit: OwnedSemaphorePermit,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold(
        (rx, shutdown, permit, true),
        |(mut rx, mut shutdown, permit, first)| async move {
            if *shutdown.borrow() {
                return None;
            }
            if !first {
                tokio::select! {
                    changed = rx.changed() => changed.ok()?,
                    _ = shutdown.changed() => return None,
                }
            }
            let status = rx.borrow_and_update().clone();
            let event = Event::default()
                .event("regions")
                .json_data(&status)
                .unwrap_or_else(|_| Event::default().comment("unencodable"));
            Some((Ok(event), (rx, shutdown, permit, false)))
        },
    )
}
