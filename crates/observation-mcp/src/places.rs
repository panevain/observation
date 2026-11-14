//! The named places user and agent share (#89, #259 part 1): labelled areas
//! and targets, edited as plain functions on a [`SceneFile`], and the tools
//! that edit them.
//!
//! Like the other scene tools, these return an edited copy of the scene.
//! Every input is checked here with a message for the agent, before
//! [`edit::finish`] reads the scene back through the app's sanitiser, which
//! would otherwise drop an unusable area or target without a word.

use crate::ToolError;
use crate::area::{bundle_for, check_inside, ground_y};
use crate::edit;
use crate::server::ObservationMcp;
use observation_core::bundle::AreaBundle;
use observation_core::coverage::{MAX_REGION_POINTS, RegionProblem, outline_problem};
use observation_core::footage::{
    AreaLabel, LabelledArea, MAX_AREAS, MAX_NAME_CHARS, MAX_TARGET_POINTS, MAX_TARGETS, Target,
    TargetPlace, TargetProblem, WallSpot, fresh_id, nearest_wall,
};
use observation_core::glam::Vec2;
use observation_core::scene::SceneFile;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How far from the point given a wall target looks for a building's wall,
/// metres.
pub const WALL_SNAP_M: f32 = 10.0;

/// What an area is, as the user says: the label is their statement about
/// the ground ("as drawn"), never inferred from data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LabelArg {
    /// The user's own land.
    Own,
    /// A neighbour's land.
    Neighbour,
    /// Public ground: a pavement, a verge, a park.
    Public,
    /// Anything else to count apart.
    Other,
    /// No label: the area only limits the figures or is counted on its own.
    None,
}

impl LabelArg {
    fn label(self) -> Option<AreaLabel> {
        match self {
            Self::Own => Some(AreaLabel::Own),
            Self::Neighbour => Some(AreaLabel::Neighbour),
            Self::Public => Some(AreaLabel::Public),
            Self::Other => Some(AreaLabel::Other),
            Self::None => None,
        }
    }
}

/// An area to add, or changes to one; omitted fields stay as they are.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct AreaEdit {
    /// The area to change; omit to add a new one.
    pub area_id: Option<u32>,
    /// Corners `[x, z]` in metres (x east, z south of the origin), in order
    /// around the area, not repeated at the end: 3 to 256 of them, all inside
    /// the scene's area. Needed for a new area.
    pub points: Option<Vec<[f32; 2]>>,
    /// What the ground is, as the user says; `none` removes the label.
    pub label: Option<LabelArg>,
    /// A name to show, such as "Our front garden" (at most 60 characters);
    /// "" removes it.
    pub name: Option<String>,
    /// Whether the area counts; a disabled area is kept but ignored.
    pub enabled: Option<bool>,
}

/// Where a target is.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlaceArg {
    /// A small patch of ground, such as a path or a gate's opening, tested at
    /// the scene's subject height above the ground.
    Ground {
        /// Corners `[x, z]`, metres, in order around the patch, not repeated
        /// at the end: 3 to 32 of them, at most 30 m across.
        points: Vec<[f32; 2]>,
    },
    /// A 1 m square of wall, such as a front door.
    Wall {
        /// Metres east of the origin: a spot on the wall when `normal` is
        /// given, otherwise a spot next to the wall meant (within 10 m).
        x: f32,
        /// Metres south of the origin, as `x`.
        z: f32,
        /// Height of the square's centre above the ground, metres. Defaults
        /// to the scene's subject height.
        height_m: Option<f32>,
        /// The direction the wall faces, out of the building, as `[x, z]`.
        /// With it, `[x, z]` is used as given; without it, the target goes
        /// on the nearest building wall to `[x, z]`, facing out.
        normal: Option<[f32; 2]>,
        /// Without `normal`: only a wall that faces this camera.
        facing_camera: Option<u32>,
        /// Without `normal`: only a wall that faces the nearest street.
        #[serde(default)]
        facing_street: bool,
    },
}

/// Where a wall target went.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WallOut {
    /// The building's index in the area (buildings are named by index and
    /// distance, never by address), if the target was put on the nearest
    /// wall.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub building: Option<u32>,
    /// How far the target was moved from the point given onto the wall,
    /// metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_m: Option<f32>,
    /// The square's centre, `[x, y, z]` (y over the terrain, as camera
    /// positions are).
    pub point: [f32; 3],
    /// The direction the wall faces, `[x, z]`.
    pub normal: [f32; 2],
    /// Height of the centre above the ground in front of the wall, metres.
    pub height_m: f32,
}

/// The result of an area or target edit.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlaceEdited {
    /// What changed.
    pub summary: String,
    /// The area added or changed, if one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area_id: Option<u32>,
    /// The target added or changed, if one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<u32>,
    /// Where a wall target went.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall: Option<WallOut>,
    /// The edited scene: pass it to the next scene tool.
    pub scene: Value,
}

impl PlaceEdited {
    fn new(summary: impl Into<String>, scene: &SceneFile) -> Result<Self, ToolError> {
        let (_, scene) = edit::finish(scene)?;
        Ok(Self {
            summary: summary.into(),
            area_id: None,
            target_id: None,
            wall: None,
            scene,
        })
    }
}

fn outline_message(what: &str, p: RegionProblem) -> String {
    match p {
        RegionProblem::TooFewCorners => format!("{what} needs at least three corners"),
        RegionProblem::NoArea => {
            format!("the corners of {what} lie on one line, so it encloses no area")
        }
        RegionProblem::SelfIntersecting => {
            format!("the outline of {what} crosses itself: list the corners in order around it")
        }
    }
}

/// Refuses an outline with more than `max` corners, a corner outside the
/// area, or one that encloses no area.
fn check_outline(
    bundle: &AreaBundle,
    points: &[[f32; 2]],
    what: &str,
    max: usize,
) -> Result<(), ToolError> {
    if points.len() > max {
        return Err(ToolError::invalid(format!(
            "{what} has at most {max} corners, not {}",
            points.len()
        )));
    }
    for p in points {
        check_inside(bundle, p[0], p[1])?;
    }
    match outline_problem(points) {
        Some(p) => Err(ToolError::invalid(outline_message(what, p))),
        None => Ok(()),
    }
}

/// A name trimmed, refused when too long or holding control characters
/// (names are shown as plain text, but they come from outside).
fn tidy_name(name: &str) -> Result<String, ToolError> {
    let name = name.trim();
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ToolError::invalid(format!(
            "a name is at most {MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(ToolError::invalid("a name cannot hold control characters"));
    }
    Ok(name.to_owned())
}

fn no_area(id: u32) -> ToolError {
    ToolError::invalid(format!("no area with id {id} in the scene"))
}

fn no_target(id: u32) -> ToolError {
    ToolError::invalid(format!("no target with id {id} in the scene"))
}

/// Adds an area, or changes the one `e.area_id` names; returns its id.
pub fn label_area(scene: &mut SceneFile, e: AreaEdit) -> Result<u32, ToolError> {
    let bundle = bundle_for(scene)?;
    let mut area = match e.area_id {
        Some(id) => scene
            .areas
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or_else(|| no_area(id))?,
        None => {
            if scene.areas.len() >= MAX_AREAS {
                return Err(ToolError::invalid(format!(
                    "a scene holds at most {MAX_AREAS} areas; remove one first"
                )));
            }
            LabelledArea {
                id: fresh_id(scene.areas.iter().map(|a| a.id)),
                label: None,
                name: None,
                points: e
                    .points
                    .clone()
                    .ok_or_else(|| ToolError::invalid("a new area needs its corners (points)"))?,
                enabled: true,
            }
        }
    };
    if let Some(points) = e.points {
        check_outline(&bundle, &points, "an area", MAX_REGION_POINTS)?;
        area.points = points;
    }
    if let Some(l) = e.label {
        area.label = l.label();
    }
    if let Some(n) = e.name {
        area.name = Some(tidy_name(&n)?).filter(|n| !n.is_empty());
    }
    if let Some(on) = e.enabled {
        area.enabled = on;
    }
    let id = area.id;
    match scene.areas.iter_mut().find(|a| a.id == id) {
        Some(slot) => *slot = area,
        None => scene.areas.push(area),
    }
    Ok(id)
}

/// Removes an area; returns whether the figures were limited to it (they no
/// longer are).
pub fn remove_area(scene: &mut SceneFile, id: u32) -> Result<bool, ToolError> {
    let before = scene.areas.len();
    scene.areas.retain(|a| a.id != id);
    if scene.areas.len() == before {
        return Err(no_area(id));
    }
    let limited = scene.limit_figures_to == Some(id);
    if limited {
        scene.limit_figures_to = None;
    }
    Ok(limited)
}

/// Limits the coverage figures to the area `id` names, or with `None` stops
/// limiting them (the area stays).
pub fn limit_figures_to(scene: &mut SceneFile, id: Option<u32>) -> Result<(), ToolError> {
    if let Some(id) = id
        && !scene.areas.iter().any(|a| a.id == id)
    {
        return Err(no_area(id));
    }
    scene.limit_figures_to = id;
    Ok(())
}

fn check_height(h: f32) -> Result<f32, ToolError> {
    if h.is_finite() && (0.0..=100.0).contains(&h) {
        Ok(h)
    } else {
        Err(ToolError::invalid(format!(
            "height {h} m is out of range: use 0 to 100 m above the ground"
        )))
    }
}

/// The target place `at` describes, and where a wall target went.
fn resolve(
    scene: &SceneFile,
    bundle: &AreaBundle,
    at: PlaceArg,
) -> Result<(TargetPlace, Option<WallOut>), ToolError> {
    match at {
        PlaceArg::Ground { points } => {
            check_outline(bundle, &points, "a ground target", MAX_TARGET_POINTS)?;
            let place = TargetPlace::Ground { points };
            if let Some(p) = place.problem() {
                return Err(ToolError::invalid(match p {
                    TargetProblem::Outline(o) => outline_message("a ground target", o),
                    TargetProblem::TooLarge => "a ground target is at most 30 m across; \
                         label an area (label_area) for larger ground"
                        .to_owned(),
                    other => other.message().to_owned(),
                }));
            }
            Ok((place, None))
        }
        PlaceArg::Wall {
            x,
            z,
            height_m,
            normal,
            facing_camera,
            facing_street,
        } => {
            check_inside(bundle, x, z)?;
            let height = check_height(height_m.unwrap_or(scene.params.subject_height_m))?;
            match normal {
                Some(n) => {
                    let len = n[0].hypot(n[1]);
                    if !len.is_finite() || len < 1e-3 {
                        return Err(ToolError::invalid(
                            "a wall's normal needs a direction: [x, z], not zero",
                        ));
                    }
                    let normal = [n[0] / len, n[1] / len];
                    let point = [x, ground_y(bundle, x, z) + height, z];
                    let wall = WallOut {
                        building: None,
                        moved_m: None,
                        point,
                        normal,
                        height_m: height,
                    };
                    Ok((TargetPlace::Wall { point, normal }, Some(wall)))
                }
                None => {
                    let spot = snap_to_wall(scene, bundle, [x, z], facing_camera, facing_street)?;
                    let n = Vec2::from_array(spot.normal);
                    let outside = Vec2::from_array(spot.xz) + n * 0.05;
                    let ground = ground_y(bundle, outside.x, outside.y);
                    let y = ground + height;
                    if y >= spot.top_y {
                        return Err(ToolError::invalid(format!(
                            "building {} is only {:.1} m tall there: give a lower height_m",
                            spot.building,
                            (spot.top_y - ground).max(0.0)
                        )));
                    }
                    if y < spot.bottom_y {
                        return Err(ToolError::invalid(format!(
                            "the wall of building {} starts {:.1} m above the ground there: \
                             give a higher height_m",
                            spot.building,
                            spot.bottom_y - ground
                        )));
                    }
                    let point = [spot.xz[0], y, spot.xz[1]];
                    let wall = WallOut {
                        building: Some(spot.building),
                        moved_m: Some(spot.distance_m),
                        point,
                        normal: spot.normal,
                        height_m: height,
                    };
                    Ok((
                        TargetPlace::Wall {
                            point,
                            normal: spot.normal,
                        },
                        Some(wall),
                    ))
                }
            }
        }
    }
}

/// The spot on the wall nearest `p` within [`WALL_SNAP_M`], facing the
/// camera or the street if asked.
fn snap_to_wall(
    scene: &SceneFile,
    bundle: &AreaBundle,
    p: [f32; 2],
    facing_camera: Option<u32>,
    facing_street: bool,
) -> Result<WallSpot, ToolError> {
    let camera = match facing_camera {
        Some(id) => {
            let c = scene
                .cameras
                .iter()
                .find(|c| c.id == id)
                .ok_or(ToolError::NoCamera(id))?;
            Some(Vec2::new(c.position[0], c.position[2]))
        }
        None => None,
    };
    let street = if facing_street {
        let road = bundle
            .nearest_road(p, |_| true)
            .ok_or_else(|| ToolError::invalid("the area has no streets for a wall to face"))?;
        Some(Vec2::from_array(road.point))
    } else {
        None
    };
    let faces = |w: &WallSpot, toward: Option<Vec2>| {
        toward.is_none_or(|t| Vec2::from_array(w.normal).dot(t - Vec2::from_array(w.xz)) > 0.0)
    };
    nearest_wall(bundle.footprints(), p, WALL_SNAP_M, |w| {
        faces(w, camera) && faces(w, street)
    })
    .ok_or_else(|| {
        let facing = match (facing_camera, facing_street) {
            (Some(id), true) => format!(" facing camera {id} and the street"),
            (Some(id), false) => format!(" facing camera {id}"),
            (None, true) => " facing the street".to_owned(),
            (None, false) => String::new(),
        };
        ToolError::invalid(format!(
            "no building wall{facing} within {WALL_SNAP_M:.0} m of [{}, {}]: give a point \
             next to the wall, or the wall's normal",
            p[0], p[1]
        ))
    })
}

/// Adds a target; returns its id and where a wall target went.
pub fn add_target(
    scene: &mut SceneFile,
    name: &str,
    at: PlaceArg,
) -> Result<(u32, Option<WallOut>), ToolError> {
    if scene.targets.len() >= MAX_TARGETS {
        return Err(ToolError::invalid(format!(
            "a scene holds at most {MAX_TARGETS} targets; remove one first"
        )));
    }
    let name = tidy_name(name)?;
    if name.is_empty() {
        return Err(ToolError::invalid(
            "a target needs a name, such as \"front door\"",
        ));
    }
    let bundle = bundle_for(scene)?;
    let (place, wall) = resolve(scene, &bundle, at)?;
    let id = fresh_id(scene.targets.iter().map(|t| t.id));
    scene.targets.push(Target {
        id,
        name,
        at: place,
    });
    Ok((id, wall))
}

/// Renames a target and/or moves it; returns where a wall target went.
pub fn update_target(
    scene: &mut SceneFile,
    id: u32,
    name: Option<String>,
    at: Option<PlaceArg>,
) -> Result<Option<WallOut>, ToolError> {
    let i = scene
        .targets
        .iter()
        .position(|t| t.id == id)
        .ok_or_else(|| no_target(id))?;
    let mut t = scene.targets[i].clone();
    if let Some(n) = name {
        let n = tidy_name(&n)?;
        if n.is_empty() {
            return Err(ToolError::invalid("a target's name cannot be blank"));
        }
        t.name = n;
    }
    let mut wall = None;
    if let Some(at) = at {
        let bundle = bundle_for(scene)?;
        let (place, w) = resolve(scene, &bundle, at)?;
        t.at = place;
        wall = w;
    }
    scene.targets[i] = t;
    Ok(wall)
}

pub fn remove_target(scene: &mut SceneFile, id: u32) -> Result<(), ToolError> {
    let before = scene.targets.len();
    scene.targets.retain(|t| t.id != id);
    if scene.targets.len() == before {
        return Err(no_target(id));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LabelAreaArgs {
    pub scene: Value,
    #[serde(flatten)]
    pub area: AreaEdit,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AreaIdArgs {
    pub scene: Value,
    /// The area's id in the scene.
    pub area_id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LimitArgs {
    pub scene: Value,
    /// The area to limit the coverage figures to; omit or null to stop
    /// limiting them (the area stays).
    pub area_id: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddTargetArgs {
    pub scene: Value,
    /// What the user calls the place, such as "front door" (at most 60
    /// characters).
    pub name: String,
    /// Where it is.
    pub at: PlaceArg,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateTargetArgs {
    pub scene: Value,
    /// The target's id in the scene.
    pub target_id: u32,
    /// A new name; omit to keep.
    pub name: Option<String>,
    /// A new place; omit to keep.
    pub at: Option<PlaceArg>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TargetIdArgs {
    pub scene: Value,
    /// The target's id in the scene.
    pub target_id: u32,
}

fn fail(e: ToolError) -> String {
    e.to_string()
}

/// How an area is described in a summary.
fn area_words(a: &LabelledArea) -> String {
    let label = a.label.map_or("unlabelled", AreaLabel::label);
    match &a.name {
        Some(n) => format!("area {} ({label}, \"{n}\")", a.id),
        None => format!("area {} ({label})", a.id),
    }
}

#[tool_router(router = places_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Add a labelled area (a ground polygon the user drew: own, neighbour, public, other or no label, with an optional name), or change one by area_id. camera_split reports how much of each camera's view falls in each area, as drawn.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn label_area(
        &self,
        Parameters(a): Parameters<LabelAreaArgs>,
    ) -> Result<Json<PlaceEdited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        let adding = a.area.area_id.is_none();
        let id = label_area(&mut s, a.area).map_err(fail)?;
        let words = s
            .areas
            .iter()
            .find(|x| x.id == id)
            .map(area_words)
            .unwrap_or_default();
        let verb = if adding { "added" } else { "updated" };
        let mut out = PlaceEdited::new(format!("{verb} {words}"), &s).map_err(fail)?;
        out.area_id = Some(id);
        Ok(Json(out))
    }

    #[tool(
        description = "Remove a labelled area from the scene. If the coverage figures were limited to it, they no longer are.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn remove_area(
        &self,
        Parameters(a): Parameters<AreaIdArgs>,
    ) -> Result<Json<PlaceEdited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        let limited = remove_area(&mut s, a.area_id).map_err(fail)?;
        let summary = if limited {
            format!(
                "removed area {}; the coverage figures are no longer limited to it",
                a.area_id
            )
        } else {
            format!("removed area {}", a.area_id)
        };
        PlaceEdited::new(summary, &s).map(Json).map_err(fail)
    }

    #[tool(
        description = "Limit compute_coverage's figures (and find_gaps) to one area, such as the user's street frontage, or with no area_id stop limiting them.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn limit_figures_to(
        &self,
        Parameters(a): Parameters<LimitArgs>,
    ) -> Result<Json<PlaceEdited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        limit_figures_to(&mut s, a.area_id).map_err(fail)?;
        let summary = match s.figures_area() {
            None => "the coverage figures are no longer limited to an area".to_owned(),
            Some(area) if area.is_active() => {
                format!("the coverage figures are limited to {}", area_words(area))
            }
            Some(area) => format!(
                "the coverage figures will be limited to {} once it is enabled with a usable \
                 outline; until then they are not limited",
                area_words(area)
            ),
        };
        let mut out = PlaceEdited::new(summary, &s).map_err(fail)?;
        out.area_id = a.area_id;
        Ok(Json(out))
    }

    #[tool(
        description = "Add a named target, such as \"front door\" or \"side gate\": a small ground patch, or a 1 m square of wall (give a point next to the wall to use the nearest building wall, optionally the one facing a camera or the street, or give the point and the wall's normal). target_verdicts then says for each camera whether the model rules it out.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn add_target(
        &self,
        Parameters(a): Parameters<AddTargetArgs>,
    ) -> Result<Json<PlaceEdited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        let (id, wall) = add_target(&mut s, &a.name, a.at).map_err(fail)?;
        let summary = match wall.as_ref().and_then(|w| w.building) {
            Some(b) => format!("added target {id} on the wall of building {b}"),
            None => format!("added target {id}"),
        };
        let mut out = PlaceEdited::new(summary, &s).map_err(fail)?;
        out.target_id = Some(id);
        out.wall = wall;
        Ok(Json(out))
    }

    #[tool(
        description = "Rename a target and/or give it a new place (as add_target takes it).",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn update_target(
        &self,
        Parameters(a): Parameters<UpdateTargetArgs>,
    ) -> Result<Json<PlaceEdited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        let wall = update_target(&mut s, a.target_id, a.name, a.at).map_err(fail)?;
        let mut out =
            PlaceEdited::new(format!("updated target {}", a.target_id), &s).map_err(fail)?;
        out.target_id = Some(a.target_id);
        out.wall = wall;
        Ok(Json(out))
    }

    #[tool(
        description = "Remove a target from the scene.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn remove_target(
        &self,
        Parameters(a): Parameters<TargetIdArgs>,
    ) -> Result<Json<PlaceEdited>, String> {
        let mut s = edit::parse(&a.scene).map_err(fail)?;
        remove_target(&mut s, a.target_id).map_err(fail)?;
        PlaceEdited::new(format!("removed target {}", a.target_id), &s)
            .map(Json)
            .map_err(fail)
    }
}

#[cfg(test)]
#[path = "places_tests.rs"]
mod tests;
