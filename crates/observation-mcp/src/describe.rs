//! The scene in words, for agents that cannot see the map: what is around a
//! point, the buildings and streets, where a camera could be mounted, the
//! whole scene in a paragraph, what lies in one camera's view, and
//! conversion between latitude and longitude and the scene's `[x, z]`.
//!
//! Small local models read these outputs, so they are compact: numbers are
//! rounded ([`geometry::m1`]), lists are capped or paged, and each result
//! has a `summary` in plain sentences. Buildings are named by index and
//! distance, never by address; anything verdict-like is one-sided ("not
//! ruled out", "ruled out: <reason>").

use crate::ToolError;
use crate::area::{bundle_for, check_inside, extent, ground_y, layers_for};
use crate::catalog::{catalog, model};
use crate::edit;
use crate::geometry::{
    self, Front, bearing_deg, bounds, centroid, compass, direction_deg, front, inside_ring, m0, m1,
    nearest_on_footprint, nearest_on_ring, nearest_street, parse_bearing, polyline_length,
    simplify, street_label, xz1,
};
use crate::queries::{ONE_SIDED, Settings, Sources, off_runtime};
use crate::schema::JsonObject;
use crate::server::{ObservationMcp, RangeRow};
use observation_core::address::distance_to_footprint;
use observation_core::building::Footprint;
use observation_core::bundle::AreaBundle;
use observation_core::camera::{CameraModel, CameraPlacement, DoriLevel, Lighting};
use observation_core::coverage::{CoverageParams, StaticLayers};
use observation_core::example::{WallMount, wall_mounts_of};
use observation_core::footage::{
    AreaLabel, FootageCamera, LabelledArea, Probe, Reason, Target, TargetPlace, Verdict,
    camera_verdicts, nearest_wall,
};
use observation_core::frustum::ViewCone;
use observation_core::geo::{LatLon, Projection};
use observation_core::glam::{Vec2, Vec3};
use observation_core::scene::{AreaRef, SceneFile};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// Default and largest radius `describe_surroundings` looks within, metres.
pub const SURROUNDINGS_RADIUS_M: (f32, f32) = (40.0, 150.0);
/// Default and largest number of items per kind `describe_surroundings`
/// lists.
pub const SURROUNDINGS_LIMIT: (usize, usize) = (8, 20);
/// Default and largest page `list_buildings` returns.
pub const BUILDINGS_PAGE: (usize, usize) = (20, 50);
/// Most corners of a street's simplified centre line.
pub const MAX_LINE_POINTS: usize = 12;
/// Most unnamed ways `list_streets` lists (the longest).
pub const MAX_UNNAMED_WAYS: usize = 15;
/// Default and largest number of mount points suggested.
pub const MOUNTS_LIMIT: (usize, usize) = (5, 20);
/// Default mount height of a suggested wall camera, metres (a bullet,
/// turret or dome's typical height).
pub const MOUNT_HEIGHT_M: f32 = 2.6;
/// A suggested mount keeps this far from cameras already placed and from
/// the other suggestions, metres.
pub const MOUNT_CLEAR_M: f32 = 3.0;
/// With `facing` a bearing, a wall qualifies when it faces within this of
/// it, degrees.
pub const MOUNT_FACING_TOLERANCE_DEG: f32 = 60.0;
/// Most cameras `scene_summary` describes one by one.
pub const MAX_SUMMARY_CAMERAS: usize = 40;
/// Most buildings `camera_view` lists (the nearest).
pub const MAX_VIEW_BUILDINGS: usize = 15;
/// Spacing of the points tested along walls and streets in `camera_view`,
/// metres.
const VIEW_STEP_M: f32 = 2.0;
/// A camera this close to a wall is described as on it, metres.
const ON_WALL_M: f32 = 1.5;

/// How to read `camera_view`; returned with it.
pub const VIEW_NOTES: &str = "Listed are the buildings and streets with some part inside the \
    camera's field of view and within its range at the scene's detail level and lighting, nearest \
    first, and every target and labelled area. \"Not ruled out\" means the model finds a clear \
    line of sight to some part; it does not mean the camera sees it, since hedges, fences, trees \
    and vehicles are not modelled and can only block more. Buildings are named by index and \
    distance, never by address.";

fn fail(e: ToolError) -> String {
    e.to_string()
}

fn parse_with_bundle(scene: &Value) -> Result<(SceneFile, Arc<AreaBundle>), ToolError> {
    let s = edit::parse(scene)?;
    let bundle = bundle_for(&s)?;
    Ok((s, bundle))
}

/// "own", "neighbour", "public", "other", or "unlabelled".
fn label_word(l: Option<AreaLabel>) -> &'static str {
    match l {
        Some(AreaLabel::Own) => "own",
        Some(AreaLabel::Neighbour) => "neighbour",
        Some(AreaLabel::Public) => "public",
        Some(AreaLabel::Other) => "other",
        None => "unlabelled",
    }
}

/// "area 2 (own, \"Front garden\")".
fn area_words(a: &LabelledArea) -> String {
    let label = label_word(a.label);
    match &a.name {
        Some(n) => format!("area {} ({label}, \"{n}\")", a.id),
        None => format!("area {} ({label})", a.id),
    }
}

/// "target 3 \"front door\"", or "target 3" when it has no name.
fn target_words(t: &Target) -> String {
    if t.name.is_empty() {
        format!("target {}", t.id)
    } else {
        format!("target {} \"{}\"", t.id, t.name)
    }
}

/// A camera's label, or "camera <id>" when it has none.
fn camera_words(c: &CameraPlacement) -> String {
    if c.label.is_empty() {
        format!("camera {}", c.id)
    } else {
        format!("camera {} \"{}\"", c.id, c.label)
    }
}

/// Where a target is in plan: a ground patch's centroid, or the wall
/// square's centre.
fn target_xz(t: &Target) -> [f32; 2] {
    match &t.at {
        TargetPlace::Ground { points } => centroid(points),
        TargetPlace::Wall { point, .. } => [point[0], point[2]],
    }
}

fn target_kind(t: &Target) -> &'static str {
    match t.at {
        TargetPlace::Ground { .. } => "ground",
        TargetPlace::Wall { .. } => "wall",
    }
}

/// "tilted 20° down", "level", "tilted 5° up".
fn tilt_words(pitch_deg: f32) -> String {
    let p = pitch_deg.round();
    if p < 0.0 {
        format!("tilted {:.0}° down", -p)
    } else if p > 0.0 {
        format!("tilted {p:.0}° up")
    } else {
        "level".to_owned()
    }
}

fn lighting_name(l: Lighting) -> &'static str {
    match l {
        Lighting::Day => "Day",
        Lighting::Night => "Night",
    }
}

fn settings_of(p: &CoverageParams) -> Settings {
    Settings {
        level: p.dori.label().to_owned(),
        ppm: p.pixels_per_metre(),
        lighting: lighting_name(p.lighting).to_owned(),
        subject_height_m: p.subject_height_m,
    }
}

/// A camera's position in words, relative to the nearest building and
/// street: "on building 4's south wall, 6 m from Elm Road".
pub fn camera_place_words(bundle: &AreaBundle, c: &CameraPlacement) -> String {
    let xz = [c.position[0], c.position[2]];
    let mut text = match nearest_wall(bundle.footprints(), xz, ON_WALL_M, |_| true) {
        Some(w) => format!(
            "on building {}'s {} wall",
            w.building,
            compass(direction_deg(w.normal))
        ),
        None => match bundle.building_near(xz, 50.0) {
            Some(i) => {
                let (p, d) = nearest_on_footprint(xz, &bundle.buildings[i].footprint);
                if d == 0.0 {
                    format!("on building {i}'s roof")
                } else {
                    format!("{:.0} m {} of building {i}", d, compass(bearing_deg(p, xz)))
                }
            }
            None => "in the open, no building within 50 m".to_owned(),
        },
    };
    if let Some(s) = nearest_street(bundle, xz) {
        text.push_str(&format!(", {:.0} m from {}", s.distance_m, s.label));
    }
    text
}

// ---------------------------------------------------------------------------
// describe_surroundings
// ---------------------------------------------------------------------------

/// A direction from the point asked about.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Toward {
    /// Plan distance, metres.
    pub distance_m: f64,
    /// Degrees clockwise from north.
    pub bearing_deg: f64,
    /// The nearest of the eight compass points.
    pub compass: &'static str,
}

impl Toward {
    fn between(from: [f32; 2], to: [f32; 2]) -> Self {
        let d = Vec2::from_array(from).distance(Vec2::from_array(to));
        let b = bearing_deg(from, to);
        Self {
            distance_m: m1(d),
            bearing_deg: m0(b) % 360.0,
            compass: compass(b),
        }
    }

    /// "6 m north-east" ("here" within half a metre).
    fn words(&self) -> String {
        if self.distance_m < 0.5 {
            "here".to_owned()
        } else {
            format!("{:.0} m {}", self.distance_m, self.compass)
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NearBuilding {
    /// The building's index in the area (never its address).
    pub building: usize,
    /// To the nearest point of its outline (0 inside it).
    #[serde(flatten)]
    pub toward: Toward,
    /// Roof height above the ground it stands on, metres.
    pub height_m: f64,
    /// `[width, depth]` in metres, width along its longest wall.
    pub size_m: [f64; 2],
    /// The street its front wall faces, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fronts: Option<String>,
    /// The compass direction its front wall faces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub front_faces: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NearStreet {
    /// Its name, or what it is when it has none.
    pub street: String,
    /// The OpenStreetMap `highway` type, such as `residential` or `service`.
    pub kind: String,
    /// To the nearest point of its centre line.
    #[serde(flatten)]
    pub toward: Toward,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NearCamera {
    pub id: u32,
    pub label: String,
    #[serde(flatten)]
    pub toward: Toward,
    /// The compass direction it faces.
    pub facing: &'static str,
    /// Its yaw, degrees clockwise from north.
    pub yaw_deg: f64,
    /// Mount height above the ground, metres.
    pub height_m: f64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NearArea {
    pub area_id: u32,
    /// `own`, `neighbour`, `public`, `other` or `unlabelled`, as drawn.
    pub label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether the point lies inside it.
    pub contains: bool,
    /// To its outline (0 when inside), metres.
    pub distance_m: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NearTarget {
    pub target_id: u32,
    pub name: String,
    /// `ground` or `wall`.
    pub kind: &'static str,
    #[serde(flatten)]
    pub toward: Toward,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GroundOut {
    /// Height above sea level, metres, if the area has elevation data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevation_m: Option<f64>,
    /// Height above the ground at the origin, metres.
    pub above_origin_m: f64,
    /// Steepness, percent (rise over run).
    pub slope_pct: f64,
    /// The compass direction the ground falls towards; absent on level
    /// ground (under 1%).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downhill: Option<&'static str>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SurroundingsOut {
    /// What is around the point, in sentences.
    pub summary: String,
    pub at: [f64; 2],
    pub radius_m: f64,
    /// The building the point is inside, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inside_building: Option<usize>,
    pub ground: GroundOut,
    /// Nearest first, at most `limit`.
    pub buildings: Vec<NearBuilding>,
    /// How many buildings lie within the radius.
    pub buildings_within: usize,
    pub streets: Vec<NearStreet>,
    pub cameras: Vec<NearCamera>,
    /// Labelled areas the point is inside or near.
    pub areas: Vec<NearArea>,
    pub targets: Vec<NearTarget>,
}

/// The ground at `p`: elevation, slope and the way it falls.
fn ground_at(bundle: &AreaBundle, p: [f32; 2]) -> GroundOut {
    const H: f32 = 2.5;
    let y = ground_y(bundle, p[0], p[1]);
    let gx = (ground_y(bundle, p[0] + H, p[1]) - ground_y(bundle, p[0] - H, p[1])) / (2.0 * H);
    let gz = (ground_y(bundle, p[0], p[1] + H) - ground_y(bundle, p[0], p[1] - H)) / (2.0 * H);
    let slope = gx.hypot(gz) * 100.0;
    GroundOut {
        elevation_m: (!bundle.terrain.is_flat()).then(|| m1(bundle.terrain.datum_m + y)),
        above_origin_m: m1(y - ground_y(bundle, 0.0, 0.0)),
        slope_pct: m1(slope),
        downhill: (slope >= 1.0).then(|| compass(direction_deg([-gx, -gz]))),
    }
}

fn ground_words(g: &GroundOut) -> String {
    match g.downhill {
        Some(d) => format!("the ground slopes {:.0}% down towards the {d}", g.slope_pct),
        None => "the ground is level".to_owned(),
    }
}

/// What is around `at` within `radius_m`, nearest first, at most `limit`
/// of each kind.
pub fn surroundings(
    scene: &Value,
    at: [f32; 2],
    radius_m: Option<f32>,
    limit: Option<usize>,
) -> Result<SurroundingsOut, ToolError> {
    let (s, bundle) = parse_with_bundle(scene)?;
    check_inside(&bundle, at[0], at[1])?;
    let radius = radius_m
        .filter(|r| r.is_finite())
        .unwrap_or(SURROUNDINGS_RADIUS_M.0)
        .clamp(1.0, SURROUNDINGS_RADIUS_M.1);
    let limit = limit
        .unwrap_or(SURROUNDINGS_LIMIT.0)
        .clamp(1, SURROUNDINGS_LIMIT.1);

    let mut near: Vec<(usize, [f32; 2], f32)> = bundle
        .buildings
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let (p, d) = nearest_on_footprint(at, &b.footprint);
            (i, p, d)
        })
        .filter(|(_, _, d)| *d <= radius)
        .collect();
    near.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)));
    let inside_building = near.first().filter(|n| n.2 == 0.0).map(|n| n.0);
    let buildings_within = near.len();
    let buildings: Vec<NearBuilding> = near
        .iter()
        .take(limit)
        .map(|&(i, p, _)| {
            let f = &bundle.buildings[i].footprint;
            let (w, d) = geometry::footprint_size(f);
            let fr = front(&bundle, f);
            // From inside, the direction of the building's middle is no help:
            // give the distance 0 and the bearing to its centre.
            let to = if p == at { centroid(&f.outer) } else { p };
            let mut toward = Toward::between(at, to);
            if p == at {
                toward.distance_m = 0.0;
            }
            NearBuilding {
                building: i,
                toward,
                height_m: m1(f.height_m),
                size_m: [m1(w), m1(d)],
                fronts: fr.as_ref().map(|f| f.street.label.clone()),
                front_faces: fr
                    .as_ref()
                    .map(|f| compass(direction_deg(f.wall.normal.to_array()))),
            }
        })
        .collect();

    // Each name (or kind of unnamed way) once, at its nearest way.
    let label_of = |r: &observation_core::bundle::Road| street_label(r.name.as_deref(), &r.highway);
    let mut labels: Vec<String> = bundle.roads.iter().map(label_of).collect();
    labels.sort_unstable();
    labels.dedup();
    let mut streets: Vec<(String, String, [f32; 2], f32)> = labels
        .into_iter()
        .filter_map(|label| {
            let near = bundle.nearest_road(at, |r| label_of(r) == label)?;
            let road = bundle.roads.get(near.index)?;
            (near.distance_m <= radius)
                .then(|| (label, road.highway.clone(), near.point, near.distance_m))
        })
        .collect();
    streets.sort_by(|a, b| a.3.total_cmp(&b.3).then(a.0.cmp(&b.0)));
    let streets: Vec<NearStreet> = streets
        .into_iter()
        .take(limit)
        .map(|(street, kind, p, _)| NearStreet {
            street,
            kind,
            toward: Toward::between(at, p),
        })
        .collect();

    let mut cameras: Vec<NearCamera> = s
        .cameras
        .iter()
        .filter_map(|c| {
            let xz = [c.position[0], c.position[2]];
            let toward = Toward::between(at, xz);
            (toward.distance_m <= f64::from(radius)).then(|| NearCamera {
                id: c.id,
                label: c.label.clone(),
                toward,
                facing: compass(c.yaw_deg),
                yaw_deg: m0(c.yaw_deg.rem_euclid(360.0)) % 360.0,
                height_m: m1(c.position[1] - ground_y(&bundle, xz[0], xz[1])),
                enabled: c.enabled,
            })
        })
        .collect();
    cameras.sort_by(|a, b| {
        a.toward
            .distance_m
            .total_cmp(&b.toward.distance_m)
            .then(a.id.cmp(&b.id))
    });
    cameras.truncate(limit);

    let mut areas: Vec<NearArea> = s
        .areas
        .iter()
        .filter(|a| a.points.len() >= 3)
        .filter_map(|a| {
            let contains = inside_ring(at, &a.points);
            let d = if contains {
                0.0
            } else {
                nearest_on_ring(at, &a.points)?.1
            };
            (d <= radius).then(|| NearArea {
                area_id: a.id,
                label: label_word(a.label),
                name: a.name.clone(),
                contains,
                distance_m: m1(d),
            })
        })
        .collect();
    areas.sort_by(|a, b| {
        a.distance_m
            .total_cmp(&b.distance_m)
            .then(a.area_id.cmp(&b.area_id))
    });
    areas.truncate(limit);

    let mut targets: Vec<NearTarget> = s
        .targets
        .iter()
        .filter_map(|t| {
            let toward = Toward::between(at, target_xz(t));
            (toward.distance_m <= f64::from(radius)).then(|| NearTarget {
                target_id: t.id,
                name: t.name.clone(),
                kind: target_kind(t),
                toward,
            })
        })
        .collect();
    targets.sort_by(|a, b| {
        a.toward
            .distance_m
            .total_cmp(&b.toward.distance_m)
            .then(a.target_id.cmp(&b.target_id))
    });
    targets.truncate(limit);

    let ground = ground_at(&bundle, at);
    let mut out = SurroundingsOut {
        summary: String::new(),
        at: xz1(at),
        radius_m: m1(radius),
        inside_building,
        ground,
        buildings,
        buildings_within,
        streets,
        cameras,
        areas,
        targets,
    };
    out.summary = surroundings_summary(&s, &out);
    Ok(out)
}

fn surroundings_summary(s: &SceneFile, o: &SurroundingsOut) -> String {
    let mut parts = Vec::new();
    let place = match o.inside_building {
        Some(i) => format!("inside building {i}"),
        None => "on open ground".to_owned(),
    };
    parts.push(format!(
        "[{:.0}, {:.0}] is {place}; {}.",
        o.at[0],
        o.at[1],
        ground_words(&o.ground)
    ));
    if o.buildings.is_empty() {
        parts.push(format!("No building within {:.0} m.", o.radius_m));
    } else {
        let list: Vec<String> = o
            .buildings
            .iter()
            .filter(|b| Some(b.building) != o.inside_building)
            .map(|b| {
                let mut t = format!(
                    "building {} {} ({:.0} m tall",
                    b.building,
                    b.toward.words(),
                    b.height_m
                );
                if let (Some(st), Some(f)) = (&b.fronts, b.front_faces) {
                    t.push_str(&format!(", front faces {f} to {st}"));
                }
                t.push(')');
                t
            })
            .collect();
        if !list.is_empty() {
            parts.push(format!("Nearest buildings: {}.", list.join("; ")));
        }
    }
    if o.streets.is_empty() {
        parts.push(format!("No street within {:.0} m.", o.radius_m));
    } else {
        let list: Vec<String> = o
            .streets
            .iter()
            .map(|st| format!("{} {}", st.street, st.toward.words()))
            .collect();
        parts.push(format!("Streets: {}.", list.join("; ")));
    }
    if !o.cameras.is_empty() {
        let list: Vec<String> = o
            .cameras
            .iter()
            .filter_map(|c| s.cameras.iter().find(|x| x.id == c.id))
            .zip(&o.cameras)
            .map(|(c, n)| {
                let off = if n.enabled { "" } else { ", disabled" };
                format!(
                    "{} {} (facing {}{off})",
                    camera_words(c),
                    n.toward.words(),
                    n.facing
                )
            })
            .collect();
        parts.push(format!("Cameras: {}.", list.join("; ")));
    }
    for a in &o.areas {
        let Some(area) = s.areas.iter().find(|x| x.id == a.area_id) else {
            continue;
        };
        if a.contains {
            parts.push(format!("Inside {}.", area_words(area)));
        } else {
            parts.push(format!("{:.0} m from {}.", a.distance_m, area_words(area)));
        }
    }
    if !o.targets.is_empty() {
        let list: Vec<String> = o
            .targets
            .iter()
            .filter_map(|t| s.targets.iter().find(|x| x.id == t.target_id).zip(Some(t)))
            .map(|(t, n)| format!("{} {}", target_words(t), n.toward.words()))
            .collect();
        parts.push(format!("Targets: {}.", list.join("; ")));
    }
    parts.join(" ")
}

// ---------------------------------------------------------------------------
// list_buildings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StreetRef {
    /// Its name, or what it is when it has none.
    pub street: String,
    /// From the building's front wall to the street's centre line, metres.
    pub distance_m: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BuildingOut {
    /// The building's index in the area (never its address).
    pub building: usize,
    /// The middle of its footprint, `[x, z]`.
    pub centre: [f64; 2],
    /// `[width, depth]` in metres, width along its longest wall.
    pub size_m: [f64; 2],
    /// Roof height above the ground it stands on, metres.
    pub height_m: f64,
    /// From the point given to its outline (0 inside), metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
    /// The street its front wall faces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street: Option<StreetRef>,
    /// The compass direction its front wall faces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub front_faces: Option<&'static str>,
    /// The bearing its front wall faces, degrees clockwise from north.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub front_bearing_deg: Option<f64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BuildingsOut {
    pub summary: String,
    /// How many buildings match (in the radius, if one was given).
    pub total: usize,
    pub offset: usize,
    pub buildings: Vec<BuildingOut>,
    /// Pass as `offset` for the next page; absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

fn building_out(bundle: &AreaBundle, i: usize, distance: Option<f32>) -> BuildingOut {
    let f = &bundle.buildings[i].footprint;
    let (w, d) = geometry::footprint_size(f);
    let fr: Option<Front> = front(bundle, f);
    let bearing = fr.as_ref().map(|f| direction_deg(f.wall.normal.to_array()));
    BuildingOut {
        building: i,
        centre: xz1(centroid(&f.outer)),
        size_m: [m1(w), m1(d)],
        height_m: m1(f.height_m),
        distance_m: distance.map(m1),
        street: fr.as_ref().map(|f| StreetRef {
            street: f.street.label.clone(),
            distance_m: m1(f.street.distance_m),
        }),
        front_faces: bearing.map(compass),
        front_bearing_deg: bearing.map(|b| m0(b) % 360.0),
    }
}

/// A page of the area's buildings: by index, or nearest `near` first
/// (within `radius_m` if given).
pub fn buildings(
    scene: &Value,
    near: Option<[f32; 2]>,
    radius_m: Option<f32>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<BuildingsOut, ToolError> {
    let (_, bundle) = parse_with_bundle(scene)?;
    let limit = limit.unwrap_or(BUILDINGS_PAGE.0).clamp(1, BUILDINGS_PAGE.1);
    let offset = offset.unwrap_or(0);
    if let Some(r) = radius_m
        && !(r.is_finite() && r > 0.0)
    {
        return Err(ToolError::invalid(
            "radius_m must be a positive number of metres",
        ));
    }
    let picked: Vec<(usize, Option<f32>)> = match near {
        Some(p) => {
            check_inside(&bundle, p[0], p[1])?;
            let mut v: Vec<(usize, Option<f32>)> = bundle
                .buildings
                .iter()
                .enumerate()
                .map(|(i, b)| (i, Some(nearest_on_footprint(p, &b.footprint).1)))
                .filter(|(_, d)| radius_m.is_none_or(|r| d.is_some_and(|d| d <= r)))
                .collect();
            v.sort_by(|a, b| {
                a.1.unwrap_or(f32::INFINITY)
                    .total_cmp(&b.1.unwrap_or(f32::INFINITY))
                    .then(a.0.cmp(&b.0))
            });
            v
        }
        None => {
            if radius_m.is_some() {
                return Err(ToolError::invalid(
                    "radius_m needs near: the point to measure from",
                ));
            }
            (0..bundle.buildings.len()).map(|i| (i, None)).collect()
        }
    };
    let total = picked.len();
    let page: Vec<(usize, Option<f32>)> = picked.into_iter().skip(offset).take(limit).collect();
    let buildings: Vec<BuildingOut> = page
        .iter()
        .map(|&(i, d)| building_out(&bundle, i, d))
        .collect();
    let end = offset + buildings.len();
    let next_offset = (end < total).then_some(end);
    let order = match near {
        Some(p) => format!("nearest [{:.0}, {:.0}] first", p[0], p[1]),
        None => "by index".to_owned(),
    };
    let summary = if buildings.is_empty() {
        format!("No buildings at offset {offset} of {total} ({order}).")
    } else {
        let more = match next_offset {
            Some(n) => format!("; for more pass offset {n}"),
            None => String::new(),
        };
        format!(
            "Buildings {} to {} of {total}, {order}{more}.",
            offset + 1,
            end
        )
    };
    Ok(BuildingsOut {
        summary,
        total,
        offset,
        buildings,
        next_offset,
    })
}

// ---------------------------------------------------------------------------
// list_streets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StreetOut {
    /// Its name, or what it is when it has none.
    pub street: String,
    /// The OpenStreetMap `highway` type, such as `residential`, `service`
    /// (a driveway or service road) or `footway`.
    pub kind: String,
    /// Carriageway width, metres.
    pub width_m: f64,
    /// Length inside the area, metres.
    pub length_m: f64,
    /// Its centre line inside the area, simplified: one line of `[x, z]`
    /// corners per stretch, at most 12 corners in all.
    pub lines: Vec<Vec<[f64; 2]>>,
    /// Smallest `[x, z]` of the centre line inside the area.
    pub min: [f64; 2],
    /// Largest `[x, z]`.
    pub max: [f64; 2],
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StreetsOut {
    pub summary: String,
    /// Named streets (each name once, longest first), then unnamed ways
    /// (driveways, service roads and footpaths), longest first.
    pub streets: Vec<StreetOut>,
    /// Unnamed ways left out, the shortest.
    pub unnamed_omitted: usize,
}

/// The parts of a polyline inside the rectangle `(lo, hi)` (Liang–Barsky
/// per segment), joined where they meet.
fn clip_polyline(points: &[[f32; 2]], lo: [f32; 2], hi: [f32; 2]) -> Vec<Vec<[f32; 2]>> {
    let mut pieces: Vec<Vec<[f32; 2]>> = Vec::new();
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        let mut keep = true;
        for k in 0..2 {
            for (p, q) in [(-d[k], a[k] - lo[k]), (d[k], hi[k] - a[k])] {
                if p == 0.0 {
                    if q < 0.0 {
                        keep = false;
                    }
                } else {
                    let r = q / p;
                    if p < 0.0 {
                        t0 = t0.max(r);
                    } else {
                        t1 = t1.min(r);
                    }
                }
            }
        }
        if !keep || t0 > t1 {
            continue;
        }
        let at = |t: f32| [a[0] + d[0] * t, a[1] + d[1] * t];
        let (s, e) = (at(t0), at(t1));
        match pieces.last_mut() {
            Some(last) if last.last() == Some(&s) => last.push(e),
            _ => pieces.push(vec![s, e]),
        }
    }
    pieces.retain(|p| polyline_length(p) > 0.5);
    pieces
}

/// The area's streets inside its extent: named streets once per name, then
/// the longest unnamed ways.
pub fn streets(scene: &Value) -> Result<StreetsOut, ToolError> {
    let (_, bundle) = parse_with_bundle(scene)?;
    let (lo, hi) = extent(&bundle);
    struct Acc {
        street: String,
        kind: String,
        width: f32,
        pieces: Vec<Vec<[f32; 2]>>,
        named: bool,
    }
    let mut acc: Vec<Acc> = Vec::new();
    for road in &bundle.roads {
        let pieces = clip_polyline(&road.points, lo, hi);
        if pieces.is_empty() {
            continue;
        }
        match &road.name {
            Some(name) => match acc.iter_mut().find(|a| a.named && a.street == *name) {
                Some(a) => {
                    a.pieces.extend(pieces);
                    a.width = a.width.max(road.width_m);
                }
                None => acc.push(Acc {
                    street: name.clone(),
                    kind: road.highway.clone(),
                    width: road.width_m,
                    pieces,
                    named: true,
                }),
            },
            None => acc.push(Acc {
                street: street_label(None, &road.highway),
                kind: road.highway.clone(),
                width: road.width_m,
                pieces,
                named: false,
            }),
        }
    }
    let length = |a: &Acc| a.pieces.iter().map(|p| polyline_length(p)).sum::<f32>();
    acc.sort_by(|a, b| {
        b.named
            .cmp(&a.named)
            .then(length(b).total_cmp(&length(a)))
            .then(a.street.cmp(&b.street))
    });
    let named = acc.iter().filter(|a| a.named).count();
    let unnamed = acc.len() - named;
    let unnamed_omitted = unnamed.saturating_sub(MAX_UNNAMED_WAYS);
    acc.truncate(named + unnamed.min(MAX_UNNAMED_WAYS));
    let streets: Vec<StreetOut> = acc
        .iter()
        .map(|a| {
            let per = (MAX_LINE_POINTS / a.pieces.len().max(1)).max(2);
            let mut pieces: Vec<&Vec<[f32; 2]>> = a.pieces.iter().collect();
            // Longest stretches first, so a street of many short pieces
            // keeps its main line within the corner budget.
            pieces.sort_by(|x, y| polyline_length(y).total_cmp(&polyline_length(x)));
            let mut budget = MAX_LINE_POINTS;
            let mut lines = Vec::new();
            for p in pieces {
                if budget < 2 {
                    break;
                }
                let line = simplify(p, per.min(budget));
                budget -= line.len();
                lines.push(line.into_iter().map(xz1).collect());
            }
            let all: Vec<[f32; 2]> = a.pieces.iter().flatten().copied().collect();
            let (min, max) = bounds(&all);
            StreetOut {
                street: a.street.clone(),
                kind: a.kind.clone(),
                width_m: m1(a.width),
                length_m: m0(length(a)),
                lines,
                min: xz1(min),
                max: xz1(max),
            }
        })
        .collect();
    let names: Vec<String> = streets
        .iter()
        .take(named)
        .map(|s| format!("{} ({:.0} m, {})", s.street, s.length_m, s.kind))
        .collect();
    let mut summary = if names.is_empty() {
        "The area has no named streets.".to_owned()
    } else {
        format!("Named streets: {}.", names.join("; "))
    };
    if unnamed > 0 {
        summary.push_str(&format!(
            " Also {unnamed} unnamed ways (driveways, service roads and footpaths)."
        ));
    }
    Ok(StreetsOut {
        summary,
        streets,
        unnamed_omitted,
    })
}

// ---------------------------------------------------------------------------
// suggest_mount_points
// ---------------------------------------------------------------------------

/// Which way a suggested camera faces.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FacingArg {
    /// A compass bearing, degrees clockwise from north.
    Degrees(f32),
    /// "street" (the default), or a compass word such as "south-east" or
    /// "NW".
    Word(String),
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MountOut {
    /// Where to put the camera, `[x, z]`: pass as place_camera's x and z.
    pub at: [f64; 2],
    /// Mount height above the ground, metres: place_camera's height_m.
    pub height_m: f64,
    /// Compass bearing to face, degrees: place_camera's yaw_deg.
    pub yaw_deg: f64,
    /// Suggested tilt, degrees (negative looks down): place_camera's
    /// pitch_deg.
    pub pitch_deg: f64,
    /// The compass word for yaw_deg.
    pub compass: &'static str,
    /// The building whose wall it is on (by index, never address).
    pub building: usize,
    /// Whether the wall faces a street.
    pub faces_street: bool,
    /// The street nearest the wall.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    /// From the point given, metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
    /// The mount in words.
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MountsOut {
    pub summary: String,
    pub mounts: Vec<MountOut>,
}

/// The street a wall mount's street point lies on, and the distance from
/// the wall to it.
fn faced_street(bundle: &AreaBundle, w: &WallMount) -> Option<(String, f32)> {
    let road = bundle.roads.get(w.road?)?;
    Some((
        street_label(road.name.as_deref(), &road.highway),
        w.street_distance_m(),
    ))
}

/// What `suggest_mount_points` is asked for.
#[derive(Debug, Clone, Default)]
pub struct MountQuery {
    pub building: Option<usize>,
    pub near: Option<[f32; 2]>,
    pub radius_m: Option<f32>,
    pub facing: Option<FacingArg>,
    pub height_m: Option<f32>,
    pub limit: Option<usize>,
}

/// Wall mounts for a camera: on house fronts facing the street (the walls
/// the example cameras use), or on walls facing a bearing; clear of the
/// cameras already placed. Only the walls of `building`, or of the
/// buildings within `radius_m` of `near`, are considered when given. Tests
/// lines of sight: call it off the async runtime.
pub fn mount_points(scene: &Value, q: MountQuery) -> Result<MountsOut, ToolError> {
    let MountQuery {
        building,
        near,
        radius_m,
        facing,
        height_m,
        limit,
    } = q;
    let (s, bundle) = parse_with_bundle(scene)?;
    let limit = limit.unwrap_or(MOUNTS_LIMIT.0).clamp(1, MOUNTS_LIMIT.1);
    let height = height_m.unwrap_or(MOUNT_HEIGHT_M);
    if !(height.is_finite() && (0.5..=20.0).contains(&height)) {
        return Err(ToolError::invalid(format!(
            "height_m {height} is out of range: use 0.5 to 20 m above the ground"
        )));
    }
    if let Some(b) = building
        && b >= bundle.buildings.len()
    {
        return Err(ToolError::invalid(format!(
            "no building {b}: the area has buildings 0 to {}",
            bundle.buildings.len().saturating_sub(1)
        )));
    }
    if let Some(p) = near {
        check_inside(&bundle, p[0], p[1])?;
    }
    if let Some(r) = radius_m
        && !(r.is_finite() && r > 0.0)
    {
        return Err(ToolError::invalid(
            "radius_m must be a positive number of metres",
        ));
    }
    if radius_m.is_some() && near.is_none() {
        return Err(ToolError::invalid(
            "radius_m needs near: the point to measure from",
        ));
    }
    let bearing: Option<f32> = match &facing {
        None => None,
        Some(FacingArg::Degrees(d)) if d.is_finite() => Some(d.rem_euclid(360.0)),
        Some(FacingArg::Degrees(_)) => {
            return Err(ToolError::invalid("facing must be a number of degrees"));
        }
        Some(FacingArg::Word(w)) if w.trim().eq_ignore_ascii_case("street") => None,
        Some(FacingArg::Word(w)) => Some(parse_bearing(w).ok_or_else(|| {
            ToolError::invalid(format!(
                "facing \"{w}\" is not \"street\", a compass word such as \"south-east\", or \
                 degrees"
            ))
        })?),
    };
    let layers = layers_for(&s)?;
    let terrain = layers.terrain();
    let top = terrain.top();
    let taken: Vec<Vec2> = s
        .cameras
        .iter()
        .map(|c| Vec2::new(c.position[0], c.position[2]))
        .collect();
    let keep = |i: usize, f: &Footprint| {
        building.is_none_or(|b| b == i)
            && match (near, radius_m) {
                (Some(p), Some(r)) => distance_to_footprint(p, f) <= r,
                _ => true,
            }
    };
    let mut candidates: Vec<(WallMount, Vec3, f32)> = wall_mounts_of(&bundle, keep)
        .into_iter()
        .filter_map(|w| {
            let eye = w.eye(terrain, height);
            let xz = Vec2::new(eye.x, eye.z);
            if !w.fits_under_roof(eye) || taken.iter().any(|t| t.distance(xz) <= MOUNT_CLEAR_M) {
                return None;
            }
            let yaw = match bearing {
                None => {
                    if !(w.faces_street() && w.sees_street(eye, layers.occluders(), terrain, top)) {
                        return None;
                    }
                    w.street_yaw_deg()
                }
                Some(b) => {
                    let out = direction_deg(w.normal.to_array());
                    if geometry::angle_between(out, b) > MOUNT_FACING_TOLERANCE_DEG {
                        return None;
                    }
                    b
                }
            };
            Some((w, eye, yaw))
        })
        .collect();
    let key = |w: &WallMount, eye: &Vec3| match near {
        Some(p) => Vec2::new(eye.x, eye.z).distance(Vec2::from_array(p)),
        None if bearing.is_none() => w.street_distance_m(),
        None => 0.0,
    };
    candidates.sort_by(|a, b| {
        key(&a.0, &a.1)
            .total_cmp(&key(&b.0, &b.1))
            .then(a.0.building.cmp(&b.0.building))
            .then(a.1.x.total_cmp(&b.1.x))
            .then(a.1.z.total_cmp(&b.1.z))
    });
    let mut picked: Vec<(WallMount, Vec3, f32)> = Vec::new();
    for c in candidates {
        if picked.len() == limit {
            break;
        }
        let xz = Vec2::new(c.1.x, c.1.z);
        if picked
            .iter()
            .all(|p| Vec2::new(p.1.x, p.1.z).distance(xz) > MOUNT_CLEAR_M)
        {
            picked.push(c);
        }
    }
    // A wall camera looks a little down; one at doorbell height looks level.
    let pitch = if height < 1.8 { 0.0 } else { -20.0 };
    let mounts: Vec<MountOut> = picked
        .iter()
        .map(|(w, eye, yaw)| {
            let xz = [eye.x, eye.z];
            let street = faced_street(&bundle, w);
            let wall = compass(direction_deg(w.normal.to_array()));
            let mut text = format!(
                "on building {}'s {wall} wall, {:.1} m up, facing {}",
                w.building,
                height,
                compass(*yaw)
            );
            if let Some((label, d)) = &street {
                let rel = if w.faces_street() { "towards" } else { "near" };
                text.push_str(&format!(" ({rel} {label}, {d:.0} m)"));
            }
            MountOut {
                at: xz1(xz),
                height_m: m1(height),
                yaw_deg: m0(*yaw) % 360.0,
                pitch_deg: pitch,
                compass: compass(*yaw),
                building: w.building,
                faces_street: w.faces_street(),
                street: street.map(|s| s.0),
                distance_m: near.map(|p| m1(Vec2::from_array(p).distance(Vec2::from_array(xz)))),
                text,
            }
        })
        .collect();
    let summary = if mounts.is_empty() {
        let what = match (building, bearing) {
            (Some(b), None) => format!(
                "building {b} has no wall facing a street within 30 m with room for a camera \
                 {height:.1} m up and a clear line to the street; give facing as a compass word \
                 or degrees instead"
            ),
            (Some(b), Some(d)) => format!(
                "building {b} has no wall facing within {MOUNT_FACING_TOLERANCE_DEG:.0}° of {} \
                 with room for a camera {height:.1} m up",
                compass(d)
            ),
            (None, _) => "no wall qualifies".to_owned(),
        };
        format!(
            "No mount points: {what} (walls within {MOUNT_CLEAR_M:.0} m of a camera are left out)."
        )
    } else {
        format!(
            "{} mount point(s). Pass at as x and z, with height_m, yaw_deg and pitch_deg, to \
             place_camera. First: {}.",
            mounts.len(),
            mounts[0].text
        )
    };
    Ok(MountsOut { summary, mounts })
}

// ---------------------------------------------------------------------------
// scene_summary
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AreaOut {
    /// "demo street" or "OpenStreetMap area".
    pub kind: &'static str,
    /// An OpenStreetMap area's box: `[south, west, north, east]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
    /// The origin positions are measured from, `[lat, lon]`.
    pub origin: [f64; 2],
    /// Smallest and largest `[x, z]` inside the area, if its data is
    /// available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extent_min: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extent_max: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub building_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osm_data_as_of: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CameraLine {
    pub id: u32,
    pub label: String,
    pub model_id: String,
    /// The model's name, if the model is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub enabled: bool,
    pub at: [f64; 2],
    /// Mount height above the ground, metres; absent when the area's ground
    /// is not available here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_m: Option<f64>,
    /// Where it is relative to the nearest building and street, if the
    /// area's data is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The compass direction it faces.
    pub facing: &'static str,
    pub yaw_deg: f64,
    pub pitch_deg: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModelLine {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub hfov_deg: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AreaLine {
    pub area_id: u32,
    /// `own`, `neighbour`, `public`, `other` or `unlabelled`, as drawn.
    pub label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub enabled: bool,
    /// The outline's area as drawn, m².
    pub area_m2: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TargetLine {
    pub target_id: u32,
    pub name: String,
    /// `ground` or `wall`.
    pub kind: &'static str,
    pub at: [f64; 2],
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SceneSummaryOut {
    /// The scene in a few sentences.
    pub summary: String,
    pub area: AreaOut,
    pub settings: Settings,
    /// At most 40, by id.
    pub cameras: Vec<CameraLine>,
    pub camera_count: usize,
    pub custom_models: Vec<ModelLine>,
    pub areas: Vec<AreaLine>,
    pub targets: Vec<TargetLine>,
    /// The area the coverage figures are limited to, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub figures_limited_to: Option<u32>,
    /// The area the scene names to limit the figures to while it is not in
    /// effect (disabled, or without a usable outline), so the figures are
    /// not limited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub figures_limit_not_in_effect: Option<u32>,
}

/// The whole scene in brief, without computing coverage.
pub fn summarise(scene: &Value) -> Result<SceneSummaryOut, ToolError> {
    let cat = catalog()?;
    let s = edit::parse(scene)?;
    // An OpenStreetMap area without a connected app has no data here; the
    // scene itself can still be described.
    let bundle = bundle_for(&s).ok();
    let b = bundle.as_deref();
    let (kind, bbox) = match &s.area {
        AreaRef::Demo => ("demo street", None),
        AreaRef::Osm { bbox } => (
            "OpenStreetMap area",
            Some([bbox.south, bbox.west, bbox.north, bbox.east]),
        ),
    };
    let ext = b.map(extent);
    let area = AreaOut {
        kind,
        bbox,
        origin: [deg7(s.origin.lat), deg7(s.origin.lon)],
        extent_min: ext.map(|e| xz1(e.0)),
        extent_max: ext.map(|e| xz1(e.1)),
        building_count: b.map(|b| b.buildings.len()),
        osm_data_as_of: b.and_then(|b| {
            b.osm_base_unix
                .map(observation_core::provenance::format_date)
        }),
    };
    let mut cams: Vec<&CameraPlacement> = s.cameras.iter().collect();
    cams.sort_by_key(|c| c.id);
    let cameras: Vec<CameraLine> = cams
        .iter()
        .take(MAX_SUMMARY_CAMERAS)
        .map(|c| {
            let xz = [c.position[0], c.position[2]];
            let ground = b.map(|b| ground_y(b, xz[0], xz[1]));
            CameraLine {
                id: c.id,
                label: c.label.clone(),
                model_id: c.model_id.clone(),
                model: model(cat, Some(&s), &c.model_id).map(CameraModel::display_name),
                enabled: c.enabled,
                at: xz1(xz),
                height_m: ground.map(|g| m1(c.position[1] - g)),
                place: b.map(|b| camera_place_words(b, c)),
                facing: compass(c.yaw_deg),
                yaw_deg: m0(c.yaw_deg.rem_euclid(360.0)) % 360.0,
                pitch_deg: m0(c.pitch_deg),
            }
        })
        .collect();
    let custom_models = s
        .custom_models
        .iter()
        .map(|m| ModelLine {
            id: m.id.clone(),
            name: m.display_name(),
            kind: m.kind.label().to_owned(),
            hfov_deg: m1(m.hfov_deg),
        })
        .collect();
    let areas: Vec<AreaLine> = s
        .areas
        .iter()
        .map(|a| AreaLine {
            area_id: a.id,
            label: label_word(a.label),
            name: a.name.clone(),
            enabled: a.enabled,
            area_m2: m0(a.area_m2()),
        })
        .collect();
    let targets: Vec<TargetLine> = s
        .targets
        .iter()
        .map(|t| TargetLine {
            target_id: t.id,
            name: t.name.clone(),
            kind: target_kind(t),
            at: xz1(target_xz(t)),
        })
        .collect();
    let settings = settings_of(&s.params);

    let mut parts = Vec::new();
    let where_ = match (&s.area, ext) {
        (AreaRef::Demo, Some((lo, hi))) => format!(
            "The scene is on the demo street, {:.0} by {:.0} m (x {:.0} to {:.0}, z {:.0} to {:.0})",
            hi[0] - lo[0],
            hi[1] - lo[1],
            lo[0],
            hi[0],
            lo[1],
            hi[1]
        ),
        (AreaRef::Osm { .. }, Some((lo, hi))) => format!(
            "The scene is on an OpenStreetMap area, {:.0} by {:.0} m (x {:.0} to {:.0}, z {:.0} to {:.0})",
            hi[0] - lo[0],
            hi[1] - lo[1],
            lo[0],
            hi[0],
            lo[1],
            hi[1]
        ),
        (_, None) => "The scene is on an OpenStreetMap area whose buildings are not available \
             here (connect the app to use it)"
            .to_owned(),
    };
    let data = match &area.osm_data_as_of {
        Some(d) => format!(", map data as of {d}"),
        None => String::new(),
    };
    let count = area
        .building_count
        .map(|n| format!(", with {n} buildings"))
        .unwrap_or_default();
    parts.push(format!("{where_}{count}{data}."));
    parts.push(format!(
        "Settings: {} ({:.0} px/m) {}, people tested {:.1} m above the ground.",
        settings.level,
        settings.ppm,
        match s.params.lighting {
            Lighting::Day => "by day",
            Lighting::Night => "at night",
        },
        settings.subject_height_m
    ));
    if cameras.is_empty() {
        parts.push("No cameras.".to_owned());
    } else {
        let enabled = s.cameras.iter().filter(|c| c.enabled).count();
        parts.push(format!("{} camera(s), {enabled} enabled:", s.cameras.len()));
        for (c, line) in cams.iter().zip(&cameras) {
            let model = line
                .model
                .clone()
                .unwrap_or_else(|| format!("unknown model {}", c.model_id));
            let place = line
                .place
                .clone()
                .unwrap_or_else(|| format!("at [{:.0}, {:.0}]", line.at[0], line.at[1]));
            let off = if c.enabled { "" } else { " (disabled)" };
            let up = match line.height_m {
                Some(h) => format!("{h:.1} m up "),
                // Without the area's ground, the height above it is unknown.
                None => String::new(),
            };
            parts.push(format!(
                "{}{off}, {model}, {up}{place}, facing {} and {}.",
                camera_words(c),
                line.facing,
                tilt_words(c.pitch_deg)
            ));
        }
        if s.cameras.len() > cameras.len() {
            parts.push(format!(
                "{} more cameras are not described.",
                s.cameras.len() - cameras.len()
            ));
        }
    }
    if !s.custom_models.is_empty() {
        let names: Vec<String> = s
            .custom_models
            .iter()
            .map(|m| format!("{} ({})", m.display_name(), m.id))
            .collect();
        parts.push(format!("Custom models: {}.", names.join("; ")));
    }
    if !s.areas.is_empty() {
        let list: Vec<String> = s
            .areas
            .iter()
            .map(|a| {
                let off = if a.enabled { "" } else { ", disabled" };
                format!("{} of {:.0} m²{off}", area_words(a), a.area_m2())
            })
            .collect();
        parts.push(format!("Areas: {}.", list.join("; ")));
    }
    if !s.targets.is_empty() {
        let list: Vec<String> = s
            .targets
            .iter()
            .map(|t| {
                let p = target_xz(t);
                format!(
                    "{} ({} at [{:.0}, {:.0}])",
                    target_words(t),
                    target_kind(t),
                    p[0],
                    p[1]
                )
            })
            .collect();
        parts.push(format!("Targets: {}.", list.join("; ")));
    }
    let limited = s.figures_area();
    match limited {
        Some(a) if a.is_active() => parts.push(format!(
            "The coverage figures are limited to {}.",
            area_words(a)
        )),
        Some(a) => parts.push(format!(
            "The coverage figures are set to be limited to {}, which is disabled or has no \
             usable outline, so they are not limited.",
            area_words(a)
        )),
        None => parts.push("The coverage figures cover the whole area.".to_owned()),
    }
    Ok(SceneSummaryOut {
        summary: parts.join(" "),
        area,
        settings,
        camera_count: s.cameras.len(),
        cameras,
        custom_models,
        areas,
        targets,
        figures_limited_to: limited.filter(|a| a.is_active()).map(|a| a.id),
        figures_limit_not_in_effect: limited.filter(|a| !a.is_active()).map(|a| a.id),
    })
}

// ---------------------------------------------------------------------------
// camera_view
// ---------------------------------------------------------------------------

/// What blocks the sight lines to the nearest part of something in view.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Blocked {
    Clear,
    Building(usize),
    /// Only walls that face away from the camera are in view.
    FacingAway,
    Terrain,
}

/// The one-sided verdict on a thing in view, in words.
fn blocked_text(b: Blocked, own: Option<usize>) -> String {
    match b {
        Blocked::Clear => "not ruled out".to_owned(),
        Blocked::Building(i) if Some(i) == own => {
            "ruled out: hidden behind its own walls".to_owned()
        }
        Blocked::Building(i) => format!("ruled out: blocked by building {i}"),
        Blocked::FacingAway => "ruled out: only walls facing away are in view".to_owned(),
        Blocked::Terrain => "ruled out: blocked by the terrain".to_owned(),
    }
}

/// One point tested from the camera: where, and the outward normal of the
/// wall it is on, if any.
struct Sample {
    p: Vec3,
    normal: Option<Vec2>,
}

/// How the points of one thing in the cone fare: the nearest point that
/// counts (see [`tally`]), the verdict and how many points were clear.
struct Tally {
    nearest_m: f32,
    nearest: Vec3,
    blocked: Blocked,
    clear: usize,
    in_cone: usize,
}

/// Tests `samples` from the cone's eye, nearest first. With `count_all`
/// every point in the cone is tested (to measure a street's clear length);
/// otherwise testing stops at the first clear one. `None` if no point lies
/// in the cone within range.
///
/// The nearest point reported is the nearest with a clear line; failing
/// that, the nearest in the cone that faces the camera (so not one on a wall
/// facing away); failing that, the nearest in the cone.
fn tally(
    layers: &StaticLayers,
    cone: &ViewCone,
    samples: &[Sample],
    count_all: bool,
) -> Option<Tally> {
    let eye = cone.eye;
    let mut near: Vec<(f32, usize)> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| cone.contains(s.p))
        .map(|(i, s)| ((s.p - eye).length(), i))
        .collect();
    if near.is_empty() {
        return None;
    }
    near.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut t = Tally {
        nearest_m: near[0].0,
        nearest: samples[near[0].1].p,
        blocked: Blocked::FacingAway,
        clear: 0,
        in_cone: near.len(),
    };
    let mut facing: Option<(f32, Vec3)> = None;
    let mut first_clear: Option<(f32, Vec3)> = None;
    let mut building: Option<usize> = None;
    let mut terrain = false;
    for &(d, i) in &near {
        let s = &samples[i];
        if s.normal
            .is_some_and(|n| n.dot(Vec2::new(eye.x - s.p.x, eye.z - s.p.z)) <= 0.0)
        {
            continue;
        }
        facing.get_or_insert((d, s.p));
        if let Some((b, _)) = layers.occluders().first_blocker(eye, s.p) {
            building.get_or_insert(b);
            continue;
        }
        if layers.blocked_by_terrain(eye, s.p) {
            terrain = true;
            continue;
        }
        first_clear.get_or_insert((d, s.p));
        t.clear += 1;
        if !count_all {
            break;
        }
    }
    if let Some((d, p)) = first_clear.or(facing) {
        t.nearest_m = d;
        t.nearest = p;
    }
    t.blocked = if t.clear > 0 {
        Blocked::Clear
    } else if terrain {
        Blocked::Terrain
    } else if let Some(b) = building {
        Blocked::Building(b)
    } else {
        Blocked::FacingAway
    };
    Some(t)
}

/// Points along the walls of footprint `i`, 5 cm out: at the subject
/// height (kept on the wall) and just under the roof.
fn wall_samples(
    layers: &StaticLayers,
    bundle: &AreaBundle,
    i: usize,
    subject_h: f32,
) -> Vec<Sample> {
    let f = &bundle.buildings[i].footprint;
    let mut out = Vec::new();
    let (bottom, roof) = (f.bottom_y(), f.roof_y());
    // As `Occluders` does, skip a footprint that cannot form a solid (and
    // would give `clamp` bounds that are not numbers).
    let finite =
        bottom.is_finite() && roof.is_finite() && f.outer.iter().flatten().all(|v| v.is_finite());
    if !finite || roof - bottom < 0.4 {
        return out;
    }
    for w in geometry::walls(f) {
        let along = Vec2::new(-w.normal.y, w.normal.x);
        let n = (w.length_m / VIEW_STEP_M).ceil().max(1.0) as usize;
        for k in 0..n {
            let off = (k as f32 + 0.5) / n as f32 - 0.5;
            let xz = w.mid + along * (off * w.length_m) + w.normal * 0.05;
            let ground = layers.terrain().height_at(xz);
            let low = (ground + subject_h).clamp(bottom + 0.2, roof - 0.2);
            for y in [low, roof - 0.2] {
                out.push(Sample {
                    p: Vec3::new(xz.x, y, xz.y),
                    normal: Some(w.normal),
                });
            }
        }
    }
    out
}

/// Points along a polyline every [`VIEW_STEP_M`], at the subject height,
/// those within `range` of `eye` only.
fn street_samples(
    layers: &StaticLayers,
    points: &[[f32; 2]],
    subject_h: f32,
    eye: Vec3,
    range: f32,
) -> Vec<Sample> {
    let eye2 = Vec2::new(eye.x, eye.z);
    let mut out = Vec::new();
    for w in points.windows(2) {
        let (a, b) = (Vec2::from_array(w[0]), Vec2::from_array(w[1]));
        let len = a.distance(b);
        if !len.is_finite() || len < 1e-3 {
            continue;
        }
        let n = (len / VIEW_STEP_M).ceil() as usize;
        for k in 0..n {
            let xz = a + (b - a) * ((k as f32 + 0.5) / n as f32);
            if xz.distance(eye2) <= range {
                out.push(Sample {
                    p: layers.subject_at(xz, subject_h),
                    normal: None,
                });
            }
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ViewBuilding {
    /// The building's index in the area (never its address).
    pub building: usize,
    /// From the camera to the nearest part of it in view with a clear line
    /// of sight; when it is ruled out, to the nearest part in view facing
    /// the camera. Metres.
    pub distance_m: f64,
    /// Towards that part from the camera, degrees clockwise from north.
    pub bearing_deg: f64,
    pub compass: &'static str,
    /// "not ruled out", or "ruled out: blocked by building N" and the like.
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ViewStreet {
    pub street: String,
    pub kind: String,
    /// From the camera to the nearest part in view with a clear line of
    /// sight (when it is ruled out, the nearest part in view), metres.
    pub distance_m: f64,
    /// Length of its centre line inside the view and range, about, metres.
    pub in_view_m: f64,
    /// Of that, the length with a clear line of sight in the model, metres.
    pub not_ruled_out_m: f64,
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ViewPlace {
    /// The target's or area's id.
    pub id: u32,
    pub name: String,
    /// `ground`, `wall` or the area's label.
    pub kind: String,
    /// "not ruled out" or "ruled out: <reason>".
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AheadOut {
    /// "ground", "building" or "nothing" within range.
    pub hits: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub building: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CameraViewOut {
    pub summary: String,
    pub camera_id: u32,
    pub label: String,
    pub enabled: bool,
    pub model_id: String,
    /// Horizontal and vertical field of view, degrees.
    pub hfov_deg: f64,
    pub vfov_deg: f64,
    /// Range at each standard detail level, by day and at night.
    pub ranges: Vec<RangeRow>,
    pub settings: Settings,
    /// The range used here: the scene's level and lighting, metres.
    pub range_m: f64,
    /// Where the middle of the view first meets the ground or a building.
    pub centre_of_view: AheadOut,
    /// What a level line straight ahead at the camera's height meets first.
    pub level_ahead: AheadOut,
    /// Nearest first, at most 15.
    pub buildings: Vec<ViewBuilding>,
    /// How many buildings have some part in view.
    pub buildings_in_view: usize,
    pub streets: Vec<ViewStreet>,
    /// Every target, with its verdict for this camera.
    pub targets: Vec<ViewPlace>,
    /// Every enabled labelled area, with its verdict for this camera.
    pub areas: Vec<ViewPlace>,
    /// How to read the verdicts; pass it on.
    pub wording: String,
    pub notes: String,
    #[serde(flatten)]
    pub sources: Sources,
}

/// What the ray from `eye` along `dir` meets first within `range`: the
/// ground (marched every 0.25 m) or a building.
fn first_along(
    layers: &StaticLayers,
    eye: Vec3,
    dir: Vec3,
    range: f32,
    test_ground: bool,
) -> AheadOut {
    let end = eye + dir * range;
    let building = layers
        .occluders()
        .first_blocker(eye, end)
        .map(|(i, t)| (i, t * range));
    let mut ground: Option<f32> = None;
    if test_ground {
        let mut d = 0.25;
        while d <= range {
            let p = eye + dir * d;
            if p.y <= layers.terrain().height_at(Vec2::new(p.x, p.z)) {
                ground = Some(d);
                break;
            }
            d += 0.25;
        }
    }
    let plan = |d: f32| {
        let p = eye + dir * d;
        xz1([p.x, p.z])
    };
    match (building, ground) {
        (Some((i, db)), g) if g.is_none_or(|g| db <= g) => AheadOut {
            hits: "building",
            building: Some(i),
            distance_m: Some(m1(db)),
            at: Some(plan(db)),
            text: format!("building {i}, {db:.0} m ahead"),
        },
        (_, Some(g)) => AheadOut {
            hits: "ground",
            building: None,
            distance_m: Some(m1(g)),
            at: Some(plan(g)),
            text: format!("the ground, {g:.0} m ahead"),
        },
        _ => AheadOut {
            hits: "nothing",
            building: None,
            distance_m: None,
            at: None,
            text: format!("nothing within the {range:.0} m range"),
        },
    }
}

/// The verdict on a target or area for one camera, naming the building in
/// the way where the line to its middle finds one.
fn place_verdict(
    layers: &StaticLayers,
    params: &CoverageParams,
    cam: &FootageCamera,
    target: &Target,
    middle: Vec3,
) -> String {
    let probe = Probe::target(layers, target, params.subject_height_m);
    let v = camera_verdicts(layers, params, cam, &probe).at(params.dori, params.lighting);
    match v {
        Some(Verdict::RuledOut(Reason::BlockedByBuilding)) => {
            match layers
                .occluders()
                .first_blocker(cam.placement.eye(), middle)
            {
                Some((i, _)) => format!("ruled out: blocked by building {i}"),
                None => Verdict::RuledOut(Reason::BlockedByBuilding).text(),
            }
        }
        Some(v) => v.text(),
        None => "no verdict".to_owned(),
    }
}

/// What lies in camera `id`'s view at the scene's level and lighting.
pub fn view(scene: &Value, id: u32) -> Result<CameraViewOut, ToolError> {
    let cat = catalog()?;
    let s = edit::parse(scene)?;
    let c = s
        .cameras
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| ToolError::no_camera(&s, id))?;
    let m =
        model(cat, Some(&s), &c.model_id).ok_or_else(|| ToolError::NoModel(c.model_id.clone()))?;
    let bundle = bundle_for(&s)?;
    let layers = layers_for(&s)?;
    let params = s.params;
    let range = m.effective_range_m(params.pixels_per_metre(), params.lighting);
    let cone = ViewCone::new(c, m, range);
    let eye = c.eye();
    let eye2 = [eye.x, eye.z];
    let h = params.subject_height_m;

    let mut seen: Vec<(usize, Tally)> = bundle
        .buildings
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            observation_core::address::distance_to_footprint(eye2, &b.footprint) <= range
        })
        .filter_map(|(i, _)| {
            let samples = wall_samples(&layers, &bundle, i, h);
            tally(&layers, &cone, &samples, false).map(|t| (i, t))
        })
        .collect();
    seen.sort_by(|a, b| a.1.nearest_m.total_cmp(&b.1.nearest_m).then(a.0.cmp(&b.0)));
    let buildings_in_view = seen.len();
    let buildings: Vec<ViewBuilding> = seen
        .iter()
        .take(MAX_VIEW_BUILDINGS)
        .map(|(i, t)| {
            let b = bearing_deg(eye2, [t.nearest.x, t.nearest.z]);
            ViewBuilding {
                building: *i,
                distance_m: m1(t.nearest_m),
                bearing_deg: m0(b) % 360.0,
                compass: compass(b),
                verdict: blocked_text(t.blocked, Some(*i)),
            }
        })
        .collect();

    let mut streets: Vec<ViewStreet> = Vec::new();
    for road in &bundle.roads {
        let samples = street_samples(&layers, &road.points, h, eye, range);
        let Some(t) = tally(&layers, &cone, &samples, true) else {
            continue;
        };
        let label = street_label(road.name.as_deref(), &road.highway);
        let in_view = t.in_cone as f32 * VIEW_STEP_M;
        let clear = t.clear as f32 * VIEW_STEP_M;
        match streets.iter_mut().find(|x| x.street == label) {
            Some(x) => {
                x.distance_m = x.distance_m.min(m1(t.nearest_m));
                x.in_view_m = m0(x.in_view_m as f32 + in_view);
                x.not_ruled_out_m = m0(x.not_ruled_out_m as f32 + clear);
                if x.not_ruled_out_m > 0.0 {
                    x.verdict = "not ruled out".to_owned();
                }
            }
            None => streets.push(ViewStreet {
                street: label,
                kind: road.highway.clone(),
                distance_m: m1(t.nearest_m),
                in_view_m: m0(in_view),
                not_ruled_out_m: m0(clear),
                verdict: blocked_text(t.blocked, None),
            }),
        }
    }
    streets.sort_by(|a, b| {
        a.distance_m
            .total_cmp(&b.distance_m)
            .then(a.street.cmp(&b.street))
    });

    let cam = FootageCamera {
        placement: c,
        model: m,
    };
    let targets: Vec<ViewPlace> = s
        .targets
        .iter()
        .map(|t| {
            let middle = match &t.at {
                TargetPlace::Ground { points } => {
                    layers.subject_at(Vec2::from_array(centroid(points)), h)
                }
                TargetPlace::Wall { point, normal } => {
                    let n = Vec2::from_array(*normal).normalize_or_zero() * 0.05;
                    Vec3::new(point[0] + n.x, point[1], point[2] + n.y)
                }
            };
            ViewPlace {
                id: t.id,
                name: t.name.clone(),
                kind: target_kind(t).to_owned(),
                verdict: place_verdict(&layers, &params, &cam, t, middle),
            }
        })
        .collect();
    let areas: Vec<ViewPlace> = s
        .areas
        .iter()
        .filter(|a| a.is_active())
        .map(|a| {
            let as_target = Target {
                id: a.id,
                name: a.name.clone().unwrap_or_default(),
                at: TargetPlace::Ground {
                    points: a.points.clone(),
                },
            };
            let middle = layers.subject_at(Vec2::from_array(centroid(&a.points)), h);
            ViewPlace {
                id: a.id,
                name: a.name.clone().unwrap_or_default(),
                kind: label_word(a.label).to_owned(),
                verdict: place_verdict(&layers, &params, &cam, &as_target, middle),
            }
        })
        .collect();

    let centre_of_view = first_along(&layers, eye, c.forward(), range, true);
    let level = observation_core::camera::forward_from_yaw_pitch(c.yaw_deg, 0.0);
    let level_ahead = first_along(&layers, eye, level, range, true);

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
    let settings = settings_of(&params);

    let mut parts = Vec::new();
    let off = if c.enabled {
        String::new()
    } else {
        " It is disabled, so the coverage figures leave it out.".to_owned()
    };
    parts.push(format!(
        "{} ({}) is {:.1} m up {}, facing {} and {}, with a {:.0}° by {:.0}° view and a range of {:.0} m at {} {}.{off}",
        camera_words(c),
        m.display_name(),
        c.position[1] - ground_y(&bundle, eye.x, eye.z),
        camera_place_words(&bundle, c),
        compass(c.yaw_deg),
        tilt_words(c.pitch_deg),
        m.hfov_clamped(),
        m.vfov_clamped(),
        range,
        settings.level,
        match params.lighting {
            Lighting::Day => "by day",
            Lighting::Night => "at night",
        }
    ));
    parts.push(format!(
        "The middle of the view meets {}; straight ahead at its height is {}.",
        centre_of_view.text, level_ahead.text
    ));
    if buildings.is_empty() {
        parts.push("No building is in view within range.".to_owned());
    } else {
        let list: Vec<String> = buildings
            .iter()
            .map(|b| {
                format!(
                    "building {} {:.0} m {} ({})",
                    b.building, b.distance_m, b.compass, b.verdict
                )
            })
            .collect();
        parts.push(format!("Buildings in view: {}.", list.join("; ")));
    }
    if streets.is_empty() {
        parts.push("No street is in view within range.".to_owned());
    } else {
        let list: Vec<String> = streets
            .iter()
            .map(|st| {
                if st.not_ruled_out_m > 0.0 {
                    format!(
                        "{} from {:.0} m (not ruled out along about {:.0} m)",
                        st.street, st.distance_m, st.not_ruled_out_m
                    )
                } else {
                    format!("{} from {:.0} m ({})", st.street, st.distance_m, st.verdict)
                }
            })
            .collect();
        parts.push(format!("Streets in view: {}.", list.join("; ")));
    }
    for t in &targets {
        let name = if t.name.is_empty() {
            format!("target {}", t.id)
        } else {
            format!("target {} \"{}\"", t.id, t.name)
        };
        parts.push(format!("{name}: {}.", t.verdict));
    }
    for a in &areas {
        if let Some(area) = s.areas.iter().find(|x| x.id == a.id) {
            parts.push(format!("{}: {}.", area_words(area), a.verdict));
        }
    }

    Ok(CameraViewOut {
        summary: parts.join(" "),
        camera_id: c.id,
        label: c.label.clone(),
        enabled: c.enabled,
        model_id: c.model_id.clone(),
        hfov_deg: m1(m.hfov_clamped()),
        vfov_deg: m1(m.vfov_clamped()),
        ranges,
        settings,
        range_m: m1(range),
        centre_of_view,
        level_ahead,
        buildings,
        buildings_in_view,
        streets,
        targets,
        areas,
        wording: ONE_SIDED.to_owned(),
        notes: VIEW_NOTES.to_owned(),
        sources: Sources::of(&bundle),
    })
}

// ---------------------------------------------------------------------------
// convert_position
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct PositionOut {
    pub summary: String,
    /// `[lat, lon]`, degrees.
    pub lat_lon: [f64; 2],
    /// `[x, z]`, metres east and south of the origin.
    pub xz: [f64; 2],
    /// Whether the point lies inside the scene's area; absent when the
    /// area's data is not available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inside_area: Option<bool>,
}

fn deg7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}

/// Converts one position between latitude and longitude and the scene's
/// `[x, z]`, with the scene's origin.
pub fn convert(
    scene: &Value,
    lat_lon: Option<[f64; 2]>,
    xz: Option<[f32; 2]>,
) -> Result<PositionOut, ToolError> {
    let s = edit::parse(scene)?;
    let proj = Projection::new(s.origin);
    let (ll, w) = match (lat_lon, xz) {
        (Some([lat, lon]), None) => {
            if !(lat.is_finite()
                && lon.is_finite()
                && (-90.0..=90.0).contains(&lat)
                && (-180.0..=180.0).contains(&lon))
            {
                return Err(ToolError::invalid(
                    "lat_lon is [latitude, longitude]: latitude -90 to 90, longitude -180 to 180",
                ));
            }
            let w = proj.to_world(LatLon::new(lat, lon));
            ([lat, lon], [w[0] as f32, w[1] as f32])
        }
        (None, Some(p)) => {
            if !(p[0].is_finite() && p[1].is_finite()) {
                return Err(ToolError::invalid("xz must be two numbers of metres"));
            }
            let ll = proj.to_latlon([f64::from(p[0]), f64::from(p[1])]);
            ([ll.lat, ll.lon], p)
        }
        _ => {
            return Err(ToolError::invalid(
                "give exactly one of lat_lon ([latitude, longitude]) or xz ([x, z] metres)",
            ));
        }
    };
    let inside_area = bundle_for(&s)
        .ok()
        .map(|b| check_inside(&b, w[0], w[1]).is_ok());
    let where_ = match inside_area {
        Some(true) => ", inside the area",
        Some(false) => ", outside the area: nothing is analysed there",
        None => "",
    };
    let lat_lon = [deg7(ll[0]), deg7(ll[1])];
    let xz = xz1(w);
    Ok(PositionOut {
        summary: format!(
            "Latitude {}, longitude {} is [{}, {}] (metres east and south of the origin){where_}.",
            lat_lon[0], lat_lon[1], xz[0], xz[1]
        ),
        lat_lon,
        xz,
        inside_area,
    })
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SurroundingsArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The point to describe, `[x, z]` metres (x east, z south).
    pub at: [f32; 2],
    /// How far to look, metres (default 40, at most 150).
    pub radius_m: Option<f32>,
    /// Most items of each kind (default 8, at most 20).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListBuildingsArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// List nearest this `[x, z]` first; omit to list by index.
    pub near: Option<[f32; 2]>,
    /// With near: only buildings within this many metres.
    pub radius_m: Option<f32>,
    /// Page size (default 20, at most 50).
    pub limit: Option<usize>,
    /// Skip this many (default 0): pass next_offset for the next page.
    pub offset: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SceneOnlyArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MountArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// Only walls of this building (its index from list_buildings).
    pub building: Option<usize>,
    /// Nearest this `[x, z]` first.
    pub near: Option<[f32; 2]>,
    /// With near: only buildings within this many metres of it.
    pub radius_m: Option<f32>,
    /// "street" (default): face the nearest street. Or a compass word
    /// ("south-east") or degrees clockwise from north.
    pub facing: Option<FacingArg>,
    /// Mount height above the ground, metres (default 2.6).
    pub height_m: Option<f32>,
    /// How many (default 5, at most 20).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CameraViewArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The camera's id.
    pub id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConvertArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// `[latitude, longitude]` to convert to `[x, z]`.
    pub lat_lon: Option<[f64; 2]>,
    /// `[x, z]` metres to convert to latitude and longitude.
    pub xz: Option<[f32; 2]>,
}

#[tool_router(router = describe_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Describe what is around a point [x, z]: the nearest buildings (by index), streets, cameras, labelled areas and targets with distance and compass direction, and the ground's slope. Read the summary.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn describe_surroundings(
        &self,
        Parameters(a): Parameters<SurroundingsArgs>,
    ) -> Result<Json<SurroundingsOut>, String> {
        surroundings(&a.scene, a.at, a.radius_m, a.limit)
            .map(Json)
            .map_err(fail)
    }

    #[tool(
        description = "List the area's buildings, a page at a time: index, centre [x, z], size, height, the street the front faces and which way it faces. Give near to list nearest first.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn list_buildings(
        &self,
        Parameters(a): Parameters<ListBuildingsArgs>,
    ) -> Result<Json<BuildingsOut>, String> {
        buildings(&a.scene, a.near, a.radius_m, a.limit, a.offset)
            .map(Json)
            .map_err(fail)
    }

    #[tool(
        description = "List the area's streets: name (or kind when unnamed), width, length and a simplified centre line of [x, z] points.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn list_streets(
        &self,
        Parameters(a): Parameters<SceneOnlyArgs>,
    ) -> Result<Json<StreetsOut>, String> {
        streets(&a.scene).map(Json).map_err(fail)
    }

    #[tool(
        description = "Suggest wall mounts for a new camera, facing the street by default: each gives at [x, z], height_m, yaw_deg and pitch_deg for place_camera. Optionally one building, near a point, or facing a compass direction.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn suggest_mount_points(
        &self,
        Parameters(a): Parameters<MountArgs>,
    ) -> Result<Json<MountsOut>, String> {
        let q = MountQuery {
            building: a.building,
            near: a.near,
            radius_m: a.radius_m,
            facing: a.facing,
            height_m: a.height_m,
            limit: a.limit,
        };
        off_runtime(move || mount_points(&a.scene, q))
            .await
            .map(Json)
    }

    #[tool(
        description = "Summarise the whole scene in a few sentences: the area, settings, each camera (where it is relative to buildings and streets, which way it faces), custom models, labelled areas and targets. Fast; computes no coverage.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn scene_summary(
        &self,
        Parameters(a): Parameters<SceneOnlyArgs>,
    ) -> Result<Json<SceneSummaryOut>, String> {
        summarise(&a.scene).map(Json).map_err(fail)
    }

    #[tool(
        description = "What lies in one camera's view: its field of view and ranges, where the middle of the view meets the ground, and the buildings, streets, targets and areas in view, each \"not ruled out\" or \"ruled out\" with what blocks it.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn camera_view(
        &self,
        Parameters(a): Parameters<CameraViewArgs>,
    ) -> Result<Json<CameraViewOut>, String> {
        off_runtime(move || view(&a.scene, a.id)).await.map(Json)
    }

    #[tool(
        description = "Convert a position between latitude/longitude and the scene's [x, z] metres: give lat_lon or xz, not both.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn convert_position(
        &self,
        Parameters(a): Parameters<ConvertArgs>,
    ) -> Result<Json<PositionOut>, String> {
        convert(&a.scene, a.lat_lon, a.xz).map(Json).map_err(fail)
    }
}

#[cfg(test)]
#[path = "describe_tests.rs"]
mod tests;
