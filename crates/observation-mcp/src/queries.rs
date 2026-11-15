//! Spatial queries on a scene (#89, #259 part 1): verdicts on targets and
//! points, where each camera's footage lands, and the gaps no camera sees.
//!
//! All of them compute with `observation-core`'s footage module on the
//! scene's area (the demo street, or the user's real area while the app is
//! connected), and every result carries the OSM data date and the caveats.
//! Verdicts are one-sided ("not ruled out" or "ruled out", never "seen"),
//! area figures are as drawn, and buildings are named by index and distance,
//! never by address.

use crate::ToolError;
use crate::area::{self, bundle_for, check_inside, layers_for};
use crate::catalog::{catalog, model};
use crate::edit::{self, Dori, Light};
use crate::schema::JsonObject;
use crate::server::{ObservationMcp, SkippedOut};
use observation_core::address::distance_to_footprint;
use observation_core::bundle::AreaBundle;
use observation_core::camera::{Catalog, Lighting};
use observation_core::coverage::{CoverageParams, RegionMask, StaticLayers};
use observation_core::footage::{
    AreaLabel, AreaMasks, CameraVerdicts, DEFAULT_OFFSET_M, FootageCamera, Gap, Reason, SeenRange,
    Target, TargetPlace, Verdict, camera_splits, point_verdicts, street_gaps, target_verdicts,
};
use observation_core::glam::Vec2;
use observation_core::provenance::{CAVEATS_SUMMARY, caveats_text, format_date};
use observation_core::scene::SceneFile;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How to read a verdict; returned with every verdict.
pub const ONE_SIDED: &str = "Verdicts are one-sided. \"Not ruled out\" means the model finds a \
    clear line of sight within the camera's range at that detail level and lighting; it does not \
    mean the camera sees the place, since hedges, fences, trees and vehicles are not modelled and \
    can only block more. \"Ruled out\" is a firm answer within the model, with the reason.";

/// How to read a split; returned with every split.
pub const SPLIT_NOTES: &str = "Areas and labels are as the user drew them, not property records. \
    Each area figure is a range in m²: low with every outline moved 1 m inward, as drawn, and high \
    moved 1 m outward; read it beside the polygon's own area. Areas may overlap, so area figures \
    need not add up; label figures count each cell once. street_m2 is the street as mapped in \
    OpenStreetMap, whatever is drawn over it: roads, and also driveways (service ways) and \
    footpaths, which are often private, so it is not all public ground. Walls are listed by building index and distance, \
    never by address. Every figure is \"up to\": what the model leaves out can only hide more.";

/// How to read the gaps; returned with them.
pub const GAP_NOTES: &str = "A gap is a connected run of mapped street (road, driveway or \
    footpath surface) that no enabled camera sees at the detail level and lighting given, largest \
    first. Gaps are estimates and read as \"at least\": trees, fences and vehicles are not \
    modelled and can only make them larger. Streets are named from OpenStreetMap; buildings by \
    index and distance, never by address.";

/// Most gaps `find_gaps` lists.
pub const MAX_GAPS: usize = 20;

/// Where the figures come from and what they leave out.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Sources {
    /// When the OpenStreetMap data was current, if known.
    pub osm_data_as_of: Option<String>,
    /// One line to show with the figures.
    pub caveats_summary: String,
    /// Everything the figures leave out; pass it on.
    pub caveats: String,
}

impl Sources {
    pub fn of(bundle: &AreaBundle) -> Self {
        Self {
            osm_data_as_of: bundle.osm_base_unix.map(format_date),
            caveats_summary: CAVEATS_SUMMARY.to_owned(),
            caveats: caveats_text(),
        }
    }
}

/// The settings a result was computed at.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Settings {
    /// The detail level.
    pub level: String,
    /// Its pixel density, px/m.
    pub ppm: f32,
    /// "Day" or "Night".
    pub lighting: String,
    /// The height above the ground people are tested at, metres.
    pub subject_height_m: f32,
}

impl Settings {
    pub(crate) fn of(p: &CoverageParams) -> Self {
        Self {
            level: p.dori.label().to_owned(),
            ppm: p.pixels_per_metre(),
            lighting: lighting_name(p.lighting).to_owned(),
            subject_height_m: p.subject_height_m,
        }
    }
}

fn lighting_name(l: Lighting) -> &'static str {
    match l {
        Lighting::Day => "Day",
        Lighting::Night => "Night",
    }
}

fn lighting_words(l: Lighting) -> &'static str {
    match l {
        Lighting::Day => "by day",
        Lighting::Night => "at night",
    }
}

/// The scene's settings with a level and lighting chosen for one query.
pub(crate) fn with_overrides(
    mut p: CoverageParams,
    dori: Option<Dori>,
    light: Option<Light>,
) -> CoverageParams {
    if let Some(d) = dori {
        p.dori = d.into();
    }
    if let Some(l) = light {
        p.lighting = l.into();
    }
    p
}

/// Runs `f` off the async runtime (line of sight is CPU work), with the
/// connection's real areas in view, as `compute_coverage` does.
pub(crate) async fn off_runtime<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, String> {
    let areas = area::current_areas();
    tokio::task::spawn_blocking(move || match areas {
        Some(areas) => area::with_areas(areas, f),
        None => f(),
    })
    .await
    .map_err(|e| format!("the computation failed: {e}"))?
    .map_err(|e| e.to_string())
}

/// The cameras the queries leave out, and why.
pub(crate) fn skipped(scene: &SceneFile, cat: &Catalog) -> Vec<SkippedOut> {
    scene
        .cameras
        .iter()
        .filter_map(|c| {
            let reason = if !c.enabled {
                "disabled"
            } else if model(cat, Some(scene), &c.model_id).is_none() {
                "unknown model"
            } else {
                return None;
            };
            Some(SkippedOut {
                id: c.id,
                reason: reason.to_owned(),
            })
        })
        .collect()
}

fn camera_label(scene: &SceneFile, id: u32) -> String {
    scene
        .cameras
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.label.clone())
        .unwrap_or_default()
}

fn no_target(id: u32) -> ToolError {
    ToolError::invalid(format!("no target with id {id} in the scene"))
}

// ---------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------

/// A verdict at one detail level and lighting.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VerdictOut {
    pub level: String,
    /// "Day" or "Night".
    pub lighting: String,
    /// The camera's range at this level and lighting, metres.
    pub range_m: f32,
    /// "not ruled out" or "ruled out".
    pub verdict: String,
    /// Why it is ruled out: `outside_view`, `out_of_range`,
    /// `blocked_by_building` or `blocked_by_terrain`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The verdict in words, such as "ruled out: out of range".
    pub text: String,
}

fn reason_code(r: Reason) -> &'static str {
    match r {
        Reason::OutsideView => "outside_view",
        Reason::OutOfRange => "out_of_range",
        Reason::BlockedByBuilding => "blocked_by_building",
        Reason::BlockedByTerrain => "blocked_by_terrain",
    }
}

/// A verdict in a few words, for summaries.
fn short(v: Verdict) -> String {
    match v {
        Verdict::NotRuledOut => "not ruled out".to_owned(),
        Verdict::RuledOut(r) => format!("ruled out ({})", r.text()),
    }
}

/// One camera's verdicts on a place.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CameraVerdictOut {
    pub camera_id: u32,
    /// The camera's name in the scene.
    pub camera_label: String,
    /// Straight-line distance from the camera to the nearest point of the
    /// place, metres.
    pub nearest_m: f32,
    /// The verdict at the detail level and lighting the query used.
    pub selected: String,
    /// Every level by day and at night, in a line.
    pub summary: String,
    /// Each detail level by day and at night.
    pub verdicts: Vec<VerdictOut>,
}

impl CameraVerdictOut {
    fn of(scene: &SceneFile, params: &CoverageParams, cv: &CameraVerdicts) -> Self {
        let verdicts = cv
            .verdicts
            .iter()
            .map(|v| VerdictOut {
                level: v.level.label().to_owned(),
                lighting: lighting_name(v.lighting).to_owned(),
                range_m: v.range_m,
                verdict: if v.verdict.is_ruled_out() {
                    "ruled out"
                } else {
                    "not ruled out"
                }
                .to_owned(),
                reason: match v.verdict {
                    Verdict::RuledOut(r) => Some(reason_code(r).to_owned()),
                    Verdict::NotRuledOut => None,
                },
                text: v.verdict.text(),
            })
            .collect();
        let selected = cv
            .at(params.dori, params.lighting)
            .map(Verdict::text)
            .unwrap_or_default();
        Self {
            camera_id: cv.camera_id,
            camera_label: camera_label(scene, cv.camera_id),
            nearest_m: cv.nearest_m,
            selected,
            summary: camera_summary(cv),
            verdicts,
        }
    }
}

/// "By day: Detect, Observe: not ruled out; Recognise, Identify: ruled out
/// (out of range). At night: every level: ruled out (out of range)."
fn camera_summary(cv: &CameraVerdicts) -> String {
    let mut parts = Vec::new();
    for lighting in [Lighting::Day, Lighting::Night] {
        let mut groups: Vec<(Verdict, Vec<&str>)> = Vec::new();
        for v in cv.verdicts.iter().filter(|v| v.lighting == lighting) {
            match groups.iter_mut().find(|g| g.0 == v.verdict) {
                Some(g) => g.1.push(v.level.label()),
                None => groups.push((v.verdict, vec![v.level.label()])),
            }
        }
        let text = match groups.as_slice() {
            [] => continue,
            [(v, _)] => format!("every level: {}", short(*v)),
            _ => groups
                .iter()
                .map(|(v, levels)| format!("{}: {}", levels.join(", "), short(*v)))
                .collect::<Vec<_>>()
                .join("; "),
        };
        let when = match lighting {
            Lighting::Day => "By day",
            Lighting::Night => "At night",
        };
        parts.push(format!("{when}: {text}."));
    }
    parts.join(" ")
}

/// "at Detect by day: not ruled out for cameras 1 and 3; ruled out for
/// camera 2 (outside the view)".
fn place_summary(params: &CoverageParams, cams: &[CameraVerdicts]) -> String {
    let at = format!(
        "at {} {}",
        params.dori.label(),
        lighting_words(params.lighting)
    );
    if cams.is_empty() {
        return format!("{at}: no enabled cameras to test");
    }
    let (open, out): (Vec<&CameraVerdicts>, Vec<&CameraVerdicts>) = cams
        .iter()
        .partition(|c| c.at(params.dori, params.lighting) == Some(Verdict::NotRuledOut));
    let ids = |cs: &[&CameraVerdicts]| {
        cs.iter()
            .map(|c| c.camera_id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let ruled = |cs: &[&CameraVerdicts]| {
        cs.iter()
            .map(|c| {
                let why = match c.at(params.dori, params.lighting) {
                    Some(Verdict::RuledOut(r)) => r.text(),
                    _ => "no verdict",
                };
                format!("camera {} ({why})", c.camera_id)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (open.is_empty(), out.is_empty()) {
        (true, _) => format!("{at}: ruled out for every camera: {}", ruled(&out)),
        (false, true) => format!(
            "{at}: not ruled out for camera(s) {}, ruled out for none",
            ids(&open)
        ),
        (false, false) => format!(
            "{at}: not ruled out for camera(s) {}; ruled out for {}",
            ids(&open),
            ruled(&out)
        ),
    }
}

/// Every camera's verdicts on one target.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TargetOut {
    pub target_id: u32,
    pub name: String,
    /// "ground" or "wall".
    pub kind: String,
    /// The verdicts at the scene's level and lighting, in a line.
    pub summary: String,
    pub cameras: Vec<CameraVerdictOut>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TargetVerdictsOut {
    /// The scene's level and lighting, which `selected` and the summaries
    /// use; the verdicts cover every level by day and at night.
    pub settings: Settings,
    pub targets: Vec<TargetOut>,
    /// Cameras not tested, and why.
    pub cameras_skipped: Vec<SkippedOut>,
    /// How to read a verdict; pass it on.
    pub wording: String,
    #[serde(flatten)]
    pub sources: Sources,
}

fn kind_of(t: &Target) -> &'static str {
    match t.at {
        TargetPlace::Ground { .. } => "ground",
        TargetPlace::Wall { .. } => "wall",
    }
}

/// Every enabled camera's verdicts on the scene's targets (or the one
/// `target_id` names).
pub fn verdicts_on_targets(
    scene: &Value,
    target_id: Option<u32>,
) -> Result<TargetVerdictsOut, ToolError> {
    let cat = catalog()?;
    let s = edit::parse(scene)?;
    if s.targets.is_empty() {
        return Err(ToolError::invalid(
            "the scene has no targets: add one with add_target",
        ));
    }
    let targets: Vec<Target> = match target_id {
        Some(id) => vec![
            s.targets
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or_else(|| no_target(id))?,
        ],
        None => s.targets.clone(),
    };
    let bundle = bundle_for(&s)?;
    let layers = layers_for(&s)?;
    let cams = s.footage_cameras(cat);
    let params = s.params;
    let found = target_verdicts(&layers, &params, &cams, &targets);
    let targets = targets
        .iter()
        .zip(found)
        .map(|(t, v)| {
            let name = if t.name.is_empty() {
                format!("Target {}", t.id)
            } else {
                t.name.clone()
            };
            TargetOut {
                target_id: t.id,
                summary: format!("{name}, {}", place_summary(&params, &v.cameras)),
                name: t.name.clone(),
                kind: kind_of(t).to_owned(),
                cameras: v
                    .cameras
                    .iter()
                    .map(|c| CameraVerdictOut::of(&s, &params, c))
                    .collect(),
            }
        })
        .collect();
    Ok(TargetVerdictsOut {
        settings: Settings::of(&params),
        targets,
        cameras_skipped: skipped(&s, cat),
        wording: ONE_SIDED.to_owned(),
        sources: Sources::of(&bundle),
    })
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PointOut {
    /// The point, metres east and south of the origin.
    pub x: f32,
    pub z: f32,
    pub settings: Settings,
    /// Whether the point lies inside a building footprint (hidden from
    /// every camera).
    pub inside_building: bool,
    /// The verdicts at the level and lighting used, in a line.
    pub summary: String,
    pub cameras: Vec<CameraVerdictOut>,
    /// Cameras not tested, and why.
    pub cameras_skipped: Vec<SkippedOut>,
    /// How to read a verdict; pass it on.
    pub wording: String,
    #[serde(flatten)]
    pub sources: Sources,
}

/// Every enabled camera's verdicts on the point `[x, z]`, at `height_m`
/// above the ground (default: the scene's subject height).
pub fn verdicts_on_point(
    scene: &Value,
    x: f32,
    z: f32,
    height_m: Option<f32>,
) -> Result<PointOut, ToolError> {
    let cat = catalog()?;
    let s = edit::parse(scene)?;
    let bundle = bundle_for(&s)?;
    check_inside(&bundle, x, z)?;
    let mut params = s.params;
    if let Some(h) = height_m {
        // The scene file's own limit for the subject height.
        if !(h.is_finite() && (0.0..=10.0).contains(&h)) {
            return Err(ToolError::invalid(format!(
                "height {h} m is out of range: use 0 to 10 m above the ground"
            )));
        }
        params.subject_height_m = h;
    }
    let layers = layers_for(&s)?;
    let cams = s.footage_cameras(cat);
    let found = point_verdicts(&layers, &params, &cams, [x, z]);
    let inside_building = layers.occluders().inside_any(Vec2::new(x, z));
    let mut summary = format!("[{x}, {z}], {}", place_summary(&params, &found));
    if inside_building {
        summary.push_str("; the point is inside a building footprint");
    }
    Ok(PointOut {
        x,
        z,
        settings: Settings::of(&params),
        inside_building,
        summary,
        cameras: found
            .iter()
            .map(|c| CameraVerdictOut::of(&s, &params, c))
            .collect(),
        cameras_skipped: skipped(&s, cat),
        wording: ONE_SIDED.to_owned(),
        sources: Sources::of(&bundle),
    })
}

// ---------------------------------------------------------------------------
// Splits
// ---------------------------------------------------------------------------

/// A ground figure under three readings of the drawn outlines, m².
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct RangeOut {
    /// With the outlines moved 1 m inward (for unlabelled ground, outward).
    pub low_m2: f32,
    pub as_drawn_m2: f32,
    /// With the outlines moved 1 m outward (for unlabelled ground, inward).
    pub high_m2: f32,
}

impl From<SeenRange> for RangeOut {
    fn from(r: SeenRange) -> Self {
        Self {
            low_m2: r.low_m2,
            as_drawn_m2: r.as_drawn_m2,
            high_m2: r.high_m2,
        }
    }
}

/// The scene file's name for a label.
fn label_name(l: AreaLabel) -> &'static str {
    match l {
        AreaLabel::Own => "own",
        AreaLabel::Neighbour => "neighbour",
        AreaLabel::Public => "public",
        AreaLabel::Other => "other",
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AreaSplitOut {
    pub area_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `own`, `neighbour`, `public` or `other`; absent when unlabelled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The outline's own area as drawn, m².
    pub polygon_m2: f32,
    /// The camera's visible ground inside it.
    pub seen: RangeOut,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LabelSplitOut {
    pub label: String,
    /// The camera's visible ground inside any area with this label.
    pub seen: RangeOut,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WallSplitOut {
    /// The building's index in the area (never its address).
    pub building: u32,
    /// Distance from the camera to the nearest patch of it the camera sees,
    /// metres.
    pub distance_m: f32,
    pub seen_m2: f32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CameraSplitOut {
    pub camera_id: u32,
    pub camera_label: String,
    /// The range counted out to, metres.
    pub range_m: f32,
    /// All the ground the camera sees, m².
    pub ground_m2: f32,
    /// The part on the street, driveways and footpaths as mapped in
    /// OpenStreetMap (driveways and footpaths are often private), m².
    pub street_m2: f32,
    /// The part neither on that mapped street nor in a labelled area.
    pub unlabelled: RangeOut,
    /// Each enabled area with a usable outline, in the scene's order.
    pub areas: Vec<AreaSplitOut>,
    /// Each label some enabled area has.
    pub labels: Vec<LabelSplitOut>,
    /// All the wall the camera sees, m².
    pub walls_m2: f32,
    /// The wall it sees per building, nearest first.
    pub walls: Vec<WallSplitOut>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SplitOut {
    pub settings: Settings,
    /// How far the outlines are moved in and out for the ranges, metres.
    pub offset_m: f32,
    pub cameras: Vec<CameraSplitOut>,
    /// Cameras not counted, and why.
    pub cameras_skipped: Vec<SkippedOut>,
    /// How to read the split; pass it on.
    pub notes: String,
    #[serde(flatten)]
    pub sources: Sources,
}

/// The enabled cameras with known models, or the one `id` names (refused if
/// it is not one of them).
fn chosen<'a>(
    scene: &'a SceneFile,
    cat: &'a Catalog,
    id: Option<u32>,
) -> Result<Vec<FootageCamera<'a>>, ToolError> {
    let cams = scene.footage_cameras(cat);
    let Some(id) = id else {
        return Ok(cams);
    };
    let c = scene
        .cameras
        .iter()
        .find(|c| c.id == id)
        .ok_or(ToolError::NoCamera(id))?;
    if !c.enabled {
        return Err(ToolError::invalid(format!(
            "camera {id} is disabled: enable it with update_camera first"
        )));
    }
    let picked: Vec<FootageCamera> = cams.into_iter().filter(|f| f.placement.id == id).collect();
    if picked.is_empty() {
        return Err(ToolError::NoModel(c.model_id.clone()));
    }
    Ok(picked)
}

/// Where each enabled camera's footage lands (or one camera's).
pub fn split(
    scene: &Value,
    camera_id: Option<u32>,
    dori: Option<Dori>,
    light: Option<Light>,
) -> Result<SplitOut, ToolError> {
    let cat = catalog()?;
    let s = edit::parse(scene)?;
    let params = with_overrides(s.params, dori, light);
    let cams = chosen(&s, cat, camera_id)?;
    let bundle = bundle_for(&s)?;
    let layers = layers_for(&s)?;
    let masks = AreaMasks::new(&layers, &s.areas, DEFAULT_OFFSET_M);
    let name_of = |id: u32| {
        s.areas
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| a.name.clone())
    };
    let cameras = camera_splits(&layers, &params, &cams, &masks)
        .into_iter()
        .map(|c| {
            let sp = c.split;
            let mut walls: Vec<WallSplitOut> = sp
                .walls
                .iter()
                .map(|w| WallSplitOut {
                    building: w.building,
                    distance_m: w.distance_m,
                    seen_m2: w.seen_m2,
                })
                .collect();
            walls.sort_by(|a, b| {
                a.distance_m
                    .total_cmp(&b.distance_m)
                    .then(a.building.cmp(&b.building))
            });
            CameraSplitOut {
                camera_id: c.camera_id,
                camera_label: camera_label(&s, c.camera_id),
                range_m: c.range_m,
                ground_m2: sp.ground_m2,
                street_m2: sp.street_m2,
                unlabelled: sp.unlabelled.into(),
                areas: sp
                    .areas
                    .iter()
                    .map(|a| AreaSplitOut {
                        area_id: a.area_id,
                        name: name_of(a.area_id),
                        label: a.label.map(|l| label_name(l).to_owned()),
                        polygon_m2: a.polygon_m2,
                        seen: a.seen.into(),
                    })
                    .collect(),
                labels: sp
                    .labels
                    .iter()
                    .map(|l| LabelSplitOut {
                        label: label_name(l.label).to_owned(),
                        seen: l.seen.into(),
                    })
                    .collect(),
                walls_m2: sp.walls_m2,
                walls,
            }
        })
        .collect();
    let cameras_skipped = match camera_id {
        Some(_) => Vec::new(),
        None => skipped(&s, cat),
    };
    Ok(SplitOut {
        settings: Settings::of(&params),
        offset_m: masks.offset_m(),
        cameras,
        cameras_skipped,
        notes: SPLIT_NOTES.to_owned(),
        sources: Sources::of(&bundle),
    })
}

// ---------------------------------------------------------------------------
// Gaps
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NearCameraOut {
    pub camera_id: u32,
    pub camera_label: String,
    /// From the camera to the nearest part of the gap, in plan, metres.
    pub distance_m: f32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NearStreetOut {
    /// The street's name in OpenStreetMap, if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its OpenStreetMap `highway` type, such as `residential` or `footway`.
    pub highway: String,
    /// From the gap's inside point to the street's centre line, metres.
    pub distance_m: f32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NearBuildingOut {
    /// The building's index in the area (never its address).
    pub building: u32,
    /// From the gap's inside point to the building, metres.
    pub distance_m: f32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GapOut {
    /// 1 for the largest.
    pub rank: usize,
    pub area_m2: f32,
    /// The mean of the gap's cells, `[x, z]`; it may lie off the gap where
    /// the street bends.
    pub centroid: [f32; 2],
    /// A point inside the gap near its centroid, `[x, z]`: point a camera
    /// here (point_camera_at) or ask who_sees_point about it.
    pub inside: [f32; 2],
    /// The smallest `[x, z]` of the gap.
    pub min: [f32; 2],
    /// The largest `[x, z]` of the gap.
    pub max: [f32; 2],
    /// The enabled camera nearest the gap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nearest_camera: Option<NearCameraOut>,
    /// The street the gap is on or near.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street: Option<NearStreetOut>,
    /// The building nearest the gap's inside point, within 30 m.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nearest_building: Option<NearBuildingOut>,
    /// The gap in a line.
    pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GapsOut {
    pub settings: Settings,
    /// Whether only the street inside the scene's area for the figures
    /// counts (see limit_figures_to).
    pub region_limited: bool,
    /// The mapped street counted, m².
    pub street_m2: f32,
    /// The part of it no enabled camera sees, m², in gaps of any size.
    pub unseen_street_m2: f32,
    /// How many gaps are at least `min_area_m2`.
    pub gap_count: usize,
    /// The largest gaps, largest first.
    pub gaps: Vec<GapOut>,
    pub cameras_counted: Vec<u32>,
    /// Cameras not counted, and why.
    pub cameras_skipped: Vec<SkippedOut>,
    /// How to read the gaps; pass it on.
    pub notes: String,
    #[serde(flatten)]
    pub sources: Sources,
}

/// The building within 30 m nearest `p`, by index and distance.
fn nearest_building(bundle: &AreaBundle, p: [f32; 2]) -> Option<NearBuildingOut> {
    let i = bundle.building_near(p, 30.0)?;
    Some(NearBuildingOut {
        building: u32::try_from(i).ok()?,
        distance_m: distance_to_footprint(p, &bundle.buildings.get(i)?.footprint),
    })
}

fn describe_gap(g: &GapOut) -> String {
    let mut text = format!(
        "{:.0} m² of street around [{:.0}, {:.0}]",
        g.area_m2, g.inside[0], g.inside[1]
    );
    if let Some(st) = &g.street {
        let what = match &st.name {
            Some(n) => n.clone(),
            None => format!("an unnamed {} way", st.highway),
        };
        if st.distance_m <= 10.0 {
            text.push_str(&format!(" on {what}"));
        } else {
            text.push_str(&format!(" near {what} ({:.0} m)", st.distance_m));
        }
    }
    let span = (g.max[0] - g.min[0]).max(g.max[1] - g.min[1]);
    text.push_str(&format!(", spanning {span:.0} m"));
    if let Some(b) = &g.nearest_building {
        text.push_str(&format!(
            "; nearest building {}, {:.0} m away",
            b.building, b.distance_m
        ));
    }
    match &g.nearest_camera {
        Some(c) => text.push_str(&format!(
            "; nearest camera {}, {:.0} m away",
            c.camera_id, c.distance_m
        )),
        None => text.push_str("; no enabled cameras"),
    }
    text
}

fn gap_out(
    scene: &SceneFile,
    bundle: &AreaBundle,
    layers: &StaticLayers,
    cams: &[FootageCamera],
    rank: usize,
    g: &Gap,
) -> GapOut {
    let nearest_camera = cams
        .iter()
        .map(|c| {
            let p = c.placement.position;
            (c.placement.id, g.nearest_to(layers.grid(), [p[0], p[2]]).0)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
        .map(|(id, d)| NearCameraOut {
            camera_id: id,
            camera_label: camera_label(scene, id),
            distance_m: d,
        });
    let street = bundle.nearest_road(g.inside, |_| true).and_then(|near| {
        let road = bundle.roads.get(near.index)?;
        // An unnamed way next to a named street: name the street if it is
        // nearly as close.
        let named = bundle
            .nearest_road(g.inside, |r| r.name.is_some())
            .filter(|n| n.distance_m <= near.distance_m + 5.0)
            .and_then(|n| bundle.roads.get(n.index).map(|r| (r, n.distance_m)));
        let (road, distance_m) = named.unwrap_or((road, near.distance_m));
        Some(NearStreetOut {
            name: road.name.clone(),
            highway: road.highway.clone(),
            distance_m,
        })
    });
    let mut out = GapOut {
        rank,
        area_m2: g.area_m2,
        centroid: g.centroid,
        inside: g.inside,
        min: g.min,
        max: g.max,
        nearest_camera,
        street,
        nearest_building: nearest_building(bundle, g.inside),
        description: String::new(),
    };
    out.description = describe_gap(&out);
    out
}

/// The largest runs of street no enabled camera sees.
pub fn gaps(
    scene: &Value,
    dori: Option<Dori>,
    light: Option<Light>,
    max_gaps: Option<usize>,
    min_area_m2: Option<f32>,
) -> Result<GapsOut, ToolError> {
    let cat = catalog()?;
    let s = edit::parse(scene)?;
    let params = with_overrides(s.params, dori, light);
    let max_gaps = max_gaps.unwrap_or(5).clamp(1, MAX_GAPS);
    let min_area = min_area_m2.unwrap_or(1.0);
    if !(min_area.is_finite() && min_area >= 0.0) {
        return Err(ToolError::invalid(
            "min_area_m2 must be a number of square metres, at least 0",
        ));
    }
    let bundle = bundle_for(&s)?;
    let layers = layers_for(&s)?;
    let region = s
        .figures_area()
        .filter(|a| a.is_active())
        .map(|a| RegionMask::new(&layers, &a.points));
    let cams = s.footage_cameras(cat);
    let found = street_gaps(&layers, &params, &cams, region.as_ref());
    let cell_m2 = layers.grid().cell_area_m2();
    let street_cells = layers
        .street_mask()
        .iter()
        .enumerate()
        .filter(|&(i, &on)| on && region.as_ref().is_none_or(|r| r.ground[i]))
        .count();
    let unseen: f64 = found.iter().map(|g| f64::from(g.area_m2)).sum();
    let big: Vec<&Gap> = found.iter().filter(|g| g.area_m2 >= min_area).collect();
    let gaps = big
        .iter()
        .take(max_gaps)
        .enumerate()
        .map(|(k, g)| gap_out(&s, &bundle, &layers, &cams, k + 1, g))
        .collect();
    Ok(GapsOut {
        settings: Settings::of(&params),
        region_limited: region.is_some(),
        street_m2: street_cells as f32 * cell_m2,
        unseen_street_m2: unseen as f32,
        gap_count: big.len(),
        gaps,
        cameras_counted: cams.iter().map(|c| c.placement.id).collect(),
        cameras_skipped: skipped(&s, cat),
        notes: GAP_NOTES.to_owned(),
        sources: Sources::of(&bundle),
    })
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TargetVerdictsArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// Only this target; omit for every target in the scene.
    pub target_id: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CameraSplitArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// Only this camera; omit for every enabled camera.
    pub camera_id: Option<u32>,
    /// The detail level; omit for the scene's.
    pub dori: Option<Dori>,
    /// Day or night; omit for the scene's.
    pub lighting: Option<Light>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WhoSeesArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// Metres east of the origin.
    pub x: f32,
    /// Metres south of the origin.
    pub z: f32,
    /// Height above the ground to test at, metres (0 to 10); omit for the
    /// scene's subject height.
    pub height_m: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindGapsArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The detail level; omit for the scene's.
    pub dori: Option<Dori>,
    /// Day or night; omit for the scene's.
    pub lighting: Option<Light>,
    /// How many gaps to list, largest first (1 to 20, default 5).
    pub max_gaps: Option<usize>,
    /// Leave out gaps smaller than this, m² (default 1).
    pub min_area_m2: Option<f32>,
}

#[tool_router(router = query_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "For every target in the scene (or one), each enabled camera's verdict at every detail level by day and at night: \"not ruled out\", or \"ruled out\" with the reason (outside the view, out of range, blocked by a building or by the terrain). Never say a camera \"sees\" a target; pass on the wording and caveats.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn target_verdicts(
        &self,
        Parameters(a): Parameters<TargetVerdictsArgs>,
    ) -> Result<Json<TargetVerdictsOut>, String> {
        off_runtime(move || verdicts_on_targets(&a.scene, a.target_id))
            .await
            .map(Json)
    }

    #[tool(
        description = "Where each enabled camera's footage lands (or one camera's), at the scene's detail level and lighting or those given: m² of visible ground in each labelled area and per label (as ranges with the outlines moved 1 m in and out, beside each polygon's area), on the street, driveways and footpaths as mapped, and on unlabelled ground, and the wall seen per building by index and distance. Pass on the notes and caveats.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn camera_split(
        &self,
        Parameters(a): Parameters<CameraSplitArgs>,
    ) -> Result<Json<SplitOut>, String> {
        off_runtime(move || split(&a.scene, a.camera_id, a.dori, a.lighting))
            .await
            .map(Json)
    }

    #[tool(
        description = "Which enabled cameras do not rule out a point [x, z] at the scene's settings, and why each other one rules it out, with the verdicts at every detail level by day and at night.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn who_sees_point(
        &self,
        Parameters(a): Parameters<WhoSeesArgs>,
    ) -> Result<Json<PointOut>, String> {
        off_runtime(move || verdicts_on_point(&a.scene, a.x, a.z, a.height_m))
            .await
            .map(Json)
    }

    #[tool(
        description = "The largest connected stretches of street no enabled camera sees at the scene's detail level and lighting (or those given), largest first: each with its area, centroid, a point inside, a bounding box, the nearest camera, the street it is on and the nearest building by index (never an address). Limited to the area for the figures if the scene has one.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn find_gaps(
        &self,
        Parameters(a): Parameters<FindGapsArgs>,
    ) -> Result<Json<GapsOut>, String> {
        off_runtime(move || gaps(&a.scene, a.dori, a.lighting, a.max_gaps, a.min_area_m2))
            .await
            .map(Json)
    }
}
