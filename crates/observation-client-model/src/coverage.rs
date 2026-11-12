//! The coverage figures and what they are counted from: each camera's
//! cached sight lines, and the cells seen at every detail level.
//!
//! The client decides what to compute and paints the overlay; this module
//! keeps the counts.

use observation_core::camera::{CameraModel, CameraPlacement, DoriLevel};
#[cfg(doc)]
use observation_core::coverage::CoverageParams;
use observation_core::coverage::{CoverageStats, RegionMask, Sight, StaticLayers, Tally};
use observation_core::coverage_job::JobResult;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use crate::editor::OverlayMode;

pub mod backend;

#[derive(Debug, Default)]
pub struct CoverageState {
    /// Each camera's cache key and sight lines.
    pub cams: BTreeMap<u32, (u64, Sight)>,
    /// One per [`CoverageParams::ladder`] level, under the current lighting.
    pub rungs: Vec<Rung>,
    /// Index into `rungs` of the selected detail level.
    pub selected: usize,
    /// The figures at the selected detail level.
    pub stats: CoverageStats,
    /// Range used for each camera at the selected detail level (for the
    /// inspector), metres.
    pub ranges: BTreeMap<u32, f32>,
    pub paint: bool,
    pub painted_mode: Option<(OverlayMode, bool)>,
    /// Hash of the camera colours last painted in per-camera mode.
    pub painted_colors: u64,
    /// The region's corners and cells, kept while the region is turned off so
    /// turning it back on needs no recomputation.
    pub region: Option<(Vec<[f32; 2]>, RegionMask)>,
    /// Whether the figures were last computed inside the region.
    pub region_on: bool,
}

/// The cells seen at one detail level, under the current lighting.
#[derive(Debug)]
pub struct Rung {
    pub level: DoriLevel,
    pub tally: Tally,
    /// The range each camera's cells were counted out to, metres.
    pub counted: BTreeMap<u32, f32>,
    pub stats: CoverageStats,
    /// The counts changed since `stats` were taken.
    pub changed: bool,
}

/// What the counts are limited to: the loaded area's layers, and the region
/// when the figures are limited to it.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    pub layers: &'a StaticLayers,
    pub region: Option<&'a RegionMask>,
}

impl Rung {
    pub fn new(level: DoriLevel, scope: Scope) -> Self {
        Self {
            level,
            tally: Tally::new(scope.layers, scope.region),
            counted: BTreeMap::default(),
            stats: CoverageStats::default(),
            changed: true,
        }
    }

    /// Stops counting a camera's cells (`sight` must be the one they were
    /// counted from).
    fn uncount(&mut self, scope: Scope, id: u32, sight: &Sight) {
        if let Some(r) = self.counted.remove(&id) {
            self.tally
                .remove_within(scope.layers, scope.region, sight, r);
            self.changed = true;
        }
    }

    /// Counts a camera's cells out to `range`, replacing what it counted
    /// before from the same `sight`.
    pub fn count(&mut self, scope: Scope, id: u32, sight: &Sight, range: f32) {
        if self.counted.get(&id) == Some(&range) {
            return;
        }
        self.uncount(scope, id, sight);
        self.tally
            .add_within(scope.layers, scope.region, sight, range);
        self.counted.insert(id, range);
        self.changed = true;
    }
}

impl CoverageState {
    /// The ground and façade cells a camera sees at the selected detail level.
    pub fn seen(
        &self,
        id: u32,
    ) -> Option<(
        impl Iterator<Item = u32> + '_,
        impl Iterator<Item = u32> + '_,
    )> {
        let (_, sight) = self.cams.get(&id)?;
        let range = *self.rungs.get(self.selected)?.counted.get(&id)?;
        Some((sight.ground_within(range), sight.facade_within(range)))
    }

    /// The figures at each [`CoverageParams::ladder`] level under the current
    /// lighting, in ladder order.
    pub fn ladder(&self) -> impl Iterator<Item = (DoriLevel, &CoverageStats)> {
        self.rungs.iter().map(|r| (r.level, &r.stats))
    }

    /// Stops counting a camera everywhere and forgets its sight lines.
    pub fn forget(&mut self, scope: Scope, id: u32) {
        if let Some((_, sight)) = self.cams.remove(&id) {
            for r in &mut self.rungs {
                r.uncount(scope, id, &sight);
            }
        }
    }
}

/// Everything a camera's sight lines depend on but range: its pose, the
/// fields of view that shape its cone, and the subject height. Range only cuts
/// them off by distance, so a cached [`Sight`] serves every range it
/// [covers](Sight::covers) whatever its key.
pub fn key(c: &CameraPlacement, m: &CameraModel, subject_height_m: f32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in c
        .position
        .iter()
        .chain([&c.yaw_deg, &c.pitch_deg, &c.roll_deg])
    {
        v.to_bits().hash(&mut h);
    }
    (
        m.hfov_deg.to_bits(),
        m.vfov_deg.to_bits(),
        subject_height_m.to_bits(),
    )
        .hash(&mut h);
    h.finish()
}

/// Keeps a camera's computed sight lines, given its current key (`None` once
/// it is deleted, disabled or without a model). A result for an earlier pose
/// still replaces older sight lines, so the overlay follows a dragged camera
/// one job behind, but never up-to-date ones. The rungs count the new sight
/// lines afterwards.
pub fn apply(state: &mut CoverageState, scope: Scope, current: Option<u64>, r: JobResult) {
    let Some(current) = current else {
        return;
    };
    if r.key != current
        && state
            .cams
            .get(&r.camera_id)
            .is_some_and(|(k, _)| *k == current)
    {
        return;
    }
    state.forget(scope, r.camera_id);
    state.cams.insert(r.camera_id, (r.key, r.sight));
}
