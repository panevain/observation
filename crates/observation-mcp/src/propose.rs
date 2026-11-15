//! Proposals: the agent suggests a change to the scene open in the app, and
//! the user accepts or rejects it there (see
//! [`observation_agent_api::proposal`]).
//!
//! The agent edits a copy of the app's shared scene with the scene tools,
//! then proposes the result. [`propose`] compares it with the app's latest
//! shared scene, item by item ([`ChangeSet::between`]), so the proposal
//! records the value each touched item had (the app uses that to tell
//! whether the user has changed it since), and checks that applying it
//! leaves a scene the app accepts. Nothing changes in the app until the
//! user accepts; the decision comes back through the app's state.

use crate::ObservationMcp;
use crate::ToolError;
use crate::catalog::{catalog, model};
use crate::edit;
use crate::schema::JsonObject;
use observation_agent_api::{MAX_SUMMARY_CHARS, ProposalOutcome, ProposalStatus, tidy_line};
use observation_core::scene::SceneFile;
use observation_core::scene::changes::{ChangeError, ChangeSet};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposeArgs {
    /// The scene as you propose it: the app's scene (from get_app_state)
    /// edited with the scene tools.
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// What the change does and why, in one sentence the user reads first
    /// (at most 200 characters).
    pub summary: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposalIdArgs {
    /// The proposal's id, as propose_changes returned it.
    pub id: u64,
}

/// A proposal just sent.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Proposed {
    /// The proposal's id, for get_proposal and withdraw_proposal.
    pub id: u64,
    /// "pending": the user has not decided yet.
    pub status: String,
    pub summary: String,
    /// The changes as the app lists them for the user.
    pub changes: Vec<String>,
    /// The app's state revision the proposal was made against.
    pub base_revision: u64,
    /// What happens next.
    pub note: String,
}

/// Where a proposal stands.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProposalStatusOut {
    pub id: u64,
    pub summary: String,
    /// pending, accepted, rejected, stale, superseded or withdrawn.
    pub status: String,
    /// The user's reason (rejected), or what the user changed (stale).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the status means and what to do.
    pub meaning: String,
}

impl ProposalStatusOut {
    pub fn of(o: &ProposalOutcome) -> Self {
        let (status, meaning) = describe(o.status);
        Self {
            id: o.id,
            summary: o.summary.clone(),
            status: status.to_owned(),
            reason: o.reason.clone(),
            meaning: meaning.to_owned(),
        }
    }
}

/// A status's name and what it means for the agent.
fn describe(status: ProposalStatus) -> (&'static str, &'static str) {
    match status {
        ProposalStatus::Pending => (
            "pending",
            "The user has not decided yet. Nothing has changed in their scene.",
        ),
        ProposalStatus::Accepted => (
            "accepted",
            "The user accepted it: it is applied to their scene as one undo step.",
        ),
        ProposalStatus::Rejected => (
            "rejected",
            "The user rejected it; nothing changed. Consider their reason before proposing again.",
        ),
        ProposalStatus::Stale => (
            "stale",
            "The user changed something it touches before deciding, so it was not applied. \
             Read get_app_state again and propose from the current scene if it still helps.",
        ),
        ProposalStatus::Superseded => (
            "superseded",
            "A newer proposal replaced it before the user decided; nothing changed.",
        ),
        ProposalStatus::Withdrawn => ("withdrawn", "You withdrew it; nothing changed."),
        ProposalStatus::Other => (
            "unknown",
            "The app reported a status this server does not know.",
        ),
    }
}

/// A model's name by id, from either scene's custom models or the catalogue.
pub(crate) fn model_name(base: &SceneFile, proposed: &SceneFile, id: &str) -> Option<String> {
    let cat = catalog().ok()?;
    model(cat, Some(proposed), id)
        .or_else(|| model(cat, Some(base), id))
        .map(|m| m.display_name())
}

/// The change set from the app's shared scene `base` to `proposed`, checked
/// and with its lines; refused if it is empty, changes the area, or leaves a
/// scene the app would refuse.
pub fn change_set(
    base: &SceneFile,
    proposed: &SceneFile,
) -> Result<(ChangeSet, Vec<String>), ToolError> {
    let changes = ChangeSet::between(base, proposed).map_err(|e| match e {
        ChangeError::OtherArea => ToolError::invalid(
            "the scene is on another area than the one open in the app; propose changes to the \
             scene get_app_state returns",
        ),
        other => ToolError::invalid(other.to_string()),
    })?;
    if changes.is_empty() {
        return Err(ToolError::invalid(
            "nothing to propose: the scene is the same as the one open in the app. Edit a copy of \
             get_app_state's scene with the scene tools, then propose the result",
        ));
    }
    changes
        .check()
        .map_err(|e| ToolError::invalid(e.to_string()))?;
    changes
        .apply(base)
        .map_err(|e| ToolError::invalid(format!("the app could not apply this proposal: {e}")))?;
    let lines = changes.lines(&|id| model_name(base, proposed, id));
    Ok((changes, lines))
}

#[tool_router(router = proposal_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Propose a change to the scene open in the connected app, for the user to accept or reject there as a whole. Pass the app's scene (from get_app_state) edited with the scene tools, and a one-sentence summary. Nothing changes until the user accepts: never say the change is made before get_proposal says accepted. A new proposal replaces one still pending.",
        annotations(
            read_only_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn propose_changes(
        &self,
        Parameters(a): Parameters<ProposeArgs>,
    ) -> Result<Json<Proposed>, String> {
        propose(self, a).await.map(Json).map_err(|e| e.to_string())
    }

    #[tool(
        description = "Where a proposal stands: pending, accepted, rejected (with the user's reason), stale (the user changed something it touches first), superseded or withdrawn. The observation://app/state resource also changes when the user decides.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn get_proposal(
        &self,
        Parameters(a): Parameters<ProposalIdArgs>,
    ) -> Result<Json<ProposalStatusOut>, String> {
        status(self, a.id)
            .await
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Withdraw a proposal the user has not decided yet; the app stops offering it.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) async fn withdraw_proposal(
        &self,
        Parameters(a): Parameters<ProposalIdArgs>,
    ) -> Result<Json<ProposalStatusOut>, String> {
        if !self.link.is_connected() {
            return Err(ToolError::NotConnected.to_string());
        }
        self.link.withdraw(a.id).await.map_err(|e| e.to_string())?;
        status(self, a.id)
            .await
            .map(Json)
            .map_err(|e| e.to_string())
    }
}

async fn propose(mcp: &ObservationMcp, a: ProposeArgs) -> Result<Proposed, ToolError> {
    if !mcp.link.is_connected() {
        return Err(ToolError::NotConnected);
    }
    let summary = tidy_line(&a.summary, MAX_SUMMARY_CHARS).ok_or_else(|| {
        ToolError::invalid("give a summary: one sentence on what the change does and why")
    })?;
    // The app's latest scene is the base: what the user sees now.
    let snapshot = mcp.link.refresh().await?;
    let view = snapshot.view.ok_or(ToolError::NotConnected)?;
    let shared = view.state.as_ref().ok_or_else(|| {
        ToolError::invalid("the app has not shared a scene yet: wait until an area has loaded")
    })?;
    let (base, _) = edit::parse(&shared.scene)
        .and_then(|f| edit::finish(&f))
        .map_err(|e| ToolError::invalid(format!("the app's scene could not be read: {e}")))?;
    let (proposed, _) = edit::finish(&edit::parse(&a.scene)?)?;
    let (changes, lines) = change_set(&base, &proposed)?;
    let id = mcp.link.propose(&summary, view.revision, changes).await?;
    Ok(Proposed {
        id,
        status: "pending".to_owned(),
        summary,
        changes: lines,
        base_revision: view.revision,
        note: "The proposal is shown to the user in the app. Nothing has changed yet: the user \
               accepts or rejects it as a whole. Tell them it is waiting for their decision, and \
               check with get_proposal (or the observation://app/state resource)."
            .to_owned(),
    })
}

async fn status(mcp: &ObservationMcp, id: u64) -> Result<ProposalStatusOut, ToolError> {
    if !mcp.link.is_connected() {
        return Err(ToolError::NotConnected);
    }
    // Fresh when the server answers; what was last read when it does not.
    let snapshot = match mcp.link.refresh().await {
        Ok(s) => s,
        Err(_) => mcp.link.snapshot(),
    };
    snapshot
        .view
        .iter()
        .flat_map(|v| &v.proposals)
        .find(|o| o.id == id)
        .map(ProposalStatusOut::of)
        .ok_or_else(|| {
            ToolError::invalid(format!(
                "no proposal {id} in this connection (the app keeps the latest 16)"
            ))
        })
}
