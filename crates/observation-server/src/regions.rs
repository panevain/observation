//! Client of the region agent (`OBS_REGIONS_URL`): queries for areas inside an
//! imported region, region management for the UI, and a live copy of the
//! agent's [`RegionsSnapshot`] kept up to date by long-polling.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use observation_core::geo::BBox;
use observation_core::provenance::parse_utc;
use observation_core::regions::{
    ActionKind, OSM_BASE_HEADER, RegionAction, RegionView, RegionsSnapshot, RegionsStatus,
    SizesRequest,
};
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

    /// Publishes the agent's reachability and, if given, its snapshot. With
    /// `only_newer`, a snapshot older than the current one is dropped (an
    /// action's reply can arrive after the poller saw later changes; the poller
    /// itself always replaces, since a restarted agent counts from 0 again).
    fn publish(&self, online: bool, snapshot: Option<RegionsSnapshot>, only_newer: bool) {
        self.tx.send_if_modified(|s| {
            let mut changed = s.agent_online != online;
            s.agent_online = online;
            if let Some(snap) = snapshot
                && snap != s.snapshot
                && !(only_newer && snap.version <= s.snapshot.version)
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
                    self.publish(true, Some(snap), false);
                }
                Err(e) => {
                    self.publish(false, None, false);
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
    /// Bundles built before then are rebuilt from the region. (Bounds are a
    /// superset of the boundary: a box just outside it is rebuilt once per
    /// import, from public Overpass. Regions without bounds, those crossing the
    /// antimeridian, invalidate nothing; their bundles age out with the TTL.)
    pub fn data_since(&self, bbox: &BBox) -> Option<SystemTime> {
        let s = self.tx.borrow();
        s.snapshot
            .regions
            .iter()
            .filter(|r| r.ready && r.bbox.is_some_and(|b| b.contains_box(bbox)))
            .filter_map(|r| r.imported_unix)
            .max()
            .map(|t| UNIX_EPOCH + Duration::from_secs(t))
    }

    /// Buildings and roads for `bbox` from a local region. `Ok(None)` when no
    /// region holds the area (or the agent is offline); errors are logged by
    /// the caller, which falls back to public Overpass.
    pub async fn fetch(&self, bbox: &BBox, timeout: Duration) -> Result<Option<OsmData>, String> {
        {
            let s = self.tx.borrow();
            let candidate = s.snapshot.regions.iter().any(|r| might_hold(r, bbox));
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
            .timeout(timeout)
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
        // The region's data date, for databases imported before the agent
        // recorded it in them (they report an empty `timestamp_osm_base`).
        let header_base = resp
            .headers()
            .get(OSM_BASE_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_utc);
        let body = crate::read_capped(resp, MAX_RESULT_BYTES)
            .await
            .map_err(|e| e.without_url().to_string())?
            .ok_or("the result is too large")?;
        let mut data = parse_response(&body).map_err(|e| e.to_string())?;
        data.osm_base_unix = data.osm_base_unix.or(header_base);
        Ok(Some(data))
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

    /// Download sizes of catalog extracts (JSON, `{"<id>": bytes or null}`).
    /// The agent caches them, so this is not cached here.
    pub async fn catalog_sizes(&self, req: &SizesRequest) -> Result<Bytes, ApiError> {
        let resp = self
            .request(reqwest::Method::POST, "/catalog/sizes")
            .json(req)
            .timeout(Duration::from_secs(45))
            .send()
            .await
            .map_err(|_| unreachable_agent())?;
        self.answer(resp).await
    }

    /// Forwards an action and publishes the snapshot the agent answers with.
    pub async fn act(
        &self,
        action: ActionKind,
        req: &RegionAction,
    ) -> Result<RegionsStatus, ApiError> {
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
            .json(req)
            .timeout(Duration::from_secs(45))
            .send()
            .await
            .map_err(|_| unreachable_agent())?;
        let body = self.answer(resp).await?;
        let snap: RegionsSnapshot = serde_json::from_slice(&body)
            .map_err(|e| ApiError::Internal(format!("agent snapshot unreadable: {e}")))?;
        self.publish(true, Some(snap), true);
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

/// Whether `r` may hold `bbox`: ready, and its bounds hold the box (a region
/// without bounds, crossing the antimeridian, is left to the agent to decide).
fn might_hold(r: &RegionView, bbox: &BBox) -> bool {
    r.ready && r.bbox.is_none_or(|b| b.contains_box(bbox))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(ready: bool, bbox: Option<BBox>) -> RegionView {
        RegionView {
            id: "r".into(),
            name: "R".into(),
            ready,
            bbox,
            data_timestamp: None,
            imported_unix: Some(1),
            last_attempt_unix: None,
            auto_refresh: true,
            refresh_days: None,
            next_refresh_unix: None,
            disk_bytes: None,
            job: None,
            last_error: None,
            last_error_unix: None,
        }
    }

    #[test]
    fn candidates() {
        let inner = BBox::new(0.1, 0.1, 0.2, 0.2).unwrap();
        let outer = BBox::new(0.0, 0.0, 1.0, 1.0).ok();
        assert!(might_hold(&region(true, outer), &inner));
        assert!(!might_hold(&region(false, outer), &inner));
        assert!(!might_hold(
            &region(true, BBox::new(0.5, 0.5, 1.0, 1.0).ok()),
            &inner
        ));
        assert!(
            might_hold(&region(true, None), &inner),
            "no bounds: ask the agent"
        );
    }
}
