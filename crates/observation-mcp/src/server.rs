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
use crate::geometry;
use crate::link::{AppLink, STATE_URI};
use crate::queries::{Report, Verbosity};
use crate::schema::JsonObject;
use crate::toolsets::{Group, Profile, ToolSelection, Toolbox};
use observation_core::address::ON_OUTLINE_M;
use observation_core::camera::{ASSUMED_IR_RANGE_M, CameraKind, CameraModel, DoriLevel, Lighting};
use observation_core::provenance::{CAVEATS_SUMMARY, caveats_text, format_date};
use observation_core::scene::AreaRef;
use observation_core::scene::SceneFile;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, Implementation, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, PromptMessage, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
    ResourceUpdatedNotificationParam, Role, ServerCapabilities, ServerConfig,
    SubscribeRequestParams, SubscriptionFilter, Tool, UnsubscribeRequestParams,
};
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, prompt, prompt_handler, prompt_router, tool, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

const USAGE_MD: &str = include_str!("../../../docs/usage.md");
const CAMERA_CATALOG_MD: &str = include_str!("../../../docs/camera-catalog.md");

/// The Observation MCP server. Its state is the connection to the user's
/// app ([`AppLink`]) and the working scenes ([`crate::store::SceneStore`]);
/// clones share both.
#[derive(Debug, Clone)]
pub struct ObservationMcp {
    pub(crate) link: Arc<AppLink>,
    pub(crate) scenes: Arc<crate::store::SceneStore>,
    /// The tools offered, as listed ([`crate::toolsets`]).
    pub(crate) tools: Arc<Toolbox>,
    /// Served without sessions (stateless HTTP), so a legacy
    /// `resources/subscribe` would have nowhere to send its notifications.
    sessionless: bool,
}

impl Default for ObservationMcp {
    /// Offers every tool ([`Toolbox::shared`]: built once per process).
    fn default() -> Self {
        Self {
            link: Arc::default(),
            scenes: Arc::default(),
            tools: Toolbox::shared(ToolSelection::default()),
            sessionless: false,
        }
    }
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
        &registry().router
    }

    /// The group `tool` is in, if there is such a tool.
    pub fn tool_group(tool: &str) -> Option<Group> {
        registry().groups.get(tool).copied()
    }

    /// Whether `tool` must be given a scene (`scene` or `scene_id`).
    pub fn scene_required(tool: &str) -> bool {
        registry().scene_required.contains(tool)
    }

    /// The tools this server offers ([`crate::toolsets`]).
    pub fn toolbox(&self) -> &Toolbox {
        &self.tools
    }

    /// The profile the tools are offered in.
    pub fn profile(&self) -> Profile {
        self.tools.profile()
    }

    /// This server, offering the tools `selection` chooses.
    pub fn with_tools(self, selection: ToolSelection) -> Self {
        self.with_toolbox(Toolbox::shared(selection))
    }

    /// This server, offering the tools of `toolbox`, which servers for the
    /// same selection share.
    pub fn with_toolbox(mut self, toolbox: Arc<Toolbox>) -> Self {
        self.tools = toolbox;
        self
    }

    /// The prompts whose tools are all offered ([`PROMPT_TOOLS`]).
    fn offered_prompts(&self) -> PromptRouter<Self> {
        let mut router = Self::prompt_router();
        for (prompt, tools) in PROMPT_TOOLS {
            if !tools.iter().all(|t| self.tools.offers(t)) {
                router.remove_route(prompt);
            }
        }
        router
    }
}

/// Every tool router, with the group its tools are in
/// ([`crate::toolsets::Group`]). A new router is one line here.
fn grouped_routers() -> Vec<(Group, ToolRouter<ObservationMcp>)> {
    vec![
        (Group::Guide, ObservationMcp::guide_tool_router()),
        (Group::Scenes, ObservationMcp::store_tool_router()),
        (Group::Scenes, ObservationMcp::scene_tool_router()),
        (Group::Cameras, ObservationMcp::camera_tool_router()),
        (Group::Models, ObservationMcp::model_tool_router()),
        (Group::Places, ObservationMcp::places_tool_router()),
        (Group::Analysis, ObservationMcp::coverage_tool_router()),
        (Group::Analysis, ObservationMcp::query_tool_router()),
        (Group::Describe, ObservationMcp::describe_tool_router()),
        (Group::Map, ObservationMcp::render_tool_router()),
        (Group::Goals, ObservationMcp::goal_tool_router()),
        (Group::App, ObservationMcp::app_tool_router()),
        (Group::Proposals, ObservationMcp::proposal_tool_router()),
        (Group::View, ObservationMcp::view_tool_router()),
    ]
}

/// Every tool, each tool's group, and the tools that need a scene.
#[derive(Debug)]
struct Registry {
    router: ToolRouter<ObservationMcp>,
    groups: HashMap<String, Group>,
    scene_required: HashSet<String>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut router = ToolRouter::new();
        let mut groups = HashMap::new();
        for (group, r) in grouped_routers() {
            for name in r.map.keys() {
                groups.insert(name.to_string(), group);
            }
            router.merge(r);
        }
        let scene_required = router
            .list_all()
            .iter()
            .filter(|t| {
                t.input_schema
                    .get("required")
                    .and_then(Value::as_array)
                    .is_some_and(|r| r.iter().any(|n| n == "scene"))
            })
            .map(|t| t.name.to_string())
            .collect();
        Registry {
            router: crate::store::document_scene_ids(router),
            groups,
            scene_required,
        }
    })
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
pub struct CoverageArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// "brief" (the scene's level and lighting, in a line) or "full" (every
    /// level, the caveats).
    pub verbosity: Option<Verbosity>,
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
    /// "brief" (the default: id, vendor, name, kind, field of view and
    /// night-vision range) or "full" (resolution and ranges as well).
    pub detail: Option<Verbosity>,
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
    #[schemars(range(min = 1, max = 20))]
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
    pub ppm: f64,
    /// Range by day, metres.
    pub day_m: f64,
    /// Range at night (limited to the infrared range), metres.
    pub night_m: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ModelInfo {
    pub id: String,
    pub vendor: String,
    pub name: String,
    pub kind: String,
    pub hfov_deg: f64,
    pub vfov_deg: f64,
    /// `[width, height]`, pixels.
    pub resolution: [u32; 2],
    /// Advertised night-vision range, metres.
    pub ir_range_m: Option<f64>,
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
                    ppm: geometry::m1(ppm),
                    day_m: geometry::m1(m.effective_range_m(ppm, Lighting::Day)),
                    night_m: geometry::m1(m.effective_range_m(ppm, Lighting::Night)),
                })
            })
            .collect();
        Self {
            id: m.id.clone(),
            vendor: m.vendor.clone(),
            name: m.name.clone(),
            kind: m.kind.label().to_owned(),
            hfov_deg: geometry::m1(m.hfov_deg),
            vfov_deg: geometry::m1(m.vfov_deg),
            resolution: m.resolution,
            ir_range_m: m.ir_range_m.map(geometry::m1),
            ir_range_assumed: m.ir_range_m.is_none(),
            custom: is_custom(&m.id),
            ranges,
            source: m.source.clone(),
            notes: m.notes.clone(),
        }
    }
}

/// A camera model in a line of the brief list.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ModelBrief {
    pub id: String,
    pub vendor: String,
    pub name: String,
    pub kind: String,
    /// Horizontal field of view, degrees.
    pub hfov_deg: f64,
    /// Advertised night-vision range, metres; absent when the maker
    /// publishes none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub night_vision_m: Option<f64>,
}

/// list_camera_models' brief form.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ModelListBrief {
    pub count: usize,
    pub models: Vec<ModelBrief>,
    /// Where the rest is.
    pub note: String,
}

impl From<&ModelList> for ModelListBrief {
    fn from(f: &ModelList) -> Self {
        Self {
            count: f.models.len(),
            models: f
                .models
                .iter()
                .map(|m| ModelBrief {
                    id: m.id.clone(),
                    vendor: m.vendor.clone(),
                    name: m.name.clone(),
                    kind: m.kind.clone(),
                    hfov_deg: m.hfov_deg,
                    night_vision_m: m.ir_range_m,
                })
                .collect(),
            note: "get_camera_model gives one model's ranges at each detail level; detail \
                   \"full\" gives them for every model"
                .to_owned(),
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
    pub ppm: f64,
    /// Street surface (roads, driveways and footpaths), m².
    pub street_m2: f64,
    pub street_seen_m2: f64,
    /// Street seen by at least two cameras, m².
    pub street_seen_multi_m2: f64,
    /// Any open ground seen, m².
    pub open_ground_seen_m2: f64,
    /// Building walls, m².
    pub facade_m2: f64,
    pub facade_seen_m2: f64,
    pub street_fraction: f64,
    pub street_multi_fraction: f64,
    pub facade_fraction: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SkippedOut {
    pub id: u32,
    pub reason: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CoverageOut {
    pub lighting: String,
    pub subject_height_m: f64,
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

/// compute_coverage's brief form: the scene's level and lighting only.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CoverageBrief {
    /// The figures in a sentence.
    pub summary: String,
    pub level: String,
    pub lighting: String,
    /// Whether only the street inside the scene's area for the figures
    /// counts (see limit_figures_to).
    pub region_limited: bool,
    /// Mapped street (roads, driveways and footpaths), m².
    pub street_m2: f64,
    /// The part not ruled out for at least one camera, m².
    pub street_seen_m2: f64,
    pub street_fraction: f64,
    /// The share not ruled out for two cameras or more.
    pub street_multi_fraction: f64,
    /// The share of building walls not ruled out.
    pub facade_fraction: f64,
    pub cameras_counted: Vec<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cameras_skipped: Vec<SkippedOut>,
    /// When the OpenStreetMap data was current, if known; pass it on.
    pub osm_data_as_of: Option<String>,
    /// The caveats in one line; pass it on.
    pub caveats_summary: String,
}

fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

impl From<&CoverageOut> for CoverageBrief {
    fn from(f: &CoverageOut) -> Self {
        let l = f
            .levels
            .iter()
            .find(|l| l.level == f.selected_level)
            .or(f.levels.first());
        let light = if f.lighting == "Night" {
            "at night"
        } else {
            "by day"
        };
        let n = f.cameras_counted.len();
        let street = if f.region_limited {
            "the mapped street inside the area for the figures"
        } else {
            "the mapped street"
        };
        let summary = match l {
            Some(l) if n > 0 => format!(
                "At {} {light}, up to {} of {street} ({} of {} m²) is not ruled out \
                 for at least one of {n} camera{}; {} for two or more; {} of building walls",
                l.level,
                percent(l.street_fraction),
                l.street_seen_m2,
                l.street_m2,
                if n == 1 { "" } else { "s" },
                percent(l.street_multi_fraction),
                percent(l.facade_fraction),
            ),
            _ => format!(
                "At {} {light}: no enabled cameras to count",
                f.selected_level
            ),
        };
        Self {
            summary,
            level: f.selected_level.clone(),
            lighting: f.lighting.clone(),
            region_limited: f.region_limited,
            street_m2: l.map_or(0.0, |l| l.street_m2),
            street_seen_m2: l.map_or(0.0, |l| l.street_seen_m2),
            street_fraction: l.map_or(0.0, |l| l.street_fraction),
            street_multi_fraction: l.map_or(0.0, |l| l.street_multi_fraction),
            facade_fraction: l.map_or(0.0, |l| l.facade_fraction),
            cameras_counted: f.cameras_counted.clone(),
            cameras_skipped: f.cameras_skipped.clone(),
            osm_data_as_of: f.osm_data_as_of.clone(),
            caveats_summary: f.caveats_summary.clone(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddressOut {
    /// The address there, if OpenStreetMap has one nearby.
    pub address: Option<String>,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Scenes: the demo street's extent, and scene files made or checked whole.
#[tool_router(router = scene_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
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
        description = "Create an empty scene on the demo street, kept as a working scene. Prefer open_scene; this also returns the scene file.",
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
}

/// Camera models: the catalogue and a scene's custom models.
#[tool_router(router = model_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "List camera models: id, vendor, name, kind, horizontal field of view and night-vision range. detail \"full\" adds resolution and the range at each detail level.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn list_camera_models(
        &self,
        Parameters(a): Parameters<ListModelsArgs>,
    ) -> Result<Json<Report<ModelListBrief, ModelList>>, String> {
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
        let full = ModelList {
            catalog_fingerprint: cat.fingerprint(),
            models,
        };
        let detail = a.detail.unwrap_or(Verbosity::Brief);
        Ok(Json(Report::of(detail, full, |f| f.into())))
    }

    #[tool(
        description = "One camera model by id: fields of view, resolution, range at each detail level by day and at night, and its spec source.",
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
}

/// Cameras: placing, moving, aiming and removing them.
#[tool_router(router = camera_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Place a camera at x, z (metres east and south of the centre), height_m above the ground. yaw_deg is a compass bearing: 0 faces north, 90 east, 180 south, 270 west. Or give look_at. With neither, it faces the nearest street.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    fn place_camera(&self, Parameters(a): Parameters<PlaceArgs>) -> Result<Json<Edited>, String> {
        let cat = catalog().map_err(fail)?;
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        let edit::Placed { id, aim } =
            edit::place_camera_aimed(&mut s, cat, a.camera).map_err(fail)?;
        let yaw = s
            .cameras
            .iter()
            .find(|c| c.id == id)
            .map_or(0.0, |c| c.yaw_deg);
        let facing = format!("{}° ({})", geometry::m0(yaw), geometry::compass(yaw));
        let summary = match aim {
            edit::PlacedAim::Given => format!("placed camera {id}, facing {facing}"),
            edit::PlacedAim::Street(street) => {
                format!("placed camera {id}, facing the nearest street, {street}, at {facing}")
            }
            edit::PlacedAim::NoStreet => {
                format!("placed camera {id}, facing {facing}: no street found to face")
            }
        };
        let mut out = Edited::new(summary, &s).map_err(fail)?;
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
        description = "Aim a camera: yaw_deg (0 north, 90 east, 180 south, 270 west), pitch_deg (negative tilts down) and roll_deg. Omitted ones stay.",
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
        description = "Point a camera at a spot: x, z and a height above the ground there (default: the scene's subject height).",
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
        description = "Add 1 to 20 example cameras on house fronts, facing the street, clear of the cameras already placed.",
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
}

/// Analysis settings and the coverage figures.
#[tool_router(router = coverage_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Set the scene's analysis settings: level (detect, observe, recognise or identify), lighting (day or night) and the height people are tested at.",
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
        description = "How much mapped street and building wall is not ruled out for the enabled cameras, with buildings and terrain in the way. Pass on the data date and caveats_summary.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn compute_coverage(
        &self,
        Parameters(a): Parameters<CoverageArgs>,
    ) -> Result<Json<Report<CoverageBrief, CoverageOut>>, String> {
        let v = self.verbosity(a.verbosity);
        // Line of sight over the whole area is CPU work: keep it off the async runtime,
        // with the connection's real areas in view.
        let areas = area::current_areas();
        tokio::task::spawn_blocking(move || match areas {
            Some(areas) => area::with_areas(areas, || coverage(&a.scene)),
            None => coverage(&a.scene),
        })
        .await
        .map_err(|e| format!("coverage failed: {e}"))?
        .map(|f| Json(Report::of(v, f, |f| f.into())))
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
            ppm: geometry::m1(l.ppm),
            street_m2: geometry::m1(l.stats.street_m2),
            street_seen_m2: geometry::m1(l.stats.street_seen_m2),
            street_seen_multi_m2: geometry::m1(l.stats.street_seen_multi_m2),
            open_ground_seen_m2: geometry::m1(l.stats.open_ground_seen_m2),
            facade_m2: geometry::m1(l.stats.facade_m2),
            facade_seen_m2: geometry::m1(l.stats.facade_seen_m2),
            street_fraction: geometry::r3(l.stats.street_fraction()),
            street_multi_fraction: geometry::r3(l.stats.street_multi_fraction()),
            facade_fraction: geometry::r3(l.stats.facade_fraction()),
        })
        .collect();
    Ok(CoverageOut {
        lighting: format!("{:?}", s.params.lighting),
        subject_height_m: geometry::m1(s.params.subject_height_m),
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

impl ObservationMcp {
    /// Step 7 of drive_the_app: how to show an accepted change, with the
    /// view tools that are offered.
    fn show_the_result(&self) -> &'static str {
        let offers = |t: &str| self.tools.offers(t);
        match (
            offers("focus_view") && offers("highlight"),
            offers("select_cameras"),
        ) {
            (true, true) => {
                "If I accept, show me the result with focus_view and highlight, or \
                 select_cameras for the cameras you changed."
            }
            (true, false) => "If I accept, show me the result with focus_view and highlight.",
            (false, true) => "If I accept, select the cameras you changed with select_cameras.",
            (false, false) => "If I accept, tell me what changed.",
        }
    }
}

/// The tools each prompt relies on: a prompt is listed only when all of
/// them are offered. Tools a prompt names only as an option ("if the app is
/// connected") are left out here, and its text names them only when they
/// are offered.
const PROMPT_TOOLS: [(&str, &[&str]); 4] = [
    (
        "assess_street_coverage",
        &[
            "open_scene",
            "render_map",
            "add_example_cameras",
            "place_camera",
            "set_coverage_params",
            "compute_coverage",
            "find_gaps",
            "export_scene",
        ],
    ),
    (
        "where_footage_lands",
        &[
            "open_scene",
            "label_area",
            "add_target",
            "camera_split",
            "target_verdicts",
            "export_scene",
        ],
    ),
    (
        "drive_the_app",
        &[
            "connect_app",
            "open_scene",
            "scene_summary",
            "render_map",
            "suggest_mount_points",
            "suggest_placement",
            "place_camera",
            "compute_coverage",
            "check_point",
            "propose_changes",
            "wait_for_decision",
        ],
    ),
    ("compare_cameras", &["get_camera_model"]),
];

#[prompt_router]
impl ObservationMcp {
    #[prompt(
        name = "assess_street_coverage",
        description = "Set up cameras on the demo street and report how much of the street they cover, with the caveats"
    )]
    fn assess_street_coverage(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            "Using the Observation tools: call get_started, then open_scene and keep the \
             scene_id it returns (such as \"s1\") for every scene tool. Look at the street with \
             render_map, then add cameras (add_example_cameras, or place_camera where I \
             describe). Run compute_coverage by day and again at night (set_coverage_params). \
             Report the share of street not ruled out by day and at night in a short table, \
             then quote the data date and the caveats_summary and explain that the figures are \
             estimates, read as \"up to\". Then run find_gaps and describe the largest \
             stretches of street ruled out for every camera, by street name and position \
             (never by a neighbour's address). Finally call export_scene and give me the scene \
             so I can import it into the Observation app.",
        )]
    }

    #[prompt(
        name = "where_footage_lands",
        description = "Mark my land, my neighbours' and the places I care about, then report where each camera's footage lands and whether the model rules out each place, with the caveats"
    )]
    fn where_footage_lands(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Using the Observation tools, help me see where my cameras' footage lands. {} \
             and pass its scene_id to every tool. Ask me which ground is mine and which is my neighbours', and mark \
             each with label_area (label own, neighbour or public, with a short name); the \
             outlines are as I describe them, not property records. Ask which places matter to \
             me, such as my front door, a side gate or a path, and mark each with add_target. \
             Then run camera_split and report for each camera, in a short table, the m² of \
             visible ground on my land, on each neighbour's, on the street, driveways and \
             footpaths as mapped (not all of it public) and elsewhere, as the low to high range beside each outline's own area. Run \
             target_verdicts with verbosity \"full\" and report each target camera by camera: say \"not ruled out\" \
             or \"ruled out\" with the reason, at each detail level by day and at night, and \
             never that a camera \"sees\" a place. Name buildings by index and distance, \
             never by address. Quote the data date and the caveats_summary, and explain that \
             hedges, fences, trees and vehicles are not modelled and can only block more. \
             Finally give me the scene from export_scene{}.",
                if self.tools.offers("connect_app") {
                    "Work on my open scene if the app is connected (open_scene with from \"app\"), \
                  otherwise on a new scene on the demo street (open_scene),"
                } else {
                    "Work on a new scene on the demo street (open_scene),"
                },
                if self.tools.offers("propose_changes") {
                    ", or, if the app is connected, propose the areas and targets with \
                  propose_changes"
                } else {
                    ""
                }
            ),
        )]
    }

    #[prompt(
        name = "drive_the_app",
        description = "Work in my open Observation app step by step: connect, look, edit a working scene, propose the change and show me the result"
    )]
    fn drive_the_app(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Using the Observation tools, work with my open Observation app, one step at a \
             time:\n\
             1. Ask me for the app's address and the code from its Connect agent dialog, then \
             call connect_app.\n\
             2. Call get_started.\n\
             3. Keep the scene_id connect_app returned, such as \"s1\": it is a working copy \
             of my scene. If it gave none, call open_scene with from \"app\" for one.\n\
             4. Look before you change anything: scene_summary, then render_map.\n\
             5. Make the change I ask for, passing that scene_id to every scene tool. For a new \
             camera, call suggest_mount_points or suggest_placement first, then place_camera. \
             Check the result with compute_coverage or check_point.\n\
             6. Call propose_changes with the scene_id and a one-sentence summary, tell me it is \
             waiting for my decision, then call wait_for_decision with its id until it is no \
             longer pending.\n\
             7. {} If I reject it, read my reason and ask me what to change.\n\
             Say \"not ruled out\" or \"ruled out\" with the reason, never that a camera sees \
             something; name buildings by index and distance, never by address; and quote the \
             data date and the caveats_summary.",
                self.show_the_result()
            ),
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

#[prompt_handler(router = self.offered_prompts())]
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
        .with_instructions(self.tools.instructions())
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

    /// The tools the selection offers, with their schemas as it lists them
    /// ([`crate::toolsets`]).
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let cache_hints = context
            .protocol_version()
            .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28);
        let mut result = ListToolsResult::with_all_items(self.tools.listed());
        result.ttl_ms = cache_hints.then_some(0);
        result.cache_scope = cache_hints.then_some(CacheScope::Public);
        Ok(result)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.get(name).cloned()
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

    /// The instructions lead with get_started and scene_id, name only
    /// offered tools, and stay short.
    #[test]
    fn the_instructions_are_short_and_name_only_offered_tools() {
        let full = ObservationMcp::new();
        let text = full.get_info().instructions.unwrap_or_default();
        assert!(text.len() < 1_800, "{} bytes", text.len());
        let lead = &text[..300];
        assert!(lead.contains("get_started") && lead.contains("scene_id"));
        for rule in ["not ruled out", "never by address", "caveats_summary"] {
            assert!(text.contains(rule), "{rule}");
        }
        for tool in [
            "propose_changes",
            "wait_for_app_change",
            "optimise_aim",
            "undo",
        ] {
            assert!(text.contains(tool), "{tool}");
        }
        for selection in crate::guide::tests::selections() {
            let server = ObservationMcp::new().with_tools(selection.clone());
            let text = server.get_info().instructions.unwrap_or_default();
            assert_eq!(
                crate::guide::tests::unoffered_tools(&server, &text),
                Vec::<String>::new(),
                "{selection:?}:\n{text}"
            );
            assert!(text.contains("get_started") && text.contains("never by address"));
        }
        let small = ObservationMcp::new().with_tools(ToolSelection::profile(Profile::Small));
        let text = small.get_info().instructions.unwrap_or_default();
        assert!(
            !text.contains("connect_app") && text.contains("render_map"),
            "{text}"
        );
    }

    #[test]
    fn brief_coverage_says_when_the_figures_are_limited_to_an_area() {
        let full = |region_limited| CoverageOut {
            lighting: "Day".into(),
            subject_height_m: 1.7,
            selected_level: "Recognise".into(),
            region_limited,
            cameras_counted: vec![1],
            cameras_skipped: Vec::new(),
            levels: vec![LevelOut {
                level: "Recognise".into(),
                ppm: 125.0,
                street_m2: 200.0,
                street_seen_m2: 50.0,
                street_seen_multi_m2: 0.0,
                open_ground_seen_m2: 60.0,
                facade_m2: 100.0,
                facade_seen_m2: 10.0,
                street_fraction: 0.25,
                street_multi_fraction: 0.0,
                facade_fraction: 0.1,
            }],
            osm_data_as_of: None,
            caveats_summary: CAVEATS_SUMMARY.into(),
            caveats: String::new(),
        };
        let limited = CoverageBrief::from(&full(true));
        assert!(limited.region_limited);
        assert!(
            limited
                .summary
                .contains("25% of the mapped street inside the area for the figures"),
            "{}",
            limited.summary
        );
        let whole = CoverageBrief::from(&full(false));
        assert!(!whole.region_limited);
        assert!(
            whole
                .summary
                .contains("25% of the mapped street (50 of 200 m²)"),
            "{}",
            whole.summary
        );
    }

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
            "check_point",
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
            .compute_coverage(Parameters(CoverageArgs {
                scene: placed.scene,
                verbosity: None,
            }))
            .await
            .unwrap();
        let out = out.full().unwrap();
        assert_eq!(out.levels.len(), 4);
        assert!(out.levels[0].street_seen_m2 > 0.0);
        assert!(out.caveats.contains("trees"));
        assert!(out.osm_data_as_of.is_some());
    }

    fn prompt_names(server: &ObservationMcp) -> Vec<String> {
        let mut names: Vec<String> = server
            .offered_prompts()
            .list_all()
            .into_iter()
            .map(|p| p.name.to_string())
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn the_prompts_are_listed_when_their_tools_are_offered() {
        let all = prompt_names(&ObservationMcp::new());
        assert_eq!(
            all,
            [
                "assess_street_coverage",
                "compare_cameras",
                "drive_the_app",
                "where_footage_lands",
            ]
        );
        for (name, _) in PROMPT_TOOLS {
            assert!(ObservationMcp::prompt_router().has_route(name), "{name}");
        }
        let small = ObservationMcp::new().with_tools(ToolSelection::profile(Profile::Small));
        assert_eq!(prompt_names(&small), ["assess_street_coverage"]);
        let app = ObservationMcp::new().with_tools(ToolSelection::profile(Profile::SmallApp));
        assert_eq!(
            prompt_names(&app),
            ["assess_street_coverage", "drive_the_app"]
        );
    }

    /// The text of `prompt` as `server` gives it.
    fn prompt_text(server: &ObservationMcp, prompt: &str) -> String {
        let messages = match prompt {
            "assess_street_coverage" => server.assess_street_coverage(),
            "where_footage_lands" => server.where_footage_lands(),
            "drive_the_app" => server.drive_the_app(),
            _ => server.compare_cameras(Parameters(CompareArgs {
                model_ids: "a, b".into(),
            })),
        };
        serde_json::to_string(&messages).unwrap()
    }

    #[test]
    fn a_listed_prompt_names_only_offered_tools() {
        let mut selections = crate::guide::tests::selections();
        selections.push(ToolSelection {
            profile: Profile::Full,
            groups: Some(vec![Group::Scenes, Group::Places, Group::Analysis]),
        });
        selections.push(ToolSelection {
            profile: Profile::Full,
            groups: Group::ALL
                .into_iter()
                .filter(|g| *g != Group::View)
                .collect::<Vec<_>>()
                .into(),
        });
        for selection in selections {
            let server = ObservationMcp::new().with_tools(selection.clone());
            for name in prompt_names(&server) {
                let text = prompt_text(&server, &name);
                assert_eq!(
                    crate::guide::tests::unoffered_tools(&server, &text),
                    Vec::<String>::new(),
                    "{name} in {selection:?}:\n{text}"
                );
            }
        }
    }
}
