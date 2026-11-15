//! The agent bridge's pairings: one open app and one local agent, relayed
//! through this server (see [`observation_agent_api`] for the protocol).
//!
//! Everything is held in memory and nothing is written anywhere: a restart
//! ends every pairing. Expiry is lazy: every operation first ends the
//! pairings whose time is up ([`AgentBridge::sweep`]), and the long polls and
//! event streams wake often enough to run it. Elapsed time is
//! `tokio::time::Instant`, so paused-time tests drive it.
//!
//! Secrets (the two tokens and the code) are compared in constant time,
//! looked up by comparing against every pairing (there are at most
//! [`MAX_PAIRINGS`]), and never logged.
//!
//! A pairing also holds the agent's pending proposal, if any, and the
//! outcomes of its latest proposals (see [`observation_agent_api::proposal`]).
//! The pending proposal is part of what its followers watch, so the event
//! streams send it to the browser without a queue: a stream sends the one
//! pending now whenever it differs from the one it sent last.
//!
//! The agent's view commands are not state: they go out once, through a
//! broadcast channel each open stream subscribes to, and are never stored
//! or sent again to a stream opened later. They have their own rate limit
//! ([`COMMANDS_PER_SECOND`], bursts of [`COMMAND_BURST`]), so a busy agent
//! cannot use up the posts the browser's state needs.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use observation_agent_api::{
    AgentCommand, BROWSER_GONE_SECS, CODE_ALPHABET, CODE_LEN, CODE_TTL_SECS, COMMAND_BURST,
    COMMANDS_PER_SECOND, Claimed, Decision, EndReason, IDLE_TIMEOUT_SECS, LONG_POLL_SECS,
    MAX_AGENT_NAME_CHARS, MAX_FAILED_CLAIMS, MAX_OUTCOMES, MAX_PAIRINGS, PROTOCOL_VERSION,
    PairingCreated, PairingPhase, Proposal, ProposalOutcome, ProposalStatus, SharedState,
    StateView, tidy_text,
};
use observation_core::scene::changes::ChangeSet;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

use crate::metrics::AGENT_PAIRINGS;

/// Most event streams one pairing may have open (a browser reconnecting
/// briefly holds two).
pub const MAX_STREAMS_PER_PAIRING: usize = 4;
/// Most long polls one pairing may have waiting.
pub const MAX_POLLS_PER_PAIRING: usize = 4;
/// Most posts one pairing may make in [`POST_WINDOW`]: the browser's state
/// posts and decisions and the agent's proposals together. The app posts
/// its state at most every half second.
pub const MAX_POSTS_PER_WINDOW: usize = 40;
/// The window [`MAX_POSTS_PER_WINDOW`] counts over.
pub const POST_WINDOW: Duration = Duration::from_secs(10);
/// How long the agent of an ended pairing is still told why it ended,
/// rather than that its token is unknown: its long poll may have been
/// between requests when the pairing ended.
pub const ENDED_MEMORY: Duration = Duration::from_secs(60);
/// At most this many ended pairings are remembered for [`ENDED_MEMORY`].
pub const MAX_ENDED_REMEMBERED: usize = 32;
/// Commands a stream may fall behind by before it skips the oldest: more
/// than a whole burst.
const COMMAND_BACKLOG: usize = 2 * COMMAND_BURST as usize;

/// A view command on its way to the browser's streams, with its id.
pub type SentCommand = Arc<(u64, AgentCommand)>;

/// What a pairing's followers (its event streams and long polls) watch.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub phase: PairingPhase,
    pub agent: Option<String>,
    pub revision: u64,
    /// Set as the pairing ends; its sender is dropped right after.
    pub ended: Option<EndReason>,
    /// The agent's pending proposal.
    pub proposal: Option<Arc<Proposal>>,
    /// The id of the proposal the agent withdrew last.
    pub withdrawn: Option<u64>,
}

/// Why a bridge operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeError {
    /// No pairing has this token (wrong, or the pairing has ended).
    Unauthorized,
    /// No unclaimed pairing has this code.
    NoSuchCode,
    /// [`MAX_PAIRINGS`] are open already.
    Full,
    /// Too many streams or polls are open, or posts were made, for one
    /// pairing; try again after this many seconds.
    TooMany(u64),
    /// The system's random number source failed.
    NoRandomness,
    /// The pairing has no proposal with this id.
    NoSuchProposal,
    /// The browser has no event stream open, so a command would be lost.
    NotListening,
    /// The user has decided the proposal already (or, for a withdrawal,
    /// it is no longer pending): its status.
    Decided(ProposalStatus),
}

#[derive(Debug)]
struct Pairing {
    /// For logs: never a secret.
    id: u64,
    /// `None` once claimed.
    code: Option<String>,
    browser_token: String,
    agent_token: Option<String>,
    agent_name: Option<String>,
    created: Instant,
    last_used: Instant,
    browser_streams: usize,
    /// When the browser last had a stream open (or the pairing was created).
    browser_seen: Instant,
    state: Option<Arc<SharedState>>,
    revision: u64,
    posts: VecDeque<Instant>,
    polls: usize,
    signal: watch::Sender<Signal>,
    /// The agent's pending proposal.
    proposal: Option<Arc<Proposal>>,
    /// How many proposals the agent has made: the last one's id.
    proposals_made: u64,
    /// Where the latest proposals stand, oldest first.
    outcomes: VecDeque<ProposalOutcome>,
    withdrawn: Option<u64>,
    /// The agent's view commands, to every open stream.
    commands: broadcast::Sender<SentCommand>,
    /// How many commands the agent has sent: the last one's id.
    commands_sent: u64,
    /// Commands the agent may send now (a token bucket of
    /// [`COMMAND_BURST`], refilled at [`COMMANDS_PER_SECOND`]), as of
    /// `budget_at`.
    command_budget: f64,
    budget_at: Instant,
}

impl Pairing {
    fn phase(&self) -> PairingPhase {
        if self.agent_token.is_some() {
            PairingPhase::Claimed
        } else {
            PairingPhase::Waiting
        }
    }

    fn publish(&self) {
        let phase = self.phase();
        self.signal.send_modify(|s| {
            s.phase = phase;
            s.agent.clone_from(&self.agent_name);
            s.revision = self.revision;
            s.proposal.clone_from(&self.proposal);
            s.withdrawn = self.withdrawn;
        });
    }

    /// Counts a post against [`MAX_POSTS_PER_WINDOW`], or says how long to
    /// wait.
    fn take_post(&mut self, now: Instant) -> Result<(), BridgeError> {
        while self
            .posts
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= POST_WINDOW)
        {
            self.posts.pop_front();
        }
        if self.posts.len() >= MAX_POSTS_PER_WINDOW {
            let wait = self.posts.front().map_or(POST_WINDOW, |t| {
                POST_WINDOW - now.saturating_duration_since(*t)
            });
            return Err(BridgeError::TooMany(wait.as_secs().max(1)));
        }
        self.posts.push_back(now);
        Ok(())
    }

    /// Counts a command against the pairing's command budget, or says how
    /// long to wait.
    fn take_command(&mut self, now: Instant) -> Result<(), BridgeError> {
        let rate = f64::from(COMMANDS_PER_SECOND);
        let elapsed = now.saturating_duration_since(self.budget_at).as_secs_f64();
        self.command_budget = (self.command_budget + elapsed * rate).min(f64::from(COMMAND_BURST));
        self.budget_at = now;
        if self.command_budget < 1.0 {
            return Err(BridgeError::TooMany(1));
        }
        self.command_budget -= 1.0;
        Ok(())
    }

    fn outcome_mut(&mut self, id: u64) -> Option<&mut ProposalOutcome> {
        self.outcomes.iter_mut().find(|o| o.id == id)
    }

    /// A proposal's status changed: the agent's long poll and the browser's
    /// streams hear of it.
    fn proposals_changed(&mut self) {
        self.revision += 1;
        self.publish();
    }

    /// Why this pairing should end at `now`, if it should.
    fn expired(&self, now: Instant) -> Option<EndReason> {
        let since = |t: Instant| now.saturating_duration_since(t);
        if self.code.is_some() && since(self.created) >= Duration::from_secs(CODE_TTL_SECS) {
            Some(EndReason::CodeExpired)
        } else if since(self.last_used) >= Duration::from_secs(IDLE_TIMEOUT_SECS) {
            Some(EndReason::Idle)
        } else if self.browser_streams == 0
            && since(self.browser_seen) >= Duration::from_secs(BROWSER_GONE_SECS)
        {
            Some(EndReason::BrowserGone)
        } else {
            None
        }
    }

    fn view(&self) -> StateView {
        StateView {
            v: PROTOCOL_VERSION,
            revision: self.revision,
            state: self.state.as_deref().cloned(),
            ended: None,
            proposals: self.outcomes.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, Default)]
struct Registry {
    pairings: Vec<Pairing>,
    next_id: u64,
    failed_claims: u32,
    /// Agent tokens of recently ended pairings, with why and when they
    /// ended, oldest first (see [`ENDED_MEMORY`]).
    ended: VecDeque<Ended>,
}

#[derive(Debug)]
struct Ended {
    agent_token: String,
    reason: EndReason,
    revision: u64,
    at: Instant,
}

/// The bridge's pairings. Cheap to share behind an `Arc`.
#[derive(Debug, Default)]
pub struct AgentBridge {
    inner: Mutex<Registry>,
}

/// Compares two secrets without stopping at the first difference.
pub fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// The index of the pairing whose `secret` matches `given`, comparing every
/// one.
fn find(
    pairings: &[Pairing],
    given: &str,
    secret: impl Fn(&Pairing) -> Option<&str>,
) -> Option<usize> {
    let mut found = None;
    for (i, p) in pairings.iter().enumerate() {
        if secret(p).is_some_and(|s| same_secret(s, given)) {
            found = Some(i);
        }
    }
    found
}

fn random_bytes<const N: usize>() -> Result<[u8; N], BridgeError> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|_| BridgeError::NoRandomness)?;
    Ok(buf)
}

/// 32 random bytes as hex.
fn new_token() -> Result<String, BridgeError> {
    Ok(random_bytes::<32>()?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// [`CODE_LEN`] characters of [`CODE_ALPHABET`] from 40 random bits.
fn new_code() -> Result<String, BridgeError> {
    let bytes = random_bytes::<5>()?;
    let bits = bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    Ok((0..CODE_LEN)
        .rev()
        .map(|i| char::from(CODE_ALPHABET[((bits >> (5 * i)) & 31) as usize]))
        .collect())
}

fn count(event: &'static str) {
    metrics::counter!(AGENT_PAIRINGS, "event" => event).increment(1);
}

/// What an open event stream holds: the pairing's signal, and a guard that
/// counts the stream as the browser's presence until it is dropped.
#[derive(Debug)]
pub struct BrowserStream {
    pub signal: watch::Receiver<Signal>,
    /// The agent's view commands sent while the stream is open.
    pub commands: broadcast::Receiver<SentCommand>,
    pub guard: StreamGuard,
}

/// Counts one open browser stream; dropping it uncounts it.
#[derive(Debug)]
pub struct StreamGuard {
    bridge: Arc<AgentBridge>,
    id: u64,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        let mut reg = self.bridge.lock();
        if let Some(p) = reg.pairings.iter_mut().find(|p| p.id == self.id) {
            p.browser_streams = p.browser_streams.saturating_sub(1);
            p.browser_seen = Instant::now();
        }
    }
}

/// Counts one waiting long poll; dropping it uncounts it.
struct PollGuard<'a> {
    bridge: &'a AgentBridge,
    id: u64,
}

impl Drop for PollGuard<'_> {
    fn drop(&mut self) {
        let mut reg = self.bridge.lock();
        if let Some(p) = reg.pairings.iter_mut().find(|p| p.id == self.id) {
            p.polls = p.polls.saturating_sub(1);
        }
    }
}

impl AgentBridge {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the pairing at `index` for `reason`: its followers see the
    /// reason, then its sender is dropped.
    fn end_at(reg: &mut Registry, index: usize, reason: EndReason) {
        let p = reg.pairings.swap_remove(index);
        p.signal.send_modify(|s| s.ended = Some(reason));
        // The agent that ended it already knows why.
        if let Some(agent_token) = p.agent_token.clone().filter(|_| reason != EndReason::Agent) {
            if reg.ended.len() >= MAX_ENDED_REMEMBERED {
                reg.ended.pop_front();
            }
            reg.ended.push_back(Ended {
                agent_token,
                reason,
                revision: p.revision,
                at: Instant::now(),
            });
        }
        count("ended");
        tracing::info!(pairing = p.id, reason = ?reason, "agent pairing ended");
    }

    fn sweep_locked(reg: &mut Registry, now: Instant) {
        while let Some((i, reason)) = reg
            .pairings
            .iter()
            .enumerate()
            .find_map(|(i, p)| p.expired(now).map(|r| (i, r)))
        {
            Self::end_at(reg, i, reason);
        }
    }

    /// Ends the pairings whose time is up.
    pub fn sweep(&self) {
        Self::sweep_locked(&mut self.lock(), Instant::now());
    }

    /// How many pairings are open (after a sweep).
    pub fn len(&self) -> usize {
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, Instant::now());
        reg.pairings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A new pairing, waiting for an agent.
    pub fn create(&self) -> Result<PairingCreated, BridgeError> {
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        if reg.pairings.len() >= MAX_PAIRINGS {
            return Err(BridgeError::Full);
        }
        let code = loop {
            let code = new_code()?;
            if !reg
                .pairings
                .iter()
                .any(|p| p.code.as_deref() == Some(code.as_str()))
            {
                break code;
            }
        };
        let browser_token = new_token()?;
        reg.next_id += 1;
        let id = reg.next_id;
        let (signal, _) = watch::channel(Signal {
            phase: PairingPhase::Waiting,
            agent: None,
            revision: 0,
            ended: None,
            proposal: None,
            withdrawn: None,
        });
        reg.pairings.push(Pairing {
            id,
            code: Some(code.clone()),
            browser_token: browser_token.clone(),
            agent_token: None,
            agent_name: None,
            created: now,
            last_used: now,
            browser_streams: 0,
            browser_seen: now,
            state: None,
            revision: 0,
            posts: VecDeque::new(),
            polls: 0,
            signal,
            proposal: None,
            proposals_made: 0,
            outcomes: VecDeque::new(),
            withdrawn: None,
            commands: broadcast::Sender::new(COMMAND_BACKLOG),
            commands_sent: 0,
            command_budget: f64::from(COMMAND_BURST),
            budget_at: now,
        });
        count("created");
        tracing::info!(pairing = id, "agent pairing created");
        Ok(PairingCreated {
            v: PROTOCOL_VERSION,
            code,
            browser_token,
            code_expires_in_s: CODE_TTL_SECS,
            idle_timeout_s: IDLE_TIMEOUT_SECS,
        })
    }

    /// Claims the unclaimed pairing with `code` (already normalised; `None`
    /// for a code that could never be valid, which counts as a wrong one).
    /// After [`MAX_FAILED_CLAIMS`] wrong codes, every unclaimed pairing ends
    /// ([`EndReason::CodeBurnt`]).
    pub fn claim(&self, code: Option<&str>, agent: Option<&str>) -> Result<Claimed, BridgeError> {
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        let index = code.and_then(|code| find(&reg.pairings, code, |p| p.code.as_deref()));
        let Some(index) = index else {
            count("claim_refused");
            reg.failed_claims += 1;
            if reg.failed_claims >= MAX_FAILED_CLAIMS {
                reg.failed_claims = 0;
                while let Some(i) = reg.pairings.iter().position(|p| p.code.is_some()) {
                    Self::end_at(&mut reg, i, EndReason::CodeBurnt);
                }
                tracing::warn!("too many wrong agent pairing codes: every unclaimed code burnt");
            }
            return Err(BridgeError::NoSuchCode);
        };
        let agent_token = new_token()?;
        reg.failed_claims = 0;
        let p = &mut reg.pairings[index];
        p.code = None;
        p.agent_token = Some(agent_token.clone());
        p.agent_name = agent.and_then(|a| tidy_text(a, MAX_AGENT_NAME_CHARS));
        p.last_used = now;
        p.publish();
        count("claimed");
        tracing::info!(pairing = p.id, "agent pairing claimed");
        Ok(Claimed {
            v: PROTOCOL_VERSION,
            agent_token,
            idle_timeout_s: IDLE_TIMEOUT_SECS,
            long_poll_s: LONG_POLL_SECS,
        })
    }

    /// Takes the browser's state; returns its revision.
    pub fn post_state(&self, browser_token: &str, state: SharedState) -> Result<u64, BridgeError> {
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        let i = find(&reg.pairings, browser_token, |p| Some(&p.browser_token))
            .ok_or(BridgeError::Unauthorized)?;
        let p = &mut reg.pairings[i];
        p.take_post(now)?;
        p.last_used = now;
        p.state = Some(Arc::new(state));
        p.revision += 1;
        p.publish();
        Ok(p.revision)
    }

    /// What the agent of a pairing that ended within [`ENDED_MEMORY`] is
    /// told: that it ended, and why. Compares every remembered token, like
    /// [`find`], so timing says nothing about which one matched.
    fn recently_ended(reg: &mut Registry, agent_token: &str, now: Instant) -> Option<StateView> {
        while reg
            .ended
            .front()
            .is_some_and(|e| now.saturating_duration_since(e.at) >= ENDED_MEMORY)
        {
            reg.ended.pop_front();
        }
        let mut found = None;
        for e in &reg.ended {
            if same_secret(&e.agent_token, agent_token) {
                found = Some((e.reason, e.revision));
            }
        }
        found.map(|(reason, revision)| StateView {
            v: PROTOCOL_VERSION,
            revision,
            state: None,
            ended: Some(reason),
            proposals: Vec::new(),
        })
    }

    /// The state as the agent reads it. With `since`, waits up to `wait`
    /// for a revision newer than it (or for the pairing to end).
    pub async fn read_state(
        &self,
        agent_token: &str,
        since: Option<u64>,
        wait: Duration,
    ) -> Result<StateView, BridgeError> {
        let (mut rx, id) = {
            let now = Instant::now();
            let mut reg = self.lock();
            Self::sweep_locked(&mut reg, now);
            let Some(i) = find(&reg.pairings, agent_token, |p| p.agent_token.as_deref()) else {
                return Self::recently_ended(&mut reg, agent_token, now)
                    .ok_or(BridgeError::Unauthorized);
            };
            let p = &mut reg.pairings[i];
            p.last_used = now;
            match since {
                Some(since) if p.revision <= since => {}
                _ => return Ok(p.view()),
            }
            if p.polls >= MAX_POLLS_PER_PAIRING {
                return Err(BridgeError::TooMany(1));
            }
            p.polls += 1;
            (p.signal.subscribe(), p.id)
        };
        let _guard = PollGuard { bridge: self, id };
        let since = since.unwrap_or(0);
        // A timeout is the ordinary end of a long poll: the current state is
        // the answer either way.
        let _ = tokio::time::timeout(wait, async {
            loop {
                {
                    let s = rx.borrow_and_update();
                    if s.ended.is_some() || s.revision > since {
                        return;
                    }
                }
                if rx.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
        let ended = |s: Signal| StateView {
            v: PROTOCOL_VERSION,
            revision: s.revision,
            state: None,
            ended: Some(s.ended.unwrap_or(EndReason::Other)),
            proposals: Vec::new(),
        };
        let last = rx.borrow().clone();
        if last.ended.is_some() {
            return Ok(ended(last));
        }
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        match reg.pairings.iter_mut().find(|p| p.id == id) {
            Some(p) => {
                p.last_used = now;
                Ok(p.view())
            }
            // Ended by the sweep just now.
            None => Ok(ended(rx.borrow().clone())),
        }
    }

    /// Takes the agent's proposal (its summary already tidied, its changes
    /// checked) as the pending one, superseding any still pending; returns
    /// its id.
    pub fn propose(
        &self,
        agent_token: &str,
        summary: String,
        base_revision: u64,
        changes: ChangeSet,
    ) -> Result<u64, BridgeError> {
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        let i = find(&reg.pairings, agent_token, |p| p.agent_token.as_deref())
            .ok_or(BridgeError::Unauthorized)?;
        let p = &mut reg.pairings[i];
        p.take_post(now)?;
        p.last_used = now;
        if let Some(old) = p.proposal.take().map(|o| o.id)
            && let Some(o) = p.outcome_mut(old)
        {
            o.status = ProposalStatus::Superseded;
        }
        p.proposals_made += 1;
        let id = p.proposals_made;
        p.outcomes.push_back(ProposalOutcome {
            id,
            summary: summary.clone(),
            status: ProposalStatus::Pending,
            reason: None,
        });
        while p.outcomes.len() > MAX_OUTCOMES {
            p.outcomes.pop_front();
        }
        p.proposal = Some(Arc::new(Proposal {
            id,
            summary,
            base_revision,
            changes,
        }));
        p.proposals_changed();
        count("proposed");
        tracing::info!(pairing = p.id, proposal = id, "agent proposal received");
        Ok(id)
    }

    /// Withdraws the agent's proposal `id` if it is pending. Otherwise its
    /// status is [`BridgeError::Decided`] (a withdrawn one too: withdrawing
    /// twice changes nothing).
    pub fn withdraw(&self, agent_token: &str, id: u64) -> Result<(), BridgeError> {
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        let i = find(&reg.pairings, agent_token, |p| p.agent_token.as_deref())
            .ok_or(BridgeError::Unauthorized)?;
        let p = &mut reg.pairings[i];
        p.last_used = now;
        if p.proposal.as_ref().is_none_or(|pending| pending.id != id) {
            let status = p
                .outcomes
                .iter()
                .find(|o| o.id == id)
                .map(|o| o.status)
                .ok_or(BridgeError::NoSuchProposal)?;
            return Err(BridgeError::Decided(status));
        }
        p.proposal = None;
        p.withdrawn = Some(id);
        if let Some(o) = p.outcome_mut(id) {
            o.status = ProposalStatus::Withdrawn;
        }
        p.proposals_changed();
        Ok(())
    }

    /// The user's decision on proposal `id` (the reason already tidied). A
    /// decision is final; one for a proposal superseded or withdrawn since
    /// the user acted on it still counts.
    pub fn decide(
        &self,
        browser_token: &str,
        id: u64,
        decision: Decision,
        reason: Option<String>,
    ) -> Result<(), BridgeError> {
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        let i = find(&reg.pairings, browser_token, |p| Some(&p.browser_token))
            .ok_or(BridgeError::Unauthorized)?;
        let p = &mut reg.pairings[i];
        p.take_post(now)?;
        p.last_used = now;
        let o = p.outcome_mut(id).ok_or(BridgeError::NoSuchProposal)?;
        if o.status.is_decided() {
            return Err(BridgeError::Decided(o.status));
        }
        o.status = decision.into();
        o.reason = reason;
        if p.proposal.as_ref().is_some_and(|pending| pending.id == id) {
            p.proposal = None;
        }
        p.proposals_changed();
        count("decided");
        tracing::info!(pairing = p.id, proposal = id, decision = ?decision, "agent proposal decided");
        Ok(())
    }

    /// Passes the agent's view command (already checked) to the browser's
    /// open event streams; returns its id. Refused when no stream is open,
    /// since nothing would hear it: commands are not kept for later.
    pub fn command(&self, agent_token: &str, command: AgentCommand) -> Result<u64, BridgeError> {
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        let i = find(&reg.pairings, agent_token, |p| p.agent_token.as_deref())
            .ok_or(BridgeError::Unauthorized)?;
        let p = &mut reg.pairings[i];
        p.last_used = now;
        // Only a command something hears is charged for: while the app is
        // reconnecting, the agent's tries do not use up its burst.
        if p.commands.receiver_count() == 0 {
            return Err(BridgeError::NotListening);
        }
        p.take_command(now)?;
        p.commands_sent += 1;
        let id = p.commands_sent;
        p.commands
            .send(Arc::new((id, command)))
            .map_err(|_| BridgeError::NotListening)?;
        count("command");
        tracing::debug!(pairing = p.id, command = id, "agent command passed on");
        Ok(id)
    }

    /// Ends the pairing that `token` belongs to, as the browser or the
    /// agent, whichever it is.
    pub fn end(&self, token: &str) -> Result<EndReason, BridgeError> {
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, Instant::now());
        let (i, reason) = match find(&reg.pairings, token, |p| Some(&p.browser_token)) {
            Some(i) => (i, EndReason::Browser),
            None => (
                find(&reg.pairings, token, |p| p.agent_token.as_deref())
                    .ok_or(BridgeError::Unauthorized)?,
                EndReason::Agent,
            ),
        };
        Self::end_at(&mut reg, i, reason);
        Ok(reason)
    }

    /// Opens an event stream for the browser that holds `browser_token`.
    pub fn open_stream(
        self: &Arc<Self>,
        browser_token: &str,
    ) -> Result<BrowserStream, BridgeError> {
        let now = Instant::now();
        let mut reg = self.lock();
        Self::sweep_locked(&mut reg, now);
        let i = find(&reg.pairings, browser_token, |p| Some(&p.browser_token))
            .ok_or(BridgeError::Unauthorized)?;
        let p = &mut reg.pairings[i];
        if p.browser_streams >= MAX_STREAMS_PER_PAIRING {
            return Err(BridgeError::TooMany(5));
        }
        p.browser_streams += 1;
        p.browser_seen = now;
        Ok(BrowserStream {
            signal: p.signal.subscribe(),
            commands: p.commands.subscribe(),
            guard: StreamGuard {
                bridge: Arc::clone(self),
                id: p.id,
            },
        })
    }

    /// Ends every pairing (the server is shutting down).
    pub fn end_all(&self, reason: EndReason) {
        let mut reg = self.lock();
        while !reg.pairings.is_empty() {
            Self::end_at(&mut reg, 0, reason);
        }
    }
}

#[cfg(test)]
mod tests;
