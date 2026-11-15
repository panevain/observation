//! Proposals: the agent proposes a whole change set to the open scene, and
//! the user accepts or rejects it in the app.
//!
//! - The agent posts a [`ProposalPost`] to [`paths::PROPOSAL`](crate::paths::PROPOSAL)
//!   and gets the proposal's id ([`ProposalSubmitted`]). A pairing has at
//!   most one pending proposal: a new one **supersedes** it.
//! - The browser hears of it as a [`BrowserEventKind::Proposal`](crate::BrowserEventKind::Proposal)
//!   event. Events carry the pending proposal itself, not a queue: a stream
//!   that opens while one is pending gets it at once, and a superseded one
//!   is simply replaced. A withdrawal is its own event.
//! - The user's [`Decision`] comes back with a [`DecisionPost`] to
//!   [`paths::DECISION`](crate::paths::DECISION): accepted, rejected (with
//!   an optional reason), or stale (the user changed something the proposal
//!   touches first, so it cannot be applied; it is never rebased). A
//!   decision is final. One may still arrive for a proposal superseded or
//!   withdrawn after the user acted on it, and then it is what counts.
//! - The agent reads where its proposals stand in the [`StateView`](crate::StateView)'s
//!   `proposals` ([`ProposalOutcome`]); every change of status raises the
//!   state's revision, so its long poll hears of it.
//!
//! Summaries, reasons and change lines are untrusted text: tidied here,
//! drawn as text, never as markup.

use observation_core::scene::agent_log::MAX_LOG_SUMMARY_CHARS;
use observation_core::scene::changes::ChangeSet;
use serde::{Deserialize, Serialize};

/// Longest summary kept, characters (as the scene's change log keeps it).
pub const MAX_SUMMARY_CHARS: usize = MAX_LOG_SUMMARY_CHARS;
/// Longest reason for a decision kept, characters.
pub const MAX_REASON_CHARS: usize = 300;
/// Most proposal outcomes a [`StateView`](crate::StateView) lists.
pub const MAX_OUTCOMES: usize = 16;
/// Largest [`ProposalPost`] body accepted, bytes (as a state post).
pub const MAX_PROPOSAL_BYTES: usize = crate::MAX_STATE_BYTES;

/// Body of `POST` [`paths::PROPOSAL`](crate::paths::PROPOSAL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalPost {
    pub v: u32,
    /// What the change does, in a sentence (cut to [`MAX_SUMMARY_CHARS`]).
    pub summary: String,
    /// The state revision whose scene the change set was made against.
    #[serde(default)]
    pub base_revision: u64,
    pub changes: ChangeSet,
}

/// The server took a [`ProposalPost`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalSubmitted {
    pub v: u32,
    /// The proposal's id within the pairing (from 1).
    pub id: u64,
}

/// A proposal as the browser receives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: u64,
    pub summary: String,
    #[serde(default)]
    pub base_revision: u64,
    pub changes: ChangeSet,
}

/// Where a proposal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    /// Waiting for the user.
    Pending,
    /// The user accepted it: it is applied to their scene, as one undo step.
    Accepted,
    /// The user rejected it, perhaps with a reason.
    Rejected,
    /// The user changed something it touches before deciding, so it cannot
    /// be applied. Propose again from the current scene.
    Stale,
    /// A newer proposal replaced it before the user decided.
    Superseded,
    /// The agent withdrew it.
    Withdrawn,
    /// A status this build does not know.
    #[serde(other)]
    Other,
}

impl ProposalStatus {
    /// Whether the user decided it (accepted, rejected or stale): nothing
    /// changes it after that.
    pub fn is_decided(self) -> bool {
        matches!(self, Self::Accepted | Self::Rejected | Self::Stale)
    }
}

/// One proposal's status, as the agent reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalOutcome {
    pub id: u64,
    pub summary: String,
    pub status: ProposalStatus,
    /// The user's reason (rejected), or what changed (stale).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What the user decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Accepted,
    Rejected,
    Stale,
}

impl From<Decision> for ProposalStatus {
    fn from(d: Decision) -> Self {
        match d {
            Decision::Accepted => Self::Accepted,
            Decision::Rejected => Self::Rejected,
            Decision::Stale => Self::Stale,
        }
    }
}

/// Body of `POST` [`paths::DECISION`](crate::paths::DECISION).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionPost {
    pub v: u32,
    /// The proposal decided.
    pub id: u64,
    pub decision: Decision,
    /// Why (cut to [`MAX_REASON_CHARS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `text` with its whitespace runs (line breaks included) made single
/// spaces, trimmed and cut to `max` characters; `None` if nothing is left.
pub fn tidy_line(text: &str, max: usize) -> Option<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::tidy_text(&flat, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrowserEvent, BrowserEventKind, PROTOCOL_VERSION, StateView};
    use observation_core::scene::changes::{Change, ItemChange};
    use serde_json::json;

    fn changes() -> ChangeSet {
        ChangeSet {
            changes: vec![Change::LimitFiguresTo {
                base: None,
                new: Some(2),
            }],
        }
    }

    #[test]
    fn a_proposal_event_carries_the_whole_change_set() {
        let event = BrowserEvent::new(BrowserEventKind::Proposal {
            proposal: Proposal {
                id: 3,
                summary: "Limit the figures to the garden".into(),
                base_revision: 7,
                changes: changes(),
            },
        });
        let text = serde_json::to_string(&event).unwrap();
        assert_eq!(
            text,
            r#"{"v":1,"type":"proposal","proposal":{"id":3,"summary":"Limit the figures to the garden","base_revision":7,"changes":{"changes":[{"item":"limit_figures_to","new":2}]}}}"#
        );
        assert_eq!(serde_json::from_str::<BrowserEvent>(&text).unwrap(), event);
        let withdrawn = BrowserEvent::new(BrowserEventKind::ProposalWithdrawn { id: 3 });
        assert_eq!(
            serde_json::to_string(&withdrawn).unwrap(),
            r#"{"v":1,"type":"proposal_withdrawn","id":3}"#
        );
    }

    #[test]
    fn outcomes_are_additive_and_read_later_statuses() {
        // A view without proposals reads, and writes as before.
        let view: StateView = serde_json::from_value(json!({"v": 1, "revision": 2})).unwrap();
        assert!(view.proposals.is_empty());
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            json!({"v": 1, "revision": 2})
        );
        let view: StateView = serde_json::from_value(json!({
            "v": 1, "revision": 4,
            "proposals": [
                {"id": 1, "summary": "a", "status": "superseded"},
                {"id": 2, "summary": "b", "status": "rejected", "reason": "too close"},
                {"id": 3, "summary": "c", "status": "pondering"}
            ]
        }))
        .unwrap();
        let statuses: Vec<ProposalStatus> = view.proposals.iter().map(|p| p.status).collect();
        assert_eq!(
            statuses,
            [
                ProposalStatus::Superseded,
                ProposalStatus::Rejected,
                ProposalStatus::Other
            ]
        );
        assert_eq!(view.proposals[1].reason.as_deref(), Some("too close"));
        assert!(ProposalStatus::Stale.is_decided());
        assert!(!ProposalStatus::Superseded.is_decided());
    }

    #[test]
    fn posts_round_trip() {
        let post = ProposalPost {
            v: PROTOCOL_VERSION,
            summary: "s".into(),
            base_revision: 1,
            changes: ChangeSet {
                changes: vec![Change::Target(ItemChange {
                    id: 1,
                    base: None,
                    new: None,
                })],
            },
        };
        let text = serde_json::to_string(&post).unwrap();
        assert_eq!(serde_json::from_str::<ProposalPost>(&text).unwrap(), post);
        let decision: DecisionPost =
            serde_json::from_value(json!({"v": 1, "id": 2, "decision": "stale"})).unwrap();
        assert_eq!(decision.decision, Decision::Stale);
        assert_eq!(decision.reason, None);
        assert_eq!(
            tidy_line("  too\n close  to  the door ", 12).as_deref(),
            Some("too close to")
        );
    }
}
