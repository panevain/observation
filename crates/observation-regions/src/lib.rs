//! Region agent for Observation: runs next to Overpass's binaries, imports
//! Geofabrik extracts into per-region Overpass databases on request, and
//! answers Overpass queries for areas inside a ready region.
//!
//! The agent is an internal service for the Observation server; keep its port
//! off the public network and set `OBS_REGIONS_TOKEN` on both.
//!
//! | Route | |
//! | --- | --- |
//! | `GET /healthz` | liveness (no token) |
//! | `GET /regions?since=<version>` | [`RegionsSnapshot`]; waits up to 25 s for a newer one |
//! | `GET /catalog` | importable extracts |
//! | `POST /catalog/sizes` | [`SizesRequest`] -> `{"<id>": bytes or null}` |
//! | `POST /regions/import`, `/cancel`, `/delete` | `{"id": "…"}` |
//! | `POST /regions/settings` | `{"id": "…", "auto_refresh": true, "refresh_days": 30}` (either or both) |
//! | `POST /interpreter?bbox=s,w,n,e` | Overpass query (form field `data`) against the region holding the box; 404 if none |

pub mod catalog;
pub mod config;
pub mod import;
pub mod poly;
pub mod registry;

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Form, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use observation_core::geo::BBox;
use observation_core::regions::{
    RegionAction, RegionsSnapshot, SizesRequest, bearer_matches, is_valid_region_id,
};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::catalog::Catalog;
use crate::config::Config;
use crate::import::Tools;
use crate::registry::{ActionError, Registry};

/// Longest a `GET /regions?since=` waits for a change.
const LONG_POLL: Duration = Duration::from_secs(25);
/// Largest query text accepted.
const MAX_QUERY_BYTES: usize = 8 * 1024;
/// Largest query result returned (the server accepts 32 MB).
const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;
/// Queries allowed to wait for a free slot.
const MAX_WAITING_QUERIES: usize = 32;

/// Reads a response body, giving up past `max` bytes (`Ok(None)`).
pub async fn read_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> Result<Option<Bytes>, reqwest::Error> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Ok(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > max {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(Bytes::from(body)))
}

/// Writes `bytes` to a temporary file next to `path`, then renames it over.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let path = path.to_owned();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || write_atomic_blocking(&path, &bytes))
        .await
        .map_err(std::io::Error::other)?
}

/// Blocking form of [`write_atomic`].
pub fn write_atomic_blocking(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    // A unique name per write, so concurrent writers never share a temp file.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".tmp-{}-{n}", std::process::id()));
    // Synced before the rename, so a power loss leaves the old file or the new one.
    let result = (|| {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Shared agent state.
#[derive(Debug)]
pub struct Agent {
    pub registry: Arc<Registry>,
    pub catalog: Catalog,
    pub tools: Tools,
    token: Option<String>,
    queries: Semaphore,
    waiting: std::sync::atomic::AtomicUsize,
    query_timeout: Duration,
    /// Flipped when shutting down, to end long polls.
    shutdown: tokio::sync::watch::Sender<bool>,
}

/// The agent could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    State(#[from] registry::OpenError),
    #[error("could not build the HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

impl Agent {
    /// Opens the data directory and builds the HTTP client.
    pub fn new(config: &Config) -> Result<Arc<Self>, StartError> {
        // Extracts and the index come from one host; redirects may not leave it,
        // and names may not resolve to private addresses unless allowed.
        let client = observation_upstream::client_builder(
            config.allow_private_upstreams,
            observation_upstream::Redirects::SameHost,
        )
        .user_agent(config.user_agent())
        .connect_timeout(Duration::from_secs(20))
        .build()?;
        let registry = Arc::new(
            Registry::open(
                &config.data_dir,
                config.max_download_bytes,
                config.refresh_days,
            )?
            .with_refresh_range(config.min_refresh_days, config.max_refresh_days),
        );
        Ok(Arc::new(Self {
            catalog: Catalog::new(
                client.clone(),
                config.catalog_url.clone(),
                config.data_dir.join("catalog.json"),
            ),
            tools: Tools {
                client,
                osmium: config.osmium.clone(),
                overpass_bin: config.overpass_bin.clone(),
            },
            registry,
            token: config.token.clone(),
            queries: Semaphore::new(config.query_concurrency),
            waiting: std::sync::atomic::AtomicUsize::new(0),
            query_timeout: config.query_timeout,
            shutdown: tokio::sync::watch::Sender::new(false),
        }))
    }

    /// Ends pending long polls so a graceful shutdown does not wait on them.
    pub fn begin_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// Runs queued imports one at a time, forever.
    pub async fn run_worker(self: Arc<Self>) {
        loop {
            let (id, job) = self.registry.next_job().await;
            let this = Arc::clone(&self);
            let region = id.clone();
            let task = tokio::spawn(async move {
                let extract = match this.catalog.find(&region).await {
                    Ok(Some(x)) => x,
                    Ok(None) => {
                        return Err(format!(
                            "`{region}` is no longer offered by the extract list"
                        ));
                    }
                    Err(e) => return Err(e.to_string()),
                };
                import::run(&this.tools, &this.registry, job, &extract).await
            });
            if !self.registry.started(&id, job, task.abort_handle()) {
                let _ = task.await;
                self.registry.finish(&id, job, None);
                continue;
            }
            tracing::info!(region = %id, "import started");
            let result = match task.await {
                Ok(r) => Some(r),
                Err(e) if e.is_cancelled() => {
                    tracing::info!(region = %id, "import cancelled");
                    None
                }
                Err(e) => Some(Err(format!("the import crashed: {e}"))),
            };
            // Cancelled and failed imports leave their downloads behind.
            let work = self
                .registry
                .data_dir()
                .join("work")
                .join(registry::slug(&id));
            let _ = tokio::fs::remove_dir_all(work).await;
            self.registry.finish(&id, job, result);
        }
    }

    /// Queues re-imports of regions older than the refresh interval, hourly.
    pub async fn run_refresher(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            for (id, name) in self.registry.due_for_refresh() {
                tracing::info!(region = %id, "queueing scheduled refresh");
                let _ = self.registry.enqueue(&id, &name);
            }
        }
    }
}

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
        .route("/regions", get(regions))
        .route("/catalog", get(catalog_list))
        .route("/catalog/sizes", post(catalog_sizes))
        .route("/regions/import", post(import_region))
        .route("/regions/cancel", post(cancel_region))
        .route("/regions/delete", post(delete_region))
        .route("/regions/settings", post(region_settings))
        .route("/interpreter", post(interpreter))
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

#[derive(Debug, Deserialize)]
struct Since {
    since: Option<u64>,
}

async fn regions(State(agent): State<Arc<Agent>>, Query(q): Query<Since>) -> Json<RegionsSnapshot> {
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

async fn catalog_list(State(agent): State<Arc<Agent>>) -> Result<Response, AgentError> {
    let list = agent
        .catalog
        .extracts()
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let entries: Vec<_> = list.iter().map(|x| &x.entry).collect();
    Ok(Json(entries).into_response())
}

async fn catalog_sizes(
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

fn action_id(a: &RegionAction) -> Result<&str, AgentError> {
    if is_valid_region_id(&a.id) {
        Ok(&a.id)
    } else {
        Err(err(StatusCode::BAD_REQUEST, "not a region id"))
    }
}

async fn import_region(
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

async fn cancel_region(
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

async fn delete_region(
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

async fn region_settings(
    State(agent): State<Arc<Agent>>,
    Json(a): Json<RegionAction>,
) -> Result<Json<RegionsSnapshot>, AgentError> {
    let id = action_id(&a)?;
    let range = agent.registry.refresh_range();
    if !a.has_settings() || a.refresh_days.is_some_and(|d| !range.contains(&d)) {
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

#[derive(Debug, Deserialize)]
struct BBoxParam {
    bbox: String,
}

#[derive(Debug, Deserialize)]
struct QueryForm {
    data: String,
}

/// Decrements the waiting counter when dropped.
struct WaitGuard<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn interpreter(
    State(agent): State<Arc<Agent>>,
    Query(p): Query<BBoxParam>,
    Form(form): Form<QueryForm>,
) -> Result<Response, AgentError> {
    let bbox = BBox::parse(&p.bbox).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    if form.data.len() > MAX_QUERY_BYTES {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "query too long"));
    }
    let (region, db) = agent
        .registry
        .covering(&bbox)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no imported region holds this area"))?;
    let guard = WaitGuard(&agent.waiting);
    if agent
        .waiting
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        >= MAX_WAITING_QUERIES
    {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many queries waiting",
        ));
    }
    let _permit = agent
        .queries
        .acquire()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    drop(guard);
    let started = std::time::Instant::now();
    let out = tokio::time::timeout(
        agent.query_timeout,
        run_query(&agent.tools.overpass_bin, &db.dir, &form.data),
    )
    .await
    .map_err(|_| err(StatusCode::GATEWAY_TIMEOUT, "the query took too long"))?
    .map_err(|e| {
        tracing::warn!(%region, error = %e, "query failed");
        err(StatusCode::INTERNAL_SERVER_ERROR, e)
    })?;
    tracing::debug!(%region, ms = started.elapsed().as_millis() as u64, bytes = out.len(), "query");
    let mut resp = ([(header::CONTENT_TYPE, "application/json")], out).into_response();
    if let Ok(v) = HeaderValue::from_str(&region) {
        resp.headers_mut().insert("x-region", v);
    }
    Ok(resp)
}

/// Runs `osm3s_query` directly on a database directory (no dispatcher: the
/// databases are read-only once imported).
async fn run_query(bin: &Path, db: &Path, query: &str) -> Result<Vec<u8>, String> {
    let mut child = Command::new(bin.join("osm3s_query"))
        .arg(format!("--db-dir={}/", db.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start osm3s_query: {e}"))?;
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("could not talk to osm3s_query".into());
    };
    let query = query.as_bytes().to_vec();
    let write = tokio::spawn(async move {
        stdin.write_all(&query).await?;
        stdin.shutdown().await
    });
    // Drained to the end (so the child never blocks on it); the start is kept.
    let errs = tokio::spawn(async move {
        let mut kept = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            if kept.len() < 64 * 1024 {
                kept.extend_from_slice(&buf[..n]);
            }
        }
        kept
    });
    let read = async {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                return Ok(out);
            }
            if out.len() + n > MAX_RESULT_BYTES {
                return Err(std::io::Error::other("the result is too large"));
            }
            out.extend_from_slice(&buf[..n]);
        }
    }
    .await;
    let out = match read {
        Ok(out) => out,
        Err(e) => {
            // Stop it now rather than let it block on the pipe until the timeout.
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(format!("osm3s_query output: {e}"));
        }
    };
    let status = child
        .wait()
        .await
        .map_err(|e| format!("osm3s_query failed: {e}"))?;
    let wrote = write.await.is_ok_and(|w| w.is_ok());
    let errs = errs.await.unwrap_or_default();
    if !status.success() || !wrote {
        let errs = String::from_utf8_lossy(&errs);
        return Err(format!(
            "osm3s_query failed ({status}): {}",
            errs.lines()
                .filter(|l| l.contains("error"))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in `osm3s_query` running `script`.
    #[cfg(unix)]
    fn fake_query_bin(script: &str) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("osm3s_query");
        std::fs::write(&bin, format!("#!/bin/sh\ncat >/dev/null\n{script}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn queries_return_output_and_errors() {
        let ok = fake_query_bin("echo '{\"elements\":[]}'");
        let out = run_query(ok.path(), Path::new("/db"), "q").await.unwrap();
        assert_eq!(out, b"{\"elements\":[]}\n");
        let bad = fake_query_bin("echo 'runtime error: broken' >&2; exit 3");
        let e = run_query(bad.path(), Path::new("/db"), "q")
            .await
            .unwrap_err();
        assert!(e.contains("runtime error: broken"), "{e}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_results_fail_at_once() {
        // Endless output: must stop at the cap, not wait for the query timeout.
        let big = fake_query_bin("yes '{\"padding\":1}'");
        let started = std::time::Instant::now();
        let e = run_query(big.path(), Path::new("/db"), "q")
            .await
            .unwrap_err();
        assert!(e.contains("too large"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
