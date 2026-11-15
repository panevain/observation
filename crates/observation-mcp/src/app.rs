//! The tools that connect to the user's open app (see [`crate::link`]).

use crate::ObservationMcp;
use crate::ToolError;
use crate::edit;
use crate::geometry::m1;
use crate::link::Snapshot;
use crate::schema::JsonObject;
use observation_agent_api::SharedState;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConnectArgs {
    /// The address the app is open at on this computer, such as
    /// `http://127.0.0.1:8080` (plain http on 127.0.0.1, `[::1]` or localhost).
    pub url: String,
    /// The code shown in the app's Connect agent dialog, such as `ABCD-EFGH`.
    pub code: String,
}

/// A point the user shared with Share with agent.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PinnedOut {
    /// Metres east of the area's origin.
    #[serde(serialize_with = "crate::geometry::widened::one")]
    pub x: f32,
    /// Metres south of the area's origin.
    #[serde(serialize_with = "crate::geometry::widened::one")]
    pub z: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    /// Grows each time the user shares a point.
    pub seq: u64,
}

/// What the user has open in the app.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct AppStateOut {
    /// Whether an app is connected.
    pub connected: bool,
    /// The app's server, while connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Why the last connection ended, if it has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    /// Grows each time the app shares a change; 0 until it has shared anything.
    pub revision: u64,
    /// What the app's scene holds, in one line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_summary: Option<String>,
    /// The app's scene file (checked as an imported file is); left out in
    /// the small profiles. To edit it, call open_scene with from "app".
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<JsonObject>")]
    pub scene: Option<Value>,
    /// Why the shared scene could not be read, if it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_problem: Option<String>,
    /// The scene's name in the app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_name: Option<String>,
    /// The loaded area: `{"kind": "demo"}` or `{"kind": "osm", "bbox": {...}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<JsonObject>")]
    pub area: Option<Value>,
    /// The ids of the cameras the user has selected.
    pub selection: Vec<u32>,
    /// The point the user shared last, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<PinnedOut>,
    /// Where this connection's latest proposals stand (see propose_changes),
    /// oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<crate::propose::ProposalStatusOut>,
}

impl AppStateOut {
    /// The state as `mcp`'s profile reports it: without the scene file in
    /// the small profiles, whose models open it with open_scene instead.
    pub fn for_profile(mut self, mcp: &ObservationMcp) -> Self {
        if mcp.profile().compact() {
            self.scene = None;
        }
        self
    }

    pub fn of(s: Snapshot) -> Self {
        let mut out = Self {
            connected: s.server.is_some(),
            server: s.server,
            ended: s.ended,
            ..Self::default()
        };
        let Some(view) = s.view else {
            return out;
        };
        out.revision = view.revision;
        out.proposals = view
            .proposals
            .iter()
            .map(crate::propose::ProposalStatusOut::of)
            .collect();
        let Some(state) = view.state else {
            return out;
        };
        match edit::parse(&state.scene).and_then(|f| edit::finish(&f)) {
            Ok((file, scene)) => {
                out.scene_summary = Some(crate::store::describe(&file));
                out.scene = Some(scene);
            }
            Err(e) => out.scene_problem = Some(e.to_string()),
        }
        out.scene_name = state.scene_name;
        out.area = state.area.and_then(|a| serde_json::to_value(a).ok());
        out.selection = state.selection;
        out.pinned = state.pinned.map(|p| PinnedOut {
            x: p.x,
            z: p.z,
            lat: p.lat,
            lon: p.lon,
            seq: p.seq,
        });
        out
    }
}

/// Longest a wait tool waits, seconds: well within a chat client's patience
/// and the HTTP transport's request timeout.
pub const MAX_WAIT_S: u64 = 25;
/// How long a wait tool waits when not told, seconds.
pub const DEFAULT_WAIT_S: u64 = 20;
/// Most change lines wait_for_app_change lists for the scene.
const MAX_SCENE_LINES: usize = 5;

/// A wait, in seconds, within 1 to [`MAX_WAIT_S`].
pub(crate) fn wait_for(timeout_s: Option<u64>) -> std::time::Duration {
    std::time::Duration::from_secs(timeout_s.unwrap_or(DEFAULT_WAIT_S).clamp(1, MAX_WAIT_S))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitChangeArgs {
    /// The revision you last saw (from get_app_state or a wait); omit to
    /// wait for the next change.
    pub since: Option<u64>,
    /// Seconds to wait, 1 to 25 (default 20).
    pub timeout_s: Option<u64>,
}

/// What changed in the app.
#[derive(Debug, Serialize, JsonSchema)]
pub struct AppChange {
    /// Whether anything changed.
    pub changed: bool,
    /// The app's state revision now: pass it as since next time.
    pub revision: u64,
    /// What changed, one line each.
    pub changes: Vec<String>,
    pub summary: String,
}

/// The lines that say how `after` differs from `before`.
fn change_lines(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    let mut lines = Vec::new();
    if after.server.is_none() {
        let why = after.ended.as_deref().unwrap_or("the connection ended");
        lines.push(format!("the connection ended: {why}"));
        return lines;
    }
    let state = |s: &Snapshot| s.view.as_ref().and_then(|v| v.state.clone());
    let (old, new) = (state(before), state(after));
    match (&old, &new) {
        (None, Some(n)) => {
            lines.push("the app shared its scene".to_owned());
            if let Ok(file) = edit::parse(&n.scene) {
                lines.push(format!("scene: {}", crate::store::describe(&file)));
            }
        }
        (Some(o), Some(n)) => {
            if o.area != n.area {
                lines.push("the user loaded another area".to_owned());
            } else if o.scene != n.scene {
                lines.extend(scene_lines(&o.scene, &n.scene));
            }
            if o.scene_name != n.scene_name {
                let name = n.scene_name.as_deref().unwrap_or("no name");
                lines.push(format!("the scene is now called {name}"));
            }
            if o.selection != n.selection {
                lines.push(selection_line(&n.selection));
            }
            let seq = |s: &SharedState| s.pinned.as_ref().map(|p| p.seq);
            if seq(o) != seq(n)
                && let Some(p) = &n.pinned
            {
                lines.push(format!(
                    "the user shared the point [{}, {}]",
                    m1(p.x),
                    m1(p.z)
                ));
            }
        }
        _ => {}
    }
    let outcomes = |s: &Snapshot| {
        s.view
            .as_ref()
            .map(|v| v.proposals.clone())
            .unwrap_or_default()
    };
    let old_outcomes = outcomes(before);
    for o in outcomes(after) {
        let was = old_outcomes.iter().find(|p| p.id == o.id).map(|p| p.status);
        if was != Some(o.status) {
            let out = crate::propose::ProposalStatusOut::of(&o);
            lines.push(format!("proposal {} is {}", out.id, out.status));
        }
    }
    lines
}

fn selection_line(selection: &[u32]) -> String {
    match selection {
        [] => "the user selected no cameras".to_owned(),
        [one] => format!("the user selected camera {one}"),
        many => {
            let ids: Vec<String> = many.iter().map(ToString::to_string).collect();
            format!("the user selected cameras {}", ids.join(", "))
        }
    }
}

/// The scene's changes in plain lines, a few at most.
fn scene_lines(old: &Value, new: &Value) -> Vec<String> {
    let changed = || vec!["the scene changed".to_owned()];
    let (Ok(o), Ok(n)) = (edit::parse(old), edit::parse(new)) else {
        return changed();
    };
    let Some((mut lines, total)) = crate::store::change_lines(&o, &n) else {
        return changed();
    };
    lines.truncate(MAX_SCENE_LINES);
    let mut out: Vec<String> = lines.into_iter().map(|l| format!("scene: {l}")).collect();
    if total > out.len() {
        out.push(format!("scene: and {} more changes", total - out.len()));
    }
    if out.is_empty() {
        return changed();
    }
    out
}

/// What the app shares now, in lines, for a change that came before the
/// wait began.
fn state_lines(s: &Snapshot) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(state) = s.view.as_ref().and_then(|v| v.state.as_ref()) {
        if let Ok(file) = edit::parse(&state.scene) {
            lines.push(format!("scene: {}", crate::store::describe(&file)));
        }
        lines.push(selection_line(&state.selection));
        if let Some(p) = &state.pinned {
            lines.push(format!(
                "the last point shared is [{}, {}]",
                m1(p.x),
                m1(p.z)
            ));
        }
    }
    lines
}

fn revision(s: &Snapshot) -> u64 {
    s.view.as_ref().map_or(0, |v| v.revision)
}

impl ObservationMcp {
    async fn wait_change(&self, a: WaitChangeArgs) -> Result<AppChange, ToolError> {
        if !self.link.is_connected() {
            return Err(ToolError::NotConnected);
        }
        let mut changes = self.link.subscribe();
        changes.borrow_and_update();
        let before = self.link.refresh().await?;
        let now = revision(&before);
        if let Some(since) = a.since
            && now > since
            && before.server.is_some()
        {
            return Ok(AppChange {
                changed: true,
                revision: now,
                changes: state_lines(&before),
                summary: format!(
                    "the app changed after revision {since} and is at revision {now}; the lines \
                     say what it shows now"
                ),
            });
        }
        let wait = wait_for(a.timeout_s);
        let waited = tokio::time::timeout(wait, async {
            loop {
                if changes.changed().await.is_err() {
                    return;
                }
                let s = self.link.snapshot();
                if s.server.is_none() || revision(&s) != now {
                    return;
                }
            }
        })
        .await;
        let after = self.link.snapshot();
        let lines = if waited.is_ok() {
            change_lines(&before, &after)
        } else {
            Vec::new()
        };
        let revision = revision(&after).max(now);
        Ok(if lines.is_empty() {
            AppChange {
                changed: false,
                revision,
                changes: Vec::new(),
                summary: format!(
                    "no change in {} s; the app is at revision {revision}",
                    wait.as_secs()
                ),
            }
        } else {
            AppChange {
                changed: true,
                revision,
                summary: format!("{} (revision {revision})", lines.join("; ")),
                changes: lines,
            }
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Disconnected {
    /// Whether an app was connected.
    pub was_connected: bool,
}

#[tool_router(router = app_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Connect to the Observation app open on this computer, with its address and the code from its Connect agent dialog. Then every tool works on the user's real area.",
        annotations(
            read_only_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn connect_app(
        &self,
        Parameters(a): Parameters<ConnectArgs>,
    ) -> Result<Json<AppStateOut>, String> {
        self.link
            .connect(&a.url, &a.code)
            .await
            .map(|s| Json(AppStateOut::of(s).for_profile(self)))
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Wait up to timeout_s seconds (at most 25) for the user to change something in the connected app. Returns the new revision and what changed: the scene, the selection, a shared point or a proposal's status.",
        annotations(
            read_only_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn wait_for_app_change(
        &self,
        Parameters(a): Parameters<WaitChangeArgs>,
    ) -> Result<Json<AppChange>, String> {
        self.wait_change(a)
            .await
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Disconnect from the Observation app.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) async fn disconnect_app(&self) -> Json<Disconnected> {
        Json(Disconnected {
            was_connected: self.link.disconnect().await,
        })
    }

    #[tool(
        description = "What the user has open in the connected app: a summary of the scene, its area, the selected cameras, a shared point and proposals. To edit the scene, call open_scene with from \"app\".",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn get_app_state(&self) -> Result<Json<AppStateOut>, String> {
        if !self.link.is_connected() {
            let out = AppStateOut::of(self.link.snapshot());
            return match out.ended {
                Some(_) => Ok(Json(out)),
                None => Err(crate::ToolError::NotConnected.to_string()),
            };
        }
        self.link
            .refresh()
            .await
            .map(|s| Json(AppStateOut::of(s).for_profile(self)))
            .map_err(|e| e.to_string())
    }
}
