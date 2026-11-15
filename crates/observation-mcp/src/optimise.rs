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
use crate::area::{bundle_for, layers_for};
use crate::catalog::{catalog, model};
use crate::edit::{self, Dori, Light};
use crate::queries::{ONE_SIDED, Settings, Sources, lighting_words, off_runtime, with_overrides};
use crate::schema::JsonObject;
use crate::server::ObservationMcp;
use observation_core::building::Footprint;
use observation_core::bundle::AreaBundle;
use observation_core::camera::{
    CameraModel, CameraPlacement, Catalog, Lighting, yaw_pitch_from_forward,
};
use observation_core::coverage::{CoverageParams, GroundGrid, RegionMask, StaticLayers};
use observation_core::example::{EXAMPLE_MODELS, WallMount, wall_mounts_of};
use observation_core::footage::{
    FootageCamera, Probe, Reach, Target, TargetPlace, Verdict, seen_ground, street_gaps,
};
use observation_core::frustum::ViewCone;
use observation_core::glam::{Vec2, Vec3};
use observation_core::scene::{MAX_CAMERAS, SceneFile};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::Cell;
use std::collections::HashSet;
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

/// "at Detect by day".
fn at_words(p: &CoverageParams) -> String {
    format!("at {} {}", p.dori.label(), lighting_words(p.lighting))
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

    /// A field of the given grid cells.
    fn from_cells(layers: &StaticLayers, cells: Vec<u32>, h: f32) -> Self {
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

/// What a camera at one eye has a clear line to, whichever way it faces:
/// the lines of sight an eye needs, tested once for all its aims.
struct Lines {
    /// Field positions.
    cells: Vec<u32>,
    /// For each of the goal's targets, its points ([`Probe::clear_from`]).
    targets: Vec<Vec<Vec3>>,
}

/// One camera, at one eye, being scored.
struct Shot<'a> {
    eye: Vec3,
    roll: f32,
    model: &'a CameraModel,
    lines: &'a Lines,
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
    /// How many aims have been scored.
    tried: Cell<usize>,
}

impl<'a> Search<'a> {
    fn new(layers: &'a StaticLayers, params: CoverageParams, goal: &'a Goal, field: Field) -> Self {
        Self {
            layers,
            params,
            goal,
            field,
            tried: Cell::new(0),
        }
    }

    /// The lines of sight from `eye` out to `range`: to the field's cells
    /// (only those in front of `front`, a wall's normal, when given: cells
    /// behind the wall a camera is on are behind its building) and to the
    /// goal's targets.
    fn lines(&self, eye: Vec3, range: f32, front: Option<Vec2>) -> Lines {
        Lines {
            cells: self.field.clear_from(self.layers, eye, range, front),
            targets: self
                .goal
                .probes
                .iter()
                .map(|p| p.clear_from(self.layers, eye, range))
                .collect(),
        }
    }

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
        self.tried.set(self.tried.get() + 1);
        let cone = self.cone(shot, aim);
        let cells = shot
            .lines
            .cells
            .iter()
            .filter(|&&k| !covered.cells[k as usize] && cone.contains(self.field.pts[k as usize]))
            .count() as u32;
        // "Not ruled out" as `Reach::verdict` finds it, from the cached lines.
        let targets = shot
            .lines
            .targets
            .iter()
            .zip(&covered.targets)
            .filter(|(pts, done)| !**done && pts.iter().any(|p| cone.contains(*p)))
            .count() as u32;
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
        for &k in &shot.lines.cells {
            if cone.contains(self.field.pts[k as usize]) {
                covered.cells[k as usize] = true;
            }
        }
        for (pts, done) in shot.lines.targets.iter().zip(&mut covered.targets) {
            if pts.iter().any(|p| cone.contains(*p)) {
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

    /// A finer search round `start`: yaw and pitch `steps` steps of
    /// `yaw_step` and `pitch_step` each way, repeated round the new best
    /// while it improves (at most three times).
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
    let light = night.map(|n| if n { Light::Night } else { Light::Day });
    let params = with_overrides(s.params, level, light);
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
    lines: Lines,
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
    // Before the search: no more suggestions than the scene has room for.
    let room = edit::camera_room(&p.scene);
    if room == 0 {
        return Err(ToolError::invalid(format!(
            "the scene already holds the most cameras it can ({MAX_CAMERAS}): remove one first"
        )));
    }
    let models = choose_models(cat, &p.scene, models)?;
    let found = search_placements(&p, spec, &models, count.min(room))?;
    let mut out = found.out;
    if count > room {
        out.summary.push_str(&format!(
            " The scene has room for only {room} more camera(s)."
        ));
    }
    if apply {
        let mut s = p.scene.clone();
        let mut ids = Vec::new();
        for (c, sug) in found.placements.into_iter().zip(&mut out.suggestions) {
            let id = edit::add_camera(&mut s, c)?;
            sug.camera_id = Some(id);
            ids.push(id);
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
    let search = Search::new(layers, params, &goal, Field::new(layers, &goal, &seen, h));
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
    // Only the buildings that could reach the goal: a wall lies inside its
    // footprint's box, so the box is at least as near the goal as the wall.
    let reachable = |_: usize, fp: &Footprint| {
        goal.bounds
            .is_some_and(|b| box_gap(footprint_box(fp), b) <= max_range)
    };
    let mut walls: Vec<(f32, WallMount)> = wall_mounts_of(&p.bundle, reachable)
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
            let lines = search.lines(eye, range, Some(w.normal));
            mounts.push(Mount {
                wall: *w,
                eye,
                height: hm,
                models: ms,
                lines,
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
                    lines: &mt.lines,
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
            lines: &mounts[i].lines,
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
        "{} mount point(s) on {wall_count} house front(s) facing a street, {} model(s), {} aim(s) scored in all",
        mounts.len(),
        models.len(),
        search.tried.get()
    );
    Ok(Placements {
        out: SuggestOut {
            summary,
            goal: goal.words(),
            settings: Settings::of(&params),
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
    /// Whether the search found a pose better for the goal than the current
    /// one; when false, the current pose is kept and nothing is applied.
    pub changed: bool,
    /// For `targets`: the change in street not ruled out for the whole
    /// scene, m² (the tie-break between aims that open the same targets).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street_change_m2: Option<f32>,
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

/// The bounding box of a footprint's outline, `(min, max)`.
fn footprint_box(fp: &Footprint) -> (Vec2, Vec2) {
    fp.outer
        .iter()
        .fold((Vec2::INFINITY, Vec2::NEG_INFINITY), |(lo, hi), q| {
            let q = Vec2::from_array(*q);
            (lo.min(q), hi.max(q))
        })
}

/// The gap between two boxes `(min, max)`, metres (0 where they overlap).
fn box_gap(a: (Vec2, Vec2), b: (Vec2, Vec2)) -> f32 {
    (b.0 - a.1).max(a.0 - b.1).max(Vec2::ZERO).length()
}

/// How far in front of `w`'s line a camera at `xz` sits, if it counts as
/// mounted on it: within [`ON_WALL_M`] in front, and alongside the wall.
fn on_wall(w: &WallMount, xz: Vec2) -> Option<f32> {
    let along_dir = Vec2::new(-w.normal.y, w.normal.x);
    let d = xz - w.at;
    let off = d.dot(w.normal);
    let along = d.dot(along_dir).abs();
    ((0.0..=ON_WALL_M).contains(&off) && along <= w.length_m / 2.0).then_some(off)
}

/// The wall a camera at `xz` is mounted on, if any: the nearest wall it
/// counts as mounted on ([`on_wall`]), among the walls of the buildings
/// next to it only.
fn wall_of(bundle: &AreaBundle, xz: Vec2) -> Option<WallMount> {
    let near = |_: usize, fp: &Footprint| box_gap(footprint_box(fp), (xz, xz)) <= ON_WALL_M;
    wall_mounts_of(bundle, near)
        .into_iter()
        .filter_map(|w| on_wall(&w, xz).map(|off| (off, w)))
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
    let search = Search::new(layers, params, &goal, Field::new(layers, &goal, &seen, h));
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
    let lines0 = search.lines(cam.eye(), range, None);
    let shot0 = Shot {
        eye: cam.eye(),
        roll: cam.roll_deg,
        model: m,
        lines: &lines0,
    };
    let now = search.score(&shot0, current, &covered);
    let coarse = coarse_aims(current);
    let start = search
        .best_of(&shot0, coarse, &covered)
        .unwrap_or((current, now));
    let mut best = (
        xz0,
        cam.eye(),
        search.refine(&shot0, start, &covered, 2.0, 1.0, 2),
    );

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
                    let lines = search.lines(eye, range, None);
                    let shot = Shot {
                        eye,
                        roll: cam.roll_deg,
                        model: m,
                        lines: &lines,
                    };
                    let c = best.2.0;
                    let ring = (-2..=2).flat_map(|i| {
                        (-1..=1).map(move |j| {
                            Aim::new(c.yaw + i as f32 * 10.0, c.pitch + j as f32 * 5.0)
                        })
                    });
                    if let Some(found) = search.best_of(&shot, ring, &covered) {
                        let found = search.refine(&shot, found, &covered, 2.0, 1.0, 1);
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
    // Only a gain on the goal itself counts: for targets, more street alone
    // (the tie-break) is no reason to move the camera.
    let changed = score1.primary > now.primary;
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
    // For targets the goal's cells are the street.
    let street_change = goal.is_targets().then_some(after_m.m2 - before_m.m2);
    let street_words = street_change
        .map(|d| format!(" Street not ruled out changes by {d:+.0} m²."))
        .unwrap_or_default();
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
            "Camera {camera_id}: facing {} ({:.0}°) tilted {:.0}°{mv} gives {} ({:.0} → {:.0} {unit}).{street_words}{wall_note} Estimates; check on site.",
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
        settings: Settings::of(&params),
        unit: unit.to_owned(),
        before: pose(xz0, current, before_m.value(&goal)),
        after: pose(xz1, aim1, after_m.value(&goal)),
        gain: r1(gain),
        moved_m: r2(moved_m),
        changed,
        street_change_m2: street_change.map(r1),
        searched: format!(
            "{} aim(s) scored at {} spot(s) (yaw all round in 10° steps, tilt -50° to 10° in 5° steps, then finer)",
            search.tried.get(),
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
    /// For `re_aim`: other targets that only this camera leaves not ruled
    /// out and the new aim would rule out. A re-aim that loses any is never
    /// applied; it is offered only when nothing else works.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets_lost: Vec<u32>,
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
        settings: Settings::of(&params),
        outcome: outcome.to_owned(),
        camera_id: None,
        aim_before: None,
        aim_after: None,
        street_change_m2: None,
        targets_lost: Vec::new(),
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

    // Re-aim an existing camera within reach of the target, by aims round
    // the one straight at it that leave it not ruled out.
    let street = street_cells(&p.scene, layers);
    let cell_m2 = layers.grid().cell_area_m2();
    let ppm = params.pixels_per_metre();
    let centre = layers.subject_at(target_centre(&t), h);
    // The scene's other targets, and which cameras leave each not ruled out
    // now: a re-aim must not lose one that only the re-aimed camera reaches.
    let other_targets: Vec<(&Target, Probe)> = p
        .scene
        .targets
        .iter()
        .filter(|o| o.id != t.id)
        .map(|o| (o, Probe::target(layers, o, h)))
        .collect();
    let open_by: Vec<Vec<u32>> = other_targets
        .iter()
        .map(|(_, pr)| {
            cams.iter()
                .filter(|c| {
                    verdict_of(layers, &params, c.placement, c.model, pr, Lighting::Day)
                        == Verdict::NotRuledOut
                })
                .map(|c| c.placement.id)
                .collect()
        })
        .collect();
    // Each camera's visible cells, worked out only when needed.
    let mut vis: Vec<Option<Vec<u32>>> = vec![None; cams.len()];
    let mut cells_of = |k: usize| -> Vec<u32> {
        vis[k]
            .get_or_insert_with(|| ground_cells(layers, &params, cams[k].placement, cams[k].model))
            .clone()
    };
    let no_goal = Goal {
        kind: GoalKind::Street,
        cells: Vec::new(),
        total_m2: 0.0,
        probes: Vec::new(),
        focus: None,
        bounds: None,
    };
    let mut best: Option<ReAim> = None;
    for (k, c) in cams.iter().enumerate() {
        let (pl, m) = (c.placement, c.model);
        let eye = pl.eye();
        let range = m.effective_range_m(ppm, Lighting::Day);
        // Out of reach: no aim can help, so nothing is computed for it.
        if eye.distance(centre) > range + 30.0 {
            continue;
        }
        let Some(straight) = Aim::at(eye, centre) else {
            continue;
        };
        let cone = |a: Aim| {
            ViewCone::from_angles(
                eye,
                a.yaw,
                a.pitch,
                pl.roll_deg,
                m.hfov_clamped(),
                m.vfov_clamped(),
                range,
            )
        };
        let reaches = |pts: &[Vec3], a: Aim| {
            let cone = cone(a);
            pts.iter().any(|p| cone.contains(*p))
        };
        let clear = probe.clear_from(layers, eye, range);
        let ok: Vec<Aim> = (-3..=3)
            .flat_map(|i| {
                (-2..=2).map(move |j| {
                    Aim::new(
                        straight.yaw + i as f32 * 10.0,
                        straight.pitch + j as f32 * 5.0,
                    )
                })
            })
            .filter(|a| reaches(&clear, *a))
            .collect();
        if ok.is_empty() {
            continue;
        }
        // The targets only this camera leaves not ruled out.
        let mine: Vec<(u32, Vec<Vec3>)> = other_targets
            .iter()
            .zip(&open_by)
            .filter(|(_, by)| by.as_slice() == [pl.id])
            .map(|((o, pr), _)| (o.id, pr.clear_from(layers, eye, range)))
            .collect();
        // What the cameras whose views could overlap this one's see, near it.
        let mut others: HashSet<u32> = HashSet::new();
        for (j, o) in cams.iter().enumerate() {
            let reach = o.model.effective_range_m(ppm, Lighting::Day);
            if j != k && o.placement.eye().distance(eye) <= range + reach {
                others.extend(cells_of(j));
            }
        }
        // The street near the camera that no other camera sees.
        let local: Vec<u32> = cells_near(layers.grid(), Vec2::new(eye.x, eye.z), range)
            .into_iter()
            .filter(|&i| street[i as usize] && !others.contains(&i))
            .collect();
        let search = Search::new(
            layers,
            params,
            &no_goal,
            Field::from_cells(layers, local, h),
        );
        let lines = search.lines(eye, range, None);
        let shot = Shot {
            eye,
            roll: pl.roll_deg,
            model: m,
            lines: &lines,
        };
        let covered = Covered {
            cells: vec![false; search.field.cells.len()],
            targets: Vec::new(),
        };
        // The aim losing the fewest targets, then keeping the most street
        // (the first on a tie: nearest the straight aim).
        let mut pick: Option<(Vec<u32>, Score, Aim)> = None;
        for a in ok {
            let lost: Vec<u32> = mine
                .iter()
                .filter(|(_, pts)| !reaches(pts, a))
                .map(|(id, _)| *id)
                .collect();
            let score = search.score(&shot, a, &covered);
            let better = match &pick {
                None => true,
                Some((l, s, _)) => lost.len() < l.len() || (lost.len() == l.len() && score > *s),
            };
            if better {
                pick = Some((lost, score, a));
            }
        }
        let Some((lost, _, aim)) = pick else {
            continue;
        };
        // The exact change in street for the scene: the camera's own cells
        // now and after, outside what the others see.
        let mut moved = pl.clone();
        moved.yaw_deg = aim.yaw;
        moved.pitch_deg = aim.pitch;
        let own = |cells: &[u32]| {
            cells
                .iter()
                .filter(|&&i| street[i as usize] && !others.contains(&i))
                .count() as f32
                * cell_m2
        };
        let change = own(&ground_cells(layers, &params, &moved, m)) - own(&cells_of(k));
        let candidate = ReAim {
            camera: pl.id,
            aim,
            lost,
            change,
            turn: turn(aim.yaw, pl.yaw_deg).abs(),
        };
        if best.as_ref().is_none_or(|b| candidate.beats(b)) {
            best = Some(candidate);
        }
    }

    let lossless = best.as_ref().filter(|b| b.lost.is_empty());
    if let Some(b) = lossless
        && let Some(pl) = p.scene.cameras.iter().find(|c| c.id == b.camera)
        && let Some(m) = model(cat, Some(&p.scene), &pl.model_id)
    {
        let mut out = re_aim_out(&base, &name, &at, pl, m, b, layers, &params, &probe);
        if apply {
            let mut s = p.scene.clone();
            edit::aim_camera(&mut s, b.camera, Some(b.aim.yaw), Some(b.aim.pitch), None)?;
            out.scene = Some(edit::finish(&s)?.1);
        }
        return Ok(out);
    }
    // Every re-aim found loses another target: say so with whatever is
    // offered instead.
    let lossy_words = |b: &ReAim| {
        format!(
            "Re-aiming camera {} would also do it, but would rule out {}.",
            b.camera,
            lost_names(&p.scene, &b.lost)
        )
    };

    // A new camera, by the same search as suggest_placement, if there is
    // room for one.
    let room = edit::camera_room(&p.scene);
    let found = if room > 0 {
        let spec = GoalSpec {
            goal: GoalArg::Targets,
            targets: Some(vec![TargetRef::Id(t.id)]),
            area_id: None,
        };
        let models = choose_models(cat, &p.scene, None)?;
        Some(search_placements(&p, &spec, &models, 1)?)
    } else {
        None
    };
    let suggested = found.and_then(|f| {
        let sug = f.out.suggestions.into_iter().next()?;
        let cam = f.placements.into_iter().next()?;
        Some((sug, cam))
    });
    if let Some((mut sug, cam)) = suggested {
        let m = model(cat, Some(&p.scene), &cam.model_id)
            .ok_or_else(|| ToolError::NoModel(cam.model_id.clone()))?;
        let night = verdict_of(layers, &params, &cam, m, &probe, Lighting::Night);
        let mut summary = format!(
            "No camera in the scene can be re-aimed to cover {name} {at} without losing another target. A new {} at [{:.0}, {:.0}], {:.1} m up, facing {} ({:.0}°), tilted {:.0}°, leaves it not ruled out; {}.",
            sug.model,
            sug.at[0],
            sug.at[1],
            sug.height_m,
            sug.aim.facing,
            sug.aim.yaw_deg,
            sug.aim.pitch_deg,
            night_words(night)
        );
        if let Some(b) = &best {
            summary.push(' ');
            summary.push_str(&lossy_words(b));
        }
        summary.push_str(" Estimates; check on site.");
        let mut out = base("new_camera", summary, night_words(night));
        if apply {
            let mut s = p.scene.clone();
            let id = edit::add_camera(&mut s, cam)?;
            sug.camera_id = Some(id);
            out.new_camera_id = Some(id);
            out.scene = Some(edit::finish(&s)?.1);
        }
        out.suggestion = Some(sug);
        return Ok(out);
    }

    // Only a re-aim that loses another target: offered, never applied.
    if let Some(b) = &best
        && let Some(pl) = p.scene.cameras.iter().find(|c| c.id == b.camera)
        && let Some(m) = model(cat, Some(&p.scene), &pl.model_id)
    {
        let mut out = re_aim_out(&base, &name, &at, pl, m, b, layers, &params, &probe);
        out.summary = format!(
            "{} {} Not applied: it would rule out {}; to accept that, aim camera {} with aim_camera (yaw {:.0}, pitch {:.0}). {}",
            lossy_words(b),
            if room == 0 {
                "The scene has no room for a new camera."
            } else {
                "No house front facing a street gives a new camera a clear line to it."
            },
            lost_names(&p.scene, &b.lost),
            b.camera,
            b.aim.yaw,
            b.aim.pitch,
            out.summary
        );
        return Ok(out);
    }

    let why = if room == 0 {
        "the scene has no room for a new camera"
    } else {
        "no house front facing a street nearby gives a clear line to it"
    };
    Ok(base(
        "none_found",
        format!(
            "No camera in the scene can be re-aimed so that {name} is not ruled out {at}, and {why}. Try a different level, or place a camera by hand nearer the target."
        ),
        "at night: no camera found".to_owned(),
    ))
}

/// A re-aim `cover_target` found for one camera.
#[derive(Debug, Clone)]
struct ReAim {
    camera: u32,
    aim: Aim,
    /// Other targets it would rule out, by id.
    lost: Vec<u32>,
    /// The change in street not ruled out for the scene, m².
    change: f32,
    /// How far the camera turns, degrees.
    turn: f32,
}

impl ReAim {
    /// Fewer targets lost, then more street, then a smaller turn; the first
    /// camera on a tie.
    fn beats(&self, other: &Self) -> bool {
        (self.lost.len(), -self.change, self.turn) < (other.lost.len(), -other.change, other.turn)
    }
}

/// "Path (target 2), Gate (target 3)".
fn lost_names(scene: &SceneFile, ids: &[u32]) -> String {
    ids.iter()
        .map(|id| match scene.targets.iter().find(|t| t.id == *id) {
            Some(t) => target_words(t),
            None => format!("target {id}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `re_aim` result for `b`, on camera `pl` of model `m`.
#[allow(clippy::too_many_arguments)]
fn re_aim_out(
    base: &dyn Fn(&str, String, String) -> CoverOut,
    name: &str,
    at: &str,
    pl: &CameraPlacement,
    m: &CameraModel,
    b: &ReAim,
    layers: &StaticLayers,
    params: &CoverageParams,
    probe: &Probe,
) -> CoverOut {
    let mut moved = pl.clone();
    moved.yaw_deg = b.aim.yaw;
    moved.pitch_deg = b.aim.pitch;
    let night = verdict_of(layers, params, &moved, m, probe, Lighting::Night);
    let mut out = base(
        "re_aim",
        format!(
            "Re-aim camera {} to face {} ({:.0}°) tilted {:.0}° (now {:.0}°, {:.0}°): {name} is then not ruled out {at}; {}. Street not ruled out changes by {:+.0} m². Estimates; check on site.",
            b.camera,
            compass(b.aim.yaw),
            b.aim.yaw,
            b.aim.pitch,
            pl.yaw_deg,
            pl.pitch_deg,
            night_words(night),
            b.change
        ),
        night_words(night),
    );
    out.camera_id = Some(b.camera);
    out.aim_before = Some(AimOut::of(pl.yaw_deg, pl.pitch_deg));
    out.aim_after = Some(AimOut::of(b.aim.yaw, b.aim.pitch));
    out.street_change_m2 = Some(r1(b.change));
    out.targets_lost = b.lost.clone();
    out
}

/// The ground cells whose centres lie in the square of half-side `r` round
/// world `[x, z]` `c`.
fn cells_near(grid: &GroundGrid, c: Vec2, r: f32) -> Vec<u32> {
    let lo = ((c - Vec2::splat(r) - grid.min) / grid.cell_m)
        .floor()
        .max(Vec2::ZERO);
    let hi = ((c + Vec2::splat(r) - grid.min) / grid.cell_m).floor();
    let (x1, z1) = (hi.x.min(grid.w as f32 - 1.0), hi.y.min(grid.h as f32 - 1.0));
    if grid.is_empty() || x1 < lo.x || z1 < lo.y {
        return Vec::new();
    }
    let (x0, z0, x1, z1) = (lo.x as u32, lo.y as u32, x1 as u32, z1 as u32);
    (z0..=z1)
        .flat_map(|z| (x0..=x1).map(move |x| z * grid.w + x))
        .collect()
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
