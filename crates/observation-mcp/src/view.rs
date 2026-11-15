//! View commands: the agent points the user at something in the connected
//! app (see [`observation_agent_api::command`]), and finds places to load.
//!
//! The tools move the user's 3D view, select cameras, mark a camera, a
//! target, an area or a spot with a short note, change the view settings,
//! and ask the user to load an area. None of them changes the scene, and the
//! app never loads an area without the user's click. The user can turn the
//! view commands off in the app's Connect agent dialog; the request to load
//! an area is shown either way.
//!
//! Where the app's shared scene is known, the cameras, targets and areas a
//! tool names are checked against it first, so a small model hears of a
//! wrong id at once and with the ids that exist.

use crate::ObservationMcp;
use crate::ToolError;
use crate::edit;
use observation_agent_api::{
    AgentCommand, DetailLevel, Highlight, ItemRef, MAX_FOCUS_DISTANCE_M, MIN_FOCUS_DISTANCE_M,
    OverlayColours, ViewOptions,
};
use observation_core::geo::{BBox, LatLon, MAX_BBOX_SIDE_M, MIN_BBOX_SIDE_M};
use observation_core::scene::SceneFile;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The side of the area asked for around a centre when no radius is given,
/// metres.
const DEFAULT_RADIUS_M: f64 = 150.0;
/// The most places search_places returns.
const MAX_PLACES: usize = 5;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FocusArgs {
    /// The spot to look at, `[x, z]` in metres from the area's origin (x
    /// east, z south). Give this or `camera`.
    #[serde(default)]
    pub at: Option<[f32; 2]>,
    /// A camera's id: look at that camera instead.
    #[serde(default)]
    pub camera: Option<u32>,
    /// How far the view is from the spot, metres (8 to 1200; 40 shows a
    /// front garden, 200 the street). Omit to keep the zoom, at most 60 m.
    #[serde(default)]
    pub distance_m: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SelectArgs {
    /// Camera ids from get_app_state's scene; `[]` clears the selection.
    /// The app selects one camera at a time: the first one is selected.
    pub ids: Vec<u32>,
}

/// An area or target by its id or its name.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum IdOrName {
    Id(u32),
    Name(String),
}

impl From<IdOrName> for ItemRef {
    fn from(i: IdOrName) -> Self {
        match i {
            IdOrName::Id(id) => Self::Id(id),
            IdOrName::Name(name) => Self::Name(name),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HighlightArgs {
    /// Mark this camera (its id).
    #[serde(default)]
    pub camera: Option<u32>,
    /// Mark this target (its id or name).
    #[serde(default)]
    pub target: Option<IdOrName>,
    /// Mark this labelled area (its id or name).
    #[serde(default)]
    pub area: Option<IdOrName>,
    /// Mark this spot on the ground, `[x, z]` metres (x east, z south).
    #[serde(default)]
    pub point: Option<[f32; 2]>,
    /// true removes the mark. Give one of camera, target, area or point, or
    /// clear.
    #[serde(default)]
    pub clear: bool,
    /// A short note shown by the mark (at most 80 characters), such as
    /// "no camera sees this gate".
    #[serde(default)]
    pub note: Option<String>,
}

/// A detail level.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// 25 px/m: can tell someone is there.
    Detect,
    /// 62 px/m: clothing and activity.
    Observe,
    /// 125 px/m: a person you know.
    #[serde(alias = "recognize")]
    Recognise,
    /// 250 px/m: a stranger, beyond doubt.
    Identify,
}

/// How the coverage overlay colours the ground.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Overlay {
    /// By how many cameras see each spot.
    Overlap,
    /// Each camera in its own colour.
    PerCamera,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ViewOptionsArgs {
    /// true: night (infrared), false: day. Saved in the scene; the user can
    /// undo it.
    #[serde(default)]
    pub night: Option<bool>,
    /// The detail level the coverage is figured at. Saved in the scene; the
    /// user can undo it.
    #[serde(default)]
    pub detail_level: Option<Level>,
    /// How the overlay colours the ground.
    #[serde(default)]
    pub overlay: Option<Overlay>,
    /// Show every camera's view cone (the selected one always shows).
    #[serde(default)]
    pub cones: Option<bool>,
    /// Show coverage on building walls.
    #[serde(default)]
    pub walls: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadAreaArgs {
    /// The area, `[south, west, north, east]` in degrees, as search_places
    /// returns it. Give this or `center`.
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    /// The area's centre, `[lat, lon]` in degrees.
    #[serde(default)]
    pub center: Option<[f64; 2]>,
    /// With `center`: half the side of the square, metres (10 to 250,
    /// default 150).
    #[serde(default)]
    pub radius_m: Option<f64>,
    /// Why, in a sentence the user reads before deciding (at most 200
    /// characters).
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// A place to look for: an address, a street and town, or a postcode.
    pub query: String,
}

/// A view command sent.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ViewDone {
    /// What the app was asked to do, in words.
    pub summary: String,
    /// The command's id in this connection.
    pub command_id: u64,
    /// The area asked for, `[south, west, north, east]`, for
    /// request_load_area.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
}

/// A place found.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlaceOut {
    /// The place's full name, as the geocoder gives it.
    pub name: String,
    /// `[lat, lon]`, degrees.
    pub center: [f64; 2],
    /// `[south, west, north, east]`, degrees, if the geocoder gave one:
    /// pass it to request_load_area.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
}

/// The places found.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlacesOut {
    pub summary: String,
    pub places: Vec<PlaceOut>,
    /// The credit for the search: pass it on with the results.
    pub credit: String,
}

/// The app's shared scene, if there is one and it reads.
async fn shared_scene(mcp: &ObservationMcp) -> Result<Option<SceneFile>, ToolError> {
    if !mcp.link.is_connected() {
        return Err(ToolError::NotConnected);
    }
    // Fresh when the server answers; what was last read when it does not.
    let snapshot = match mcp.link.refresh().await {
        Ok(s) => s,
        Err(_) => mcp.link.snapshot(),
    };
    if !mcp.link.is_connected() {
        return Err(ToolError::NotConnected);
    }
    Ok(snapshot
        .view
        .and_then(|v| v.state)
        .and_then(|s| edit::parse(&s.scene).ok()))
}

/// The ids in a list, for an error.
fn listed(ids: impl Iterator<Item = u32>) -> String {
    let ids: Vec<String> = ids.map(|i| i.to_string()).collect();
    if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.join(", ")
    }
}

fn no_camera(scene: &SceneFile, id: u32) -> ToolError {
    ToolError::invalid(format!(
        "no camera {id} in the app's scene (its cameras: {})",
        listed(scene.cameras.iter().map(|c| c.id))
    ))
}

/// A camera's words: "camera 3 ‘Porch’".
fn camera_words(scene: Option<&SceneFile>, id: u32) -> String {
    match scene
        .and_then(|s| s.cameras.iter().find(|c| c.id == id))
        .map(|c| c.label.trim())
        .filter(|l| !l.is_empty())
    {
        Some(label) => format!("camera {id} ‘{label}’"),
        None => format!("camera {id}"),
    }
}

/// Finds the item `r` names among `(id, name)`; its id and words.
fn find_item<'a>(
    items: impl Iterator<Item = (u32, Option<&'a str>)> + Clone,
    r: &ItemRef,
    what: &str,
) -> Result<(u32, String), ToolError> {
    items
        .clone()
        .find(|(id, name)| r.matches(*id, *name))
        .map(|(id, name)| {
            let words = match name.map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) => format!("{what} {id} ‘{n}’"),
                None => format!("{what} {id}"),
            };
            (id, words)
        })
        .ok_or_else(|| {
            let known: Vec<String> = items
                .map(|(id, name)| match name {
                    Some(n) if !n.trim().is_empty() => format!("{id} ‘{}’", n.trim()),
                    _ => id.to_string(),
                })
                .collect();
            ToolError::invalid(format!(
                "no such {what} in the app's scene (its {what}s: {})",
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            ))
        })
}

/// The area the app can load for `bbox`: it as it is when each side is 20
/// to 500 m, else the square around its centre with the nearest allowed
/// side, with a note saying so.
pub fn loadable(bbox: BBox) -> (BBox, Option<String>) {
    if bbox.validate_size().is_ok() {
        return (bbox, None);
    }
    let (w, h) = (bbox.width_m(), bbox.height_m());
    let side = w.max(h).clamp(MIN_BBOX_SIDE_M, MAX_BBOX_SIDE_M);
    let note = format!(
        "The area given was {w:.0} by {h:.0} m; the app loads 20 to 500 m a side, so the \
         {side:.0} m square around its centre was asked for."
    );
    (BBox::around(bbox.center(), side), Some(note))
}

fn as_array(b: &BBox) -> [f64; 4] {
    [b.south, b.west, b.north, b.east]
}

#[tool_router(router = view_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Move the user's 3D view in the connected app to look at a spot [x, z] (metres, x east, z south) or a camera. Changes only the view, never the scene.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) async fn focus_view(
        &self,
        Parameters(a): Parameters<FocusArgs>,
    ) -> Result<Json<ViewDone>, String> {
        focus(self, a).await.map(Json).map_err(|e| e.to_string())
    }

    #[tool(
        description = "Select cameras in the connected app, by id ([] clears the selection). The app selects one camera at a time, so the first id is selected. Changes only the selection, never the scene.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) async fn select_cameras(
        &self,
        Parameters(a): Parameters<SelectArgs>,
    ) -> Result<Json<ViewDone>, String> {
        select(self, a).await.map(Json).map_err(|e| e.to_string())
    }

    #[tool(
        description = "Mark a camera, a target, a labelled area or a spot in the connected app's 3D view, with a short note shown by it; clear: true removes the mark. A new mark replaces the last. Never changes the scene.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) async fn highlight(
        &self,
        Parameters(a): Parameters<HighlightArgs>,
    ) -> Result<Json<ViewDone>, String> {
        mark(self, a).await.map(Json).map_err(|e| e.to_string())
    }

    #[tool(
        description = "Change the connected app's view settings: night (infrared) or day, the detail level (detect, observe, recognise, identify), the overlay colours (overlap or per_camera), and whether every view cone and the coverage on walls show. Give only what to change. Night and detail level are saved in the scene, and the user can undo them.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) async fn set_view_options(
        &self,
        Parameters(a): Parameters<ViewOptionsArgs>,
    ) -> Result<Json<ViewDone>, String> {
        view_options(self, a)
            .await
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Ask the user to load an area in the connected app: give its bbox [south, west, north, east] (from search_places) or a center [lat, lon] with radius_m, and a reason. The app shows the request with Load and Dismiss; nothing loads unless the user clicks Load, so never say it has loaded until get_app_state shows the new area.",
        annotations(
            read_only_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn request_load_area(
        &self,
        Parameters(a): Parameters<LoadAreaArgs>,
    ) -> Result<Json<ViewDone>, String> {
        load_area(self, a)
            .await
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Search for a place (an address, a street and town, or a postcode) with the connected app's server's place search, if its operator turned it on. Returns up to five places with their bbox, for request_load_area, and the credit to pass on.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    pub(crate) async fn search_places(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<Json<PlacesOut>, String> {
        search(self, a).await.map(Json).map_err(|e| e.to_string())
    }
}

/// What the agent is told after every command but a request to load: that
/// the user may have turned commands off.
const VIEW_NOTE: &str = "The user can turn the agent's view commands off in the app; if the view \
                         did not change, ask them.";

async fn focus(mcp: &ObservationMcp, a: FocusArgs) -> Result<ViewDone, ToolError> {
    let scene = shared_scene(mcp).await?;
    let (at, what) = match (a.at, a.camera) {
        (Some(at), None) => (at, format!("x {:.1}, z {:.1}", at[0], at[1])),
        (None, Some(id)) => {
            let scene = scene.as_ref().ok_or_else(|| {
                ToolError::invalid("the app has not shared a scene yet: give `at` instead")
            })?;
            let c = scene
                .cameras
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| no_camera(scene, id))?;
            (
                [c.position[0], c.position[2]],
                camera_words(Some(scene), id),
            )
        }
        _ => {
            return Err(ToolError::invalid(
                "give either `at` ([x, z] metres) or `camera` (an id), not both",
            ));
        }
    };
    let mut clamped = None;
    let distance_m = a.distance_m.map(|d| {
        let c = if d.is_finite() {
            d.clamp(MIN_FOCUS_DISTANCE_M, MAX_FOCUS_DISTANCE_M)
        } else {
            MIN_FOCUS_DISTANCE_M.max(60.0)
        };
        if c != d {
            clamped = Some(c);
        }
        c
    });
    let id = mcp
        .link
        .command(AgentCommand::FocusView { at, distance_m })
        .await?;
    let mut summary = match distance_m {
        Some(d) => format!("Asked the app to look at {what} from {d:.0} m away."),
        None => format!("Asked the app to look at {what}."),
    };
    if let Some(c) = clamped {
        summary.push_str(&format!(" The distance was kept to {c:.0} m (8 to 1200)."));
    }
    summary.push(' ');
    summary.push_str(VIEW_NOTE);
    Ok(ViewDone {
        summary,
        command_id: id,
        bbox: None,
    })
}

async fn select(mcp: &ObservationMcp, a: SelectArgs) -> Result<ViewDone, ToolError> {
    let scene = shared_scene(mcp).await?;
    if let Some(scene) = &scene
        && let Some(&missing) = a
            .ids
            .iter()
            .find(|id| !scene.cameras.iter().any(|c| c.id == **id))
    {
        return Err(no_camera(scene, missing));
    }
    let first = a.ids.first().copied();
    let extra = a.ids.len().saturating_sub(1);
    let id = mcp
        .link
        .command(AgentCommand::SelectCameras { ids: a.ids })
        .await?;
    let mut summary = match first {
        Some(first) => format!(
            "Asked the app to select {}.",
            camera_words(scene.as_ref(), first)
        ),
        None => "Asked the app to clear the selection.".to_owned(),
    };
    if extra > 0 {
        summary.push_str(" The app selects one camera at a time, so only the first is selected.");
    }
    summary.push(' ');
    summary.push_str(VIEW_NOTE);
    Ok(ViewDone {
        summary,
        command_id: id,
        bbox: None,
    })
}

async fn mark(mcp: &ObservationMcp, a: HighlightArgs) -> Result<ViewDone, ToolError> {
    let given = [
        a.camera.is_some(),
        a.target.is_some(),
        a.area.is_some(),
        a.point.is_some(),
        a.clear,
    ]
    .iter()
    .filter(|g| **g)
    .count();
    if given != 1 {
        return Err(ToolError::invalid(
            "give exactly one of camera, target, area or point, or clear: true",
        ));
    }
    let scene = shared_scene(mcp).await?;
    let (what, words) = if let Some(id) = a.camera {
        if let Some(scene) = &scene
            && !scene.cameras.iter().any(|c| c.id == id)
        {
            return Err(no_camera(scene, id));
        }
        (Highlight::Camera { id }, camera_words(scene.as_ref(), id))
    } else if let Some(t) = a.target {
        let item = ItemRef::from(t);
        let words = match &scene {
            Some(s) => {
                let items = s.targets.iter().map(|t| (t.id, Some(t.name.as_str())));
                find_item(items, &item, "target")?.1
            }
            None => "the target".to_owned(),
        };
        (Highlight::Target { item }, words)
    } else if let Some(r) = a.area {
        let item = ItemRef::from(r);
        let words = match &scene {
            Some(s) => {
                let items = s.areas.iter().map(|a| (a.id, a.name.as_deref()));
                find_item(items, &item, "area")?.1
            }
            None => "the area".to_owned(),
        };
        (Highlight::Area { item }, words)
    } else if let Some(at) = a.point {
        (
            Highlight::Point { at },
            format!("the spot at x {:.1}, z {:.1}", at[0], at[1]),
        )
    } else {
        (Highlight::Clear, String::new())
    };
    let clear = what == Highlight::Clear;
    let id = mcp
        .link
        .command(AgentCommand::Highlight { what, note: a.note })
        .await?;
    let summary = if clear {
        "Asked the app to remove the mark.".to_owned()
    } else {
        format!("Asked the app to mark {words}. {VIEW_NOTE}")
    };
    Ok(ViewDone {
        summary,
        command_id: id,
        bbox: None,
    })
}

async fn view_options(mcp: &ObservationMcp, a: ViewOptionsArgs) -> Result<ViewDone, ToolError> {
    if !mcp.link.is_connected() {
        return Err(ToolError::NotConnected);
    }
    let options = ViewOptions {
        night: a.night,
        detail_level: a.detail_level.map(|l| match l {
            Level::Detect => DetailLevel::Detect,
            Level::Observe => DetailLevel::Observe,
            Level::Recognise => DetailLevel::Recognise,
            Level::Identify => DetailLevel::Identify,
        }),
        overlay: a.overlay.map(|o| match o {
            Overlay::Overlap => OverlayColours::Overlap,
            Overlay::PerCamera => OverlayColours::PerCamera,
        }),
        cones: a.cones,
        walls: a.walls,
    };
    if options.is_empty() {
        return Err(ToolError::invalid(
            "give at least one of night, detail_level, overlay, cones or walls",
        ));
    }
    let mut changes = Vec::new();
    if let Some(n) = a.night {
        changes.push(if n { "night (infrared)" } else { "day" }.to_owned());
    }
    if let Some(l) = a.detail_level {
        let name = match l {
            Level::Detect => "detect",
            Level::Observe => "observe",
            Level::Recognise => "recognise",
            Level::Identify => "identify",
        };
        changes.push(format!("detail level {name}"));
    }
    if let Some(o) = a.overlay {
        changes.push(
            match o {
                Overlay::Overlap => "overlay by overlap",
                Overlay::PerCamera => "overlay per camera",
            }
            .to_owned(),
        );
    }
    if let Some(c) = a.cones {
        changes.push(format!(
            "every view cone {}",
            if c { "shown" } else { "hidden" }
        ));
    }
    if let Some(w) = a.walls {
        changes.push(format!(
            "coverage on walls {}",
            if w { "shown" } else { "hidden" }
        ));
    }
    let id = mcp
        .link
        .command(AgentCommand::SetViewOptions(options))
        .await?;
    let mut summary = format!("Asked the app for {}.", changes.join(", "));
    if a.night.is_some() || a.detail_level.is_some() {
        summary.push_str(
            " Night and the detail level are saved in the scene; the user can undo them.",
        );
    }
    summary.push(' ');
    summary.push_str(VIEW_NOTE);
    Ok(ViewDone {
        summary,
        command_id: id,
        bbox: None,
    })
}

async fn load_area(mcp: &ObservationMcp, a: LoadAreaArgs) -> Result<ViewDone, ToolError> {
    if !mcp.link.is_connected() {
        return Err(ToolError::NotConnected);
    }
    let (bbox, mut note) = match (a.bbox, a.center) {
        (Some([south, west, north, east]), None) => {
            let bbox = BBox::new(south, west, north, east).map_err(|e| {
                ToolError::invalid(format!(
                    "the bbox is not usable ({e}): give [south, west, north, east] in degrees"
                ))
            })?;
            loadable(bbox)
        }
        (None, Some([lat, lon])) => {
            let r = a.radius_m.unwrap_or(DEFAULT_RADIUS_M);
            let half = (MIN_BBOX_SIDE_M / 2.0, MAX_BBOX_SIDE_M / 2.0);
            if !r.is_finite() {
                return Err(ToolError::invalid("radius_m must be a number of metres"));
            }
            let kept = r.clamp(half.0, half.1);
            let bbox = BBox::around(LatLon::new(lat, lon), 2.0 * kept);
            let bbox = BBox::new(bbox.south, bbox.west, bbox.north, bbox.east).map_err(|e| {
                ToolError::invalid(format!(
                    "the centre is not usable ({e}): give [lat, lon] in degrees"
                ))
            })?;
            let note =
                (kept != r).then(|| format!("The radius was kept to {kept:.0} m (10 to 250)."));
            (bbox, note)
        }
        _ => {
            return Err(ToolError::invalid(
                "give either bbox [south, west, north, east] or center [lat, lon], not both",
            ));
        }
    };
    let id = mcp
        .link
        .command(AgentCommand::RequestLoadArea {
            bbox,
            reason: a.reason,
        })
        .await?;
    let mut summary = format!(
        "The app now asks the user to load the area {:.0} by {:.0} m around {:.5}, {:.5}. \
         Nothing loads unless they click Load: tell them it is waiting for them, and check \
         get_app_state for the new area.",
        bbox.width_m(),
        bbox.height_m(),
        bbox.center().lat,
        bbox.center().lon
    );
    if let Some(n) = note.take() {
        summary.push(' ');
        summary.push_str(&n);
    }
    Ok(ViewDone {
        summary,
        command_id: id,
        bbox: Some(as_array(&bbox)),
    })
}

async fn search(mcp: &ObservationMcp, a: SearchArgs) -> Result<PlacesOut, ToolError> {
    let query = a.query.trim();
    if query.is_empty() {
        return Err(ToolError::invalid(
            "say what to look for: an address, a street and town, or a postcode",
        ));
    }
    let answer = mcp.link.search_places(query).await?;
    let places: Vec<PlaceOut> = answer
        .results
        .into_iter()
        .filter(|p| p.lat.is_finite() && p.lon.is_finite())
        .take(MAX_PLACES)
        .map(|p| PlaceOut {
            name: p.label.chars().take(200).collect(),
            center: [p.lat, p.lon],
            bbox: p.bbox.filter(|b| b.iter().all(|v| v.is_finite())),
        })
        .collect();
    let summary = match places.first() {
        None => format!("No place found for “{query}”. Try a fuller address or the town's name."),
        Some(first) => format!(
            "Found {} place{} for “{query}”; the first is {}. Pass a place's bbox to \
             request_load_area to ask the user to load it.",
            places.len(),
            if places.len() == 1 { "" } else { "s" },
            first.name
        ),
    };
    Ok(PlacesOut {
        summary,
        places,
        credit: answer.attribution,
    })
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod view_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_area_too_large_or_small_becomes_the_nearest_the_app_loads() {
        let fine = BBox::new(51.5, -0.13, 51.502, -0.127).unwrap();
        assert_eq!(loadable(fine), (fine, None));
        let town = BBox::new(51.0, -1.0, 51.1, -0.9).unwrap();
        let (b, note) = loadable(town);
        assert!(b.validate_size().is_ok(), "{b:?}");
        assert!(note.unwrap().contains("500 m square"));
        let centre = (b.center().lat - town.center().lat).abs();
        assert!(centre < 1e-9);
        let speck = BBox::new(51.5, -0.13, 51.50001, -0.12999).unwrap();
        let (b, note) = loadable(speck);
        assert!(b.validate_size().is_ok(), "{b:?}");
        assert!(note.unwrap().contains("20 m square"));
    }

    #[test]
    fn items_are_found_by_id_or_name_and_a_miss_lists_the_rest() {
        let items = [(1, Some("Front door")), (2, None)];
        let found = find_item(
            items.into_iter(),
            &ItemRef::Name("front DOOR".into()),
            "target",
        );
        assert_eq!(found.unwrap(), (1, "target 1 ‘Front door’".to_owned()));
        let found = find_item(items.into_iter(), &ItemRef::Id(2), "target");
        assert_eq!(found.unwrap(), (2, "target 2".to_owned()));
        let miss = find_item(items.into_iter(), &ItemRef::Name("gate".into()), "target");
        let why = miss.unwrap_err().to_string();
        assert!(why.contains("1 ‘Front door’, 2"), "{why}");
    }
}
