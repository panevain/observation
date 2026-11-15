//! View commands: the agent points the user at something in the connected
//! app (see [`observation_agent_api::command`]), and finds places to load.
//!
//! The tools move the user's 3D view, select cameras, mark a camera, a
//! target, an area or a spot with a short note, change the view settings
//! (the overlay colours, the cones, the walls), and ask the user to load an
//! area. None of them changes the scene: another lighting or detail level
//! is a scene change, for propose_changes. The app never loads an area
//! without the user's click. The user can turn the view commands off in the
//! app's Connect agent dialog; the request to load an area is shown either
//! way.
//!
//! When a tool names a camera, a target or an area, it is looked up in the
//! app's shared scene first, so a small model hears of a wrong id at once
//! and with the ids that exist. A tool given only a position does not read
//! the scene.

use crate::ObservationMcp;
use crate::ToolError;
use crate::edit;
use observation_agent_api::{
    AgentCommand, Highlight, ItemRef, MAX_FOCUS_DISTANCE_M, MIN_FOCUS_DISTANCE_M, OverlayColours,
    ViewOptions, find_item, item_words,
};
use observation_core::geo::{BBox, LatLon, MAX_BBOX_SIDE_M, MIN_BBOX_SIDE_M};
use observation_core::scene::SceneFile;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Half the side of the area asked for around a centre (or a place given
/// as a point) when no radius is given, metres.
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HighlightArgs {
    /// Mark this camera (its id).
    #[serde(default)]
    pub camera: Option<u32>,
    /// Mark this target (its id or name).
    #[serde(default)]
    pub target: Option<ItemRef>,
    /// Mark this labelled area (its id or name).
    #[serde(default)]
    pub area: Option<ItemRef>,
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ViewOptionsArgs {
    /// How the overlay colours the ground: by how many cameras see each
    /// spot, or each camera in its own colour.
    #[serde(default)]
    pub overlay: Option<OverlayColours>,
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
    /// returns it.
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    /// The place's own point, `[lat, lon]` in degrees: with `bbox`, an area
    /// too large or too small is squared around it; alone, the square of
    /// `radius_m` around it is asked for.
    #[serde(default)]
    pub center: Option<[f64; 2]>,
    /// With `center` alone: half the side of the square, metres (10 to 250,
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
    /// `[lat, lon]`, degrees: pass it to request_load_area as `center`.
    pub center: [f64; 2],
    /// `[south, west, north, east]`, degrees, if the geocoder gave one:
    /// pass it to request_load_area as `bbox`.
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

fn connected(mcp: &ObservationMcp) -> Result<(), ToolError> {
    if mcp.link.is_connected() {
        Ok(())
    } else {
        Err(ToolError::NotConnected)
    }
}

/// The app's shared scene, read afresh, if there is one and it reads.
async fn shared_scene(mcp: &ObservationMcp) -> Result<Option<SceneFile>, ToolError> {
    connected(mcp)?;
    // Fresh when the server answers; what was last read when it does not.
    let snapshot = match mcp.link.refresh().await {
        Ok(s) => s,
        Err(_) => mcp.link.snapshot(),
    };
    connected(mcp)?;
    Ok(snapshot
        .view
        .and_then(|v| v.state)
        .and_then(|s| edit::parse(&s.scene).ok()))
}

/// Items in a list, for an error: "1 ‘Porch’, 2", or "none".
fn listed<'a>(items: impl Iterator<Item = (u32, Option<&'a str>)>) -> String {
    let known: Vec<String> = items
        .map(
            |(id, name)| match name.map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) => format!("{id} ‘{n}’"),
                None => id.to_string(),
            },
        )
        .collect();
    if known.is_empty() {
        "none".to_owned()
    } else {
        known.join(", ")
    }
}

fn cameras(scene: &SceneFile) -> impl Iterator<Item = (u32, Option<&str>)> + Clone {
    scene.cameras.iter().map(|c| (c.id, Some(c.label.as_str())))
}

fn no_camera(scene: &SceneFile, id: u32) -> ToolError {
    ToolError::invalid(format!(
        "no camera {id} in the app's scene (its cameras: {})",
        listed(cameras(scene))
    ))
}

/// A camera of the scene in words, or an error listing the cameras there.
fn camera(scene: &SceneFile, id: u32) -> Result<String, ToolError> {
    find_item(cameras(scene), &ItemRef::Id(id), "camera")
        .map_or_else(|| Err(no_camera(scene, id)), |(_, words)| Ok(words))
}

/// The item `r` names in `items`, in words, or an error listing them.
fn item<'a>(
    items: impl Iterator<Item = (u32, Option<&'a str>)> + Clone,
    r: &ItemRef,
    what: &str,
) -> Result<String, ToolError> {
    find_item(items.clone(), r, what)
        .map(|(_, words)| words)
        .ok_or_else(|| {
            ToolError::invalid(format!(
                "no such {what} in the app's scene (its {what}s: {})",
                listed(items)
            ))
        })
}

/// The area the app can load for `bbox`: it as it is when each side is 20
/// to 500 m, else the square with the nearest allowed side around `about`
/// (the place's own point) or else the box's middle, with a note saying so.
pub fn loadable(bbox: BBox, about: Option<LatLon>) -> (BBox, Option<String>) {
    if bbox.validate_size().is_ok() {
        return (bbox, None);
    }
    let (w, h) = (bbox.width_m(), bbox.height_m());
    let side = w.max(h).clamp(MIN_BBOX_SIDE_M, MAX_BBOX_SIDE_M);
    let around = if about.is_some() {
        "the place's point"
    } else {
        "its middle"
    };
    let note = format!(
        "The area given was {w:.0} by {h:.0} m; the app loads 20 to 500 m a side, so the \
         {side:.0} m square around {around} was asked for."
    );
    (
        BBox::around(about.unwrap_or_else(|| bbox.center()), side),
        Some(note),
    )
}

/// A box the agent gave, `[south, west, north, east]`, as the app can load
/// it: edges given the wrong way round are put right, and a box with no
/// size (a place given as a point) becomes the default square around it.
fn bbox_from(b: [f64; 4], about: Option<LatLon>) -> Result<(BBox, Option<String>), ToolError> {
    let refuse = |why: String| {
        ToolError::invalid(format!(
            "the bbox is not usable ({why}): give [south, west, north, east] in degrees"
        ))
    };
    if b.iter().any(|v| !v.is_finite()) {
        return Err(refuse("not all numbers".into()));
    }
    let (south, north) = (b[0].min(b[2]), b[0].max(b[2]));
    let (west, east) = (b[1].min(b[3]), b[1].max(b[3]));
    if south == north || west == east {
        let at = about.unwrap_or_else(|| LatLon::new((south + north) / 2.0, (west + east) / 2.0));
        let square = BBox::around(at, 2.0 * DEFAULT_RADIUS_M);
        let square = BBox::new(square.south, square.west, square.north, square.east)
            .map_err(|e| refuse(e.to_string()))?;
        let note = format!(
            "The bbox given had no size, so the {:.0} m square around the place was asked for.",
            2.0 * DEFAULT_RADIUS_M
        );
        return Ok((square, Some(note)));
    }
    let bbox = BBox::new(south, west, north, east).map_err(|e| refuse(e.to_string()))?;
    Ok(loadable(bbox, about))
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
        description = "Change how the connected app shows coverage: the overlay colours (overlap or per_camera), and whether every view cone and the coverage on walls show. Give only what to change. Never changes the scene: for night or another detail level, edit the scene's analysis settings and use propose_changes.",
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
        description = "Ask the user to load an area in the connected app, with a reason. Give a place's bbox [south, west, north, east] and its center [lat, lon] from search_places (a bbox over 500 m is cut to the 500 m square around the center), or a center with radius_m. The app shows the request with Load and Dismiss; nothing loads unless the user clicks Load, so never say it has loaded until get_app_state shows the new area.",
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
        description = "Search for a place (an address, a street and town, or a postcode) with the connected app's server's place search, if its operator turned it on. Returns up to five places with their center and bbox, for request_load_area, and the credit to pass on.",
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

fn done(summary: String, command_id: u64) -> ViewDone {
    ViewDone {
        summary,
        command_id,
        bbox: None,
    }
}

async fn focus(mcp: &ObservationMcp, a: FocusArgs) -> Result<ViewDone, ToolError> {
    let (at, what) = match (a.at, a.camera) {
        (Some(at), None) => {
            connected(mcp)?;
            (at, format!("x {:.1}, z {:.1}", at[0], at[1]))
        }
        (None, Some(id)) => {
            let scene = shared_scene(mcp).await?.ok_or_else(|| {
                ToolError::invalid("the app has not shared a scene yet: give `at` instead")
            })?;
            let words = camera(&scene, id)?;
            let c = scene
                .cameras
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| no_camera(&scene, id))?;
            ([c.position[0], c.position[2]], words)
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
    Ok(done(summary, id))
}

async fn select(mcp: &ObservationMcp, a: SelectArgs) -> Result<ViewDone, ToolError> {
    let scene = shared_scene(mcp).await?;
    let first = match (&scene, a.ids.first()) {
        (Some(scene), _) => {
            if let Some(&missing) = a
                .ids
                .iter()
                .find(|id| !scene.cameras.iter().any(|c| c.id == **id))
            {
                return Err(no_camera(scene, missing));
            }
            a.ids.first().map(|id| camera(scene, *id)).transpose()?
        }
        (None, first) => first.map(|id| item_words("camera", *id, None)),
    };
    let extra = a.ids.len().saturating_sub(1);
    let id = mcp
        .link
        .command(AgentCommand::SelectCameras { ids: a.ids })
        .await?;
    let mut summary = match first {
        Some(words) => format!("Asked the app to select {words}."),
        None => "Asked the app to clear the selection.".to_owned(),
    };
    if extra > 0 {
        summary.push_str(" The app selects one camera at a time, so only the first is selected.");
    }
    summary.push(' ');
    summary.push_str(VIEW_NOTE);
    Ok(done(summary, id))
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
    // The scene is read only to look up what is named.
    let named = a.camera.is_some() || a.target.is_some() || a.area.is_some();
    let scene = if named {
        shared_scene(mcp).await?
    } else {
        connected(mcp)?;
        None
    };
    let (what, words) = if let Some(id) = a.camera {
        let words = match &scene {
            Some(s) => camera(s, id)?,
            None => item_words("camera", id, None),
        };
        (Highlight::Camera { id }, words)
    } else if let Some(r) = a.target {
        let words = match &scene {
            Some(s) => item(
                s.targets.iter().map(|t| (t.id, Some(t.name.as_str()))),
                &r,
                "target",
            )?,
            None => "the target".to_owned(),
        };
        (Highlight::Target { item: r }, words)
    } else if let Some(r) = a.area {
        let words = match &scene {
            Some(s) => item(
                s.areas.iter().map(|a| (a.id, a.name.as_deref())),
                &r,
                "area",
            )?,
            None => "the area".to_owned(),
        };
        (Highlight::Area { item: r }, words)
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
    Ok(done(summary, id))
}

async fn view_options(mcp: &ObservationMcp, a: ViewOptionsArgs) -> Result<ViewDone, ToolError> {
    connected(mcp)?;
    let options = ViewOptions {
        overlay: a.overlay,
        cones: a.cones,
        walls: a.walls,
    };
    if options.is_empty() {
        return Err(ToolError::invalid(
            "give at least one of overlay, cones or walls (night and the detail level are scene \
             settings: change them with propose_changes)",
        ));
    }
    let mut changes = Vec::new();
    match a.overlay {
        Some(OverlayColours::Overlap) => changes.push("the overlay by overlap".to_owned()),
        Some(OverlayColours::PerCamera) => changes.push("the overlay per camera".to_owned()),
        // `checked` refuses it, below.
        Some(OverlayColours::Unknown) | None => {}
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
    Ok(done(
        format!("Asked the app for {}. {VIEW_NOTE}", changes.join(", ")),
        id,
    ))
}

async fn load_area(mcp: &ObservationMcp, a: LoadAreaArgs) -> Result<ViewDone, ToolError> {
    connected(mcp)?;
    let about = match a.center {
        Some([lat, lon]) if lat.is_finite() && lon.is_finite() => Some(LatLon::new(lat, lon)),
        Some(_) => {
            return Err(ToolError::invalid(
                "the center is not usable: give [lat, lon] in degrees",
            ));
        }
        None => None,
    };
    let (bbox, mut note) = match (a.bbox, about) {
        (Some(b), about) => bbox_from(b, about)?,
        (None, Some(at)) => {
            let r = a.radius_m.unwrap_or(DEFAULT_RADIUS_M);
            if !r.is_finite() {
                return Err(ToolError::invalid("radius_m must be a number of metres"));
            }
            let kept = r.clamp(MIN_BBOX_SIDE_M / 2.0, MAX_BBOX_SIDE_M / 2.0);
            let bbox = BBox::around(at, 2.0 * kept);
            let bbox = BBox::new(bbox.south, bbox.west, bbox.north, bbox.east).map_err(|e| {
                ToolError::invalid(format!(
                    "the center is not usable ({e}): give [lat, lon] in degrees"
                ))
            })?;
            let note =
                (kept != r).then(|| format!("The radius was kept to {kept:.0} m (10 to 250)."));
            (bbox, note)
        }
        (None, None) => {
            return Err(ToolError::invalid(
                "give a bbox [south, west, north, east] or a center [lat, lon]",
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
            "Found {} place{} for “{query}”; the first is {}. Pass a place's bbox and center to \
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
        assert_eq!(loadable(fine, None), (fine, None));
        let town = BBox::new(51.0, -1.0, 51.1, -0.9).unwrap();
        let (b, note) = loadable(town, None);
        assert!(b.validate_size().is_ok(), "{b:?}");
        assert!(note.unwrap().contains("500 m square around its middle"));
        assert!((b.center().lat - town.center().lat).abs() < 1e-9);
        // Around the place's own point, when it is given.
        let place = LatLon::new(51.02, -0.98);
        let (b, note) = loadable(town, Some(place));
        assert!(note.unwrap().contains("around the place's point"));
        assert!((b.center().lat - place.lat).abs() < 1e-9);
        assert!((b.center().lon - place.lon).abs() < 1e-9);
        let speck = BBox::new(51.5, -0.13, 51.50001, -0.12999).unwrap();
        let (b, note) = loadable(speck, None);
        assert!(b.validate_size().is_ok(), "{b:?}");
        assert!(note.unwrap().contains("20 m square"));
    }

    #[test]
    fn a_point_or_backwards_bbox_is_made_one_the_app_loads() {
        // A place given as a point, as geocoders give some.
        let (b, note) = bbox_from([51.5, -0.12, 51.5, -0.12], None).unwrap();
        assert!(b.validate_size().is_ok(), "{b:?}");
        assert!((b.center().lat - 51.5).abs() < 1e-9);
        assert!(note.unwrap().contains("300 m square"));
        // Edges the wrong way round are put right.
        let (b, note) = bbox_from([51.502, -0.127, 51.5, -0.13], None).unwrap();
        assert_eq!(b, BBox::new(51.5, -0.13, 51.502, -0.127).unwrap());
        assert_eq!(note, None);
        assert!(bbox_from([f64::NAN, 0.0, 1.0, 1.0], None).is_err());
        assert!(bbox_from([89.0, 0.0, 89.5, 0.1], None).is_err());
    }

    #[test]
    fn items_are_found_by_id_or_name_and_a_miss_lists_the_rest() {
        let items = [(1, Some("Front door")), (2, None)];
        let found = item(
            items.into_iter(),
            &ItemRef::Name("front  DOOR".into()),
            "target",
        );
        assert_eq!(found.unwrap(), "target 1 ‘Front door’");
        let found = item(items.into_iter(), &ItemRef::Id(2), "target");
        assert_eq!(found.unwrap(), "target 2");
        let miss = item(items.into_iter(), &ItemRef::Name("gate".into()), "target");
        let why = miss.unwrap_err().to_string();
        assert!(why.contains("1 ‘Front door’, 2"), "{why}");
    }

    #[test]
    fn the_overlay_schema_has_no_unknown() {
        let schema = schemars::schema_for!(ViewOptionsArgs);
        let text = serde_json::to_string(&schema).unwrap();
        assert!(text.contains("per_camera"), "{text}");
        assert!(!text.contains("unknown"), "{text}");
    }
}
