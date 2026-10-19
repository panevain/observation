//! API error type with a JSON `IntoResponse` implementation.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use thiserror::Error;

/// Errors returned by the HTTP API. Cheap to clone so single-flight fetches
/// can hand the same outcome to every waiting request.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ApiError {
    /// The request itself is invalid (400).
    #[error("{0}")]
    BadRequest(String),
    /// Unknown resource (404).
    #[error("not found")]
    NotFound,
    /// An upstream provider (Overpass, tile server) failed (502).
    #[error("{0}")]
    Upstream(String),
    /// An upstream provider did not answer in time (504).
    #[error("{0}")]
    UpstreamTimeout(String),
    /// An upstream provider is rate limiting us, or we are cooling down (503).
    #[error("the map data provider is busy, please retry shortly")]
    RateLimited {
        /// Value for the `retry-after` header, seconds.
        retry_after_secs: u64,
    },
    /// Unexpected local failure (500). The detail is logged, never sent.
    #[error("internal error")]
    Internal(String),
}

impl ApiError {
    /// HTTP status for this error.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::UpstreamTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
            Self::RateLimited { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Internal(detail) = &self {
            tracing::error!(%detail, "internal error");
        }
        let mut resp = (self.status(), Json(json!({ "error": self.to_string() }))).into_response();
        if let Self::RateLimited { retry_after_secs } = self
            && let Ok(v) = HeaderValue::from_str(&retry_after_secs.to_string())
        {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        resp
    }
}
