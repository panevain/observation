//! The MCP tools, resources and prompts.
//!
//! The scene tools return an edited copy of the scene they were given, for
//! the agent to pass to the next call or save for the app to import; given a
//! working scene's `scene_id` instead, they store the edit as its next
//! version ([`crate::store`]). The
//! app tools ([`crate::app`]) open and close the one connection the server
//! holds, to the user's open app.

use crate::ToolError;
use crate::area::{self, extent};
use crate::catalog::{catalog, is_custom, model};
use crate::edit::{self, CameraUpdate, Kind, ModelUpdate, NewModel, ParamsUpdate, Place, Target};
use crate::figures::scene_figures;
use crate::link::{AppLink, STATE_URI};
use crate::schema::JsonObject;
use observation_core::address::ON_OUTLINE_M;
use observation_core::camera::{ASSUMED_IR_RANGE_M, CameraKind, CameraModel, DoriLevel, Lighting};
use observation_core::provenance::{CAVEATS_SUMMARY, caveats_text, format_date};
use observation_core::scene::AreaRef;
use observation_core::scene::SceneFile;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListResourcesResult,
    PaginatedRequestParams, PromptMessage, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ResourceUpdatedNotificationParam, Role, ServerCapabilities, ServerConfig,
    SubscribeRequestParams, SubscriptionFilter, UnsubscribeRequestParams,
};
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, prompt, prompt_handler, prompt_router, tool,
    tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, OnceLock};

const INSTRUCTIONS: &str = "\
Observation estimates how much of a street home security cameras can see. \
Start with open_scene: it gives a scene_id such as \"s1\", which you pass as scene_id to every \
scene tool, while the server keeps each edited version (undo, diff_scenes, apply_edits for \
several edits at once, export_scene for the file). Without an id, pass the value of the `scene` \
field of one tool's result as `scene` to the next. The result is a scene file the \
Observation app can import. Positions are metres from the area's origin: x east, z south \
(north is negative z); heights are metres above the ground. Bearings are degrees clockwise \
from north. Offline only the demo street is available, so load_demo_area gives the area's \
extent. If the user has the Observation app open on this computer, they can connect it: ask \
for the code in its Connect agent dialog and call connect_app with the app's address (such as \
http://127.0.0.1:8080). Then get_app_state returns their open scene (pass it to the scene \
tools), its area, the selected cameras and a point they shared, the resource \
observation://app/state follows it, and every tool works on their real area. Edits \
made here do not change the app by themselves. To suggest a change to the open scene, edit a \
copy of get_app_state's scene with the scene tools and call propose_changes with the result and \
a one-sentence summary: the user reviews it in the app and accepts or rejects it as a whole. It \
is only a proposal until get_proposal (or observation://app/state) says accepted, so never say \
it has been applied before then; if it comes back stale, read the state again and propose \
afresh. Without a connection, give the user the scene to import. \
Places have names the user and you share: label_area marks ground the user drew as their own, \
a neighbour's, public or other (as drawn, never property records), and add_target marks a \
named target such as a front door. camera_split says where each camera's footage lands, \
target_verdicts and who_sees_point say whether the model rules out that a camera sees a place \
and why, and find_gaps lists the largest stretches of street no camera sees. Verdicts are \
one-sided: say \"not ruled out\" or \"ruled out: <reason>\", never that a camera sees \
something. Name buildings by index and distance, never by a neighbour's address. \
To get a sense of the scene without guessing coordinates, call scene_summary, \
describe_surroundings, list_buildings, list_streets and camera_view, and suggest_mount_points \
for wall mounts to pass to place_camera. \
To see the scene, call render_map: a north-up character map with a legend (format \"image\" \
adds a picture), zoomed with center [x, z] and radius_m. \
To choose where a camera goes or how to aim it, call suggest_placement, optimise_aim or \
cover_target instead of trying positions yourself: they search on the server and return the \
model's estimates. \
Coverage figures are estimates that leave out trees, fences and vehicles: always pass on \
the data date and the caveats the tools return.";

const USAGE_MD: &str = include_str!("../../../docs/usage.md");
const CAMERA_CATALOG_MD: &str = include_str!("../../../docs/camera-catalog.md");

/// The Observation MCP server. Its state is the connection to the user's
/// app ([`AppLink`]) and the working scenes ([`crate::store::SceneStore`]);
/// clones share both.
#[derive(Debug, Clone, Default)]
pub struct ObservationMcp {
    pub(crate) link: Arc<AppLink>,
    pub(crate) scenes: Arc<crate::store::SceneStore>,
    /// Served without sessions (stateless HTTP), so a legacy
    /// `resources/subscribe` would have nowhere to send its notifications.
    sessionless: bool,
}

impl ObservationMcp {
    pub fn new() -> Self {
        Self::default()
    }

    /// A server for a transport without sessions (stateless Streamable
    /// HTTP): it refuses the legacy `resources/subscribe`, whose
    /// notifications would be lost, and points to `subscriptions/listen`.
    pub fn sessionless() -> Self {
        Self {
            sessionless: true,
            ..Self::default()
        }
    }

    /// The connection to the user's app.
    pub fn link(&self) -> &Arc<AppLink> {
        &self.link
    }

    /// Runs `tool` as a call with `arguments` runs: the area of their scene
    /// is fetched first if it is a real one and an app is connected, and the
    /// connection's areas are in view while it runs (see [`area::bundle_for`]).
    pub async fn in_tool_scope<F: std::future::Future>(
        &self,
        arguments: Option<&serde_json::Map<String, Value>>,
        tool: F,
    ) -> F::Output {
        if let Some(area) = scene_area(arguments) {
            self.link.prepare_area(&area).await;
        }
        area::scope_areas(self.link.areas(), tool).await
    }

    /// Every tool, with `scene_id` documented on each that takes a scene
    /// ([`crate::store::document_scene_ids`]). Built once, on first use.
    pub fn all_tools() -> &'static ToolRouter<Self> {
        static TOOLS: OnceLock<ToolRouter<ObservationMcp>> = OnceLock::new();
        TOOLS.get_or_init(|| {
            let tools = Self::tool_router()
                + Self::app_tool_router()
                + Self::places_tool_router()
                + Self::query_tool_router()
                + Self::proposal_tool_router()
                + Self::store_tool_router()
                + Self::describe_tool_router()
                + Self::render_tool_router()
                + Self::goal_tool_router();
            crate::store::document_scene_ids(tools)
        })
    }
}

/// The area of the `scene` argument of a tool call, if it has one.
fn scene_area(arguments: Option<&serde_json::Map<String, Value>>) -> Option<AreaRef> {
    let scene = arguments?.get("scene")?;
    let area = match scene {
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()?
            .get("area")?
            .clone(),
        other => other.get("area")?.clone(),
    };
    serde_json::from_value(area).ok()
}

pub(crate) fn fail(e: ToolError) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Tool inputs and outputs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SceneArg {
    /// A scene file: the value of the `scene` field of the last scene tool's
    /// result, or a file saved from the app (as a JSON object or a string).
    #[schemars(with = "JsonObject")]
    pub scene: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListModelsArgs {
    /// Only models of this kind.
    pub kind: Option<Kind>,
    /// Only models by this manufacturer (case-insensitive).
    pub vendor: Option<String>,
    /// A scene whose custom models to list as well.
    #[schemars(with = "Option<JsonObject>")]
    pub scene: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetModelArgs {
    /// Catalogue or custom model id.
    pub id: String,
    /// The scene that holds the model, for a custom one.
    #[schemars(with = "Option<JsonObject>")]
    pub scene: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CameraArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The camera's id in the scene.
    pub id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaceArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    #[serde(flatten)]
    pub camera: Place,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MoveArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The camera's id in the scene.
    pub id: u32,
    /// New metres east of the origin; omit to keep.
    pub x: Option<f32>,
    /// New metres south of the origin; omit to keep.
    pub z: Option<f32>,
    /// New height above the ground, metres; omit to keep the current one.
    pub height_m: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AimArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The camera's id in the scene.
    pub id: u32,
    /// Compass bearing, degrees clockwise from north.
    pub yaw_deg: Option<f32>,
    /// Tilt, degrees from -90 (straight down) to 90.
    pub pitch_deg: Option<f32>,
    /// Rotation about the view axis, degrees.
    pub roll_deg: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PointArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The camera's id in the scene.
    pub id: u32,
    /// The spot to point at.
    pub target: Target,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateCameraArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The camera's id in the scene.
    pub id: u32,
    #[serde(flatten)]
    pub changes: CameraUpdate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExampleArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// How many cameras to add (1 to 20).
    pub count: usize,
    /// Models to cycle through; omit for the app's example models (a mix of
    /// types and brands).
    #[serde(default)]
    pub model_ids: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NewModelArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    pub model: NewModel,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateModelArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The custom model's id (`custom-...`).
    pub id: String,
    pub changes: ModelUpdate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModelIdArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The custom model's id (`custom-...`).
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ParamsArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    #[serde(flatten)]
    pub params: ParamsUpdate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PointQueryArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// Metres east of the origin.
    pub x: f32,
    /// Metres south of the origin.
    pub z: f32,
}

/// The result of a scene tool.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Edited {
    /// What changed.
    pub summary: String,
    /// The camera placed or changed, if one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_id: Option<u32>,
    /// The cameras added, if several were.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_ids: Option<Vec<u32>>,
    /// The custom model created or changed, if one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// The edited scene file: pass this field's value as `scene` to the next
    /// scene tool.
    #[schemars(with = "JsonObject")]
    pub scene: Value,
}

impl Edited {
    fn new(summary: impl Into<String>, scene: &SceneFile) -> Result<Self, ToolError> {
        let (_, scene) = edit::finish(scene)?;
        Ok(Self {
            summary: summary.into(),
            camera_id: None,
            camera_ids: None,
            model_id: None,
            scene,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RangeRow {
    pub level: String,
    /// Pixel density, px/m.
    pub ppm: f32,
    /// Range by day, metres.
    pub day_m: f32,
    /// Range at night (limited to the infrared range), metres.
    pub night_m: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ModelInfo {
    pub id: String,
    pub vendor: String,
    pub name: String,
    pub kind: String,
    pub hfov_deg: f32,
    pub vfov_deg: f32,
    /// `[width, height]`, pixels.
    pub resolution: [u32; 2],
    /// Advertised night-vision range, metres.
    pub ir_range_m: Option<f32>,
    /// Whether night ranges assume the default infrared range because the
    /// maker publishes none.
    pub ir_range_assumed: bool,
    /// Whether the model is one of the scene's custom models.
    pub custom: bool,
    /// Range at each standard detail level.
    pub ranges: Vec<RangeRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl ModelInfo {
    fn of(m: &CameraModel) -> Self {
        let ranges = DoriLevel::ALL
            .iter()
            .filter_map(|&l| {
                let ppm = l.pixels_per_metre()?;
                Some(RangeRow {
                    level: l.label().to_owned(),
                    ppm,
                    day_m: m.effective_range_m(ppm, Lighting::Day),
                    night_m: m.effective_range_m(ppm, Lighting::Night),
                })
            })
            .collect();
        Self {
            id: m.id.clone(),
            vendor: m.vendor.clone(),
            name: m.name.clone(),
            kind: m.kind.label().to_owned(),
            hfov_deg: m.hfov_deg,
            vfov_deg: m.vfov_deg,
            resolution: m.resolution,
            ir_range_m: m.ir_range_m,
            ir_range_assumed: m.ir_range_m.is_none(),
            custom: is_custom(&m.id),
            ranges,
            source: m.source.clone(),
            notes: m.notes.clone(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ModelList {
    /// The catalogue's version, as a saved scene records it.
    pub catalog_fingerprint: String,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AreaInfo {
    /// The area's centre, which positions are measured from.
    pub origin_lat: f64,
    pub origin_lon: f64,
    /// Smallest `[x, z]` inside the area, metres.
    pub extent_min: [f32; 2],
    /// Largest `[x, z]` inside the area, metres.
    pub extent_max: [f32; 2],
    pub building_count: usize,
    /// Named streets in the area.
    pub streets: Vec<String>,
    /// When the OpenStreetMap data was current.
    pub osm_data_as_of: Option<String>,
    pub attribution: String,
    pub elevation_attribution: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Validated {
    /// The scene as the app would load it (tidied where it can be).
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    pub cameras: usize,
    pub custom_models: usize,
    /// Cameras whose model is in neither the catalogue nor the scene.
    pub cameras_with_unknown_model: Vec<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LevelOut {
    pub level: String,
    /// Pixel density, px/m.
    pub ppm: f32,
    /// Street surface (roads, driveways and footpaths), m².
    pub street_m2: f32,
    pub street_seen_m2: f32,
    /// Street seen by at least two cameras, m².
    pub street_seen_multi_m2: f32,
    /// Any open ground seen, m².
    pub open_ground_seen_m2: f32,
    /// Building walls, m².
    pub facade_m2: f32,
    pub facade_seen_m2: f32,
    pub street_fraction: f32,
    pub street_multi_fraction: f32,
    pub facade_fraction: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SkippedOut {
    pub id: u32,
    pub reason: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CoverageOut {
    pub lighting: String,
    pub subject_height_m: f32,
    /// The level the scene has selected.
    pub selected_level: String,
    /// Whether the figures are limited to the scene's area for the figures
    /// (its `limit_figures_to`, which an old single `region` becomes).
    pub region_limited: bool,
    pub cameras_counted: Vec<u32>,
    pub cameras_skipped: Vec<SkippedOut>,
    /// Figures per detail level, least detail first.
    pub levels: Vec<LevelOut>,
    /// When the OpenStreetMap data was current, if known.
    pub osm_data_as_of: Option<String>,
    /// One line to show with the figures.
    pub caveats_summary: String,
    /// Everything the figures leave out; pass it on.
    pub caveats: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddressOut {
    /// The address there, if OpenStreetMap has one nearby.
    pub address: Option<String>,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl ObservationMcp {
    #[tool(
        description = "List camera models from the catalogue (and a scene's custom models), with fields of view, resolution and range at each detail level.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn list_camera_models(
        &self,
        Parameters(a): Parameters<ListModelsArgs>,
    ) -> Result<Json<ModelList>, String> {
        let cat = catalog().map_err(fail)?;
        let scene = a
            .scene
            .as_ref()
            .map(edit::parse)
            .transpose()
            .map_err(fail)?;
        let kind = a.kind.map(CameraKind::from);
        let vendor = a.vendor.map(|v| v.to_lowercase());
        let customs = scene.iter().flat_map(|s| &s.custom_models);
        let models = cat
            .models
            .iter()
            .chain(customs)
            .filter(|m| kind.is_none_or(|k| m.kind == k))
            .filter(|m| {
                vendor
                    .as_ref()
                    .is_none_or(|v| m.vendor.to_lowercase() == *v)
            })
            .map(ModelInfo::of)
            .collect();
        Ok(Json(ModelList {
            catalog_fingerprint: cat.fingerprint(),
            models,
        }))
    }

    #[tool(
        description = "Get one camera model by id, with its range at each detail level and its spec source.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn get_camera_model(
        &self,
        Parameters(a): Parameters<GetModelArgs>,
    ) -> Result<Json<ModelInfo>, String> {
        let cat = catalog().map_err(fail)?;
        let scene = a
            .scene
            .as_ref()
            .map(edit::parse)
            .transpose()
            .map_err(fail)?;
        model(cat, scene.as_ref(), &a.id)
            .map(|m| Json(ModelInfo::of(m)))
            .ok_or_else(|| fail(ToolError::NoModel(a.id)))
    }

    #[tool(
        description = "Describe the demo street: its extent in metres (where cameras can go), building count, street names and data date.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn load_demo_area(&self) -> Result<Json<AreaInfo>, String> {
        let b = area::demo().map_err(fail)?;
        let (extent_min, extent_max) = extent(b);
        let mut streets: Vec<String> = b.roads.iter().filter_map(|r| r.name.clone()).collect();
        streets.sort();
        streets.dedup();
        Ok(Json(AreaInfo {
            origin_lat: b.origin.lat,
            origin_lon: b.origin.lon,
            extent_min,
            extent_max,
            building_count: b.buildings.len(),
            streets,
            osm_data_as_of: b.osm_base_unix.map(format_date),
            attribution: b.attribution.clone(),
            elevation_attribution: b.terrain.attribution.clone(),
        }))
    }

    #[tool(
        description = "Create a new, empty scene on the demo street, kept as a working scene. Prefer passing its scene_id (such as \"s1\") to the scene tools; the scene file is also in the `scene` field of the result.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn create_scene(&self) -> Result<Json<crate::store::Created>, String> {
        self.create().map(Json).map_err(fail)
    }

    #[tool(
        description = "Check a scene file the way the app checks one it imports; the `scene` field of the result holds it as the app would load it.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn validate_scene(
        &self,
        Parameters(a): Parameters<SceneArg>,
    ) -> Result<Json<Validated>, String> {
        let cat = catalog().map_err(fail)?;
        let s = edit::parse(&a.scene).map_err(fail)?;
        let unknown = s
            .cameras
            .iter()
            .filter(|c| model(cat, Some(&s), &c.model_id).is_none())
            .map(|c| c.id)
            .collect();
        let (s, scene) = edit::finish(&s).map_err(fail)?;
        Ok(Json(Validated {
            scene,
            cameras: s.cameras.len(),
            custom_models: s.custom_models.len(),
            cameras_with_unknown_model: unknown,
        }))
    }

    #[tool(
        description = "Set the scene's analysis settings: detail level, day or night, the height people are tested at, and the custom pixel density.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn set_coverage_params(
        &self,
        Parameters(a): Parameters<ParamsArgs>,
    ) -> Result<Json<Edited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        edit::set_coverage_params(&mut s, a.params);
        Edited::new("updated the analysis settings", &s)
            .map(Json)
            .map_err(fail)
    }

    #[tool(
        description = "Place a camera at [x, z] (metres east and south of the origin), mounted height_m above the ground, aimed by yaw and pitch or at a look_at spot.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn place_camera(&self, Parameters(a): Parameters<PlaceArgs>) -> Result<Json<Edited>, String> {
        let cat = catalog().map_err(fail)?;
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        let id = edit::place_camera(&mut s, cat, a.camera).map_err(fail)?;
        let mut out = Edited::new(format!("placed camera {id}"), &s).map_err(fail)?;
        out.camera_id = Some(id);
        Ok(Json(out))
    }

    #[tool(
        description = "Move a camera to a new [x, z] and/or height above the ground. Where it points is unchanged.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn move_camera(&self, Parameters(a): Parameters<MoveArgs>) -> Result<Json<Edited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        edit::move_camera(&mut s, a.id, a.x, a.z, a.height_m).map_err(fail)?;
        let mut out = Edited::new(format!("moved camera {}", a.id), &s).map_err(fail)?;
        out.camera_id = Some(a.id);
        Ok(Json(out))
    }

    #[tool(
        description = "Aim a camera: set any of its compass bearing (yaw), tilt (pitch) and roll, in degrees.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn aim_camera(&self, Parameters(a): Parameters<AimArgs>) -> Result<Json<Edited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        edit::aim_camera(&mut s, a.id, a.yaw_deg, a.pitch_deg, a.roll_deg).map_err(fail)?;
        let mut out = Edited::new(format!("aimed camera {}", a.id), &s).map_err(fail)?;
        out.camera_id = Some(a.id);
        Ok(Json(out))
    }

    #[tool(
        description = "Point a camera at a spot on the area: [x, z] and a height above the ground there (default: the scene's subject height).",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn point_camera_at(
        &self,
        Parameters(a): Parameters<PointArgs>,
    ) -> Result<Json<Edited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        edit::point_camera_at(&mut s, a.id, a.target).map_err(fail)?;
        let mut out = Edited::new(format!("pointed camera {}", a.id), &s).map_err(fail)?;
        out.camera_id = Some(a.id);
        Ok(Json(out))
    }

    #[tool(
        description = "Change a placed camera's label, model, enabled state or group.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn update_camera(
        &self,
        Parameters(a): Parameters<UpdateCameraArgs>,
    ) -> Result<Json<Edited>, String> {
        let cat = catalog().map_err(fail)?;
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        edit::update_camera(&mut s, cat, a.id, a.changes).map_err(fail)?;
        let mut out = Edited::new(format!("updated camera {}", a.id), &s).map_err(fail)?;
        out.camera_id = Some(a.id);
        Ok(Json(out))
    }

    #[tool(
        description = "Remove a placed camera from the scene.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn remove_camera(&self, Parameters(a): Parameters<CameraArgs>) -> Result<Json<Edited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        edit::remove_camera(&mut s, a.id).map_err(fail)?;
        Edited::new(format!("removed camera {}", a.id), &s)
            .map(Json)
            .map_err(fail)
    }

    #[tool(
        description = "Add example cameras on house fronts facing the street, clear of the cameras already in the scene, like those the app puts on a new demo street.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn add_example_cameras(
        &self,
        Parameters(a): Parameters<ExampleArgs>,
    ) -> Result<Json<Edited>, String> {
        let cat = catalog().map_err(fail)?;
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        let ids = edit::add_example_cameras(&mut s, cat, a.count.clamp(1, 20), &a.model_ids)
            .map_err(fail)?;
        let mut out =
            Edited::new(format!("added {} example cameras", ids.len()), &s).map_err(fail)?;
        out.camera_ids = Some(ids);
        Ok(Json(out))
    }

    #[tool(
        description = "Create a custom camera model in the scene from its specifications; returns its id for place_camera.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn create_custom_model(
        &self,
        Parameters(a): Parameters<NewModelArgs>,
    ) -> Result<Json<Edited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        let id = edit::create_custom_model(&mut s, a.model).map_err(fail)?;
        let mut out = Edited::new(format!("created custom model {id}"), &s).map_err(fail)?;
        out.model_id = Some(id);
        Ok(Json(out))
    }

    #[tool(
        description = "Change a custom camera model's specifications. Cameras using it keep using it.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn update_custom_model(
        &self,
        Parameters(a): Parameters<UpdateModelArgs>,
    ) -> Result<Json<Edited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        edit::update_custom_model(&mut s, &a.id, a.changes).map_err(fail)?;
        let mut out = Edited::new(format!("updated custom model {}", a.id), &s).map_err(fail)?;
        out.model_id = Some(a.id);
        Ok(Json(out))
    }

    #[tool(
        description = "Delete a custom camera model. Refused while cameras use it.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn delete_custom_model(
        &self,
        Parameters(a): Parameters<ModelIdArgs>,
    ) -> Result<Json<Edited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        edit::delete_custom_model(&mut s, &a.id).map_err(fail)?;
        Edited::new(format!("deleted custom model {}", a.id), &s)
            .map(Json)
            .map_err(fail)
    }

    #[tool(
        description = "Compute how much street and building wall the scene's enabled cameras see, at each detail level, with buildings and terrain blocking the view. Pass on the caveats.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn compute_coverage(
        &self,
        Parameters(a): Parameters<SceneArg>,
    ) -> Result<Json<CoverageOut>, String> {
        // Line of sight over the whole area is CPU work: keep it off the async runtime,
        // with the connection's real areas in view.
        let areas = area::current_areas();
        tokio::task::spawn_blocking(move || match areas {
            Some(areas) => area::with_areas(areas, || coverage(&a.scene)),
            None => coverage(&a.scene),
        })
        .await
        .map_err(|e| format!("coverage failed: {e}"))?
        .map(Json)
        .map_err(fail)
    }

    #[tool(
        description = "The OpenStreetMap address at [x, z]: the building there, or the nearest addressed one.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn address_at_point(
        &self,
        Parameters(a): Parameters<PointQueryArgs>,
    ) -> Result<Json<AddressOut>, String> {
        let s = edit::parse(&a.scene).map_err(fail)?;
        let b = area::bundle_for(&s).map_err(fail)?;
        area::check_inside(&b, a.x, a.z).map_err(fail)?;
        Ok(Json(AddressOut {
            address: b
                .address_pointed_at([a.x, a.z], ON_OUTLINE_M)
                .map(|ad| ad.label()),
        }))
    }
}

fn coverage(scene: &Value) -> Result<CoverageOut, ToolError> {
    let cat = catalog()?;
    let s = edit::parse(scene)?;
    let layers = area::layers_for(&s)?;
    let osm_data_as_of = area::bundle_for(&s)?.osm_base_unix.map(format_date);
    let f = scene_figures(&s, cat, &layers);
    let levels = f
        .levels
        .iter()
        .map(|l| LevelOut {
            level: l.level.label().to_owned(),
            ppm: l.ppm,
            street_m2: l.stats.street_m2,
            street_seen_m2: l.stats.street_seen_m2,
            street_seen_multi_m2: l.stats.street_seen_multi_m2,
            open_ground_seen_m2: l.stats.open_ground_seen_m2,
            facade_m2: l.stats.facade_m2,
            facade_seen_m2: l.stats.facade_seen_m2,
            street_fraction: l.stats.street_fraction(),
            street_multi_fraction: l.stats.street_multi_fraction(),
            facade_fraction: l.stats.facade_fraction(),
        })
        .collect();
    Ok(CoverageOut {
        lighting: format!("{:?}", s.params.lighting),
        subject_height_m: s.params.subject_height_m,
        selected_level: s.params.dori.label().to_owned(),
        region_limited: f.region_limited,
        cameras_counted: f.counted,
        cameras_skipped: f
            .skipped
            .into_iter()
            .map(|k| SkippedOut {
                id: k.id,
                reason: k.reason.to_owned(),
            })
            .collect(),
        levels,
        osm_data_as_of,
        caveats_summary: CAVEATS_SUMMARY.to_owned(),
        caveats: caveats_text(),
    })
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompareArgs {
    /// Model ids to compare, separated by commas.
    pub model_ids: String,
}

#[prompt_router]
impl ObservationMcp {
    #[prompt(
        name = "assess_street_coverage",
        description = "Set up cameras on the demo street and report what they see, with the caveats"
    )]
    fn assess_street_coverage(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            "Using the Observation tools: create a scene with create_scene, call load_demo_area \
             for the street's extent, then add cameras (add_example_cameras, or place_camera \
             where I describe). Run compute_coverage by day and again at night \
             (set_coverage_params). Report the share of street seen at each detail level, \
             by day and at night, in a short table, then quote the caveats_summary and \
             explain that the figures are estimates, read as \"up to\". Then run find_gaps \
             and describe the largest stretches of street no camera sees, by street name and \
             position (never by a neighbour's address). Finally give me the scene JSON so I can \
             import it into the Observation app.",
        )]
    }

    #[prompt(
        name = "where_footage_lands",
        description = "Mark my land, my neighbours' and the places I care about, then report where each camera's footage lands and whether the model rules out each place, with the caveats"
    )]
    fn where_footage_lands(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            "Using the Observation tools, help me see where my cameras' footage lands. Work on \
             my open scene if the app is connected (get_app_state), otherwise on a scene on the \
             demo street. Ask me which ground is mine and which is my neighbours', and mark \
             each with label_area (label own, neighbour or public, with a short name); the \
             outlines are as I describe them, not property records. Ask which places matter to \
             me, such as my front door, a side gate or a path, and mark each with add_target. \
             Then run camera_split and report for each camera, in a short table, the m² of \
             visible ground on my land, on each neighbour's, on the street, driveways and \
             footpaths as mapped (not all of it public) and elsewhere, as the low to high range beside each outline's own area. Run \
             target_verdicts and report each target camera by camera: say \"not ruled out\" \
             or \"ruled out\" with the reason, at each detail level by day and at night, and \
             never that a camera \"sees\" a place. Name buildings by index and distance, \
             never by address. Quote the data date and the caveats_summary, and explain that \
             hedges, fences, trees and vehicles are not modelled and can only block more. \
             Finally give me the scene JSON so I can import it into the Observation app.",
        )]
    }

    #[prompt(
        name = "compare_cameras",
        description = "Compare camera models' fields of view, resolution and range"
    )]
    fn compare_cameras(&self, Parameters(a): Parameters<CompareArgs>) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Using get_camera_model for each of these Observation camera models: {}. \
                 Compare them in a table: field of view, resolution, night-vision range \
                 (say when it is assumed), and range at each detail level by day and at night. \
                 Note that ranges come from published specifications, not from testing.",
                a.model_ids
            ),
        )]
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// The resources: `(uri, name, MIME type, description)`.
const RESOURCES: [(&str, &str, &str, &str); 4] = [
    (
        "observation://catalog",
        "camera-catalog",
        "application/json",
        "The camera catalogue as JSON, with its fingerprint",
    ),
    (
        "observation://caveats",
        "caveats",
        "text/plain",
        "What the coverage figures leave out and how to read them",
    ),
    (
        "observation://docs/usage",
        "usage",
        "text/markdown",
        "Using Observation: cameras, detail levels, scene files, limitations and accuracy",
    ),
    (
        "observation://docs/camera-catalog",
        "camera-catalog-docs",
        "text/markdown",
        "Where the camera data comes from and how ranges are derived",
    ),
];

fn read(uri: &str) -> Result<String, ErrorData> {
    match uri {
        "observation://catalog" => {
            let cat = catalog().map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            let body = serde_json::json!({
                "fingerprint": cat.fingerprint(),
                "assumed_ir_range_m": ASSUMED_IR_RANGE_M,
                "models": cat.models,
            });
            Ok(serde_json::to_string_pretty(&body).unwrap_or_default())
        }
        "observation://caveats" => Ok(format!("{CAVEATS_SUMMARY}\n\n{}", caveats_text())),
        "observation://docs/usage" => Ok(USAGE_MD.to_owned()),
        "observation://docs/camera-catalog" => Ok(CAMERA_CATALOG_MD.to_owned()),
        _ => Err(ErrorData::resource_not_found(
            format!("no resource {uri}"),
            None,
        )),
    }
}

/// The resource the connected app's state is read from.
const STATE_RESOURCE: (&str, &str, &str, &str) = (
    STATE_URI,
    "app-state",
    "application/json",
    "What the user has open in the connected app (as get_app_state returns it); subscribe to \
     hear when it changes",
);

impl ObservationMcp {
    async fn app_state_json(&self) -> String {
        // Fresh when the server answers; what was last read when it does not.
        let snapshot = match self.link.refresh().await {
            Ok(s) => s,
            Err(_) => self.link.snapshot(),
        };
        serde_json::to_string_pretty(&crate::app::AppStateOut::of(snapshot)).unwrap_or_default()
    }
}

#[tool_handler(router = Self::all_tools())]
#[prompt_handler]
impl ServerHandler for ObservationMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_prompts()
                .build(),
        )
        .with_server_info(
            Implementation::new("observation-mcp", env!("CARGO_PKG_VERSION"))
                .with_title("Observation"),
        )
        .with_instructions(INSTRUCTIONS)
    }

    /// Reads a `scene_id` from the working scenes ([`crate::store`]), fetches
    /// the area of the call's scene from the connected app's server first,
    /// if it is a real area, then runs the tool with the connection's areas
    /// in view (see [`area::bundle_for`]) and stores an edited scene.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.call_with_scenes(request, context).await
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let wants = requested
            .resource_subscriptions
            .as_ref()
            .is_some_and(|uris| uris.iter().any(|u| u == STATE_URI));
        wants.then(|| {
            SubscriptionFilter::builder()
                .resource_subscription(STATE_URI)
                .build()
        })
    }

    /// Sends `notifications/resources/updated` for the app's state whenever
    /// it changes, until the subscription is cancelled.
    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let mut changes = self.link.subscribe();
        changes.borrow_and_update();
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                changed = changes.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    if context.sink().notify_resource_updated(STATE_URI).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// The legacy form of [`Self::listen`], for clients on older protocol
    /// versions.
    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        if request.uri != STATE_URI {
            return Err(ErrorData::resource_not_found(
                format!(
                    "{} does not change, so there is nothing to subscribe to",
                    request.uri
                ),
                None,
            ));
        }
        if self.sessionless {
            return Err(ErrorData::invalid_request(
                "this server has no sessions to send notifications on: use subscriptions/listen \
                 (MCP 2026-07-28), or read observation://app/state or get_app_state again",
                None,
            ));
        }
        let mut changes = self.link.subscribe();
        changes.borrow_and_update();
        let peer = context.peer;
        let task = tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let note = ResourceUpdatedNotificationParam::new(STATE_URI);
                if peer.notify_resource_updated(note).await.is_err() {
                    return;
                }
            }
        });
        self.link.set_legacy_subscription(Some(task.abort_handle()));
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.link.set_legacy_subscription(None);
        Ok(())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = RESOURCES
            .iter()
            .chain(std::iter::once(&STATE_RESOURCE))
            .map(|(uri, name, mime, about)| {
                Resource::new(*uri, *name)
                    .with_mime_type(*mime)
                    .with_description(*about)
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, ErrorData> {
        let text = if request.uri == STATE_URI {
            self.app_state_json().await
        } else {
            read(&request.uri)?
        };
        let mime = RESOURCES
            .iter()
            .chain(std::iter::once(&STATE_RESOURCE))
            .find(|r| r.0 == request.uri)
            .map_or("text/plain", |r| r.2);
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, &request.uri).with_mime_type(mime),
        ])
        .into())
    }
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod link_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_resource_reads() {
        for (uri, ..) in RESOURCES {
            assert!(!read(uri).unwrap().is_empty(), "{uri}");
        }
        assert!(read("observation://nope").is_err());
    }

    #[test]
    fn only_the_app_state_can_be_subscribed_to() {
        let s = ObservationMcp::new();
        let wanted = SubscriptionFilter::builder()
            .resource_subscription(STATE_URI)
            .resource_subscription("observation://catalog")
            .tools_list_changed()
            .build();
        let accepted = s.accepted_subscription_filter(&wanted).unwrap();
        assert_eq!(
            accepted.resource_subscriptions,
            Some(vec![STATE_URI.to_owned()])
        );
        assert_eq!(accepted.tools_list_changed, None);
        let other = SubscriptionFilter::builder()
            .resource_subscription("observation://catalog")
            .build();
        assert!(s.accepted_subscription_filter(&other).is_none());
        assert!(s.get_info().capabilities.resources.unwrap().subscribe == Some(true));
    }

    #[test]
    fn the_tools_are_listed() {
        let names: Vec<String> = ObservationMcp::all_tools()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for n in [
            "list_camera_models",
            "place_camera",
            "point_camera_at",
            "create_custom_model",
            "compute_coverage",
            "connect_app",
            "disconnect_app",
            "get_app_state",
            "label_area",
            "remove_area",
            "limit_figures_to",
            "add_target",
            "update_target",
            "remove_target",
            "target_verdicts",
            "camera_split",
            "who_sees_point",
            "find_gaps",
            "propose_changes",
            "get_proposal",
            "withdraw_proposal",
            "open_scene",
            "export_scene",
            "list_scenes",
            "close_scene",
            "undo",
            "diff_scenes",
            "apply_edits",
            "describe_surroundings",
            "list_buildings",
            "list_streets",
            "suggest_mount_points",
            "scene_summary",
            "camera_view",
            "convert_position",
        ] {
            assert!(names.iter().any(|x| x == n), "{n} in {names:?}");
        }
    }

    /// Collects the paths of boolean subschemas (`true` or `false`) in a
    /// JSON Schema. They are valid JSON Schema, but some clients that compile
    /// tool schemas into grammars, llama.cpp's server among them, reject the
    /// tools when one appears.
    fn boolean_subschemas(schema: &Value, path: &str, found: &mut Vec<String>) {
        const ONE: [&str; 12] = [
            "items",
            "additionalItems",
            "additionalProperties",
            "unevaluatedItems",
            "unevaluatedProperties",
            "propertyNames",
            "contains",
            "not",
            "if",
            "then",
            "else",
            "contentSchema",
        ];
        const MAPS: [&str; 5] = [
            "properties",
            "patternProperties",
            "dependentSchemas",
            "$defs",
            "definitions",
        ];
        const LISTS: [&str; 4] = ["allOf", "anyOf", "oneOf", "prefixItems"];
        match schema {
            Value::Bool(_) => found.push(path.to_owned()),
            Value::Object(map) => {
                for (key, sub) in map {
                    let here = format!("{path}/{key}");
                    if ONE.contains(&key.as_str()) {
                        boolean_subschemas(sub, &here, found);
                    } else if MAPS.contains(&key.as_str()) {
                        for (name, s) in sub.as_object().into_iter().flatten() {
                            boolean_subschemas(s, &format!("{here}/{name}"), found);
                        }
                    } else if LISTS.contains(&key.as_str()) {
                        for (i, s) in sub.as_array().into_iter().flatten().enumerate() {
                            boolean_subschemas(s, &format!("{here}/{i}"), found);
                        }
                    }
                }
            }
            _ => found.push(format!("{path} (neither an object nor a boolean)")),
        }
    }

    #[test]
    fn no_tool_schema_holds_a_boolean_subschema() {
        let mut found = Vec::new();
        for tool in ObservationMcp::all_tools().list_all() {
            let input = Value::Object((*tool.input_schema).clone());
            boolean_subschemas(&input, &format!("{} input #", tool.name), &mut found);
            if let Some(output) = &tool.output_schema {
                let output = Value::Object((**output).clone());
                boolean_subschemas(&output, &format!("{} output #", tool.name), &mut found);
            }
        }
        assert!(
            found.is_empty(),
            "boolean subschemas:\n{}",
            found.join("\n")
        );
    }

    #[test]
    fn the_walk_finds_boolean_subschemas() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "scene": true,
                "list": {"type": "array", "items": false},
                "either": {"anyOf": [{"type": "string"}, true]},
            },
            "$defs": {"Thing": {"additionalProperties": true}},
        });
        let mut found = Vec::new();
        boolean_subschemas(&schema, "#", &mut found);
        found.sort();
        assert_eq!(
            found,
            [
                "#/$defs/Thing/additionalProperties",
                "#/properties/either/anyOf/1",
                "#/properties/list/items",
                "#/properties/scene",
            ]
        );
    }

    #[tokio::test]
    async fn coverage_of_a_scene_carries_the_caveats() {
        let s = ObservationMcp::new();
        let Json(created) = s.create_scene().unwrap();
        let Json(placed) = s
            .add_example_cameras(Parameters(ExampleArgs {
                scene: created.scene,
                count: 3,
                model_ids: Vec::new(),
            }))
            .unwrap();
        let Json(out) = s
            .compute_coverage(Parameters(SceneArg {
                scene: placed.scene,
            }))
            .await
            .unwrap();
        assert_eq!(out.levels.len(), 4);
        assert!(out.levels[0].street_seen_m2 > 0.0);
        assert!(out.caveats.contains("trees"));
        assert!(out.osm_data_as_of.is_some());
    }

    #[test]
    fn the_prompts_are_listed() {
        let names: Vec<String> = ObservationMcp::prompt_router()
            .list_all()
            .into_iter()
            .map(|p| p.name.to_string())
            .collect();
        for n in [
            "assess_street_coverage",
            "compare_cameras",
            "where_footage_lands",
        ] {
            assert!(names.iter().any(|x| x == n), "{n} in {names:?}");
        }
    }
}
