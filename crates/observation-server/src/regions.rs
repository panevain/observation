//! Client of the region agent (`OBS_REGIONS_URL`): queries for areas inside an
//! imported region, region management for the UI, and a live copy of the
//! agent's [`RegionsSnapshot`] kept up to date by long-polling.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use observation_core::geo::BBox;
use observation_core::regions::{RegionAction, RegionsSnapshot, RegionsStatus};
use reqwest::StatusCode;
use tokio::sync::watch;

use crate::error::ApiError;
use crate::overpass::{OsmData, build_query, parse_response};

/// Largest query result accepted (as for public Overpass).
const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;
/// Largest snapshot or catalog accepted.
const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
/// How long the catalog is reused.
const CATALOG_TTL: Duration = Duration::from_secs(3600);

/// A region management action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Import,
    Cancel,
    Delete,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "import" => Some(Self::Import),
            "cancel" => Some(Self::Cancel),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Cancel => "cancel",
            Self::Delete => "delete",
        }
    }
}

#[derive(Debug)]
pub struct Regions {
    client: reqwest::Client,
    base: String,
    token: Option<String>,
    manageable: bool,
    tx: watch::Sender<RegionsStatus>,
    catalog: tokio::sync::Mutex<Option<(Instant, Bytes)>>,
}

impl Regions {
    /// `client` must carry the project `User-Agent` and no overall timeout
    /// (snapshot requests long-poll).
    pub fn new(
        client: reqwest::Client,
        base: String,
        token: Option<String>,
        manageable: bool,
    ) -> Self {
        let (tx, _) = watch::channel(RegionsStatus {
            agent_online: false,
            manageable,
            snapshot: RegionsSnapshot::default(),
        });
        Self {
            client,
            base,
            token,
            manageable,
            tx,
            catalog: tokio::sync::Mutex::new(None),
        }
    }

    pub fn status(&self) -> RegionsStatus {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<RegionsStatus> {
        self.tx.subscribe()
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let req = self.client.request(method, format!("{}{path}", self.base));
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    fn publish(&self, online: bool, snapshot: Option<RegionsSnapshot>) {
        self.tx.send_if_modified(|s| {
            let mut changed = s.agent_online != online;
            s.agent_online = online;
            if let Some(snap) = snapshot
                && snap != s.snapshot
            {
                s.snapshot = snap;
                changed = true;
            }
            changed
        });
    }

    /// Follows the agent's snapshot forever: long-polls for changes, backing
    /// off while the agent is unreachable.
    pub async fn run_poller(self: std::sync::Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        // Last time an unreachable agent was logged.
        let mut warned: Option<Instant> = None;
        loop {
            let since = {
                let s = self.tx.borrow();
                s.agent_online.then_some(s.snapshot.version)
            };
            let path =
                since.map_or_else(|| "/regions".to_owned(), |v| format!("/regions?since={v}"));
            let result = async {
                let resp = self
                    .request(reqwest::Method::GET, &path)
                    .timeout(Duration::from_secs(40))
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|e| e.without_url().to_string())?;
                let body = crate::read_capped(resp, MAX_JSON_BYTES)
                    .await
                    .map_err(|e| e.without_url().to_string())?
                    .ok_or("the snapshot is too large")?;
                serde_json::from_slice::<RegionsSnapshot>(&body).map_err(|e| e.to_string())
            }
            .await;
            match result {
                Ok(snap) => {
                    backoff = Duration::from_secs(1);
                    self.publish(true, Some(snap));
                }
                Err(e) => {
                    self.publish(false, None);
                    if warned.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) {
                        tracing::warn!(error = %e, "region agent unreachable");
                        warned = Some(Instant::now());
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    }

    /// When the newest ready region whose bounds hold `bbox` was imported.
    /// Bundles built before then are rebuilt from the region.
    pub fn data_since(&self, bbox: &BBox) -> Option<SystemTime> {
        let s = self.tx.borrow();
        s.snapshot
            .regions
            .iter()
            .filter(|r| r.ready && r.bbox.is_some_and(|b| holds(&b, bbox)))
            .filter_map(|r| r.imported_unix)
            .max()
            .map(|t| UNIX_EPOCH + Duration::from_secs(t))
    }

    /// Buildings and roads for `bbox` from a local region. `Ok(None)` when no
    /// region holds the area (or the agent is offline); errors are logged by
    /// the caller, which falls back to public Overpass.
    pub async fn fetch(&self, bbox: &BBox) -> Result<Option<OsmData>, String> {
        {
            let s = self.tx.borrow();
            let candidate = s
                .snapshot
                .regions
                .iter()
                .any(|r| r.ready && r.bbox.is_some_and(|b| holds(&b, bbox)));
            if !s.agent_online || !candidate {
                return Ok(None);
            }
        }
        let resp = self
            // Digits, `.`, `-` and `,` only: no escaping needed.
            .request(
                reqwest::Method::POST,
                &format!("/interpreter?bbox={}", bbox.overpass()),
            )
            .form(&[("data", build_query(bbox))])
            .timeout(Duration::from_secs(35))
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = crate::read_capped(resp, 4096).await.ok().flatten();
            return Err(format!(
                "agent answered {status}: {}",
                body.map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default()
            ));
        }
        let body = crate::read_capped(resp, MAX_RESULT_BYTES)
            .await
            .map_err(|e| e.without_url().to_string())?
            .ok_or("the result is too large")?;
        parse_response(&body).map(Some).map_err(|e| e.to_string())
    }

    /// The importable extracts (JSON), cached for an hour.
    pub async fn catalog(&self) -> Result<Bytes, ApiError> {
        let mut cached = self.catalog.lock().await;
        if let Some((at, body)) = cached.as_ref()
            && at.elapsed() < CATALOG_TTL
        {
            return Ok(body.clone());
        }
        let resp = self
            .request(reqwest::Method::GET, "/catalog")
            .timeout(Duration::from_secs(45))
            .send()
            .await
            .map_err(|_| unreachable_agent())?;
        let body = self.answer(resp).await?;
        *cached = Some((Instant::now(), body.clone()));
        Ok(body)
    }

    /// Forwards an action and publishes the snapshot the agent answers with.
    pub async fn act(&self, action: Action, id: &str) -> Result<RegionsStatus, ApiError> {
        if !self.manageable {
            return Err(ApiError::Forbidden(
                "region management is turned off on this server (set OBS_ADMIN_TOKEN)",
            ));
        }
        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/regions/{}", action.path()),
            )
            .json(&RegionAction { id: id.to_owned() })
            .timeout(Duration::from_secs(45))
            .send()
            .await
            .map_err(|_| unreachable_agent())?;
        let body = self.answer(resp).await?;
        let snap: RegionsSnapshot = serde_json::from_slice(&body)
            .map_err(|e| ApiError::Internal(format!("agent snapshot unreadable: {e}")))?;
        self.publish(true, Some(snap));
        Ok(self.status())
    }

    /// The body of a successful agent answer; its `error` text otherwise.
    async fn answer(&self, resp: reqwest::Response) -> Result<Bytes, ApiError> {
        let status = resp.status();
        let body = crate::read_capped(resp, MAX_JSON_BYTES)
            .await
            .map_err(|_| unreachable_agent())?
            .ok_or_else(|| ApiError::Upstream("the region agent sent too much data".into()))?;
        if status.is_success() {
            return Ok(body);
        }
        let msg = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("the region agent answered HTTP {}", status.as_u16()));
        Err(match status {
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND => ApiError::BadRequest(msg),
            StatusCode::CONFLICT => ApiError::Conflict(msg),
            StatusCode::UNAUTHORIZED => {
                tracing::error!("the region agent refused OBS_REGIONS_TOKEN");
                ApiError::Upstream(
                    "the server is not allowed to use its region agent; check OBS_REGIONS_TOKEN"
                        .into(),
                )
            }
            _ => ApiError::Upstream(msg),
        })
    }
}

fn unreachable_agent() -> ApiError {
    ApiError::Upstream("the region agent is not reachable; try again shortly".into())
}

/// Whether `outer` holds all of `inner`.
fn holds(outer: &BBox, inner: &BBox) -> bool {
    outer.south <= inner.south
        && outer.west <= inner.west
        && outer.north >= inner.north
        && outer.east >= inner.east
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_holds() {
        let outer = BBox::new(0.0, 0.0, 1.0, 1.0).unwrap();
        assert!(holds(&outer, &BBox::new(0.1, 0.1, 0.2, 0.2).unwrap()));
        assert!(!holds(&outer, &BBox::new(0.9, 0.9, 1.1, 1.1).unwrap()));
    }

    #[test]
    fn actions() {
        assert_eq!(Action::parse("import"), Some(Action::Import));
        assert_eq!(Action::parse("nuke"), None);
    }
}
