//! The MCP tools, resources and prompts.
//!
//! The scene tools are read-only in MCP's sense: none changes anything
//! outside the call. They return an edited copy of the scene they were given,
//! for the agent to pass to the next call or save for the app to import. The
//! app tools ([`crate::app`]) open and close the one connection the server
//! holds, to the user's open app.

use crate::ToolError;
use crate::area::{self, extent};
use crate::catalog::{catalog, is_custom, model};
use crate::edit::{self, CameraUpdate, Kind, ModelUpdate, NewModel, ParamsUpdate, Place, Target};
use crate::figures::scene_figures;
use crate::link::{AppLink, STATE_URI};
use observation_core::address::ON_OUTLINE_M;
use observation_core::camera::{ASSUMED_IR_RANGE_M, CameraKind, CameraModel, DoriLevel, Lighting};
use observation_core::provenance::{CAVEATS_SUMMARY, caveats_text, format_date};
use observation_core::scene::AreaRef;
use observation_core::scene::SceneFile;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
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
use std::sync::Arc;

const INSTRUCTIONS: &str = "\
Observation estimates how much of a street home security cameras can see. \
Scenes are not stored here: create one with create_scene, then pass the returned `scene` \
to each editing tool and use the `scene` it returns. The result is a scene file the \
Observation app can import. Positions are metres from the area's origin: x east, z south \
(north is negative z); heights are metres above the ground. Bearings are degrees clockwise \
from north. Offline only the demo street is available, so load_demo_area gives the area's \
extent. If the user has the Observation app open on this computer, they can connect it: ask \
for the code in its Connect agent dialog and call connect_app with the app's address (such as \
http://127.0.0.1:8080). Then get_app_state returns their open scene (pass it to the scene \
tools), its area, the selected cameras and a point they shared, the resource \
observation://app/state follows it, and coverage and addresses work on their real area. Edits \
made here do not change the app; give the user the scene to import. \
Coverage figures are estimates that leave out trees, fences and vehicles: always pass on \
the caveats that compute_coverage returns.";

const USAGE_MD: &str = include_str!("../../../docs/usage.md");
const CAMERA_CATALOG_MD: &str = include_str!("../../../docs/camera-catalog.md");

/// The Observation MCP server. Its only state is the connection to the
/// user's app ([`AppLink`]); clones share it.
#[derive(Debug, Clone, Default)]
pub struct ObservationMcp {
    pub(crate) link: Arc<AppLink>,
}

impl ObservationMcp {
    pub fn new() -> Self {
        Self::default()
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

    /// Every tool: the scene tools and the app tools.
    pub fn all_tools() -> ToolRouter<Self> {
        Self::tool_router() + Self::app_tool_router()
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

fn fail(e: ToolError) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Tool inputs and outputs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SceneArg {
    /// A scene file: the `scene` returned by the last scene tool, or a file
    /// saved from the app (as a JSON object or a string).
    pub scene: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListModelsArgs {
    /// Only models of this kind.
    pub kind: Option<Kind>,
    /// Only models by this manufacturer (case-insensitive).
    pub vendor: Option<String>,
    /// A scene whose custom models to list as well.
    pub scene: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetModelArgs {
    /// Catalogue or custom model id.
    pub id: String,
    /// The scene that holds the model, for a custom one.
    pub scene: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CameraArgs {
    pub scene: Value,
    /// The camera's id in the scene.
    pub id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaceArgs {
    pub scene: Value,
    #[serde(flatten)]
    pub camera: Place,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MoveArgs {
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
    pub scene: Value,
    /// The camera's id in the scene.
    pub id: u32,
    /// The spot to point at.
    pub target: Target,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateCameraArgs {
    pub scene: Value,
    /// The camera's id in the scene.
    pub id: u32,
    #[serde(flatten)]
    pub changes: CameraUpdate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExampleArgs {
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
    pub scene: Value,
    pub model: NewModel,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateModelArgs {
    pub scene: Value,
    /// The custom model's id (`custom-...`).
    pub id: String,
    pub changes: ModelUpdate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModelIdArgs {
    pub scene: Value,
    /// The custom model's id (`custom-...`).
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ParamsArgs {
    pub scene: Value,
    #[serde(flatten)]
    pub params: ParamsUpdate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PointQueryArgs {
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
    /// The edited scene: pass it to the next scene tool.
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
    /// Whether the figures are limited to the scene's drawn region.
    pub region_limited: bool,
    pub cameras_counted: Vec<u32>,
    pub cameras_skipped: Vec<SkippedOut>,
    /// Figures per detail level, least detail first.
    pub levels: Vec<LevelOut>,
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
        description = "Create a new, empty scene on the demo street. Pass the returned `scene` to the editing tools.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn create_scene(&self) -> Result<Json<Edited>, String> {
        let s = edit::new_demo_scene().map_err(fail)?;
        Edited::new("created an empty scene on the demo street", &s)
            .map(Json)
            .map_err(fail)
    }

    #[tool(
        description = "Check a scene file the way the app checks one it imports, and return it as the app would load it.",
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
             explain that the figures are estimates, read as \"up to\". Finally give me the \
             scene JSON so I can import it into the Observation app.",
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

    /// Fetches the area of the call's scene from the connected app's server
    /// first, if it is a real area, then runs the tool with the connection's
    /// areas in view (see [`area::bundle_for`]).
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request.arguments.clone();
        let tcc = ToolCallContext::new(self, request, context);
        self.in_tool_scope(arguments.as_ref(), Self::all_tools().call(tcc))
            .await
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
        ] {
            assert!(names.iter().any(|x| x == n), "{n} in {names:?}");
        }
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
    }
}
