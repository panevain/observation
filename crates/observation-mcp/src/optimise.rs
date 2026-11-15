//! Goal tools: the server searches placements and aims with the coverage
//! engine, so that a small model can ask for "more street" or "the front
//! door" instead of trying positions itself.
//!
//! Every search is bounded and deterministic. It works on the cells that
//! matter for the goal only (the street, a labelled area, the largest gaps,
//! or the targets with the street as a tie-break) that no other camera
//! already sees, and tests each line of sight once per mount point: aims
//! then only filter those cells by the view cone, which is cheap, so many
//! aims cost little more than one. The figures returned are then computed
//! afresh over the whole scene with the same functions as `find_gaps` and
//! `target_verdicts`, so they agree with those tools.
//!
//! Candidate mounts are the house fronts the example cameras use
//! ([`wall_mounts`], [`WallMount::faces_street`]), at each model's typical
//! height, with a few aims each; placements are picked greedily, the best
//! first. Results are the model's estimate under its caveats, with the same
//! one-sided wording as the queries.

use crate::ToolError;
use crate::area::{self, bundle_for, layers_for};
use crate::catalog::{catalog, model};
use crate::edit::{self, Dori};
use crate::queries::{ONE_SIDED, Settings, Sources};
use crate::schema::JsonObject;
use crate::server::ObservationMcp;
use observation_core::bundle::AreaBundle;
use observation_core::camera::{
    CameraModel, CameraPlacement, Catalog, Lighting, yaw_pitch_from_forward,
};
use observation_core::coverage::{CoverageParams, RegionMask, StaticLayers};
use observation_core::example::{EXAMPLE_MODELS, WallMount, wall_mounts};
use observation_core::footage::{
    FootageCamera, Probe, Reach, Target, TargetPlace, Verdict, fresh_id, seen_ground, street_gaps,
};
use observation_core::frustum::ViewCone;
use observation_core::glam::{Vec2, Vec3};
use observation_core::scene::{MAX_CAMERAS, SceneFile};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// Most cameras `suggest_placement` suggests in one call.
pub const MAX_SUGGESTIONS: usize = 4;
/// Most models it chooses between.
pub const MAX_MODELS: usize = 4;
/// Most house fronts it tries, nearest the goal first.
pub const MAX_WALLS: usize = 96;
/// The largest gaps the `gaps` goal covers (as `find_gaps` lists them by
/// default).
pub const GOAL_GAPS: usize = 5;
/// A suggested camera keeps at least this far from every other camera,
/// metres.
const APART_M: f32 = 2.0;
/// The smallest gain worth suggesting a camera for, m².
const MIN_GAIN_M2: f32 = 1.0;
/// How far `allow_move` may slide a camera along its wall, each way, metres.
const MOVES_M: [f32; 6] = [-3.0, -2.0, -1.0, 1.0, 2.0, 3.0];
/// A camera this close to a wall's line counts as mounted on it, metres.
const ON_WALL_M: f32 = 1.0;

/// How to read a suggestion; returned with every result.
pub const GOAL_NOTES: &str = "These are the model's estimates from a bounded search, not \
    a survey: candidates are house fronts facing a street at each model's typical height, with \
    a few aims each, picked best first. Figures are as the coverage engine counts them (street, \
    driveways and footpaths as mapped; walls and ground by line of sight), \"not ruled out\" is \
    one-sided, and what the model leaves out (trees, fences, vehicles) can only reduce them. \
    Buildings are named by index, never by address; check a suggestion on site and with the \
    owner's consent before mounting anything.";

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// What a search tries to cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalArg {
    /// The most street (as mapped) not ruled out that no other camera
    /// covers.
    #[default]
    Street,
    /// The most targets not ruled out; ties go to the most street.
    Targets,
    /// The most visible ground in one labelled area (`area_id`).
    Area,
    /// The largest gaps `find_gaps` lists.
    Gaps,
}

/// A target, by its id or its name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum TargetRef {
    Id(u32),
    Name(String),
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// The 8-point compass word for a bearing, degrees clockwise from north.
pub fn compass(yaw_deg: f32) -> &'static str {
    const NAMES: [&str; 8] = [
        "north",
        "north-east",
        "east",
        "south-east",
        "south",
        "south-west",
        "west",
        "north-west",
    ];
    NAMES[((yaw_deg.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

fn round_to(v: f32, step: f32) -> f32 {
    (v / step).round() * step
}

fn r1(v: f32) -> f32 {
    round_to(v, 0.1)
}

fn r2(v: f32) -> f32 {
    round_to(v, 0.01)
}

/// The bearing of `v` (`[x, z]`), degrees clockwise from north.
fn bearing(v: Vec2) -> f32 {
    yaw_pitch_from_forward(Vec3::new(v.x, 0.0, v.y)).map_or(0.0, |(y, _)| y)
}

/// The signed difference `a - b` between two bearings, in `[-180, 180)`.
fn turn(a: f32, b: f32) -> f32 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
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

fn settings_of(p: &CoverageParams) -> Settings {
    Settings {
        level: p.dori.label().to_owned(),
        ppm: p.pixels_per_metre(),
        lighting: lighting_name(p.lighting).to_owned(),
        subject_height_m: p.subject_height_m,
    }
}

/// "at Detect by day".
fn at_words(p: &CoverageParams) -> String {
    format!("at {} {}", p.dori.label(), lighting_words(p.lighting))
}

/// Runs `f` off the async runtime, with the connection's real areas in
/// view, as the queries do.
async fn off_runtime<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, String> {
    let areas = area::current_areas();
    tokio::task::spawn_blocking(move || match areas {
        Some(areas) => area::with_areas(areas, f),
        None => f(),
    })
    .await
    .map_err(|e| format!("the search failed: {e}"))?
    .map_err(|e| e.to_string())
}

/// A camera's aim.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Aim {
    yaw: f32,
    pitch: f32,
}

impl Aim {
    fn new(yaw: f32, pitch: f32) -> Self {
        Self {
            yaw: yaw.rem_euclid(360.0),
            pitch: pitch.clamp(-89.0, 89.0),
        }
    }

    /// The aim from `eye` at `p`, to whole degrees.
    fn at(eye: Vec3, p: Vec3) -> Option<Self> {
        let (yaw, pitch) = yaw_pitch_from_forward(p - eye)?;
        Some(Self::new(yaw.round(), pitch.round()))
    }
}

/// An aim, as the tools return it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AimOut {
    /// Compass bearing, degrees clockwise from north.
    pub yaw_deg: f32,
    /// The bearing as an 8-point compass word, such as "south-east".
    pub facing: String,
    /// Tilt, degrees (negative looks down).
    pub pitch_deg: f32,
}

impl AimOut {
    fn of(yaw: f32, pitch: f32) -> Self {
        Self {
            yaw_deg: r1(yaw),
            facing: compass(yaw).to_owned(),
            pitch_deg: r1(pitch),
        }
    }
}

// ---------------------------------------------------------------------------
// The goal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum GoalKind {
    Street,
    Area { words: String },
    Gaps { count: usize },
    Targets { targets: Vec<Target> },
}

/// A goal resolved against a scene: which cells count, and the targets.
struct Goal {
    kind: GoalKind,
    /// The cells the goal counts (for targets, the street, as the tie-break).
    cells: Vec<bool>,
    /// Their area, m².
    total_m2: f32,
    /// Each target's test points (targets only).
    probes: Vec<Probe>,
    /// A point the goal is about, if it has one: aims at it are tried too.
    focus: Option<Vec2>,
    /// The bounds of the goal's cells (and targets), to try the house fronts
    /// near them first.
    bounds: Option<(Vec2, Vec2)>,
}

impl Goal {
    fn is_targets(&self) -> bool {
        matches!(self.kind, GoalKind::Targets { .. })
    }

    fn unit(&self) -> &'static str {
        if self.is_targets() { "targets" } else { "m²" }
    }

    /// "street ground (road, driveways and footpaths as mapped)", ...
    fn words(&self) -> String {
        match &self.kind {
            GoalKind::Street => "street, driveways and footpaths as mapped".to_owned(),
            GoalKind::Area { words } => format!("ground in {words}"),
            GoalKind::Gaps { count } => format!("{count} largest gaps in the street"),
            GoalKind::Targets { targets } => {
                let names: Vec<String> = targets.iter().map(target_words).collect();
                names.join(", ")
            }
        }
    }

    fn total(&self) -> f32 {
        match &self.kind {
            GoalKind::Targets { targets } => targets.len() as f32,
            _ => self.total_m2,
        }
    }
}

fn target_words(t: &Target) -> String {
    if t.name.is_empty() {
        format!("target {}", t.id)
    } else {
        format!("{} (target {})", t.name, t.id)
    }
}

fn target_centre(t: &Target) -> Vec2 {
    match &t.at {
        TargetPlace::Ground { points } => {
            let sum: Vec2 = points.iter().map(|p| Vec2::from_array(*p)).sum();
            sum / points.len().max(1) as f32
        }
        TargetPlace::Wall { point, normal } => {
            Vec2::new(point[0], point[2]) + Vec2::from_array(*normal).normalize_or_zero() * 0.5
        }
    }
}

/// The targets `refs` names (every target when it is empty or absent).
fn resolve_targets(
    scene: &SceneFile,
    refs: Option<&[TargetRef]>,
) -> Result<Vec<Target>, ToolError> {
    if scene.targets.is_empty() {
        return Err(ToolError::invalid(
            "the scene has no targets: add one with add_target",
        ));
    }
    let Some(refs) = refs.filter(|r| !r.is_empty()) else {
        return Ok(scene.targets.clone());
    };
    let known = || {
        scene
            .targets
            .iter()
            .map(|t| format!("{} = {:?}", t.id, t.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out: Vec<Target> = Vec::new();
    for r in refs {
        let found = match r {
            TargetRef::Id(id) => scene.targets.iter().find(|t| t.id == *id),
            TargetRef::Name(name) => {
                let name = name.trim();
                let by_id = name
                    .parse::<u32>()
                    .ok()
                    .and_then(|id| scene.targets.iter().find(|t| t.id == id));
                by_id.or_else(|| {
                    scene
                        .targets
                        .iter()
                        .find(|t| t.name.trim().eq_ignore_ascii_case(name))
                })
            }
        };
        let t = found.ok_or_else(|| {
            let what = match r {
                TargetRef::Id(id) => id.to_string(),
                TargetRef::Name(n) => format!("{n:?}"),
            };
            ToolError::invalid(format!(
                "no target {what} in the scene; its targets are (id = name): {}",
                known()
            ))
        })?;
        if !out.iter().any(|o| o.id == t.id) {
            out.push(t.clone());
        }
    }
    Ok(out)
}

/// The street the figures count: the street mask, inside the scene's area
/// for the figures if it has one.
fn street_cells(scene: &SceneFile, layers: &StaticLayers) -> Vec<bool> {
    let region = scene
        .figures_area()
        .filter(|a| a.is_active())
        .map(|a| RegionMask::new(layers, &a.points));
    layers
        .street_mask()
        .iter()
        .enumerate()
        .map(|(i, &on)| on && region.as_ref().is_none_or(|r| r.ground[i]))
        .collect()
}

/// What the caller asked for, before it is resolved against the scene.
#[derive(Debug, Clone, Default)]
pub struct GoalSpec {
    pub goal: GoalArg,
    pub targets: Option<Vec<TargetRef>>,
    pub area_id: Option<u32>,
}

fn resolve_goal(
    spec: &GoalSpec,
    scene: &SceneFile,
    layers: &StaticLayers,
    params: &CoverageParams,
    cams: &[FootageCamera],
) -> Result<Goal, ToolError> {
    let grid = layers.grid();
    let cell_m2 = grid.cell_area_m2();
    let mut probes = Vec::new();
    let mut focus = None;
    let (kind, cells) = match spec.goal {
        GoalArg::Street => (GoalKind::Street, street_cells(scene, layers)),
        GoalArg::Area => {
            let active: Vec<_> = scene.areas.iter().filter(|a| a.is_active()).collect();
            let area = match spec.area_id {
                Some(id) => scene
                    .areas
                    .iter()
                    .find(|a| a.id == id)
                    .filter(|a| a.is_active())
                    .ok_or_else(|| {
                        ToolError::invalid(format!(
                            "no enabled area with id {id} and a usable outline in the scene"
                        ))
                    })?,
                None => match active.as_slice() {
                    [one] => *one,
                    [] => {
                        return Err(ToolError::invalid(
                            "the scene has no enabled labelled area: draw one with label_area",
                        ));
                    }
                    many => {
                        let ids: Vec<String> = many
                            .iter()
                            .map(|a| match &a.name {
                                Some(n) => format!("{} ({n})", a.id),
                                None => a.id.to_string(),
                            })
                            .collect();
                        return Err(ToolError::invalid(format!(
                            "say which area with area_id: {}",
                            ids.join(", ")
                        )));
                    }
                },
            };
            let mask = RegionMask::new(layers, &area.points);
            let buildings = layers.building_mask();
            let cells: Vec<bool> = mask
                .ground
                .iter()
                .zip(buildings)
                .map(|(&inside, &built)| inside && !built)
                .collect();
            let sum: Vec2 = area.points.iter().map(|p| Vec2::from_array(*p)).sum();
            focus = Some(sum / area.points.len().max(1) as f32);
            let words = match &area.name {
                Some(n) => format!("area {} ({n})", area.id),
                None => format!("area {}", area.id),
            };
            (GoalKind::Area { words }, cells)
        }
        GoalArg::Gaps => {
            let region = scene
                .figures_area()
                .filter(|a| a.is_active())
                .map(|a| RegionMask::new(layers, &a.points));
            let gaps = street_gaps(layers, params, cams, region.as_ref());
            let big: Vec<_> = gaps
                .iter()
                .filter(|g| g.area_m2 >= 1.0)
                .take(GOAL_GAPS)
                .collect();
            let mut cells = vec![false; grid.len()];
            for g in &big {
                for &i in &g.cells {
                    cells[i as usize] = true;
                }
            }
            focus = big.first().map(|g| Vec2::from_array(g.inside));
            (GoalKind::Gaps { count: big.len() }, cells)
        }
        GoalArg::Targets => {
            let targets = resolve_targets(scene, spec.targets.as_deref())?;
            probes = targets
                .iter()
                .map(|t| Probe::target(layers, t, params.subject_height_m))
                .collect();
            let sum: Vec2 = targets.iter().map(target_centre).sum();
            focus = Some(sum / targets.len().max(1) as f32);
            (GoalKind::Targets { targets }, street_cells(scene, layers))
        }
    };
    let total_m2 = cells.iter().filter(|&&c| c).count() as f32 * cell_m2;
    // Bounds: of the targets for the targets goal, else of the goal's cells.
    let mut lo = Vec2::splat(f32::INFINITY);
    let mut hi = Vec2::splat(f32::NEG_INFINITY);
    match &kind {
        GoalKind::Targets { targets } => {
            for t in targets {
                let c = target_centre(t);
                lo = lo.min(c);
                hi = hi.max(c);
            }
        }
        _ => {
            for (i, _) in cells.iter().enumerate().filter(|(_, c)| **c) {
                let c = grid.center(i);
                lo = lo.min(c);
                hi = hi.max(c);
            }
        }
    }
    let bounds = (lo.x <= hi.x).then_some((lo, hi));
    Ok(Goal {
        kind,
        cells,
        total_m2,
        probes,
        focus,
        bounds,
    })
}

// ---------------------------------------------------------------------------
// The search
// ---------------------------------------------------------------------------

/// The goal's cells left to cover, with their subject points.
struct Field {
    /// Grid indices.
    cells: Vec<u32>,
    /// Each one's subject point.
    pts: Vec<Vec3>,
}

impl Field {
    /// The goal's cells that `seen` leaves unseen.
    fn new(layers: &StaticLayers, goal: &Goal, seen: &[bool], h: f32) -> Self {
        let cells: Vec<u32> = goal
            .cells
            .iter()
            .zip(seen)
            .enumerate()
            .filter(|(_, (g, s))| **g && !**s)
            .map(|(i, _)| i as u32)
            .collect();
        let pts = cells
            .iter()
            .map(|&i| layers.ground_subject(i as usize, h))
            .collect();
        Self { cells, pts }
    }

    /// The cells (as positions in the field) with a clear line from `eye`
    /// within `range`, only those in front of `front` (a wall's normal) when
    /// given. Cells behind the wall a camera is on are behind its building.
    fn clear_from(
        &self,
        layers: &StaticLayers,
        eye: Vec3,
        range: f32,
        front: Option<Vec2>,
    ) -> Vec<u32> {
        let r2 = range * range;
        let mut out = Vec::new();
        for (k, p) in self.pts.iter().enumerate() {
            let d = *p - eye;
            if d.length_squared() > r2 {
                continue;
            }
            if front.is_some_and(|n| d.x * n.x + d.z * n.y < 0.0) {
                continue;
            }
            if !layers.segment_blocked(eye, *p) {
                out.push(k as u32);
            }
        }
        out
    }
}

/// How good an aim is: the primary count (targets for the targets goal,
/// else cells), then cells as the tie-break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
struct Score {
    primary: u32,
    cells: u32,
}

/// One camera, at one eye, being scored.
struct Shot<'a> {
    eye: Vec3,
    roll: f32,
    model: &'a CameraModel,
    /// Field positions with a clear line from the eye.
    clear: &'a [u32],
}

/// What is already covered: field cells and targets.
struct Covered {
    cells: Vec<bool>,
    targets: Vec<bool>,
}

struct Search<'a> {
    layers: &'a StaticLayers,
    params: CoverageParams,
    goal: &'a Goal,
    field: Field,
}

impl Search<'_> {
    fn range(&self, m: &CameraModel) -> f32 {
        m.effective_range_m(self.params.pixels_per_metre(), self.params.lighting)
    }

    fn cone(&self, shot: &Shot, aim: Aim) -> ViewCone {
        let m = shot.model;
        ViewCone::from_angles(
            shot.eye,
            aim.yaw,
            aim.pitch,
            shot.roll,
            m.hfov_clamped(),
            m.vfov_clamped(),
            self.range(m),
        )
    }

    fn score(&self, shot: &Shot, aim: Aim, covered: &Covered) -> Score {
        let cone = self.cone(shot, aim);
        let cells = shot
            .clear
            .iter()
            .filter(|&&k| !covered.cells[k as usize] && cone.contains(self.field.pts[k as usize]))
            .count() as u32;
        let targets = if self.goal.is_targets() {
            self.goal
                .probes
                .iter()
                .zip(&covered.targets)
                .filter(|(p, done)| {
                    !**done
                        && Reach::of(self.layers, &cone, p).verdict(cone.range)
                            == Verdict::NotRuledOut
                })
                .count() as u32
        } else {
            0
        };
        if self.goal.is_targets() {
            Score {
                primary: targets,
                cells,
            }
        } else {
            Score {
                primary: cells,
                cells,
            }
        }
    }

    fn cover(&self, shot: &Shot, aim: Aim, covered: &mut Covered) {
        let cone = self.cone(shot, aim);
        for &k in shot.clear {
            if cone.contains(self.field.pts[k as usize]) {
                covered.cells[k as usize] = true;
            }
        }
        for (p, done) in self.goal.probes.iter().zip(&mut covered.targets) {
            if Reach::of(self.layers, &cone, p).verdict(cone.range) == Verdict::NotRuledOut {
                *done = true;
            }
        }
    }

    /// The best of `aims` (the first on a tie), with its score.
    fn best_of(
        &self,
        shot: &Shot,
        aims: impl IntoIterator<Item = Aim>,
        covered: &Covered,
    ) -> Option<(Aim, Score)> {
        let mut best: Option<(Aim, Score)> = None;
        for aim in aims {
            let s = self.score(shot, aim, covered);
            if best.is_none_or(|(_, b)| s > b) {
                best = Some((aim, s));
            }
        }
        best
    }

    /// A finer search round `start`: yaw within ±`yaw_span` and pitch
    /// within ±`pitch_span` in `steps` steps each way, repeated while it
    /// improves (at most three times).
    fn refine(
        &self,
        shot: &Shot,
        start: (Aim, Score),
        covered: &Covered,
        yaw_step: f32,
        pitch_step: f32,
        steps: i32,
    ) -> (Aim, Score) {
        let mut best = start;
        for _ in 0..3 {
            let centre = best.0;
            let aims = (-steps..=steps).flat_map(|i| {
                (-steps..=steps).map(move |j| {
                    Aim::new(
                        centre.yaw + i as f32 * yaw_step,
                        centre.pitch + j as f32 * pitch_step,
                    )
                })
            });
            match self.best_of(shot, aims, covered) {
                Some(b) if b.1 > best.1 => best = b,
                _ => break,
            }
        }
        best
    }
}

/// The goal's measure for a whole set of cameras, computed afresh: m² of
/// the goal's cells some camera sees, or targets some camera does not rule
/// out (with the street m² as the second figure).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Measure {
    m2: f32,
    targets: usize,
}

impl Measure {
    fn value(&self, goal: &Goal) -> f32 {
        if goal.is_targets() {
            self.targets as f32
        } else {
            self.m2
        }
    }
}

/// The goal's measure, given which ground cells some camera sees and which
/// targets some camera does not rule out.
fn tally(layers: &StaticLayers, goal: &Goal, seen: &[bool], open: &[bool]) -> Measure {
    let n = goal
        .cells
        .iter()
        .zip(seen)
        .filter(|(g, s)| **g && **s)
        .count();
    Measure {
        m2: n as f32 * layers.grid().cell_area_m2(),
        targets: open.iter().filter(|&&o| o).count(),
    }
}

/// The ground cells one camera sees at `params`' level and lighting, as
/// `seen_ground` counts them.
fn ground_cells(
    layers: &StaticLayers,
    params: &CoverageParams,
    c: &CameraPlacement,
    m: &CameraModel,
) -> Vec<u32> {
    let range = m.effective_range_m(params.pixels_per_metre(), params.lighting);
    layers
        .visibility(&ViewCone::new(c, m, range), params.subject_height_m)
        .ground
}

fn mark(seen: &mut [bool], cells: &[u32]) {
    for &i in cells {
        seen[i as usize] = true;
    }
}

/// For each of the goal's targets, whether one of `cams` does not rule it
/// out at `params`' level and lighting.
fn targets_open(
    layers: &StaticLayers,
    params: &CoverageParams,
    goal: &Goal,
    cams: &[FootageCamera],
) -> Vec<bool> {
    goal.probes
        .iter()
        .map(|p| {
            cams.iter().any(|c| {
                verdict_of(layers, params, c.placement, c.model, p, params.lighting)
                    == Verdict::NotRuledOut
            })
        })
        .collect()
}

/// `open` with what camera `c` does not rule out added.
fn open_with(
    layers: &StaticLayers,
    params: &CoverageParams,
    goal: &Goal,
    open: &[bool],
    c: &CameraPlacement,
    m: &CameraModel,
) -> Vec<bool> {
    goal.probes
        .iter()
        .zip(open)
        .map(|(p, &o)| {
            o || verdict_of(layers, params, c, m, p, params.lighting) == Verdict::NotRuledOut
        })
        .collect()
}

/// One camera's verdict on a place at `params`' level, under `lighting`.
fn verdict_of(
    layers: &StaticLayers,
    params: &CoverageParams,
    c: &CameraPlacement,
    m: &CameraModel,
    probe: &Probe,
    lighting: Lighting,
) -> Verdict {
    let range = m.effective_range_m(params.pixels_per_metre(), lighting);
    let cone = ViewCone::new(c, m, range);
    Reach::of(layers, &cone, probe).verdict(range)
}

/// Everything a goal tool needs from its arguments, parsed and checked.
struct Prepared {
    scene: SceneFile,
    bundle: Arc<AreaBundle>,
    layers: Arc<StaticLayers>,
    params: CoverageParams,
}

fn prepare(scene: &Value, level: Option<Dori>, night: Option<bool>) -> Result<Prepared, ToolError> {
    let s = edit::parse(scene)?;
    let bundle = bundle_for(&s)?;
    let layers = layers_for(&s)?;
    let mut params = s.params;
    if let Some(d) = level {
        params.dori = d.into();
    }
    if let Some(n) = night {
        params.lighting = if n { Lighting::Night } else { Lighting::Day };
    }
    Ok(Prepared {
        scene: s,
        bundle,
        layers,
        params,
    })
}

// ---------------------------------------------------------------------------
// suggest_placement
// ---------------------------------------------------------------------------

/// The models to choose from: those named, or one of each kind among the
/// app's example models (a doorbell, a floodlight and a bullet).
fn choose_models<'a>(
    cat: &'a Catalog,
    scene: &'a SceneFile,
    ids: Option<&[String]>,
) -> Result<Vec<&'a CameraModel>, ToolError> {
    let mut out: Vec<&CameraModel> = Vec::new();
    match ids.filter(|ids| !ids.is_empty()) {
        Some(ids) => {
            for id in ids {
                let m =
                    model(cat, Some(scene), id).ok_or_else(|| ToolError::NoModel(id.clone()))?;
                if !out.iter().any(|o| o.id == m.id) {
                    out.push(m);
                }
            }
            if out.len() > MAX_MODELS {
                return Err(ToolError::invalid(format!(
                    "give at most {MAX_MODELS} models to choose from"
                )));
            }
        }
        None => {
            for id in EXAMPLE_MODELS {
                if let Some(m) = cat.get(id)
                    && !out.iter().any(|o| o.kind == m.kind)
                {
                    out.push(m);
                }
                if out.len() == 3 {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// One suggested camera.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SuggestionOut {
    /// 1 for the first picked.
    pub rank: usize,
    pub model_id: String,
    /// The model's name.
    pub model: String,
    /// Where it goes, `[x, z]` metres (x east, z south of the origin): in
    /// front of a house front.
    pub at: [f32; 2],
    /// Mount height above the ground, metres.
    pub height_m: f32,
    pub aim: AimOut,
    /// The building whose wall it goes on, by index (never an address).
    pub building: u32,
    /// What it adds: m² of the goal, or targets newly not ruled out.
    pub gain: f32,
    /// The goal's total with it and the ones before it.
    pub total_after: f32,
    /// Its id in the returned scene, when applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_id: Option<u32>,
    /// Why, in a line.
    pub reason: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SuggestOut {
    /// The result in a line or two; pass it on.
    pub summary: String,
    /// What was searched for.
    pub goal: String,
    pub settings: Settings,
    /// "m²" or "targets".
    pub unit: String,
    /// The goal's total with the cameras already in the scene.
    pub before: f32,
    /// ... and with the suggestions added.
    pub after: f32,
    /// The most there is: m² of the goal's ground, or the number of targets.
    pub out_of: f32,
    /// Best first; empty when nothing found adds anything.
    pub suggestions: Vec<SuggestionOut>,
    /// What was searched.
    pub searched: String,
    /// The new cameras' ids, when applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_ids: Option<Vec<u32>>,
    /// The edited scene, when applied: pass it to the next scene tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<JsonObject>")]
    pub scene: Option<Value>,
    /// How to read the suggestions; pass it on.
    pub notes: String,
    #[serde(flatten)]
    pub sources: Sources,
}

/// A candidate mount: a house front at one height.
struct Mount {
    wall: WallMount,
    eye: Vec3,
    height: f32,
    /// The models that go at this height, by index in the models list.
    models: Vec<usize>,
    /// Field positions with a clear line from the eye.
    clear: Vec<u32>,
}

/// The aims tried from a house front: across the wall's outward bearing,
/// with the model's typical tilt and 10° either side, and at the goal's
/// focus when it lies in front of the wall.
fn wall_aims(wall: &WallMount, eye: Vec3, m: &CameraModel, focus: Option<Vec3>) -> Vec<Aim> {
    let out = bearing(wall.normal);
    let p0 = m.kind.default_pitch_deg();
    let pitches = [p0 - 10.0, p0, (p0 + 10.0).min(0.0)];
    let mut aims = Vec::new();
    for k in -5..=5 {
        let yaw = out + k as f32 * 15.0;
        for p in pitches {
            let a = Aim::new(yaw.round(), p);
            if !aims.contains(&a) {
                aims.push(a);
            }
        }
    }
    let street_yaw = wall.street_yaw_deg().round();
    aims.push(Aim::new(street_yaw, p0));
    if let Some(a) = focus.and_then(|f| Aim::at(eye, f))
        && turn(a.yaw, out).abs() < 85.0
    {
        aims.push(a);
    }
    aims
}

/// Suggests up to `count` cameras for the goal, best first.
#[allow(clippy::too_many_arguments)]
pub fn suggest(
    scene: &Value,
    spec: &GoalSpec,
    count: Option<usize>,
    models: Option<&[String]>,
    night: Option<bool>,
    level: Option<Dori>,
    apply: bool,
) -> Result<SuggestOut, ToolError> {
    let cat = catalog()?;
    let p = prepare(scene, level, night)?;
    let count = count.unwrap_or(1);
    if !(1..=MAX_SUGGESTIONS).contains(&count) {
        return Err(ToolError::invalid(format!(
            "count must be 1 to {MAX_SUGGESTIONS}"
        )));
    }
    let models = choose_models(cat, &p.scene, models)?;
    let found = search_placements(&p, spec, &models, count)?;
    let mut out = found.out;
    if apply {
        let mut s = p.scene.clone();
        if s.cameras.len() + found.placements.len() > MAX_CAMERAS {
            return Err(ToolError::invalid(format!(
                "a scene holds at most {MAX_CAMERAS} cameras"
            )));
        }
        let mut ids = Vec::new();
        for (mut c, sug) in found.placements.into_iter().zip(&mut out.suggestions) {
            c.id = fresh_id(s.cameras.iter().map(|c| c.id));
            c.label = format!("Camera {}", c.id);
            sug.camera_id = Some(c.id);
            ids.push(c.id);
            s.cameras.push(c);
        }
        let (_, value) = edit::finish(&s)?;
        if !ids.is_empty() {
            out.summary.push_str(&format!(
                " Added as camera(s) {}.",
                ids.iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.camera_ids = Some(ids);
        out.scene = Some(value);
    }
    Ok(out)
}

struct Placements {
    out: SuggestOut,
    /// The suggested cameras, ids left at 0.
    placements: Vec<CameraPlacement>,
}

fn search_placements(
    p: &Prepared,
    spec: &GoalSpec,
    models: &[&CameraModel],
    count: usize,
) -> Result<Placements, ToolError> {
    let cat = catalog()?;
    let layers = &*p.layers;
    let params = p.params;
    let h = params.subject_height_m;
    let cams = p.scene.footage_cameras(cat);
    let goal = resolve_goal(spec, &p.scene, layers, &params, &cams)?;
    let seen = seen_ground(layers, &params, &cams);
    let search = Search {
        layers,
        params,
        goal: &goal,
        field: Field::new(layers, &goal, &seen, h),
    };
    let open = targets_open(layers, &params, &goal, &cams);
    let before = tally(layers, &goal, &seen, &open);
    let mut covered = Covered {
        cells: vec![false; search.field.cells.len()],
        targets: open.clone(),
    };
    let terrain = layers.terrain();
    let focus = goal.focus.map(|f| layers.subject_at(f, h));
    let max_range = models.iter().map(|m| search.range(m)).fold(0.0, f32::max);

    // House fronts clear of the cameras already there, those that could
    // reach the goal at all, nearest it first; at most MAX_WALLS.
    let taken: Vec<Vec2> = p
        .scene
        .cameras
        .iter()
        .map(|c| Vec2::new(c.position[0], c.position[2]))
        .collect();
    let to_goal = |w: &WallMount| match goal.bounds {
        Some((lo, hi)) => (w.at.clamp(lo, hi) - w.at).length(),
        None => f32::INFINITY,
    };
    let mut walls: Vec<(f32, WallMount)> = wall_mounts(&p.bundle)
        .into_iter()
        .filter(WallMount::faces_street)
        .filter(|w| taken.iter().all(|t| t.distance(w.at) > APART_M))
        .map(|w| (to_goal(&w), w))
        .filter(|(d, _)| *d <= max_range)
        .collect();
    walls.sort_by(|(da, a), (db, b)| {
        da.total_cmp(db)
            .then(a.at.x.total_cmp(&b.at.x))
            .then(a.at.y.total_cmp(&b.at.y))
    });
    walls.truncate(MAX_WALLS);
    let wall_count = walls.len();

    // One mount per house front and height; lines of sight once per mount.
    let mut mounts: Vec<Mount> = Vec::new();
    for (_, w) in &walls {
        let mut heights: Vec<(f32, Vec<usize>)> = Vec::new();
        for (k, m) in models.iter().enumerate() {
            let hm = m.kind.default_mount_height_m();
            match heights.iter_mut().find(|(x, _)| *x == hm) {
                Some((_, v)) => v.push(k),
                None => heights.push((hm, vec![k])),
            }
        }
        for (hm, ms) in heights {
            // To the centimetre, so the suggestion and the scene agree.
            let raw = w.eye(terrain, hm);
            let xz = Vec2::new(r2(raw.x), r2(raw.z));
            let eye = Vec3::new(xz.x, terrain.height_at(xz) + hm, xz.y);
            if !w.fits_under_roof(eye) {
                continue;
            }
            let range = ms
                .iter()
                .map(|&k| search.range(models[k]))
                .fold(0.0, f32::max);
            let clear = search.field.clear_from(layers, eye, range, Some(w.normal));
            mounts.push(Mount {
                wall: *w,
                eye,
                height: hm,
                models: ms,
                clear,
            });
        }
    }

    // Greedy: the best mount, model and aim; then the next, given it.
    let aims: Vec<Vec<Vec<Aim>>> = mounts
        .iter()
        .map(|mt| {
            mt.models
                .iter()
                .map(|&k| wall_aims(&mt.wall, mt.eye, models[k], focus))
                .collect()
        })
        .collect();
    let aim_count: usize = aims.iter().flatten().map(Vec::len).sum();
    let mut picked: Vec<(usize, usize, Aim)> = Vec::new();
    for _ in 0..count {
        let mut best: Option<(usize, usize, Aim, Score)> = None;
        for (i, mt) in mounts.iter().enumerate() {
            if picked
                .iter()
                .any(|(j, _, _)| mounts[*j].eye.distance(mt.eye) <= APART_M)
            {
                continue;
            }
            for (slot, &k) in mt.models.iter().enumerate() {
                let shot = Shot {
                    eye: mt.eye,
                    roll: 0.0,
                    model: models[k],
                    clear: &mt.clear,
                };
                if let Some((aim, s)) =
                    search.best_of(&shot, aims[i][slot].iter().copied(), &covered)
                    && best.is_none_or(|b| s > b.3)
                {
                    best = Some((i, k, aim, s));
                }
            }
        }
        let Some((i, k, aim, s)) = best else { break };
        let shot = Shot {
            eye: mounts[i].eye,
            roll: 0.0,
            model: models[k],
            clear: &mounts[i].clear,
        };
        let (aim, s) = search.refine(&shot, (aim, s), &covered, 5.0, 5.0, 1);
        let cell_m2 = layers.grid().cell_area_m2();
        let worth = if goal.is_targets() {
            s.primary > 0
        } else {
            s.primary as f32 * cell_m2 >= MIN_GAIN_M2
        };
        if !worth {
            break;
        }
        search.cover(&shot, aim, &mut covered);
        picked.push((i, k, aim));
    }

    // The figures afresh, adding the picks one at a time.
    let mut placements: Vec<CameraPlacement> = Vec::new();
    let mut suggestions = Vec::new();
    let mut last = before;
    let mut seen_now = seen;
    let mut open_now = open;
    for (rank, &(i, k, aim)) in picked.iter().enumerate() {
        let mt = &mounts[i];
        let m = models[k];
        placements.push(CameraPlacement {
            id: 0,
            label: String::new(),
            model_id: m.id.clone(),
            position: mt.eye.to_array(),
            yaw_deg: aim.yaw,
            pitch_deg: aim.pitch,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        });
        // Adding one camera at a time: its cells and the targets it opens.
        let Some(new) = placements.last() else {
            break;
        };
        mark(&mut seen_now, &ground_cells(layers, &params, new, m));
        open_now = open_with(layers, &params, &goal, &open_now, new, m);
        let now = tally(layers, &goal, &seen_now, &open_now);
        let gain = now.value(&goal) - last.value(&goal);
        let street_gain = now.m2 - last.m2;
        let reason = suggestion_reason(&goal, &params, gain, now, street_gain, &mt.wall, aim);
        suggestions.push(SuggestionOut {
            rank: rank + 1,
            model_id: m.id.clone(),
            model: m.display_name(),
            at: [mt.eye.x, mt.eye.z],
            height_m: mt.height,
            aim: AimOut::of(aim.yaw, aim.pitch),
            building: u32::try_from(mt.wall.building).unwrap_or(u32::MAX),
            gain: r1(gain),
            total_after: r1(now.value(&goal)),
            camera_id: None,
            reason,
        });
        last = now;
    }
    let after = last;
    let summary = suggest_summary(&goal, &params, before, after, &suggestions);
    let searched = format!(
        "{} mount point(s) on {wall_count} house front(s) facing a street, {} model(s), {aim_count} aim(s) in all",
        mounts.len(),
        models.len()
    );
    Ok(Placements {
        out: SuggestOut {
            summary,
            goal: goal.words(),
            settings: settings_of(&params),
            unit: goal.unit().to_owned(),
            before: r1(before.value(&goal)),
            after: r1(after.value(&goal)),
            out_of: r1(goal.total()),
            suggestions,
            searched,
            camera_ids: None,
            scene: None,
            notes: GOAL_NOTES.to_owned(),
            sources: Sources::of(&p.bundle),
        },
        placements,
    })
}

/// "3 more target(s) not ruled out at Detect by day", or "120 m² more of
/// the street, driveways and footpaths as mapped not ruled out at ...".
fn gain_words(goal: &Goal, params: &CoverageParams, gain: f32) -> String {
    let at = at_words(params);
    if goal.is_targets() {
        format!("{gain:.0} more target(s) not ruled out {at}")
    } else {
        format!(
            "{gain:.0} m² more of the {} not ruled out {at}",
            goal.words()
        )
    }
}

fn suggestion_reason(
    goal: &Goal,
    params: &CoverageParams,
    gain: f32,
    now: Measure,
    street_gain: f32,
    wall: &WallMount,
    aim: Aim,
) -> String {
    let what = match &goal.kind {
        GoalKind::Targets { .. } => format!(
            "{} (now {} of {}), and {street_gain:.0} m² more street",
            gain_words(goal, params, gain),
            now.targets,
            goal.total()
        ),
        _ => format!(
            "{} (now {:.0} of {:.0} m²)",
            gain_words(goal, params, gain),
            now.m2,
            goal.total_m2
        ),
    };
    format!(
        "{what}; on a house front of building {}, facing {}",
        wall.building,
        compass(aim.yaw)
    )
}

fn suggest_summary(
    goal: &Goal,
    params: &CoverageParams,
    before: Measure,
    after: Measure,
    s: &[SuggestionOut],
) -> String {
    let at = at_words(params);
    if s.is_empty() {
        return format!(
            "No house front facing a street adds anything for the goal ({}) {at}; try another goal, level or model, or place a camera by hand.",
            goal.words()
        );
    }
    let first = &s[0];
    let lead = format!(
        "Best: {} at [{:.0}, {:.0}], {:.1} m up, facing {} ({:.0}°), tilted {:.0}°.",
        first.model,
        first.at[0],
        first.at[1],
        first.height_m,
        first.aim.facing,
        first.aim.yaw_deg,
        first.aim.pitch_deg
    );
    let total = if goal.is_targets() {
        format!(
            "Targets not ruled out {at}: {} of {} before, {} with {} suggestion(s).",
            before.targets,
            goal.total(),
            after.targets,
            s.len()
        )
    } else {
        format!(
            "{} not ruled out {at}: {:.0} of {:.0} m² before, {:.0} m² with {} suggestion(s).",
            capitalise(&goal.words()),
            before.m2,
            goal.total_m2,
            after.m2,
            s.len()
        )
    };
    format!("{lead} {total} Estimates; check on site.")
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// optimise_aim
// ---------------------------------------------------------------------------

/// A camera's pose and the goal's total with it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PoseOut {
    /// `[x, z]` metres (x east, z south of the origin).
    pub at: [f32; 2],
    /// Mount height above the ground, metres.
    pub height_m: f32,
    pub aim: AimOut,
    /// The goal's total for the whole scene with the camera so: m² or
    /// targets not ruled out.
    pub total: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OptimiseOut {
    /// The result in a line; pass it on.
    pub summary: String,
    pub camera_id: u32,
    pub goal: String,
    pub settings: Settings,
    /// "m²" or "targets".
    pub unit: String,
    pub before: PoseOut,
    pub after: PoseOut,
    /// `after.total - before.total`.
    pub gain: f32,
    /// How far the camera moved along its wall, metres.
    pub moved_m: f32,
    /// Whether the search found anything better than the current pose.
    pub changed: bool,
    /// What was searched.
    pub searched: String,
    /// The edited scene, when applied: pass it to the next scene tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<JsonObject>")]
    pub scene: Option<Value>,
    /// How to read the result; pass it on.
    pub notes: String,
    #[serde(flatten)]
    pub sources: Sources,
}

/// The wall a camera at `xz` is mounted on (within [`ON_WALL_M`] of its
/// line, and alongside it), if any.
fn wall_of(bundle: &AreaBundle, xz: Vec2) -> Option<WallMount> {
    wall_mounts(bundle)
        .into_iter()
        .filter_map(|w| {
            let along_dir = Vec2::new(-w.normal.y, w.normal.x);
            let d = xz - w.at;
            let off = d.dot(w.normal);
            let along = d.dot(along_dir).abs();
            ((0.0..=ON_WALL_M).contains(&off) && along <= w.length_m / 2.0).then_some((off, w))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, w)| w)
}

/// The coarse aims from the current one: yaw all round in 10° steps (the
/// nearest first), pitch from -50° to 10° in 5° steps.
fn coarse_aims(centre: Aim) -> Vec<Aim> {
    let mut aims = vec![centre];
    // 0, 10, -10, 20, -20, ..., 180: ties go to the smallest turn.
    let turns = std::iter::once(0)
        .chain((1..18).flat_map(|i| [i, -i]))
        .chain([18]);
    for i in turns {
        for j in 0..=12 {
            aims.push(Aim::new(
                (centre.yaw + i as f32 * 10.0).round(),
                -50.0 + j as f32 * 5.0,
            ));
        }
    }
    aims
}

/// Searches a better aim (and with `allow_move` a better spot along the
/// same wall) for one camera.
pub fn optimise(
    scene: &Value,
    camera_id: u32,
    spec: &GoalSpec,
    allow_move: bool,
    night: Option<bool>,
    level: Option<Dori>,
    apply: bool,
) -> Result<OptimiseOut, ToolError> {
    let cat = catalog()?;
    let p = prepare(scene, level, night)?;
    let layers = &*p.layers;
    let params = p.params;
    let h = params.subject_height_m;
    let cam = p
        .scene
        .cameras
        .iter()
        .find(|c| c.id == camera_id)
        .ok_or(ToolError::NoCamera(camera_id))?;
    if !cam.enabled {
        return Err(ToolError::invalid(format!(
            "camera {camera_id} is disabled: enable it with update_camera first"
        )));
    }
    let m = model(cat, Some(&p.scene), &cam.model_id)
        .ok_or_else(|| ToolError::NoModel(cam.model_id.clone()))?;
    let all = p.scene.footage_cameras(cat);
    let others: Vec<FootageCamera> = all
        .iter()
        .filter(|c| c.placement.id != camera_id)
        .copied()
        .collect();
    // The gaps the other cameras leave, so the camera's own share counts.
    let goal = resolve_goal(spec, &p.scene, layers, &params, &others)?;
    let seen = seen_ground(layers, &params, &others);
    let search = Search {
        layers,
        params,
        goal: &goal,
        field: Field::new(layers, &goal, &seen, h),
    };
    let open = targets_open(layers, &params, &goal, &others);
    let covered = Covered {
        cells: vec![false; search.field.cells.len()],
        targets: open.clone(),
    };
    let terrain = layers.terrain();
    let range = search.range(m);
    let current = Aim::new(cam.yaw_deg, cam.pitch_deg);
    let xz0 = Vec2::new(cam.position[0], cam.position[2]);
    let height = (cam.position[1] - terrain.height_at(xz0)).max(0.0);

    // At the camera's own spot: coarse, then fine.
    let clear0 = search.field.clear_from(layers, cam.eye(), range, None);
    let shot0 = Shot {
        eye: cam.eye(),
        roll: cam.roll_deg,
        model: m,
        clear: &clear0,
    };
    let now = search.score(&shot0, current, &covered);
    let coarse = coarse_aims(current);
    let mut aims_tried = coarse.len();
    let start = search
        .best_of(&shot0, coarse, &covered)
        .unwrap_or((current, now));
    let mut best = (
        xz0,
        cam.eye(),
        search.refine(&shot0, start, &covered, 2.0, 1.0, 2),
    );
    aims_tried += 25;

    // Along the same wall: the best aim so far and a coarse ring round it at
    // each spot, then fine at the best spot.
    let mut spots = 0;
    let mut wall_note = String::new();
    if allow_move {
        match wall_of(&p.bundle, xz0) {
            Some(w) => {
                let dir = Vec2::new(-w.normal.y, w.normal.x);
                let off = (xz0 - w.at).dot(w.normal);
                let along0 = (xz0 - w.at).dot(dir);
                let half = (w.length_m / 2.0 - 0.3).max(0.0);
                for d in MOVES_M {
                    let along = along0 + d;
                    if along.abs() > half {
                        continue;
                    }
                    let q = w.at + dir * along + w.normal * off;
                    let xz = Vec2::new(r2(q.x), r2(q.y));
                    let eye = Vec3::new(xz.x, terrain.height_at(xz) + height, xz.y);
                    if !w.fits_under_roof(eye) {
                        continue;
                    }
                    spots += 1;
                    let clear = search.field.clear_from(layers, eye, range, None);
                    let shot = Shot {
                        eye,
                        roll: cam.roll_deg,
                        model: m,
                        clear: &clear,
                    };
                    let c = best.2.0;
                    let ring = (-2..=2).flat_map(|i| {
                        (-1..=1).map(move |j| {
                            Aim::new(c.yaw + i as f32 * 10.0, c.pitch + j as f32 * 5.0)
                        })
                    });
                    aims_tried += 15;
                    if let Some(found) = search.best_of(&shot, ring, &covered) {
                        let found = search.refine(&shot, found, &covered, 2.0, 1.0, 1);
                        aims_tried += 9;
                        if found.1 > best.2.1 {
                            best = (xz, eye, found);
                        }
                    }
                }
            }
            None => {
                wall_note = " It is not on a building wall, so it was not moved.".to_owned();
            }
        }
    }

    let (xz1, eye1, (aim1, score1)) = best;
    let changed = score1 > now;
    let (xz1, eye1, aim1) = if changed {
        (xz1, eye1, aim1)
    } else {
        (xz0, cam.eye(), current)
    };
    let mut edited = p.scene.clone();
    if let Some(c) = edited.cameras.iter_mut().find(|c| c.id == camera_id) {
        c.position = eye1.to_array();
        c.yaw_deg = aim1.yaw;
        c.pitch_deg = aim1.pitch;
    }
    // The figures with the camera as it was and as it would be.
    let with = |c: &CameraPlacement| {
        let mut s = seen.clone();
        mark(&mut s, &ground_cells(layers, &params, c, m));
        tally(
            layers,
            &goal,
            &s,
            &open_with(layers, &params, &goal, &open, c, m),
        )
    };
    let before_m = with(cam);
    let after_m = match edited.cameras.iter().find(|c| c.id == camera_id) {
        Some(c) => with(c),
        None => before_m,
    };
    let gain = after_m.value(&goal) - before_m.value(&goal);
    let moved_m = xz1.distance(xz0);
    let pose = |xz: Vec2, aim: Aim, total: f32| PoseOut {
        at: [r2(xz.x), r2(xz.y)],
        height_m: r2(height),
        aim: AimOut::of(aim.yaw, aim.pitch),
        total: r1(total),
    };
    let at = at_words(&params);
    let unit = goal.unit();
    let summary = if changed {
        let mv = if moved_m > 0.0 {
            format!(", moved {moved_m:.1} m along its wall")
        } else {
            String::new()
        };
        format!(
            "Camera {camera_id}: facing {} ({:.0}°) tilted {:.0}°{mv} gives {} ({:.0} → {:.0} {unit}).{wall_note} Estimates; check on site.",
            compass(aim1.yaw),
            aim1.yaw,
            aim1.pitch,
            gain_words(&goal, &params, gain),
            before_m.value(&goal),
            after_m.value(&goal)
        )
    } else {
        format!(
            "Camera {camera_id}: no aim{} found that adds anything for the goal ({}) {at}; its current pose is kept ({:.0} {unit}).{wall_note}",
            if allow_move {
                " or spot along its wall"
            } else {
                ""
            },
            goal.words(),
            before_m.value(&goal)
        )
    };
    let scene_out = if apply {
        Some(edit::finish(&edited)?.1)
    } else {
        None
    };
    Ok(OptimiseOut {
        summary,
        camera_id,
        goal: goal.words(),
        settings: settings_of(&params),
        unit: unit.to_owned(),
        before: pose(xz0, current, before_m.value(&goal)),
        after: pose(xz1, aim1, after_m.value(&goal)),
        gain: r1(gain),
        moved_m: r2(moved_m),
        changed,
        searched: format!(
            "{aims_tried} aim(s) at {} spot(s) (yaw all round in 10° steps, tilt -50° to 10° in 5° steps, then finer)",
            spots + 1
        ),
        scene: scene_out,
        notes: GOAL_NOTES.to_owned(),
        sources: Sources::of(&p.bundle),
    })
}

// ---------------------------------------------------------------------------
// cover_target
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct CoverOut {
    /// The result in a line or two; pass it on.
    pub summary: String,
    pub target_id: u32,
    pub target_name: String,
    /// The scene's level, by day.
    pub settings: Settings,
    /// `already_not_ruled_out`, `re_aim`, `new_camera` or `none_found`.
    pub outcome: String,
    /// The camera that already does not rule it out, or to re-aim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_id: Option<u32>,
    /// For `re_aim`: the camera's aim now, and the new one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim_before: Option<AimOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim_after: Option<AimOut>,
    /// For `re_aim`: the change in street not ruled out for the whole
    /// scene at the level by day, m² (negative: the re-aim gives some up).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street_change_m2: Option<f32>,
    /// For `new_camera`: where it would go.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<SuggestionOut>,
    /// The verdict at the same level at night, with the outcome's camera.
    pub at_night: String,
    /// The new camera's id, when a new camera was applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_camera_id: Option<u32>,
    /// The edited scene, when applied: pass it to the next scene tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<JsonObject>")]
    pub scene: Option<Value>,
    /// How to read a verdict; pass it on.
    pub wording: String,
    pub notes: String,
    #[serde(flatten)]
    pub sources: Sources,
}

fn night_words(v: Verdict) -> String {
    format!("at night: {}", v.text())
}

/// Makes `target` not ruled out at the scene's level by day: says which
/// camera already does it, or re-aims one, or suggests a new one.
pub fn cover(scene: &Value, target: &TargetRef, apply: bool) -> Result<CoverOut, ToolError> {
    let cat = catalog()?;
    let p = prepare(scene, None, Some(false))?;
    let layers = &*p.layers;
    let params = p.params;
    let h = params.subject_height_m;
    let t = resolve_targets(&p.scene, Some(std::slice::from_ref(target)))?
        .into_iter()
        .next()
        .ok_or_else(|| ToolError::invalid("no such target"))?;
    let probe = Probe::target(layers, &t, h);
    let cams = p.scene.footage_cameras(cat);
    let name = target_words(&t);
    let at = at_words(&params);
    let base = |outcome: &str, summary: String, at_night: String| CoverOut {
        summary,
        target_id: t.id,
        target_name: t.name.clone(),
        settings: settings_of(&params),
        outcome: outcome.to_owned(),
        camera_id: None,
        aim_before: None,
        aim_after: None,
        street_change_m2: None,
        suggestion: None,
        at_night,
        new_camera_id: None,
        scene: None,
        wording: ONE_SIDED.to_owned(),
        notes: GOAL_NOTES.to_owned(),
        sources: Sources::of(&p.bundle),
    };

    // Already not ruled out?
    if let Some(c) = cams.iter().find(|c| {
        verdict_of(layers, &params, c.placement, c.model, &probe, Lighting::Day)
            == Verdict::NotRuledOut
    }) {
        let night = verdict_of(
            layers,
            &params,
            c.placement,
            c.model,
            &probe,
            Lighting::Night,
        );
        let id = c.placement.id;
        let mut out = base(
            "already_not_ruled_out",
            format!(
                "{} is already not ruled out {at} for camera {id}; {}. Nothing to change.",
                capitalise(&name),
                night_words(night)
            ),
            night_words(night),
        );
        out.camera_id = Some(id);
        return Ok(out);
    }

    // Re-aim an existing camera: aims round the one straight at the target
    // that leave it not ruled out, keeping the most street for the scene.
    let street_goal = Goal {
        kind: GoalKind::Street,
        cells: street_cells(&p.scene, layers),
        total_m2: 0.0,
        probes: Vec::new(),
        focus: None,
        bounds: None,
    };
    // Each camera's cells, once, and how many cameras see each cell, so that
    // what the others see is a subtraction.
    let vis: Vec<Vec<u32>> = cams
        .iter()
        .map(|c| ground_cells(layers, &params, c.placement, c.model))
        .collect();
    let mut seers = vec![0u16; layers.grid().len()];
    for v in &vis {
        for &i in v {
            seers[i as usize] = seers[i as usize].saturating_add(1);
        }
    }
    let seen_all: Vec<bool> = seers.iter().map(|&n| n > 0).collect();
    let street_now = tally(layers, &street_goal, &seen_all, &[]).m2;
    let centre = layers.subject_at(target_centre(&t), h);
    let mut best: Option<(f32, f32, u32, Aim)> = None; // (street after, -turn, id, aim)
    for (c, own) in cams.iter().zip(&vis) {
        let (pl, m) = (c.placement, c.model);
        let range = m.effective_range_m(params.pixels_per_metre(), Lighting::Day);
        if pl.eye().distance(centre) > range + 30.0 {
            continue;
        }
        let Some(straight) = Aim::at(pl.eye(), centre) else {
            continue;
        };
        let mut aims = Vec::new();
        for i in -3..=3 {
            for j in -2..=2 {
                aims.push(Aim::new(
                    straight.yaw + i as f32 * 10.0,
                    straight.pitch + j as f32 * 5.0,
                ));
            }
        }
        let ok: Vec<Aim> = aims
            .into_iter()
            .filter(|a| {
                let cone = ViewCone::from_angles(
                    pl.eye(),
                    a.yaw,
                    a.pitch,
                    pl.roll_deg,
                    m.hfov_clamped(),
                    m.vfov_clamped(),
                    range,
                );
                Reach::of(layers, &cone, &probe).verdict(range) == Verdict::NotRuledOut
            })
            .collect();
        if ok.is_empty() {
            continue;
        }
        // The aim keeping the most street, given the other cameras.
        let mut mine = vec![false; seers.len()];
        mark(&mut mine, own);
        let seen: Vec<bool> = seers
            .iter()
            .zip(&mine)
            .map(|(&n, &me)| n > u16::from(me))
            .collect();
        let search = Search {
            layers,
            params,
            goal: &street_goal,
            field: Field::new(layers, &street_goal, &seen, h),
        };
        let clear = search.field.clear_from(layers, pl.eye(), range, None);
        let shot = Shot {
            eye: pl.eye(),
            roll: pl.roll_deg,
            model: m,
            clear: &clear,
        };
        let covered = Covered {
            cells: vec![false; search.field.cells.len()],
            targets: Vec::new(),
        };
        let Some((aim, _)) = search.best_of(&shot, ok, &covered) else {
            continue;
        };
        let mut moved = pl.clone();
        moved.yaw_deg = aim.yaw;
        moved.pitch_deg = aim.pitch;
        let mut with = seen;
        mark(&mut with, &ground_cells(layers, &params, &moved, m));
        let street = tally(layers, &street_goal, &with, &[]).m2;
        let key = (street, -turn(aim.yaw, pl.yaw_deg).abs(), pl.id, aim);
        let better = match &best {
            None => true,
            Some(b) => key.0 > b.0 || (key.0 == b.0 && key.1 > b.1),
        };
        if better {
            best = Some(key);
        }
    }
    if let Some((street, _, id, aim)) = best
        && let Some(pl) = p.scene.cameras.iter().find(|c| c.id == id)
        && let Some(m) = model(cat, Some(&p.scene), &pl.model_id)
    {
        let mut moved = pl.clone();
        moved.yaw_deg = aim.yaw;
        moved.pitch_deg = aim.pitch;
        let night = verdict_of(layers, &params, &moved, m, &probe, Lighting::Night);
        let change = street - street_now;
        let mut out = base(
            "re_aim",
            format!(
                "Re-aim camera {id} to face {} ({:.0}°) tilted {:.0}° (now {:.0}°, {:.0}°): {name} is then not ruled out {at}; {}. Street not ruled out changes by {change:+.0} m². Estimates; check on site.",
                compass(aim.yaw),
                aim.yaw,
                aim.pitch,
                pl.yaw_deg,
                pl.pitch_deg,
                night_words(night)
            ),
            night_words(night),
        );
        out.camera_id = Some(id);
        out.aim_before = Some(AimOut::of(pl.yaw_deg, pl.pitch_deg));
        out.aim_after = Some(AimOut::of(aim.yaw, aim.pitch));
        out.street_change_m2 = Some(r1(change));
        if apply {
            let mut s = p.scene.clone();
            edit::aim_camera(&mut s, id, Some(aim.yaw), Some(aim.pitch), None)?;
            out.scene = Some(edit::finish(&s)?.1);
        }
        return Ok(out);
    }

    // A new camera, by the same search as suggest_placement.
    let spec = GoalSpec {
        goal: GoalArg::Targets,
        targets: Some(vec![TargetRef::Id(t.id)]),
        area_id: None,
    };
    let models = choose_models(cat, &p.scene, None)?;
    let found = search_placements(&p, &spec, &models, 1)?;
    let Some(mut sug) = found.out.suggestions.into_iter().next() else {
        return Ok(base(
            "none_found",
            format!(
                "No camera in the scene can be re-aimed so that {name} is not ruled out {at}, and no house front facing a street nearby gives a clear line to it. Try a different level, or place a camera by hand nearer the target."
            ),
            "at night: no camera found".to_owned(),
        ));
    };
    let Some(mut cam) = found.placements.into_iter().next() else {
        return Err(ToolError::invalid("the search lost its suggestion"));
    };
    let m = model(cat, Some(&p.scene), &cam.model_id)
        .ok_or_else(|| ToolError::NoModel(cam.model_id.clone()))?;
    let night = verdict_of(layers, &params, &cam, m, &probe, Lighting::Night);
    let summary = format!(
        "No camera in the scene can be re-aimed to cover {name} {at}. A new {} at [{:.0}, {:.0}], {:.1} m up, facing {} ({:.0}°), tilted {:.0}°, leaves it not ruled out; {}. Estimates; check on site.",
        sug.model,
        sug.at[0],
        sug.at[1],
        sug.height_m,
        sug.aim.facing,
        sug.aim.yaw_deg,
        sug.aim.pitch_deg,
        night_words(night)
    );
    let mut out = base("new_camera", summary, night_words(night));
    if apply {
        let mut s = p.scene.clone();
        if s.cameras.len() >= MAX_CAMERAS {
            return Err(ToolError::invalid(format!(
                "a scene holds at most {MAX_CAMERAS} cameras"
            )));
        }
        cam.id = fresh_id(s.cameras.iter().map(|c| c.id));
        cam.label = format!("Camera {}", cam.id);
        sug.camera_id = Some(cam.id);
        out.new_camera_id = Some(cam.id);
        s.cameras.push(cam);
        out.scene = Some(edit::finish(&s)?.1);
    }
    out.suggestion = Some(sug);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SuggestArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// What to cover: `street` (default), `targets`, `area` or `gaps`.
    #[serde(default)]
    pub goal: GoalArg,
    /// For `targets`: target ids or names; omit for every target.
    pub targets: Option<Vec<TargetRef>>,
    /// For `area`: the labelled area's id; omit when the scene has one.
    pub area_id: Option<u32>,
    /// How many cameras to suggest, 1 to 4 (default 1).
    pub count: Option<usize>,
    /// Model ids to choose from, at most 4; omit for a doorbell, a
    /// floodlight and a bullet camera.
    pub models: Option<Vec<String>>,
    /// true to plan for night (infrared range); omit for the scene's
    /// lighting.
    pub night: Option<bool>,
    /// The detail level; omit for the scene's.
    pub level: Option<Dori>,
    /// true to add the suggested cameras and return the edited scene.
    #[serde(default)]
    pub apply: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OptimiseArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The camera to re-aim.
    pub camera_id: u32,
    /// What to cover: `street` (default), `targets`, `area` or `gaps`.
    #[serde(default)]
    pub goal: GoalArg,
    /// For `targets`: target ids or names; omit for every target.
    pub targets: Option<Vec<TargetRef>>,
    /// For `area`: the labelled area's id; omit when the scene has one.
    pub area_id: Option<u32>,
    /// true to also try spots up to 3 m either way along the same wall.
    #[serde(default)]
    pub allow_move: bool,
    /// true to plan for night; omit for the scene's lighting.
    pub night: Option<bool>,
    /// The detail level; omit for the scene's.
    pub level: Option<Dori>,
    /// true to apply the new aim and return the edited scene.
    #[serde(default)]
    pub apply: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CoverArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The target's id or name.
    pub target: TargetRef,
    /// true to make the change and return the edited scene.
    #[serde(default)]
    pub apply: bool,
}

#[tool_router(router = goal_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Suggest where to mount new cameras (1 to 4) on house fronts for a goal: street (most extra street not ruled out), targets, area or gaps. Returns each camera's model, [x, z], height, aim and gain, best first; apply=true adds them and returns the scene. Estimates: pass on the notes and caveats.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn suggest_placement(
        &self,
        Parameters(a): Parameters<SuggestArgs>,
    ) -> Result<Json<SuggestOut>, String> {
        off_runtime(move || {
            let spec = GoalSpec {
                goal: a.goal,
                targets: a.targets,
                area_id: a.area_id,
            };
            suggest(
                &a.scene,
                &spec,
                a.count,
                a.models.as_deref(),
                a.night,
                a.level,
                a.apply,
            )
        })
        .await
        .map(Json)
    }

    #[tool(
        description = "Find a better yaw and pitch for one camera for a goal (street by default, or targets, area, gaps), optionally sliding it along its wall (allow_move). Returns before and after figures and the new aim; apply=true returns the edited scene.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn optimise_aim(
        &self,
        Parameters(a): Parameters<OptimiseArgs>,
    ) -> Result<Json<OptimiseOut>, String> {
        off_runtime(move || {
            let spec = GoalSpec {
                goal: a.goal,
                targets: a.targets,
                area_id: a.area_id,
            };
            optimise(
                &a.scene,
                a.camera_id,
                &spec,
                a.allow_move,
                a.night,
                a.level,
                a.apply,
            )
        })
        .await
        .map(Json)
    }

    #[tool(
        description = "Make one target (id or name) not ruled out at the scene's level by day: says which camera already does it, or how to re-aim one, or where a new camera would go, and what happens at night. apply=true makes the change and returns the scene.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) async fn cover_target(
        &self,
        Parameters(a): Parameters<CoverArgs>,
    ) -> Result<Json<CoverOut>, String> {
        off_runtime(move || cover(&a.scene, &a.target, a.apply))
            .await
            .map(Json)
    }
}

#[cfg(test)]
#[path = "optimise_tests.rs"]
mod tests;
