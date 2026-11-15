//! `/api/agent/...`: the agent bridge (see [`crate::agent`] and
//! [`observation_agent_api`]). Mounted only when `OBS_AGENT_BRIDGE` is on,
//! and every route answers only requests from this computer.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use futures_util::stream::{self, Stream};
use observation_agent_api::{
    BrowserEvent, BrowserEventKind, ClaimRequest, CommandPost, CommandSent, CreatePairing,
    DecisionPost, LONG_POLL_SECS, MAX_COMMAND_BYTES, MAX_PROPOSAL_BYTES, MAX_REASON_CHARS,
    MAX_STATE_BYTES, MAX_SUMMARY_CHARS, PROTOCOL_VERSION, PairingPhase, ProposalPost,
    ProposalStatus, ProposalSubmitted, StateAccepted, StatePost, normalise_code, paths, tidy_line,
    version_ok,
};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{OwnedSemaphorePermit, watch};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

use crate::AppState;
use crate::agent::{AgentBridge, BridgeError, BrowserStream};
use crate::error::ApiError;

/// How often an open event stream ends pairings whose time is up.
const SWEEP_EVERY: Duration = Duration::from_secs(15);

/// The bridge's routes, or none when it is off (then `/api/agent/...`
/// falls through to the JSON 404 like any unknown API path).
pub fn router(state: AppState) -> Router {
    if state.agent().is_none() {
        return Router::new();
    }
    let timeout = state.config().request_timeout;
    let requests = Router::new()
        .route(paths::PAIRINGS, post(create))
        .route(paths::CLAIM, post(claim))
        .route(paths::STATE, get(read_state).put(post_state))
        .route(paths::PAIRING, delete(end))
        .route(paths::PROPOSAL, post(propose).delete(withdraw))
        .route(paths::DECISION, post(decide))
        .merge(
            // A command is small: its own, much lower limit.
            Router::new()
                .route(paths::COMMAND, post(command))
                .layer(RequestBodyLimitLayer::new(MAX_COMMAND_BYTES)),
        )
        .layer(RequestBodyLimitLayer::new(
            MAX_STATE_BYTES.max(MAX_PROPOSAL_BYTES) + 4096,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ));
    Router::new()
        .route(paths::EVENTS, get(events))
        .merge(requests)
        .route_layer(axum::middleware::from_fn(same_computer_only))
        .with_state(state)
}

/// Whether `ip` is this computer (IPv4-mapped forms included).
pub fn is_loopback(ip: IpAddr) -> bool {
    ip.to_canonical().is_loopback()
}

/// Whether a `Host` header names this computer: `localhost`, or a loopback
/// address, with any port.
pub fn is_loopback_host(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inside, port)) if port.is_empty() || port.starts_with(':') => inside,
            _ => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((name, port))
                if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) =>
            {
                name
            }
            Some(_) => return false,
            None => host,
        }
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost" || name.parse::<IpAddr>().is_ok_and(is_loopback)
}

/// The connection's peer: never `X-Forwarded-For`, which a client can set.
fn peer(req_extensions: &axum::http::Extensions) -> Option<IpAddr> {
    req_extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

/// Whether a request comes from this computer and is addressed to it. The
/// `Host` check stops a web page that resolves its own name to 127.0.0.1
/// (DNS rebinding) from talking to the bridge as a same-origin page.
pub fn from_this_computer(
    extensions: &axum::http::Extensions,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    if !peer(extensions).is_some_and(is_loopback) {
        // As for any unknown address: the bridge is invisible from elsewhere.
        return Err(ApiError::NotFound("no such API address on this server"));
    }
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    if !host.is_some_and(is_loopback_host) {
        return Err(ApiError::Forbidden(
            "the agent bridge answers only requests addressed to this computer \
             (127.0.0.1, ::1 or localhost)",
        ));
    }
    Ok(())
}

async fn same_computer_only(req: Request, next: Next) -> Response {
    match from_this_computer(req.extensions(), req.headers()) {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

fn bridge(state: &AppState) -> Result<&Arc<AgentBridge>, ApiError> {
    state
        .agent()
        .ok_or(ApiError::NotFound("no such API address on this server"))
}

fn refused(e: BridgeError) -> ApiError {
    match e {
        BridgeError::Unauthorized => ApiError::Unauthorized(
            "the pairing token is missing or wrong, or the pairing has ended",
        ),
        BridgeError::NoSuchCode => ApiError::NotFound(
            "no pairing has that code: check it in the app, or start a new connection there",
        ),
        BridgeError::Full => ApiError::Conflict(
            "too many agent connections are open on this server; disconnect one or wait for \
             an unused code to expire"
                .into(),
        ),
        BridgeError::TooMany(retry_after_secs) => ApiError::Limited {
            what: "too many requests for this agent connection",
            retry_after_secs,
        },
        BridgeError::NoRandomness => {
            ApiError::Internal("the system's random number source failed".into())
        }
        BridgeError::NoSuchProposal => {
            ApiError::NotFound("this connection has no proposal with that id")
        }
        BridgeError::NotListening => ApiError::Conflict(
            "the app is not listening right now (its page may be reconnecting): try again in a \
             few seconds"
                .into(),
        ),
        BridgeError::Decided(status) => ApiError::Conflict(format!(
            "that proposal is no longer pending: it is {}",
            status_word(status)
        )),
    }
}

/// A proposal's status in words.
fn status_word(status: ProposalStatus) -> &'static str {
    match status {
        ProposalStatus::Pending => "pending",
        ProposalStatus::Accepted => "accepted",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Stale => "stale",
        ProposalStatus::Superseded => "superseded",
        ProposalStatus::Withdrawn => "withdrawn",
        ProposalStatus::Other => "decided",
    }
}

fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// The bearer token, if one was sent.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn token(headers: &HeaderMap) -> Result<&str, ApiError> {
    bearer(headers).ok_or_else(|| refused(BridgeError::Unauthorized))
}

/// Parses a JSON body of protocol version [`PROTOCOL_VERSION`]. A body must
/// say it is JSON: a page on another site cannot send that content type to
/// this computer without the browser asking first, which the bridge never
/// allows.
fn parse<T: serde::de::DeserializeOwned>(
    headers: &HeaderMap,
    body: &[u8],
    version: impl Fn(&T) -> u32,
    expected: &str,
) -> Result<T, ApiError> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
        });
    if !json {
        return Err(ApiError::BadRequest(
            "expected a JSON body (content-type: application/json)".into(),
        ));
    }
    let value: T = serde_json::from_slice(body)
        .map_err(|e| ApiError::BadRequest(format!("expected {expected}: {e}")))?;
    let v = version(&value);
    if !version_ok(v) {
        return Err(ApiError::BadRequest(format!(
            "this server speaks agent bridge protocol version {PROTOCOL_VERSION}, not {v}"
        )));
    }
    Ok(value)
}

/// `POST /api/agent/pairings` (browser).
async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    parse::<CreatePairing>(&headers, &body, |b| b.v, r#"{"v": 1}"#)?;
    let created = bridge.create().map_err(refused)?;
    Ok(no_store(
        (StatusCode::CREATED, Json(created)).into_response(),
    ))
}

/// `POST /api/agent/claim` (agent).
async fn claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    let req = parse::<ClaimRequest>(&headers, &body, |b| b.v, r#"{"v": 1, "code": "..."}"#)?;
    let code = normalise_code(&req.code);
    let claimed = bridge
        .claim(code.as_deref(), req.agent.as_deref())
        .map_err(refused)?;
    Ok(no_store(Json(claimed).into_response()))
}

/// `PUT /api/agent/state` (browser).
async fn post_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    let token = token(&headers)?;
    let mut post = parse::<StatePost>(
        &headers,
        &body,
        |b| b.v,
        r#"{"v": 1, "state": {"scene": {...}}}"#,
    )?;
    if !post.state.scene.is_object() {
        return Err(ApiError::BadRequest(
            "the state's `scene` must be a scene file (a JSON object)".into(),
        ));
    }
    post.state.tidy();
    let revision = bridge.post_state(token, post.state).map_err(refused)?;
    Ok(no_store(
        Json(StateAccepted {
            v: PROTOCOL_VERSION,
            revision,
        })
        .into_response(),
    ))
}

#[derive(Debug, Deserialize)]
struct Since {
    since: Option<u64>,
}

/// `GET /api/agent/state[?since=<revision>]` (agent).
async fn read_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Since>,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    let token = token(&headers)?;
    let view = bridge
        .read_state(token, q.since, Duration::from_secs(LONG_POLL_SECS))
        .await
        .map_err(refused)?;
    Ok(no_store(Json(view).into_response()))
}

/// `POST /api/agent/proposal` (agent): a change set for the user to decide
/// on. The summary must say something and the change set must be well
/// formed; whether it fits the user's scene is the browser's to judge.
async fn propose(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    let token = token(&headers)?;
    let post = parse::<ProposalPost>(
        &headers,
        &body,
        |b| b.v,
        r#"{"v": 1, "summary": "...", "changes": {"changes": [...]}}"#,
    )?;
    let summary = tidy_line(&post.summary, MAX_SUMMARY_CHARS)
        .ok_or_else(|| ApiError::BadRequest("a proposal needs a summary of what it does".into()))?;
    post.changes
        .check()
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let id = bridge
        .propose(token, summary, post.base_revision, post.changes)
        .map_err(refused)?;
    Ok(no_store(
        (
            StatusCode::CREATED,
            Json(ProposalSubmitted {
                v: PROTOCOL_VERSION,
                id,
            }),
        )
            .into_response(),
    ))
}

#[derive(Debug, Deserialize)]
struct ProposalId {
    id: Option<u64>,
}

/// `DELETE /api/agent/proposal?id=<id>` (agent): withdraws a pending
/// proposal. Withdrawing one already withdrawn is no error.
async fn withdraw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ProposalId>,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    let token = token(&headers)?;
    let id =
        q.id.ok_or_else(|| ApiError::BadRequest("say which proposal: ?id=<id>".into()))?;
    match bridge.withdraw(token, id) {
        Ok(()) | Err(BridgeError::Decided(ProposalStatus::Withdrawn)) => {
            Ok(no_store(StatusCode::NO_CONTENT.into_response()))
        }
        Err(e) => Err(refused(e)),
    }
}

/// `POST /api/agent/decision` (browser): the user's decision on a proposal.
async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    let token = token(&headers)?;
    let post = parse::<DecisionPost>(
        &headers,
        &body,
        |b| b.v,
        r#"{"v": 1, "id": 1, "decision": "accepted"}"#,
    )?;
    let reason = post
        .reason
        .as_deref()
        .and_then(|r| tidy_line(r, MAX_REASON_CHARS));
    bridge
        .decide(token, post.id, post.decision, reason)
        .map_err(refused)?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

/// `POST /api/agent/command` (agent): a view command, passed to the app's
/// open event streams once it is checked, and answered `202` with its id.
async fn command(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    let token = token(&headers)?;
    let post = parse::<CommandPost>(
        &headers,
        &body,
        |b| b.v,
        r#"{"v": 1, "command": {"kind": "focus_view", "at": [0, 0]}}"#,
    )?;
    let command = post.command.checked().map_err(ApiError::BadRequest)?;
    let id = bridge.command(token, command).map_err(refused)?;
    Ok(no_store(
        (
            StatusCode::ACCEPTED,
            Json(CommandSent {
                v: PROTOCOL_VERSION,
                id,
            }),
        )
            .into_response(),
    ))
}

/// `DELETE /api/agent/pairing` (either side).
async fn end(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    bridge.end(token(&headers)?).map_err(refused)?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    token: Option<String>,
}

/// `GET /api/agent/events?token=<browser token>` (browser): the pairing's
/// [`BrowserEvent`]s. The first says where it stands, then the pending
/// proposal follows if there is one, and the agent's view commands as they
/// arrive; after an `ended` event the stream closes. Not subject to the request timeout; counts against the
/// event-stream cap shared with `/api/regions/events`.
async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    let bridge = bridge(&state)?;
    let token = q
        .token
        .as_deref()
        .or_else(|| bearer(&headers))
        .ok_or_else(|| refused(BridgeError::Unauthorized))?;
    let stream = bridge.open_stream(token).map_err(refused)?;
    let permit = Arc::clone(state.event_streams())
        .try_acquire_owned()
        .map_err(|_| ApiError::RateLimited {
            retry_after_secs: 30,
        })?;
    let events = event_stream(stream, Arc::clone(bridge), state.shutdown_signal(), permit);
    Ok(no_store(
        Sse::new(events)
            .keep_alive(KeepAlive::default())
            .into_response(),
    ))
}

fn event(kind: BrowserEventKind) -> Event {
    Event::default()
        .event(paths::EVENT)
        .json_data(BrowserEvent::new(kind))
        .unwrap_or_else(|_| Event::default().comment("unencodable"))
}

struct Follow {
    stream: BrowserStream,
    bridge: Arc<AgentBridge>,
    shutdown: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
    /// The phase and agent last sent.
    sent: Option<(PairingPhase, Option<String>)>,
    /// The id of the pending proposal last sent.
    sent_proposal: Option<u64>,
    /// The pairing's command channel has closed (it ended).
    commands_closed: bool,
    done: bool,
}

fn event_stream(
    stream: BrowserStream,
    bridge: Arc<AgentBridge>,
    shutdown: watch::Receiver<bool>,
    permit: OwnedSemaphorePermit,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let follow = Follow {
        stream,
        bridge,
        shutdown,
        _permit: permit,
        sent: None,
        sent_proposal: None,
        commands_closed: false,
        done: false,
    };
    stream::unfold(follow, |mut f| async move {
        if f.done {
            return None;
        }
        loop {
            let now = f.stream.signal.borrow_and_update().clone();
            if let Some(reason) = now.ended {
                f.done = true;
                return Some((Ok(event(BrowserEventKind::Ended { reason })), f));
            }
            let status = (now.phase, now.agent.clone());
            if f.sent.as_ref() != Some(&status) {
                f.sent = Some(status);
                let kind = BrowserEventKind::Status {
                    phase: now.phase,
                    agent: now.agent,
                };
                return Some((Ok(event(kind)), f));
            }
            // The proposal pending now, if it is not the one sent last; or
            // that the one sent was withdrawn. One decided by the browser
            // needs no event.
            let pending = now.proposal.as_ref().map(|p| p.id);
            if pending != f.sent_proposal {
                let previous = std::mem::replace(&mut f.sent_proposal, pending);
                match (now.proposal, previous) {
                    (Some(p), _) => {
                        let kind = BrowserEventKind::Proposal {
                            proposal: (*p).clone(),
                        };
                        return Some((Ok(event(kind)), f));
                    }
                    (None, Some(id)) if now.withdrawn == Some(id) => {
                        let kind = BrowserEventKind::ProposalWithdrawn { id };
                        return Some((Ok(event(kind)), f));
                    }
                    _ => {}
                }
            }
            if *f.shutdown.borrow() {
                return None;
            }
            tokio::select! {
                biased;
                changed = f.stream.signal.changed() => {
                    // The sender goes once the pairing has ended, after it
                    // said why; anything else is not worth following.
                    if changed.is_err() && f.stream.signal.borrow().ended.is_none() {
                        return None;
                    }
                }
                received = f.stream.commands.recv(), if !f.commands_closed => match received {
                    Ok(sent) => {
                        let (id, command) = (*sent).clone();
                        return Some((Ok(event(BrowserEventKind::Command { id, command })), f));
                    }
                    // Too far behind: the oldest commands are skipped.
                    Err(RecvError::Lagged(_)) => {}
                    // The pairing ended; the signal says so.
                    Err(RecvError::Closed) => f.commands_closed = true,
                },
                _ = f.shutdown.changed() => return None,
                () = tokio::time::sleep(SWEEP_EVERY) => f.bridge.sweep(),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_computer_counts_as_loopback() {
        for ip in ["127.0.0.1", "127.1.2.3", "::1", "::ffff:127.0.0.1"] {
            assert!(is_loopback(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "10.0.0.1",
            "192.168.1.2",
            "::",
            "0.0.0.0",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_loopback(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn host_headers_must_name_this_computer() {
        for host in [
            "localhost",
            "localhost:8080",
            "LOCALHOST.:80",
            "127.0.0.1",
            "127.0.0.1:8080",
            "[::1]",
            "[::1]:8080",
        ] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in [
            "",
            "evil.example",
            "evil.example:8080",
            "127.0.0.1.evil.example",
            "localhost.evil.example",
            "192.168.1.2:8080",
            "[::1]x",
            "::1",
            "localhost:80x",
        ] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }
}
