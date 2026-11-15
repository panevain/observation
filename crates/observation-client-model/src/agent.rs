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
//! second, and nothing goes out while nothing changes). The state, and its
//! JSON, are built only when their [`StateKey`] or the pinned point changed.
//! A refused post is tried again after [`RETRY_AFTER`] seconds. The event
//! stream reconnects by itself when the connection drops; when the server
//! closes it or says the pairing ended, the link ends. Pairings do not
//! survive a reload: the token is held in memory only.
//!
//! Replies come back through the [`Http`] port's callbacks into a shared
//! inbox, so the link needs no channel of the client's; each carries the
//! pairing it belongs to, and replies for an older pairing are dropped.
//!
//! The link also holds the agent's proposal, if one came (see
//! [`crate::proposal`]): a new one replaces the one shown, which the agent
//! superseded. Proposal ids rise within a pairing, and the server sends the
//! pending proposal again to every event stream it opens, so after a
//! reconnect one with an id already seen is ignored: the card keeps its
//! state, and nothing is toasted or decided twice.
//! [`AgentLink::follow_proposal`] keeps it checked against the document and
//! tells the agent, once, when it goes stale; Accept and Reject send the
//! user's decision.
//!
//! A decision is sent at most once per proposal, and goes out ahead of the
//! state: while one is on its way no state is posted, so the agent never
//! reads a scene with a proposal applied before it hears that it was
//! accepted, and the two share the pairing's post budget without the state
//! starving the decision. A decision that gets no answer, a 429, a 408 or a
//! 5xx is tried again, after the server's `Retry-After` when it gives one,
//! else after [`DECISION_RETRY_FIRST`] seconds, doubling up to
//! [`DECISION_RETRY_MAX`]; the card says it is telling the agent meanwhile.
//! It stops when it is delivered, when the server answers 409 (decided
//! already; after a retry that means an earlier try got through) or any
//! other refusal, which the card then shows, or when the pairing ends.

use crate::document::{Document, Models};
use crate::platform::{Failure, Http, HttpRequest, Reply, StatusReply, StreamEvent, StreamStop};
use crate::proposal::{Accepted, PendingProposal, stale_reason};
use observation_agent_api::{
    BrowserEvent, BrowserEventKind, CreatePairing, Decision, DecisionPost, PROTOCOL_VERSION,
    PairingCreated, PairingPhase, PinnedPoint, Proposal, SharedState, StatePost, display_code,
    paths, version_ok,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

/// Seconds between checks of whether the state changed.
pub const POST_INTERVAL: f32 = 0.5;
/// Seconds before a refused post is tried again.
pub const RETRY_AFTER: f32 = 5.0;
/// Seconds before an undelivered decision is first tried again, when the
/// server does not say; each further try waits twice as long as the last.
pub const DECISION_RETRY_FIRST: f32 = 1.0;
/// The longest wait between tries of a decision, seconds, unless the
/// server's `Retry-After` asks for longer.
pub const DECISION_RETRY_MAX: f32 = 30.0;
/// The longest `Retry-After` honoured for a decision, seconds.
const DECISION_RETRY_AFTER_MAX: f32 = 300.0;

/// What the shared state is built from, cheap to compare:
/// [`AgentLink::update`] builds the state, and its JSON, only when this or
/// the pinned point changed since it last did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateKey {
    /// The document's revision, which every change of it (its area
    /// included) bumps.
    pub revision: u64,
    /// Changes whenever the camera models may have (the client passes its
    /// `Models` resource's change tick): the custom models are part of the
    /// scene.
    pub models: u64,
    pub scene_name: String,
    pub selected: Option<u32>,
}

/// A decision on its way to the server.
#[derive(Debug)]
struct Outgoing {
    /// The proposal decided.
    id: u64,
    body: Option<String>,
    /// Tries sent so far.
    tries: u32,
    /// Seconds before the next try.
    wait: f32,
    /// A try is waiting for its answer.
    in_flight: bool,
}

/// How long to wait before trying an undelivered decision again, after
/// `tries` tries and this failure; `None` when trying again cannot help.
fn retry_wait(failure: &Failure, tries: u32) -> Option<f32> {
    let worth_it = match failure.status {
        // No answer at all: offline, or the server is restarting.
        None => true,
        Some(status) => status == 408 || status == 429 || (500..=599).contains(&status),
    };
    if !worth_it {
        return None;
    }
    let doublings = tries.saturating_sub(1).min(5);
    let backoff = (DECISION_RETRY_FIRST * f32::from(1u8 << doublings)).min(DECISION_RETRY_MAX);
    Some(failure.retry_after_s.map_or(backoff, |s| {
        (s as f32).clamp(DECISION_RETRY_FIRST, DECISION_RETRY_AFTER_MAX)
    }))
}

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
    /// The reply to a decision on this proposal.
    Decided(u64, StatusReply),
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
    /// What the state was last built from, with the pinned point's number.
    built_from: Option<(StateKey, u64)>,
    posting: bool,
    timer: f32,
    /// Seconds to wait before the next post (after a refusal).
    backoff: f32,
    /// The last post failed, with why.
    pub post_error: Option<String>,
    pinned: Option<PinnedPoint>,
    pin_seq: u64,
    /// The proposal shown on the card, if any.
    proposal: Option<PendingProposal>,
    /// A proposal that arrived and is not on the card yet.
    arrived: Option<Proposal>,
    /// The highest proposal id taken in this pairing (0 for none).
    newest_proposal: u64,
    /// Decisions to deliver, oldest first; only the first is ever sent.
    outbox: VecDeque<Outgoing>,
    /// The proposals a decision was sent for in this pairing.
    decided: Vec<u64>,
    /// The proposal card is open.
    pub card_open: bool,
    /// What to tell the user in a toast.
    notices: Vec<String>,
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
            built_from: None,
            posting: false,
            timer: 0.0,
            backoff: 0.0,
            post_error: None,
            pinned: None,
            pin_seq: 0,
            proposal: None,
            arrived: None,
            newest_proposal: 0,
            outbox: VecDeque::new(),
            decided: Vec::new(),
            card_open: false,
            notices: Vec::new(),
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
    // Through the JSON text, so numbers read exactly as in an exported file
    // (an `f32` straight into a `Value` would carry its binary noise).
    let scene = serde_json::from_str(&serde_json::to_string(&file).ok()?).ok()?;
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

    /// The agent's name, while connected (if it gave one).
    pub fn agent_name(&self) -> Option<&str> {
        match &self.phase {
            LinkPhase::Connected { agent } => agent.as_deref(),
            _ => None,
        }
    }

    /// The proposal on the card, if any.
    pub fn proposal(&self) -> Option<&PendingProposal> {
        self.proposal.as_ref()
    }

    /// The same, to edit the reason being typed.
    pub fn proposal_mut(&mut self) -> Option<&mut PendingProposal> {
        self.proposal.as_mut()
    }

    /// What to tell the user in toasts since the last call.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Puts a proposal that arrived on the card, then checks the card's
    /// proposal against the document; when that makes it stale, tells the
    /// agent. Run after every frame's edits.
    pub fn follow_proposal(&mut self, http: &dyn Http, doc: &Document, models: &Models) {
        // One already taken (sent again to a reopened stream) stays as it is.
        if let Some(p) = self.arrived.take().filter(|p| p.id > self.newest_proposal) {
            self.newest_proposal = p.id;
            let p = PendingProposal::new(p, models);
            self.notices
                .push(format!("The agent proposes: {}", p.summary));
            self.proposal = Some(p);
            self.card_open = true;
        }
        let Some(p) = self.proposal.as_mut() else {
            return;
        };
        if let Some(what) = p.follow(doc, models) {
            let id = p.id;
            self.send_decision(http, id, Decision::Stale, Some(stale_reason(&what)));
        }
    }

    /// Accept: applies the proposal to the document as one edit (the history
    /// records it as one step) and tells the agent. An error says why
    /// nothing was applied.
    pub fn accept(
        &mut self,
        http: &dyn Http,
        doc: &mut Document,
        models: &mut Models,
        now_unix: u64,
    ) -> Result<(), String> {
        let p = self.proposal.as_mut().ok_or("There is no proposal.")?;
        let id = p.id;
        match p.accept(doc, models, now_unix) {
            Accepted::Applied => {
                self.send_decision(http, id, Decision::Accepted, None);
                Ok(())
            }
            Accepted::Stale(what) => {
                self.send_decision(http, id, Decision::Stale, Some(stale_reason(&what)));
                Err("The proposal is stale: the scene changed under it.".into())
            }
            Accepted::Refused(why) => Err(format!("The proposal could not be applied: {why}")),
        }
    }

    /// Reject: tells the agent, with the reason typed on the card.
    pub fn reject(&mut self, http: &dyn Http) {
        let Some(p) = self.proposal.as_mut().filter(|p| p.standing.is_open()) else {
            return;
        };
        let (id, reason) = (p.id, p.reject());
        self.send_decision(http, id, Decision::Rejected, reason);
    }

    /// Closes the card; a decided or closed proposal is forgotten.
    pub fn close_card(&mut self) {
        self.card_open = false;
        if self
            .proposal
            .as_ref()
            .is_some_and(|p| !p.standing.is_open())
        {
            self.proposal = None;
        }
    }

    /// Queues the user's decision on proposal `id` and sends it if nothing
    /// is ahead of it. A proposal is decided once: a second decision for it
    /// is dropped.
    fn send_decision(
        &mut self,
        http: &dyn Http,
        id: u64,
        decision: Decision,
        reason: Option<String>,
    ) {
        if self.decided.contains(&id) {
            return;
        }
        if self.token.is_none() {
            if let Some(p) = self.proposal_with(id) {
                p.decision_error = Some("the connection to the agent has ended".into());
            }
            return;
        }
        self.decided.push(id);
        let body = serde_json::to_string(&DecisionPost {
            v: PROTOCOL_VERSION,
            id,
            decision,
            reason,
        })
        .ok();
        self.outbox.push_back(Outgoing {
            id,
            body,
            tries: 0,
            wait: 0.0,
            in_flight: false,
        });
        self.send_next_decision(http, 0.0);
    }

    /// Sends the first queued decision once its wait is over (`dt` seconds
    /// have passed since the last call), unless a try of it is in flight.
    fn send_next_decision(&mut self, http: &dyn Http, dt: f32) {
        let Some(token) = self.token.clone() else {
            return;
        };
        let Some(next) = self.outbox.front_mut().filter(|o| !o.in_flight) else {
            return;
        };
        next.wait -= dt;
        if next.wait > 0.0 {
            return;
        }
        next.in_flight = true;
        next.tries += 1;
        let (id, body) = (next.id, next.body.clone());
        let (inbox, generation) = (Arc::clone(&self.inbox), self.generation);
        http.fetch_status(
            HttpRequest {
                url: paths::DECISION.to_owned(),
                method: "POST",
                body,
                bearer: Some(token),
            },
            Box::new(move |reply| push(&inbox, generation, Incoming::Decided(id, reply))),
        );
    }

    /// The reply to a try of the first queued decision: delivered, to be
    /// tried again, or given up.
    fn decided_reply(&mut self, id: u64, reply: StatusReply) {
        let Some(sent) = self
            .outbox
            .front_mut()
            .filter(|o| o.id == id && o.in_flight)
        else {
            return;
        };
        sent.in_flight = false;
        let tries = sent.tries;
        let failure = match reply {
            Ok(_) => None,
            Err(failure) => match retry_wait(&failure, tries) {
                Some(wait) => {
                    sent.wait = wait;
                    if let Some(p) = self.proposal_with(id) {
                        p.decision_retrying = true;
                    }
                    return;
                }
                None => Some(failure),
            },
        };
        self.outbox.pop_front();
        // A 409 after a retry: an earlier try reached the server, and only
        // its answer was lost.
        let error = failure
            .filter(|f| f.status != Some(409) || tries == 1)
            .map(|f| f.message);
        if let Some(p) = self.proposal_with(id) {
            p.decision_retrying = false;
            p.decision_error = error;
        }
    }

    /// The proposal on the card, if it is proposal `id`.
    fn proposal_with(&mut self, id: u64) -> Option<&mut PendingProposal> {
        self.proposal.as_mut().filter(|p| p.id == id)
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
        self.built_from = None;
        self.posting = false;
        self.timer = 0.0;
        self.backoff = 0.0;
        self.post_error = None;
        self.pinned = None;
        self.pin_seq = 0;
        self.arrived = None;
        self.newest_proposal = 0;
        self.decided.clear();
        for undelivered in std::mem::take(&mut self.outbox) {
            if let Some(p) = self.proposal_with(undelivered.id) {
                p.decision_retrying = false;
                p.decision_error =
                    Some("the connection to the agent ended before it was told".into());
            }
        }
        if let Some(p) = &mut self.proposal {
            p.close("The connection to the agent ended before you decided.");
        }
    }

    fn end(&mut self, why: impl Into<String>) {
        self.reset();
        self.phase = LinkPhase::Ended { why: why.into() };
    }

    /// Takes in what arrived, sends a decision that is due, then posts the
    /// state if it is time and it changed (never while a decision is on its
    /// way). `dt` is the frame's wall-clock seconds; `key` says what the
    /// state is built from, and `state` builds it, given the pinned point
    /// (see [`shared_state`]), only when `key` or the pinned point changed.
    pub fn update(
        &mut self,
        http: &dyn Http,
        dt: f32,
        key: StateKey,
        state: impl FnOnce(Option<PinnedPoint>) -> Option<SharedState>,
    ) {
        self.take_in(http);
        if !matches!(
            self.phase,
            LinkPhase::Waiting { .. } | LinkPhase::Connected { .. }
        ) {
            return;
        }
        self.send_next_decision(http, dt);
        if self.posting || !self.outbox.is_empty() {
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
        let built_from = (key, self.pin_seq);
        if self.built_from.as_ref() == Some(&built_from) {
            return;
        }
        self.built_from = Some(built_from);
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
                            self.built_from = None;
                            self.backoff = RETRY_AFTER;
                            self.post_error = Some(why);
                        }
                    }
                }
                Incoming::Stream(event) => self.stream(event),
                Incoming::Decided(id, reply) => self.decided_reply(id, reply),
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
                    BrowserEventKind::Proposal { proposal } => {
                        // The card takes it after the frame's edits, with
                        // the models' names (see `follow_proposal`). Ids
                        // rise, so an older one is never wanted.
                        if self.arrived.as_ref().is_none_or(|a| a.id < proposal.id) {
                            self.arrived = Some(proposal);
                        }
                    }
                    BrowserEventKind::ProposalWithdrawn { id } => {
                        if self.arrived.as_ref().is_some_and(|p| p.id == id) {
                            self.arrived = None;
                        }
                        if let Some(p) = self
                            .proposal
                            .as_mut()
                            .filter(|p| p.id == id && p.standing.is_open())
                        {
                            p.close("The agent withdrew this proposal.");
                            self.notices
                                .push("The agent withdrew its proposal".to_owned());
                        }
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
