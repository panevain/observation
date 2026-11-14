//! The link to a local agent through the server's agent bridge (see
//! [`observation_agent_api`]): what Connect agent, Disconnect and Share with
//! agent do, and when the app's state is posted.
//!
//! ```text
//! Idle ──connect──▶ Starting ──created──▶ Waiting (code shown) ──claimed──▶ Connected
//!   ▲                  │ failed               │ ended                         │ ended
//!   └──disconnect──────┴──────────────────────┴──▶ Ended (why) ◀──────────────┘
//! ```
//!
//! [`AgentLink::update`] runs every frame: it takes in the replies and
//! events that arrived, and every [`POST_INTERVAL`] seconds, while a pairing
//! is open, posts the state if its JSON differs from what was posted last
//! (so a change of scene, selection or pinned point goes out within half a
//! second, and nothing goes out while nothing changes). A refused post is
//! tried again after [`RETRY_AFTER`] seconds. The event stream reconnects by
//! itself when the connection drops; when the server closes it or says the
//! pairing ended, the link ends. Pairings do not survive a reload: the token
//! is held in memory only.
//!
//! Replies come back through the [`Http`] port's callbacks into a shared
//! inbox, so the link needs no channel of the client's; each carries the
//! pairing it belongs to, and replies for an older pairing are dropped.

use crate::document::{Document, Models};
use crate::platform::{Http, HttpRequest, Reply, StreamEvent, StreamStop};
use observation_agent_api::{
    BrowserEvent, BrowserEventKind, CreatePairing, PROTOCOL_VERSION, PairingCreated, PairingPhase,
    PinnedPoint, SharedState, StatePost, display_code, paths, version_ok,
};
use std::sync::{Arc, Mutex, PoisonError};

/// Seconds between checks of whether the state changed.
pub const POST_INTERVAL: f32 = 0.5;
/// Seconds before a refused post is tried again.
pub const RETRY_AFTER: f32 = 5.0;

/// Where the link stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPhase {
    /// No pairing.
    Idle,
    /// A pairing was asked for.
    Starting,
    /// The pairing waits for the agent: show the code.
    Waiting { code: String },
    /// An agent holds the pairing (with its name, if it gave one).
    Connected { agent: Option<String> },
    /// The pairing ended or could not start, with why (for the dialog).
    Ended { why: String },
}

#[derive(Debug)]
enum Incoming {
    Created(Reply),
    Posted(Reply),
    Stream(StreamEvent),
}

type Inbox = Arc<Mutex<Vec<(u64, Incoming)>>>;

/// The link to a local agent, held by the client as a resource.
#[derive(Debug)]
pub struct AgentLink {
    /// The server offers the bridge (from `/api/config`).
    pub available: bool,
    /// The Connect agent dialog is open.
    pub dialog_open: bool,
    phase: LinkPhase,
    /// Counts pairings, so replies for an older one are dropped.
    generation: u64,
    token: Option<String>,
    stop: Option<StreamStop>,
    inbox: Inbox,
    /// The state JSON posted last (or being posted).
    last_posted: Option<String>,
    posting: bool,
    timer: f32,
    /// Seconds to wait before the next post (after a refusal).
    backoff: f32,
    /// The last post failed, with why.
    pub post_error: Option<String>,
    pinned: Option<PinnedPoint>,
    pin_seq: u64,
}

impl Default for AgentLink {
    fn default() -> Self {
        Self {
            available: false,
            dialog_open: false,
            phase: LinkPhase::Idle,
            generation: 0,
            token: None,
            stop: None,
            inbox: Arc::default(),
            last_posted: None,
            posting: false,
            timer: 0.0,
            backoff: 0.0,
            post_error: None,
            pinned: None,
            pin_seq: 0,
        }
    }
}

/// The state the app shares: the open scene as it would be exported, its
/// name, the loaded area, the selection and the pinned point. `None` until
/// an area has loaded (there is no scene file before).
pub fn shared_state(
    doc: &Document,
    models: &Models,
    scene_name: &str,
    selected: Option<u32>,
    pinned: Option<PinnedPoint>,
) -> Option<SharedState> {
    let file = doc.to_file(models)?;
    let scene = serde_json::from_str(&file.to_json()).ok()?;
    let mut state = SharedState {
        scene,
        scene_name: Some(scene_name.to_owned()),
        area: Some(file.area),
        selection: selected.into_iter().collect(),
        pinned,
    };
    state.tidy();
    Some(state)
}

fn push(inbox: &Inbox, generation: u64, item: Incoming) {
    inbox
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((generation, item));
}

impl AgentLink {
    pub fn phase(&self) -> &LinkPhase {
        &self.phase
    }

    /// The code to show, formatted for reading aloud, while waiting.
    pub fn code(&self) -> Option<String> {
        match &self.phase {
            LinkPhase::Waiting { code } => Some(display_code(code)),
            _ => None,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.phase, LinkPhase::Connected { .. })
    }

    /// Whether a pairing is open (waiting or connected).
    pub fn is_open(&self) -> bool {
        matches!(
            self.phase,
            LinkPhase::Starting | LinkPhase::Waiting { .. } | LinkPhase::Connected { .. }
        )
    }

    /// The point shared last in this pairing.
    pub fn pinned(&self) -> Option<PinnedPoint> {
        self.pinned
    }

    /// Asks for a new pairing (Connect agent), unless one is open.
    pub fn connect(&mut self, http: &dyn Http) {
        if self.is_open() || !self.available {
            return;
        }
        self.reset();
        self.phase = LinkPhase::Starting;
        let (inbox, generation) = (Arc::clone(&self.inbox), self.generation);
        let body = serde_json::to_string(&CreatePairing::default()).ok();
        http.fetch(
            HttpRequest {
                url: paths::PAIRINGS.to_owned(),
                method: "POST",
                body,
                bearer: None,
            },
            Box::new(move |reply| push(&inbox, generation, Incoming::Created(reply))),
        );
    }

    /// Ends the pairing (Disconnect): tells the server (best effort) and
    /// stops following it.
    pub fn disconnect(&mut self, http: &dyn Http) {
        if let Some(token) = self.token.take() {
            http.fetch(
                HttpRequest {
                    url: paths::PAIRING.to_owned(),
                    method: "DELETE",
                    body: None,
                    bearer: Some(token),
                },
                Box::new(|_| {}),
            );
        }
        self.reset();
        self.phase = LinkPhase::Idle;
    }

    /// Shares the point at world `[x, z]` (with its latitude and longitude)
    /// with the agent; false unless a pairing is open.
    pub fn share_point(&mut self, x: f32, z: f32, lat_lon: Option<(f64, f64)>) -> bool {
        if !self.is_open() || !x.is_finite() || !z.is_finite() {
            return false;
        }
        self.pin_seq += 1;
        self.pinned = Some(PinnedPoint {
            x,
            z,
            lat: lat_lon.map(|l| l.0),
            lon: lat_lon.map(|l| l.1),
            seq: self.pin_seq,
        });
        true
    }

    /// Forgets the pairing on this side and starts a new generation.
    fn reset(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.stop();
        }
        self.token = None;
        self.generation += 1;
        self.last_posted = None;
        self.posting = false;
        self.timer = 0.0;
        self.backoff = 0.0;
        self.post_error = None;
        self.pinned = None;
        self.pin_seq = 0;
    }

    fn end(&mut self, why: impl Into<String>) {
        self.reset();
        self.phase = LinkPhase::Ended { why: why.into() };
    }

    /// Takes in what arrived, then posts the state if it is time and it
    /// changed. `dt` is the frame's wall-clock seconds; `state` builds what
    /// the app shares, given the pinned point (see [`shared_state`]).
    pub fn update(
        &mut self,
        http: &dyn Http,
        dt: f32,
        state: impl FnOnce(Option<PinnedPoint>) -> Option<SharedState>,
    ) {
        self.take_in(http);
        if !matches!(
            self.phase,
            LinkPhase::Waiting { .. } | LinkPhase::Connected { .. }
        ) || self.posting
        {
            return;
        }
        self.timer += dt;
        if self.timer < POST_INTERVAL.max(self.backoff) {
            return;
        }
        self.timer = 0.0;
        let Some(token) = self.token.clone() else {
            return;
        };
        let Some(state) = state(self.pinned) else {
            return;
        };
        let post = StatePost {
            v: PROTOCOL_VERSION,
            state,
        };
        let Ok(json) = serde_json::to_string(&post) else {
            return;
        };
        if self.last_posted.as_deref() == Some(json.as_str()) {
            return;
        }
        self.last_posted = Some(json.clone());
        self.posting = true;
        let (inbox, generation) = (Arc::clone(&self.inbox), self.generation);
        http.fetch(
            HttpRequest {
                url: paths::STATE.to_owned(),
                method: "PUT",
                body: Some(json),
                bearer: Some(token),
            },
            Box::new(move |reply| push(&inbox, generation, Incoming::Posted(reply))),
        );
    }

    fn take_in(&mut self, http: &dyn Http) {
        let arrived =
            std::mem::take(&mut *self.inbox.lock().unwrap_or_else(PoisonError::into_inner));
        for (generation, item) in arrived {
            if generation != self.generation {
                continue;
            }
            match item {
                Incoming::Created(reply) => self.created(http, reply),
                Incoming::Posted(reply) => {
                    self.posting = false;
                    match reply {
                        Ok(_) => {
                            self.backoff = 0.0;
                            self.post_error = None;
                        }
                        Err(why) => {
                            // Posted again once the wait is over.
                            self.last_posted = None;
                            self.backoff = RETRY_AFTER;
                            self.post_error = Some(why);
                        }
                    }
                }
                Incoming::Stream(event) => self.stream(event),
            }
        }
    }

    fn created(&mut self, http: &dyn Http, reply: Reply) {
        if self.phase != LinkPhase::Starting {
            return;
        }
        let created = reply.and_then(|body| {
            serde_json::from_slice::<PairingCreated>(&body)
                .map_err(|e| format!("the server's answer could not be read: {e}"))
                .and_then(|c| {
                    if version_ok(c.v) {
                        Ok(c)
                    } else {
                        Err(
                            "the server speaks another version of the bridge; reload the page"
                                .into(),
                        )
                    }
                })
        });
        let created = match created {
            Ok(c) => c,
            Err(why) => return self.end(format!("Could not start the connection: {why}")),
        };
        let stop = StreamStop::default();
        self.stop = Some(stop.clone());
        self.token = Some(created.browser_token.clone());
        self.phase = LinkPhase::Waiting { code: created.code };
        let (inbox, generation) = (Arc::clone(&self.inbox), self.generation);
        let url = format!(
            "{}?{}={}",
            paths::EVENTS,
            paths::TOKEN_PARAM,
            created.browser_token
        );
        http.follow(
            &url,
            paths::EVENT,
            stop,
            Box::new(move |event| push(&inbox, generation, Incoming::Stream(event))),
        );
    }

    fn stream(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Data(bytes) => {
                let Ok(event) = serde_json::from_slice::<BrowserEvent>(&bytes) else {
                    return;
                };
                match event.kind {
                    BrowserEventKind::Status { phase, agent } => {
                        self.phase = match (phase, &self.phase) {
                            (PairingPhase::Claimed, _) => LinkPhase::Connected { agent },
                            (PairingPhase::Waiting, LinkPhase::Waiting { code }) => {
                                LinkPhase::Waiting { code: code.clone() }
                            }
                            (PairingPhase::Waiting, other) => other.clone(),
                        };
                    }
                    BrowserEventKind::Ended { reason } => {
                        let why = reason.describe();
                        let mut words = why.chars();
                        let why = words
                            .next()
                            .map(|c| c.to_uppercase().chain(words).collect::<String>())
                            .unwrap_or_default();
                        self.end(format!("Disconnected: {why}."));
                    }
                    // A kind this build does not know (a later feature).
                    BrowserEventKind::Unknown => {}
                }
            }
            StreamEvent::Dropped => {}
            StreamEvent::Closed(why) => {
                self.end(format!("The connection to the server was lost ({why})."));
            }
        }
    }
}

#[cfg(test)]
mod tests;
