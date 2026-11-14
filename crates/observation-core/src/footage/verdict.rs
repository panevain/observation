//! Verdicts: whether the model rules out that a camera sees a target or a
//! point, and if it does, why.
//!
//! The wording is one-sided on purpose. Hedges, fences, trees and vehicles
//! are not modelled and can only block more, so "not ruled out" is the most
//! the model can say for a camera; "ruled out" is a firm answer within the
//! model.

use super::{FootageCamera, Target, TargetPlace, WALL_TARGET_SIZE_M};
use crate::camera::{DoriLevel, Lighting};
use crate::coverage::{CoverageParams, FACADE_OFFSET_M, StaticLayers, scan_inside};
use crate::frustum::ViewCone;
use crate::util::planar::{point_in_ring, ring_bounds};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Most points a target is tested at; a larger set is thinned evenly.
pub const MAX_TARGET_SAMPLES: usize = 4096;

/// Why the model rules out that a camera sees a place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// No part of it lies inside the camera's field of view.
    OutsideView,
    /// Some of it is in view, but none within the range for the detail
    /// level and lighting.
    OutOfRange,
    /// Every part in view and in range is hidden by a building (or lies
    /// under one, or on the far side of the wall it is on).
    BlockedByBuilding,
    /// Some part in view and in range is clear of buildings, but the ground
    /// hides all of those.
    BlockedByTerrain,
}

impl Reason {
    pub fn text(self) -> &'static str {
        match self {
            Self::OutsideView => "outside the view",
            Self::OutOfRange => "out of range",
            Self::BlockedByBuilding => "blocked by a building",
            Self::BlockedByTerrain => "blocked by the terrain",
        }
    }
}

/// The model's answer for one camera, at one detail level and lighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Some of the place is visible in the model. Not a statement that the
    /// camera sees it: what the model leaves out can only block more.
    NotRuledOut,
    RuledOut(Reason),
}

impl Verdict {
    pub fn text(self) -> String {
        match self {
            Self::NotRuledOut => "not ruled out".to_owned(),
            Self::RuledOut(r) => format!("ruled out: {}", r.text()),
        }
    }

    pub fn is_ruled_out(self) -> bool {
        matches!(self, Self::RuledOut(_))
    }
}

/// A verdict at one detail level and lighting.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LevelVerdict {
    pub level: DoriLevel,
    pub lighting: Lighting,
    /// The camera's range at this level and lighting, metres.
    pub range_m: f32,
    pub verdict: Verdict,
}

/// One camera's verdicts on one place, at every level of
/// [`CoverageParams::ladder`] by day and by night.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraVerdicts {
    pub camera_id: u32,
    /// Straight-line distance from the camera to the nearest point the
    /// place is tested at, metres.
    pub nearest_m: f32,
    /// Level by level in ladder order, each by day and then by night.
    pub verdicts: Vec<LevelVerdict>,
}

impl CameraVerdicts {
    /// The verdict at `level` and `lighting`, if that level was asked about.
    pub fn at(&self, level: DoriLevel, lighting: Lighting) -> Option<Verdict> {
        self.verdicts
            .iter()
            .find(|v| v.level == level && v.lighting == lighting)
            .map(|v| v.verdict)
    }
}

/// Every enabled camera's verdicts on one target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetVerdicts {
    pub target_id: u32,
    pub cameras: Vec<CameraVerdicts>,
}

/// One point a place is tested at.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    p: Vec3,
    /// A wall's outward normal: the point is only visible from in front.
    normal: Option<Vec3>,
    /// Inside a building footprint, so hidden by it from everywhere.
    buried: bool,
}

/// The points a place is tested at, worked out once and tested from every
/// camera.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    samples: Vec<Sample>,
}

impl Probe {
    /// The points `target` is tested at.
    ///
    /// A ground target: the centres of the ground cells inside its outline
    /// (the cells the coverage overlay tests), at the subject height above
    /// the terrain; cells under a building count as hidden by it. An outline
    /// too small to hold a cell centre is tested on a lattice a quarter of a
    /// cell apart instead, or failing that at the mean of its corners. A
    /// wall target: the centre and the four corners, 0.1 m in, of its square,
    /// each 5 cm in front of the wall, as a wall patch is tested.
    pub fn target(layers: &StaticLayers, target: &Target, subject_height_m: f32) -> Self {
        match &target.at {
            TargetPlace::Ground { points } => Self::ground(layers, points, subject_height_m),
            TargetPlace::Wall { point, normal } => Self::wall(*point, *normal),
        }
    }

    /// A single point at the subject height above the terrain at `xz`.
    pub fn point(layers: &StaticLayers, xz: [f32; 2], subject_height_m: f32) -> Self {
        let xz = Vec2::from_array(xz);
        Self {
            samples: vec![Sample {
                p: layers.subject_at(xz, subject_height_m),
                normal: None,
                buried: layers.occluders().inside_any(xz),
            }],
        }
    }

    fn ground(layers: &StaticLayers, points: &[[f32; 2]], h: f32) -> Self {
        let grid = layers.grid();
        let buildings = layers.building_mask();
        let mut samples = Vec::new();
        scan_inside(grid, points, |start, end| {
            samples.extend((start..end).map(|i| Sample {
                p: layers.ground_subject(i, h),
                normal: None,
                buried: buildings[i],
            }));
        });
        if samples.is_empty() && !points.is_empty() {
            let at = |xz: Vec2| Sample {
                p: layers.subject_at(xz, h),
                normal: None,
                buried: layers.occluders().inside_any(xz),
            };
            let step = grid.cell_m / 4.0;
            let (lo, hi) = ring_bounds(points);
            let (lo, hi) = (Vec2::from_array(lo), Vec2::from_array(hi));
            let n = ((hi - lo) / step).ceil().min(Vec2::splat(512.0));
            for z in 0..n.y as u32 {
                for x in 0..n.x as u32 {
                    let xz = lo + (Vec2::new(x as f32, z as f32) + 0.5) * step;
                    if point_in_ring(xz, points) {
                        samples.push(at(xz));
                    }
                }
            }
            if samples.is_empty() {
                let sum: Vec2 = points.iter().map(|p| Vec2::from_array(*p)).sum();
                samples.push(at(sum / points.len() as f32));
            }
        }
        if samples.len() > MAX_TARGET_SAMPLES {
            let keep = samples.len().div_ceil(MAX_TARGET_SAMPLES);
            samples = samples.into_iter().step_by(keep).collect();
        }
        Self { samples }
    }

    fn wall(point: [f32; 3], normal: [f32; 2]) -> Self {
        let n = Vec3::new(normal[0], 0.0, normal[1]).normalize_or_zero();
        let along = Vec3::new(-n.z, 0.0, n.x);
        let c = Vec3::from_array(point) + n * FACADE_OFFSET_M;
        let d = WALL_TARGET_SIZE_M / 2.0 - 0.1;
        let samples = [(0.0, 0.0), (-d, -d), (d, -d), (d, d), (-d, d)]
            .iter()
            .map(|&(a, b)| Sample {
                p: c + along * a + Vec3::Y * b,
                normal: Some(n),
                buried: false,
            })
            .collect();
        Self { samples }
    }

    /// How many points the place is tested at.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// What one camera's sight lines reach of one place, from which its verdict
/// at any range up to [`Self::range`] follows without another line-of-sight
/// test (as [`Sight`](crate::coverage::Sight) does for cells).
///
/// The verdict at a range takes the place's points through four filters in
/// turn, and names the filter after which none is left: in the field of view
/// ([`Reason::OutsideView`]), within the range ([`Reason::OutOfRange`]),
/// clear of buildings ([`Reason::BlockedByBuilding`]), clear of the ground
/// ([`Reason::BlockedByTerrain`]). A point left after all four means the
/// place is [`Verdict::NotRuledOut`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reach {
    /// The longest range the verdicts are exact for, metres.
    pub range: f32,
    /// Squared distance to the nearest point, in view or not.
    nearest_d2: f32,
    /// Squared distance to the nearest point in the field of view.
    in_view_d2: Option<f32>,
    /// ... and within [`Self::range`], clear of buildings.
    unbuilt_d2: Option<f32>,
    /// ... and clear of the ground too.
    clear_d2: Option<f32>,
}

impl Reach {
    /// Tests `probe` from `cone`'s eye, out to `cone.range`. Points are
    /// tested nearest first, and testing stops at the first clear one.
    pub fn of(layers: &StaticLayers, cone: &ViewCone, probe: &Probe) -> Self {
        let eye = cone.eye;
        let mut reach = Self {
            range: cone.range,
            nearest_d2: f32::INFINITY,
            in_view_d2: None,
            unbuilt_d2: None,
            clear_d2: None,
        };
        let r2 = cone.range * cone.range;
        let mut near: Vec<(f32, usize)> = Vec::new();
        for (i, s) in probe.samples.iter().enumerate() {
            // As `ViewCone::contains` computes it.
            let d2 = (s.p - eye).length_squared();
            reach.nearest_d2 = reach.nearest_d2.min(d2);
            if !cone.in_view(s.p) {
                continue;
            }
            reach.in_view_d2 = Some(reach.in_view_d2.map_or(d2, |m| m.min(d2)));
            if d2 <= r2 {
                near.push((d2, i));
            }
        }
        near.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (d2, i) in near {
            let s = &probe.samples[i];
            let behind_wall = s
                .normal
                .is_some_and(|n| (eye - (s.p - n * FACADE_OFFSET_M)).dot(n) <= 0.0);
            if s.buried || behind_wall || layers.blocked_by_building(eye, s.p) {
                continue;
            }
            reach.unbuilt_d2.get_or_insert(d2);
            if !layers.blocked_by_terrain(eye, s.p) {
                reach.clear_d2 = Some(d2);
                break;
            }
        }
        reach
    }

    /// The verdict at `range` (exact up to [`Self::range`]).
    pub fn verdict(&self, range: f32) -> Verdict {
        let r2 = range * range;
        let within = |d2: Option<f32>| d2.is_some_and(|d2| d2 <= r2);
        match self.in_view_d2 {
            None => Verdict::RuledOut(Reason::OutsideView),
            Some(d2) if d2 > r2 => Verdict::RuledOut(Reason::OutOfRange),
            _ if within(self.clear_d2) => Verdict::NotRuledOut,
            _ if within(self.unbuilt_d2) => Verdict::RuledOut(Reason::BlockedByTerrain),
            _ => Verdict::RuledOut(Reason::BlockedByBuilding),
        }
    }

    /// Straight-line distance to the nearest point, metres.
    pub fn nearest_m(&self) -> f32 {
        self.nearest_d2.sqrt()
    }
}

/// One camera's verdicts on `probe` at every level of `params`' ladder, by
/// day and by night: one line-of-sight pass out to the longest of those
/// ranges, then a distance cut-off per level.
pub fn camera_verdicts(
    layers: &StaticLayers,
    params: &CoverageParams,
    camera: &FootageCamera,
    probe: &Probe,
) -> CameraVerdicts {
    let (c, m) = (camera.placement, camera.model);
    let cone = ViewCone::new(c, m, params.sight_range_m(m));
    let reach = Reach::of(layers, &cone, probe);
    let verdicts = params
        .ladder()
        .into_iter()
        .flat_map(|(level, ppm)| {
            [Lighting::Day, Lighting::Night].map(|lighting| {
                let range_m = m.effective_range_m(ppm, lighting);
                LevelVerdict {
                    level,
                    lighting,
                    range_m,
                    verdict: reach.verdict(range_m),
                }
            })
        })
        .collect();
    CameraVerdicts {
        camera_id: c.id,
        nearest_m: reach.nearest_m(),
        verdicts,
    }
}

/// Every enabled camera's verdicts on each target, in the order given.
pub fn target_verdicts(
    layers: &StaticLayers,
    params: &CoverageParams,
    cameras: &[FootageCamera],
    targets: &[Target],
) -> Vec<TargetVerdicts> {
    targets
        .iter()
        .map(|t| {
            let probe = Probe::target(layers, t, params.subject_height_m);
            TargetVerdicts {
                target_id: t.id,
                cameras: verdicts_on(layers, params, cameras, &probe),
            }
        })
        .collect()
}

/// Every enabled camera's verdicts on the point at world `[x, z]`, at the
/// subject height above the terrain: which cameras do not rule it out, and
/// why each other one does. The same classifier as for targets.
pub fn point_verdicts(
    layers: &StaticLayers,
    params: &CoverageParams,
    cameras: &[FootageCamera],
    xz: [f32; 2],
) -> Vec<CameraVerdicts> {
    let probe = Probe::point(layers, xz, params.subject_height_m);
    verdicts_on(layers, params, cameras, &probe)
}

fn verdicts_on(
    layers: &StaticLayers,
    params: &CoverageParams,
    cameras: &[FootageCamera],
    probe: &Probe,
) -> Vec<CameraVerdicts> {
    cameras
        .iter()
        .filter(|c| c.placement.enabled)
        .map(|c| camera_verdicts(layers, params, c, probe))
        .collect()
}
