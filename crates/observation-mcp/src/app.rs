//! The tools that connect to the user's open app (see [`crate::link`]).

use crate::ObservationMcp;
use crate::edit;
use crate::link::Snapshot;
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
    pub x: f32,
    /// Metres south of the area's origin.
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
    /// The scene open in the app, as the scene tools take it (checked as an
    /// imported file is). Edits made with the tools do not reach the app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<Value>,
    /// Why the shared scene could not be read, if it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_problem: Option<String>,
    /// The scene's name in the app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_name: Option<String>,
    /// The loaded area: `{"kind": "demo"}` or `{"kind": "osm", "bbox": {...}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
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
            Ok((_, scene)) => out.scene = Some(scene),
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

#[derive(Debug, Serialize, JsonSchema)]
pub struct Disconnected {
    /// Whether an app was connected.
    pub was_connected: bool,
}

#[tool_router(router = app_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Connect to the Observation app the user has open on this computer, with the code from its Connect agent dialog. Then get_app_state reads their open scene, selection and shared point, and coverage and address tools work on their real area.",
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
            .map(|s| Json(AppStateOut::of(s)))
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
        description = "Read what the user has open in the connected Observation app: the scene (pass it to the scene tools), its area, the selected cameras and the point they shared. Subscribe to observation://app/state to hear of changes.",
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
            .map(|s| Json(AppStateOut::of(s)))
            .map_err(|e| e.to_string())
    }
}
