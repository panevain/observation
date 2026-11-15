//! The connection to the user's open app, through the agent bridge of an
//! `observation-server` on this computer (see [`observation_agent_api`]).
//!
//! It is the only state the MCP server keeps. [`AppLink::connect`] claims a
//! pairing with the code the app shows, then a background task follows the
//! app's state with the bridge's long poll, so [`AppLink::subscribe`] can
//! tell resource subscribers when it changes. While connected, the real
//! areas the user's scenes are on are fetched from the same server and held
//! in the connection's [`AreaCache`].
//!
//! Every request goes through `observation-upstream`'s [`GuardedHttp`],
//! with private addresses allowed (the bridge is on loopback), no proxy and
//! no redirects; and the only URLs it is given are built from a base that
//! [`bridge_base`] has checked is plain `http` on this computer. So no tool
//! argument can steer it anywhere else.

use crate::ToolError;
use crate::area::AreaCache;
use observation_agent_api::{
    ClaimRequest, Claimed, EndReason, ErrorBody, LONG_POLL_SECS, MAX_PROPOSAL_BYTES,
    MAX_STATE_BYTES, PROTOCOL_VERSION, ProposalPost, ProposalSubmitted, StateView, paths,
    version_ok,
};
use observation_core::bundle::{AreaBundle, BUNDLE_VERSION};
use observation_core::scene::AreaRef;
use observation_core::scene::changes::ChangeSet;
use observation_upstream::{
    GuardedHttp, HttpClient, HttpRequest, HttpResponse, Redirects, TransportError, Url,
};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;
use tokio::sync::watch;

/// The resource that holds the app's state.
pub const STATE_URI: &str = "observation://app/state";
/// What this process calls itself when it claims a pairing.
pub const AGENT_NAME: &str = "Observation MCP";
/// Largest bundle accepted from the server.
const MAX_BUNDLE_BYTES: usize = 64 << 20;
/// Longest a bundle may take: the server may build it from Overpass (its
/// own request timeout is 80 s).
pub(crate) const BUNDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Longest any other request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Failed long polls in a row after which the connection counts as lost.
const MAX_POLL_FAILURES: u32 = 5;

/// The bridge's base URL for `url`, if `url` is plain `http` on this
/// computer: `127.0.0.1` (or any `127.x.y.z`), `[::1]` or `localhost`, with
/// any port and no path, query, user or password. `localhost` becomes
/// `127.0.0.1`, so no name is looked up.
pub fn bridge_base(url: &str) -> Result<String, ToolError> {
    let refuse = |why: &str| {
        Err(ToolError::invalid(format!(
            "{why}: give the address the app is open at on this computer, such as \
             http://127.0.0.1:8080 or http://localhost:8080"
        )))
    };
    let Ok(parsed) = Url::parse(url.trim()) else {
        return refuse("not a URL");
    };
    if parsed.scheme() != "http" {
        return refuse("the app's server is reached over plain http on this computer");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return refuse("the address may not hold a user name or password");
    }
    if !matches!(parsed.path(), "" | "/") || parsed.query().is_some() || parsed.fragment().is_some()
    {
        return refuse("give only the server's address, without a path");
    }
    let host = match parsed.host_str() {
        Some(h) => h.trim_matches(['[', ']']).to_ascii_lowercase(),
        None => return refuse("the address has no host"),
    };
    let host = if host == "localhost" {
        "127.0.0.1".to_owned()
    } else {
        match host.parse::<IpAddr>() {
            Ok(ip) if ip.to_canonical().is_loopback() => match ip.to_canonical() {
                IpAddr::V4(v4) => v4.to_string(),
                IpAddr::V6(v6) => format!("[{v6}]"),
            },
            _ => return refuse("only this computer's address is accepted"),
        }
    };
    let port = parsed.port_or_known_default().unwrap_or(80);
    Ok(format!("http://{host}:{port}"))
}

/// The app's state as the tools report it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// The server's base URL while connected.
    pub server: Option<String>,
    /// The latest state read, if connected.
    pub view: Option<StateView>,
    /// Why the last connection ended, if it has.
    pub ended: Option<String>,
}

#[derive(Debug)]
struct Conn {
    id: u64,
    base: String,
    token: String,
    view: StateView,
    watcher: Option<tokio::task::AbortHandle>,
}

/// A refused request: the status and the server's words.
#[derive(Debug)]
struct Refusal {
    status: u16,
    message: String,
}

/// The connection to the app, if any. Share it behind an `Arc`.
#[derive(Debug)]
pub struct AppLink {
    http: OnceLock<Result<Arc<dyn HttpClient>, String>>,
    conn: Mutex<Option<Conn>>,
    ended: Mutex<Option<String>>,
    next_id: AtomicU64,
    changes: watch::Sender<u64>,
    areas: Arc<AreaCache>,
    /// The task that forwards changes to a client subscribed the legacy way
    /// (`resources/subscribe`).
    legacy: Mutex<Option<tokio::task::AbortHandle>>,
}

impl Default for AppLink {
    fn default() -> Self {
        Self {
            http: OnceLock::new(),
            conn: Mutex::new(None),
            ended: Mutex::new(None),
            next_id: AtomicU64::new(0),
            changes: watch::Sender::new(0),
            areas: Arc::default(),
            legacy: Mutex::new(None),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn transport(base: &str, e: &TransportError) -> String {
    format!("could not reach the app's server at {base}: {e}")
}

impl AppLink {
    fn http(&self) -> Result<Arc<dyn HttpClient>, ToolError> {
        self.http
            .get_or_init(|| {
                GuardedHttp::builder(true, Redirects::None)
                    .no_proxy()
                    .user_agent(&format!("observation-mcp/{}", env!("CARGO_PKG_VERSION")))
                    .connect_timeout(Duration::from_secs(5))
                    .build()
                    .map(|c| Arc::new(c) as Arc<dyn HttpClient>)
                    .map_err(|e| e.to_string())
            })
            .clone()
            .map_err(|e| ToolError::Link(format!("could not start the HTTP client: {e}")))
    }

    /// Sends `req`; a 2xx answer's body, or the refusal in the server's words.
    async fn send(
        &self,
        base: &str,
        req: HttpRequest,
    ) -> Result<Result<Vec<u8>, Refusal>, ToolError> {
        let resp: HttpResponse = self
            .http()?
            .send(req)
            .await
            .map_err(|e| ToolError::Link(transport(base, &e)))?;
        let body = resp.bytes().map(|b| b.to_vec());
        if resp.status.is_success() {
            return body
                .map(Ok)
                .ok_or_else(|| ToolError::Link("the app's server sent too much data".into()));
        }
        // The server's answer for an address it does not have (the bridge
        // is off) says nothing useful here.
        let words = body
            .and_then(|b| serde_json::from_slice::<ErrorBody>(&b).ok())
            .map(|e| e.error)
            .filter(|w| w != "no such API address on this server");
        let message = match (resp.status.as_u16(), words) {
            (300..=399, _) => format!(
                "the server at {base} answered with a redirect, which is not followed; \
                 is it the Observation server?"
            ),
            (404, None) => format!(
                "the server at {base} has no agent bridge: start it with OBS_AGENT_BRIDGE=on"
            ),
            (_, Some(w)) => w,
            (s, None) => format!("the server at {base} answered HTTP {s}"),
        };
        Ok(Err(Refusal {
            status: resp.status.as_u16(),
            message,
        }))
    }

    fn json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ToolError> {
        serde_json::from_slice(body).map_err(|e| {
            ToolError::Link(format!("the app's server sent an unreadable answer: {e}"))
        })
    }

    async fn read_state(
        &self,
        base: &str,
        token: &str,
        since: Option<u64>,
    ) -> Result<Result<StateView, Refusal>, ToolError> {
        let url = match since {
            Some(s) => format!("{base}{}?since={s}", paths::STATE),
            None => format!("{base}{}", paths::STATE),
        };
        let req = HttpRequest::get(url, 2 * MAX_STATE_BYTES)
            .bearer(token)
            .map_err(|_| ToolError::Link("the pairing token is malformed".into()))?
            .timeout(Duration::from_secs(LONG_POLL_SECS + 10));
        match self.send(base, req).await? {
            Ok(body) => {
                let view: StateView = Self::json(&body)?;
                if !version_ok(view.v) {
                    return Err(ToolError::Link(format!(
                        "the app's server speaks bridge protocol {}, this agent {PROTOCOL_VERSION}",
                        view.v
                    )));
                }
                Ok(Ok(view))
            }
            Err(r) => Ok(Err(r)),
        }
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Bumped whenever what [`Self::snapshot`] returns changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Replaces the task serving a legacy resource subscription (`None`:
    /// the client unsubscribed).
    pub fn set_legacy_subscription(&self, task: Option<tokio::task::AbortHandle>) {
        if let Some(old) = std::mem::replace(&mut *lock(&self.legacy), task) {
            old.abort();
        }
    }

    /// The real areas held for this connection.
    pub fn areas(&self) -> Arc<AreaCache> {
        Arc::clone(&self.areas)
    }

    pub fn is_connected(&self) -> bool {
        lock(&self.conn).is_some()
    }

    /// What is known now, without asking the server.
    pub fn snapshot(&self) -> Snapshot {
        let conn = lock(&self.conn);
        Snapshot {
            server: conn.as_ref().map(|c| c.base.clone()),
            view: conn.as_ref().map(|c| c.view.clone()),
            ended: lock(&self.ended).clone(),
        }
    }

    /// Takes in a state read for connection `id`; false if that connection
    /// is gone or the read says it ended.
    fn apply(&self, id: u64, view: StateView) -> bool {
        let mut conn = lock(&self.conn);
        let Some(c) = conn.as_mut().filter(|c| c.id == id) else {
            return false;
        };
        if let Some(reason) = view.ended {
            drop(conn);
            self.lose(id, reason.describe().to_owned());
            return false;
        }
        if c.view != view {
            c.view = view;
            drop(conn);
            self.changed();
        }
        true
    }

    /// Ends connection `id` on this side, recording why.
    fn lose(&self, id: u64, why: String) {
        let mut conn = lock(&self.conn);
        if conn.as_ref().is_some_and(|c| c.id == id) {
            if let Some(w) = conn.take().and_then(|c| c.watcher) {
                w.abort();
            }
            drop(conn);
            *lock(&self.ended) = Some(why);
            self.areas.clear();
            self.changed();
        }
    }

    /// Claims the pairing with `code` on the server at `url` (see
    /// [`bridge_base`]) and switches to it. A connection already open is
    /// ended (telling its server, best effort) only once the new pairing is
    /// claimed and read; until then it stays as it was, and if the new code
    /// does not work it is kept and the error says so. Returns what the app
    /// shares now.
    pub async fn connect(self: &Arc<Self>, url: &str, code: &str) -> Result<Snapshot, ToolError> {
        let base = bridge_base(url).map_err(|e| self.kept(e))?;
        let (token, view) = self.claim(&base, code).await.map_err(|e| self.kept(e))?;
        self.disconnect().await;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let since = view.revision;
        *lock(&self.conn) = Some(Conn {
            id,
            base: base.clone(),
            token: token.clone(),
            view,
            watcher: None,
        });
        *lock(&self.ended) = None;
        self.areas.clear();
        let task = tokio::spawn(follow(Arc::downgrade(self), id, base, token, since));
        if let Some(c) = lock(&self.conn).as_mut().filter(|c| c.id == id) {
            c.watcher = Some(task.abort_handle());
        }
        self.changed();
        Ok(self.snapshot())
    }

    /// Claims the pairing with `code` on the server at `base` and reads its
    /// state: the agent token and the state. Touches no connection already
    /// open. A pairing claimed but not read is ended again (best effort), so
    /// that the app is not left waiting on an agent that is not there.
    async fn claim(&self, base: &str, code: &str) -> Result<(String, StateView), ToolError> {
        let body = serde_json::to_vec(&ClaimRequest {
            v: PROTOCOL_VERSION,
            code: code.trim().to_owned(),
            agent: Some(AGENT_NAME.to_owned()),
        })
        .map_err(|e| ToolError::Link(e.to_string()))?;
        let req = HttpRequest::post(format!("{base}{}", paths::CLAIM), 64 * 1024)
            .json(body)
            .timeout(REQUEST_TIMEOUT);
        let claimed: Claimed = match self.send(base, req).await? {
            Ok(b) => Self::json(&b)?,
            Err(r) if r.status == 404 && !r.message.contains("OBS_AGENT_BRIDGE") => {
                return Err(ToolError::Link(format!(
                    "{}. Codes expire after 10 minutes, and a few wrong tries withdraw them",
                    r.message
                )));
            }
            Err(r) => return Err(ToolError::Link(r.message)),
        };
        let read = match self.read_state(base, &claimed.agent_token, None).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(r)) => Err(ToolError::Link(r.message)),
            Err(e) => Err(e),
        };
        if read.is_err() {
            self.end_pairing(base, &claimed.agent_token).await;
        }
        read.map(|v| (claimed.agent_token, v))
    }

    /// `e`, saying that the connection already open, if any, is kept.
    fn kept(&self, e: ToolError) -> ToolError {
        let Some(base) = lock(&self.conn).as_ref().map(|c| c.base.clone()) else {
            return e;
        };
        let note = |why: String| {
            format!(
                "{}. The connection to the app at {base} is kept",
                why.trim_end_matches('.')
            )
        };
        match e {
            ToolError::Link(why) => ToolError::Link(note(why)),
            ToolError::Invalid(why) => ToolError::Invalid(note(why)),
            other => other,
        }
    }

    /// Tells the server at `base` that the pairing `token` names has ended
    /// (best effort: one already ended there is as good as ended now).
    async fn end_pairing(&self, base: &str, token: &str) {
        if let Ok(req) = HttpRequest::new(
            http::Method::DELETE,
            format!("{base}{}", paths::PAIRING),
            4096,
        )
        .bearer(token)
        {
            let _ = self.send(base, req.timeout(REQUEST_TIMEOUT)).await;
        }
    }

    /// Ends the connection, telling the server (best effort). Returns
    /// whether there was one.
    pub async fn disconnect(&self) -> bool {
        let Some(c) = lock(&self.conn).take() else {
            return false;
        };
        if let Some(w) = c.watcher {
            w.abort();
        }
        self.end_pairing(&c.base, &c.token).await;
        *lock(&self.ended) = Some(EndReason::Agent.describe().to_owned());
        self.areas.clear();
        self.changed();
        true
    }

    /// The open connection's base URL and token.
    fn credentials(&self) -> Result<(String, String), ToolError> {
        lock(&self.conn)
            .as_ref()
            .map(|c| (c.base.clone(), c.token.clone()))
            .ok_or(ToolError::NotConnected)
    }

    /// Sends a proposal for the user to decide on in the app; returns its id.
    /// `summary` is what it does, `base_revision` the state revision whose
    /// scene `changes` was made against.
    pub async fn propose(
        &self,
        summary: &str,
        base_revision: u64,
        changes: ChangeSet,
    ) -> Result<u64, ToolError> {
        let (base, token) = self.credentials()?;
        let body = serde_json::to_vec(&ProposalPost {
            v: PROTOCOL_VERSION,
            summary: summary.to_owned(),
            base_revision,
            changes,
        })
        .map_err(|e| ToolError::Link(e.to_string()))?;
        if body.len() > MAX_PROPOSAL_BYTES {
            return Err(ToolError::invalid(format!(
                "the proposal is {} KiB, over the bridge's limit of {} KiB: propose fewer changes at once",
                body.len() / 1024,
                MAX_PROPOSAL_BYTES / 1024
            )));
        }
        let req = HttpRequest::post(format!("{base}{}", paths::PROPOSAL), 64 * 1024)
            .bearer(&token)
            .map_err(|_| ToolError::Link("the pairing token is malformed".into()))?
            .json(body)
            .timeout(REQUEST_TIMEOUT);
        let submitted: ProposalSubmitted = match self.send(&base, req).await? {
            Ok(b) => Self::json(&b)?,
            Err(r) => return Err(ToolError::Link(r.message)),
        };
        // The bridge raised the state's revision: read it, so the
        // proposal's status is there at once.
        let _ = self.refresh().await;
        Ok(submitted.id)
    }

    /// Withdraws proposal `id` while it is pending (a withdrawn one again
    /// is no error).
    pub async fn withdraw(&self, id: u64) -> Result<(), ToolError> {
        let (base, token) = self.credentials()?;
        let req = HttpRequest::new(
            http::Method::DELETE,
            format!("{base}{}?id={id}", paths::PROPOSAL),
            64 * 1024,
        )
        .bearer(&token)
        .map_err(|_| ToolError::Link("the pairing token is malformed".into()))?
        .timeout(REQUEST_TIMEOUT);
        match self.send(&base, req).await? {
            Ok(_) => {
                let _ = self.refresh().await;
                Ok(())
            }
            Err(r) => Err(ToolError::Link(r.message)),
        }
    }

    /// Reads the app's state now.
    pub async fn refresh(&self) -> Result<Snapshot, ToolError> {
        let Some((id, base, token)) = lock(&self.conn)
            .as_ref()
            .map(|c| (c.id, c.base.clone(), c.token.clone()))
        else {
            return Ok(self.snapshot());
        };
        match self.read_state(&base, &token, None).await? {
            Ok(view) => {
                self.apply(id, view);
            }
            Err(r) if r.status == 401 => {
                self.lose(id, "the pairing has ended".into());
            }
            Err(r) => return Err(ToolError::Link(r.message)),
        }
        Ok(self.snapshot())
    }

    /// Fetches the bundle of `area` from the connected server into the
    /// cache, unless it is there or no app is connected. A failure is kept
    /// for the tool that needs the area.
    pub async fn prepare_area(&self, area: &AreaRef) {
        let AreaRef::Osm { bbox } = area else {
            return;
        };
        if self.areas.contains(bbox) {
            return;
        }
        let Some(base) = lock(&self.conn).as_ref().map(|c| c.base.clone()) else {
            return;
        };
        let url = format!(
            "{base}/api/area?bbox={:.6},{:.6},{:.6},{:.6}&v={BUNDLE_VERSION}",
            bbox.south, bbox.west, bbox.north, bbox.east
        );
        let req = HttpRequest::get(url, MAX_BUNDLE_BYTES).timeout(BUNDLE_TIMEOUT);
        let result = match self.send(&base, req).await {
            Ok(Ok(bytes)) => AreaBundle::decode(&bytes).map_err(|e| e.to_string()),
            Ok(Err(r)) => Err(r.message),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(bundle) => self.areas.insert(*bbox, bundle),
            Err(why) => self.areas.fail(*bbox, why),
        }
    }
}

/// Follows connection `id`'s state with the bridge's long poll until it
/// ends, is replaced, or the link is dropped.
async fn follow(link: Weak<AppLink>, id: u64, base: String, token: String, mut since: u64) {
    let mut failures = 0u32;
    loop {
        let Some(l) = link.upgrade() else { return };
        match l.read_state(&base, &token, Some(since)).await {
            Ok(Ok(view)) => {
                failures = 0;
                since = view.revision;
                if !l.apply(id, view) {
                    return;
                }
            }
            Ok(Err(r)) if r.status == 401 => {
                l.lose(id, "the pairing has ended".into());
                return;
            }
            Ok(Err(r)) => {
                failures += 1;
                tracing::warn!(error = %r.message, "reading the app's state failed");
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(error = %e, "reading the app's state failed");
            }
        }
        if failures >= MAX_POLL_FAILURES {
            l.lose(id, "the app's server stopped answering".into());
            return;
        }
        drop(l);
        if failures > 0 {
            tokio::time::sleep(Duration::from_secs(2 * u64::from(failures))).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_http_on_this_computer_is_accepted() {
        for (url, base) in [
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("http://127.0.0.1:8080/", "http://127.0.0.1:8080"),
            ("  http://localhost:3000 ", "http://127.0.0.1:3000"),
            ("http://LOCALHOST", "http://127.0.0.1:80"),
            ("http://127.1.2.3:9", "http://127.1.2.3:9"),
            ("http://[::1]:8080", "http://[::1]:8080"),
        ] {
            assert_eq!(bridge_base(url).unwrap(), base, "{url}");
        }
        for url in [
            "https://127.0.0.1:8080",
            "https://observation.example",
            "http://observation.example:8080",
            "http://192.168.1.2:8080",
            "http://10.0.0.1",
            "http://169.254.169.254/latest",
            "http://0.0.0.0:8080",
            "http://[::]:8080",
            "http://localhost.example:8080",
            "http://user:pw@127.0.0.1:8080",
            "http://127.0.0.1:8080/api/agent",
            "http://127.0.0.1:8080/?x=1",
            "ftp://127.0.0.1",
            "file:///etc/passwd",
            "127.0.0.1:8080",
            "",
        ] {
            assert!(bridge_base(url).is_err(), "{url}");
        }
    }
}
