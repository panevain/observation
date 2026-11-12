//! [`RegionAgent`] over HTTP.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use http::{Method, StatusCode};
use observation_core::geo::BBox;
use observation_core::provenance::parse_utc;
use observation_upstream::{Body, HttpClient, HttpRequest, HttpResponse, redact};

use crate::{
    ActionKind, AgentError, OSM_BASE_HEADER, QueryResult, RegionAction, RegionAgent,
    RegionsSnapshot, SizesRequest, paths,
};

/// Largest query result accepted (as for public Overpass).
const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;
/// Largest snapshot, catalog or error reply accepted.
const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
/// How much of a failed query's reply is read, for the log.
const QUERY_ERROR_BYTES: usize = 4096;
/// Longest a snapshot request may take: the agent's long poll, and a margin.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(40);
/// Longest a catalog request or an action may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(45);

/// The region agent at `base`, called through `client` with the server's
/// token. Its `Debug` output hides the token and any credentials in `base`.
pub struct HttpRegionAgent {
    client: Arc<dyn HttpClient>,
    base: String,
    token: Option<String>,
}

impl fmt::Debug for HttpRegionAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRegionAgent")
            .field("client", &self.client)
            .field("base", &redact(&self.base))
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl HttpRegionAgent {
    /// `client` must carry the project `User-Agent` and no overall timeout
    /// (snapshot requests long-poll; every request sets its own).
    pub fn new(client: Arc<dyn HttpClient>, base: String, token: Option<String>) -> Self {
        Self {
            client,
            base,
            token,
        }
    }

    /// A request to `path` with the token. A token that cannot be a header
    /// value fails the request as reqwest's builder did.
    fn request(&self, method: Method, path: &str, max_bytes: usize) -> Result<HttpRequest, String> {
        let req = HttpRequest::new(method, format!("{}{path}", self.base), max_bytes);
        match &self.token {
            Some(t) => req.bearer(t).map_err(|_| "builder error".to_owned()),
            None => Ok(req),
        }
    }

    async fn snapshot_inner(&self, since: Option<u64>) -> Result<RegionsSnapshot, String> {
        let path = since.map_or_else(
            || paths::REGIONS.to_owned(),
            |v| format!("{}?since={v}", paths::REGIONS),
        );
        // An error status's body is never read. Nor is a 3xx's (the client
        // follows no redirects), which then reads as too large.
        let req = self
            .request(Method::GET, &path, MAX_JSON_BYTES)?
            .timeout(SNAPSHOT_TIMEOUT)
            .error_max_bytes(0);
        let resp = self.client.send(req).await.map_err(|e| e.to_string())?;
        if resp.status.is_client_error() || resp.status.is_server_error() {
            return Err(status_error(resp.status));
        }
        let Body::Complete(body) = resp.body else {
            return Err("the snapshot is too large".into());
        };
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    }

    async fn query_inner(
        &self,
        bbox: &BBox,
        query: &str,
        timeout: Duration,
    ) -> Result<Option<QueryResult>, String> {
        // Digits, `.`, `-` and `,` only: no escaping needed.
        let path = format!("{}?bbox={}", paths::INTERPRETER, bbox.overpass());
        // A 404's body is never read; another error's is read for the log,
        // and is left out of it if it breaks off.
        let req = self
            .request(Method::POST, &path, MAX_RESULT_BYTES)?
            .form([("data", query)])
            .timeout(timeout)
            .error_max_bytes(QUERY_ERROR_BYTES)
            .skip_body_of(StatusCode::NOT_FOUND);
        let resp = match self.client.send(req).await {
            Ok(resp) => resp,
            Err(e) => {
                return match e.status {
                    Some(status) if !status.is_success() => {
                        Err(format!("agent answered {status}: "))
                    }
                    _ => Err(e.to_string()),
                };
            }
        };
        if resp.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status.is_success() {
            return Err(format!(
                "agent answered {}: {}",
                resp.status,
                resp.bytes()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default()
            ));
        }
        // The region's data date, for databases imported before the agent
        // recorded it in them (they report an empty `timestamp_osm_base`).
        let osm_base_unix = resp
            .headers
            .get(OSM_BASE_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_utc);
        let Body::Complete(body) = resp.body else {
            return Err("the result is too large".into());
        };
        Ok(Some(QueryResult {
            body,
            osm_base_unix,
        }))
    }

    /// Sends a catalog request or an action and reads its JSON answer.
    async fn call(
        &self,
        method: Method,
        path: &str,
        json: Option<Result<Vec<u8>, serde_json::Error>>,
    ) -> Result<Bytes, AgentError> {
        let mut req = self
            .request(method, path, MAX_JSON_BYTES)
            .map_err(|_| AgentError::Unreachable)?
            .timeout(CALL_TIMEOUT)
            .error_max_bytes(MAX_JSON_BYTES);
        if let Some(json) = json {
            req = req.json(json.map_err(|_| AgentError::Unreachable)?);
        }
        let resp = self
            .client
            .send(req)
            .await
            .map_err(|_| AgentError::Unreachable)?;
        answer(resp)
    }
}

/// The body of a successful agent answer; its `error` text otherwise.
fn answer(resp: HttpResponse) -> Result<Bytes, AgentError> {
    let status = resp.status;
    let Body::Complete(body) = resp.body else {
        return Err(AgentError::TooLarge);
    };
    if status.is_success() {
        return Ok(body);
    }
    let msg = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("the region agent answered HTTP {}", status.as_u16()));
    Err(match status {
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND => AgentError::BadRequest(msg),
        StatusCode::CONFLICT => AgentError::Conflict(msg),
        StatusCode::UNAUTHORIZED => AgentError::Unauthorized,
        _ => AgentError::Other(msg),
    })
}

/// A 4xx or 5xx snapshot reply, worded as reqwest's `error_for_status`,
/// with the status's canonical reason phrase: the port does not carry the
/// one the reply sent, which reqwest printed when it differed.
fn status_error(status: StatusCode) -> String {
    let class = if status.is_client_error() {
        "client"
    } else {
        "server"
    };
    format!("HTTP status {class} error ({status})")
}

impl RegionAgent for HttpRegionAgent {
    fn snapshot(&self, since: Option<u64>) -> BoxFuture<'_, Result<RegionsSnapshot, String>> {
        self.snapshot_inner(since).boxed()
    }

    fn query<'a>(
        &'a self,
        bbox: &'a BBox,
        query: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Option<QueryResult>, String>> {
        self.query_inner(bbox, query, timeout).boxed()
    }

    fn catalog(&self) -> BoxFuture<'_, Result<Bytes, AgentError>> {
        self.call(Method::GET, paths::CATALOG, None).boxed()
    }

    fn catalog_sizes<'a>(
        &'a self,
        req: &'a SizesRequest,
    ) -> BoxFuture<'a, Result<Bytes, AgentError>> {
        self.call(
            Method::POST,
            paths::CATALOG_SIZES,
            Some(serde_json::to_vec(req)),
        )
        .boxed()
    }

    fn act<'a>(
        &'a self,
        action: ActionKind,
        req: &'a RegionAction,
    ) -> BoxFuture<'a, Result<Bytes, AgentError>> {
        self.call(
            Method::POST,
            paths::action(action),
            Some(serde_json::to_vec(req)),
        )
        .boxed()
    }
}
