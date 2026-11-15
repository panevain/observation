//! A proposal from the connected agent (see
//! [`observation_agent_api::proposal`]): a whole change set the user
//! accepts or rejects in the app, never in part.
//!
//! ```text
//! Open ──accept──▶ Accepted (applied as one undo step, logged in the scene)
//!  │ ──reject──▶ Rejected (with an optional reason)
//!  │ ──the user changes an item it touches──▶ Stale (the agent is told once)
//!  │ ──the agent withdraws it, or the connection ends──▶ Closed
//!  └─▶ Unusable (it fits, but would leave a scene the app refuses)
//! ```
//!
//! After every change of the document, [`PendingProposal::follow`] checks
//! the change set against the scene as it would be exported, tidied as the
//! agent read it: if any item it touches differs from its base value, the
//! proposal is stale for good (proposals are never rebased); otherwise the
//! scene it would leave is kept for the before/after figures and the ghost
//! preview. Accepting applies the change set item by item to the current
//! document, so the user's unrelated edits stay, and appends the summary to
//! the scene's change log; that log entry is what names the undo step
//! ("Agent: <summary>", see [`crate::history`]).

use crate::document::{Document, Models};
use observation_agent_api::Proposal;
use observation_core::camera::{CameraModel, DoriLevel, Lighting};
use observation_core::coverage::CoverageStats;
use observation_core::scene::SceneFile;
use observation_core::scene::agent_log::{AgentLogEntry, push_entry};
use observation_core::scene::changes::{Change, ChangeError, ChangeSet};

/// Where a proposal stands, in the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Waiting for the user, and it still fits the scene.
    Open,
    /// The user changed these items (as "camera 3") before deciding.
    Stale {
        what: Vec<String>,
    },
    /// It fits, but applying it would leave a scene the app refuses: why.
    Unusable {
        why: String,
    },
    Accepted,
    Rejected,
    /// The agent withdrew it, or the connection ended, before a decision:
    /// why, for the card.
    Closed {
        why: String,
    },
}

impl Standing {
    /// Whether the user can still decide.
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open | Self::Unusable { .. })
    }
}

/// What accepting did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// Applied to the document.
    Applied,
    /// The user had changed these items: now stale, nothing applied.
    Stale(Vec<String>),
    /// Nothing applied, for this reason.
    Refused(String),
}

/// The agent's proposal the app shows.
#[derive(Debug, Clone)]
pub struct PendingProposal {
    pub id: u64,
    /// Tidied by the server; drawn as text.
    pub summary: String,
    pub changes: ChangeSet,
    /// One line per change, for the card (plain text).
    pub lines: Vec<String>,
    pub standing: Standing,
    /// The reason the user is typing for a rejection.
    pub reason: String,
    /// The decision could not be sent to the agent: why.
    pub decision_error: Option<String>,
    /// The decision did not get through yet and is being tried again.
    pub decision_retrying: bool,
    /// The scene accepting would leave, from the current document; `None`
    /// unless the proposal is open and usable.
    proposed: Option<SceneFile>,
    /// Bumped whenever `proposed` changes.
    proposed_revision: u64,
    /// The document as last checked: its revision and custom models.
    checked: Option<(u64, Vec<CameraModel>)>,
}

/// The document's scene as it would be exported and then read back, tidied
/// as the agent read it; `None` before an area has loaded.
pub fn current_scene(doc: &Document, models: &Models) -> Option<SceneFile> {
    let file = doc.to_file(models)?;
    // Compact rather than the export's indented text: it reads the same.
    SceneFile::from_json(&serde_json::to_string(&file).ok()?).ok()
}

impl PendingProposal {
    /// A proposal as it arrived, with its lines worded with the models'
    /// names (the proposal's own custom models first).
    pub fn new(p: Proposal, models: &Models) -> Self {
        let own: Vec<&CameraModel> = p
            .changes
            .changes
            .iter()
            .filter_map(|c| match c {
                Change::CustomModel(m) => m.new.as_ref().or(m.base.as_ref()),
                _ => None,
            })
            .collect();
        let name = |id: &str| {
            own.iter()
                .find(|m| m.id == id)
                .copied()
                .or_else(|| models.get(id))
                .map(CameraModel::display_name)
        };
        let lines = p.changes.lines(&name);
        let standing = match p.changes.check() {
            Ok(()) => Standing::Open,
            Err(e) => Standing::Unusable {
                why: format!("The app cannot read it: {e}"),
            },
        };
        Self {
            id: p.id,
            summary: p.summary,
            changes: p.changes,
            lines,
            standing,
            reason: String::new(),
            decision_error: None,
            decision_retrying: false,
            proposed: None,
            proposed_revision: 0,
            checked: None,
        }
    }

    /// The scene accepting would leave, with a counter that changes with
    /// it, while the proposal is open and usable.
    pub fn proposed(&self) -> Option<(u64, &SceneFile)> {
        self.proposed.as_ref().map(|s| (self.proposed_revision, s))
    }

    /// Checks the proposal against the document if it changed since the
    /// last check. Returns what the user changed when this check made the
    /// proposal stale, so the agent can be told once.
    pub fn follow(&mut self, doc: &Document, models: &Models) -> Option<Vec<String>> {
        if !self.standing.is_open() {
            return None;
        }
        if self
            .checked
            .as_ref()
            .is_some_and(|(rev, custom)| *rev == doc.revision && *custom == models.custom)
        {
            return None;
        }
        self.checked = Some((doc.revision, models.custom.clone()));
        let current = current_scene(doc, models)?;
        let what = self.changes.conflicts(&current);
        if !what.is_empty() {
            self.set_proposed(None);
            self.standing = Standing::Stale { what: what.clone() };
            return Some(what);
        }
        match self.changes.apply(&current) {
            Ok(scene) => {
                self.standing = Standing::Open;
                self.set_proposed(Some(scene));
            }
            Err(e) => {
                self.standing = Standing::Unusable { why: e.to_string() };
                self.set_proposed(None);
            }
        }
        None
    }

    fn set_proposed(&mut self, scene: Option<SceneFile>) {
        if self.proposed != scene {
            self.proposed = scene;
            self.proposed_revision += 1;
        }
    }

    /// Applies the whole change set to the document, as one edit, and logs
    /// it in the scene's change log at `now_unix`.
    pub fn accept(&mut self, doc: &mut Document, models: &mut Models, now_unix: u64) -> Accepted {
        if self.standing != Standing::Open {
            return Accepted::Refused("This proposal can no longer be accepted.".into());
        }
        let Some(current) = current_scene(doc, models) else {
            return Accepted::Refused("No area is loaded.".into());
        };
        match self.changes.apply(&current) {
            Ok(mut scene) => {
                if let Some(entry) = AgentLogEntry::new(&self.summary, now_unix) {
                    push_entry(&mut scene.agent_log, entry);
                }
                doc.take_items(scene, models);
                self.standing = Standing::Accepted;
                self.set_proposed(None);
                Accepted::Applied
            }
            Err(ChangeError::Stale(_)) => {
                let what = self.changes.conflicts(&current);
                self.standing = Standing::Stale { what: what.clone() };
                self.set_proposed(None);
                Accepted::Stale(what)
            }
            Err(e) => {
                let why = e.to_string();
                self.standing = Standing::Unusable { why: why.clone() };
                Accepted::Refused(why)
            }
        }
    }

    /// The user rejected it. Returns the reason they gave, if any.
    pub fn reject(&mut self) -> Option<String> {
        self.standing = Standing::Rejected;
        self.set_proposed(None);
        let reason = self.reason.trim();
        (!reason.is_empty()).then(|| reason.to_owned())
    }

    /// The agent withdrew it, or the connection ended, before a decision.
    pub fn close(&mut self, why: &str) {
        if self.standing.is_open() {
            self.standing = Standing::Closed {
                why: why.to_owned(),
            };
            self.set_proposed(None);
        }
    }
}

/// What the user changed, in the words of the stale decision's reason.
pub fn stale_reason(what: &[String]) -> String {
    format!(
        "The user changed {} before deciding",
        what.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
    )
}

/// The proposal card, as a panel lays it out.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalCardVm {
    /// "Proposal from <agent>".
    pub title: String,
    pub summary: String,
    pub lines: Vec<String>,
    /// Where it stands, in a sentence; `None` while open.
    pub status: Option<String>,
    /// Whether that sentence is a warning (stale, unusable).
    pub warning: bool,
    pub can_accept: bool,
    pub can_reject: bool,
    /// Why Accept is disabled, for its tooltip.
    pub accept_tip: String,
    /// The decision could not be sent.
    pub decision_error: Option<String>,
    /// The decision is being tried again ("Telling the agent…").
    pub telling_agent: bool,
}

impl ProposalCardVm {
    pub fn new(p: &PendingProposal, agent: Option<&str>) -> Self {
        let (status, warning) = match &p.standing {
            Standing::Open => (None, false),
            Standing::Stale { what } => (
                Some(format!(
                    "Stale: you changed {} since the agent proposed this, so it cannot be \
                     applied. The agent has been told; ask it to propose again.",
                    what.join(", ")
                )),
                true,
            ),
            Standing::Unusable { why } => (Some(format!("This cannot be applied: {why}")), true),
            Standing::Accepted => (
                Some("Accepted and applied: undo reverts it as one step.".to_owned()),
                false,
            ),
            Standing::Rejected => (Some("Rejected: nothing was changed.".to_owned()), false),
            Standing::Closed { why } => (Some(why.clone()), false),
        };
        let can_accept = p.standing == Standing::Open;
        let accept_tip = match &p.standing {
            Standing::Open => "Apply every change above as one step (undo reverts it)".to_owned(),
            Standing::Stale { .. } => "Stale: the scene changed under it".to_owned(),
            _ => "It can no longer be accepted".to_owned(),
        };
        Self {
            title: format!("Proposal from {}", agent.unwrap_or("the agent")),
            summary: p.summary.clone(),
            lines: p.lines.clone(),
            status,
            warning,
            can_accept,
            can_reject: p.standing.is_open(),
            accept_tip,
            decision_error: p.decision_error.clone(),
            telling_agent: p.decision_retrying,
        }
    }
}

/// The before and after figures on the card: the street seen at the
/// selected detail level and lighting.
#[derive(Debug, Clone, PartialEq)]
pub struct FiguresVm {
    /// "Street seen at Recognise by day".
    pub lead: String,
    /// "312 m² (41%)".
    pub before: String,
    /// The same after accepting (with its settings when the proposal changes
    /// them); `None` while it is being computed.
    pub after: Option<String>,
    /// "+53 m²", "−20 m²" or "no change", once computed.
    pub change: Option<String>,
}

/// A detail level and lighting, as figures were counted at.
pub type Settings = (DoriLevel, Lighting);

fn settings_words((level, lighting): Settings) -> String {
    let light = match lighting {
        Lighting::Day => "by day",
        Lighting::Night => "at night",
    };
    format!("{} {light}", level.label())
}

impl FiguresVm {
    /// From the figures now (`before`, at the document's settings) and,
    /// once computed, those of the proposed scene (`after`, at its own
    /// settings, which the proposal may change).
    pub fn new(
        before: (Settings, &CoverageStats),
        after: Option<(Settings, &CoverageStats)>,
    ) -> Self {
        let show = |s: &CoverageStats| {
            format!(
                "{:.0} m² ({:.0}%)",
                s.street_seen_m2,
                s.street_fraction() * 100.0
            )
        };
        let change = after.map(|(_, a)| {
            let d = a.street_seen_m2 - before.1.street_seen_m2;
            if d.abs() < 0.5 {
                "no change".to_owned()
            } else if d > 0.0 {
                format!("+{d:.0} m²")
            } else {
                format!("\u{2212}{:.0} m²", -d)
            }
        });
        let after_text = after.map(|(settings, a)| {
            if settings == before.0 {
                show(a)
            } else {
                format!("{} at {}", show(a), settings_words(settings))
            }
        });
        Self {
            lead: format!("Street seen at {}", settings_words(before.0)),
            before: show(before.1),
            after: after_text,
            change,
        }
    }
}

#[cfg(test)]
mod tests;
