//! The coverage figures and what they are counted from: each camera's
//! cached sight lines, and the cells seen at every detail level.
//!
//! The client decides what to compute and paints the overlay; this module
//! keeps the counts.

use observation_core::camera::{CameraModel, CameraPlacement, DoriLevel};
#[cfg(doc)]
use observation_core::coverage::CoverageParams;
use observation_core::coverage::{
    CoverageCounts, CoverageStats, RegionMask, Sight, StaticLayers, Tally,
};
use observation_core::coverage_job::JobResult;
use observation_core::frustum::ViewCone;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use crate::document::{Document, Models};
use crate::editor::OverlayMode;
use backend::CoverageBackend;

pub mod backend;

#[derive(Debug, Default)]
pub struct CoverageState {
    /// Each camera's cache key and sight lines.
    cams: BTreeMap<u32, (u64, Sight)>,
    /// One per [`CoverageParams::ladder`] level, under the current lighting.
    rungs: Vec<Rung>,
    /// Index into `rungs` of the selected detail level.
    selected: usize,
    /// The figures at the selected detail level.
    pub stats: CoverageStats,
    /// Range used for each camera at the selected detail level (for the
    /// inspector), metres.
    pub ranges: BTreeMap<u32, f32>,
    paint: bool,
    painted_mode: Option<(OverlayMode, bool)>,
    /// Hash of the camera colours last painted in per-camera mode.
    painted_colors: u64,
    /// The region's corners and cells, kept while the region is turned off so
    /// turning it back on needs no recomputation.
    region: Option<(Vec<[f32; 2]>, RegionMask)>,
    /// Whether the figures were last computed inside the region.
    region_on: bool,
}

/// The cells seen at one detail level, under the current lighting.
#[derive(Debug)]
struct Rung {
    level: DoriLevel,
    tally: Tally,
    /// The range each camera's cells were counted out to, metres.
    counted: BTreeMap<u32, f32>,
    stats: CoverageStats,
    /// The counts changed since `stats` were taken.
    changed: bool,
}

/// What the counts are limited to: the loaded area's layers, and the region
/// when the figures are limited to it.
#[derive(Debug, Clone, Copy)]
struct Scope<'a> {
    layers: &'a StaticLayers,
    region: Option<&'a RegionMask>,
}

impl Rung {
    fn new(level: DoriLevel, scope: Scope) -> Self {
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
    fn count(&mut self, scope: Scope, id: u32, sight: &Sight, range: f32) {
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

    /// Whether anything has been counted since the state was made (a new
    /// area resets it).
    pub fn counted(&self) -> bool {
        !self.rungs.is_empty()
    }

    /// The cell counts at the selected detail level, once counted.
    pub fn selected_counts(&self) -> Option<&CoverageCounts> {
        self.rungs.get(self.selected).map(|r| &r.tally.counts)
    }

    /// Asks for the overlay to be painted again (a theme change recolours
    /// it).
    pub fn repaint(&mut self) {
        self.paint = true;
    }

    /// The hash of the camera colours last painted in per-camera mode.
    pub fn painted_colors(&self) -> u64 {
        self.painted_colors
    }

    /// Whether the overlay already shows these counts in this mode (overlay
    /// mode and façades shown) with these camera colours.
    pub fn is_painted(&self, mode: (OverlayMode, bool), colors: u64) -> bool {
        !self.paint && colors == self.painted_colors && self.painted_mode == Some(mode)
    }

    /// Records that the overlay was painted in `mode` with `colors`.
    pub fn mark_painted(&mut self, mode: (OverlayMode, bool), colors: u64) {
        self.paint = false;
        self.painted_mode = Some(mode);
        self.painted_colors = colors;
    }

    /// Stops counting a camera everywhere and forgets its sight lines.
    fn forget(&mut self, scope: Scope, id: u32) {
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
fn key(c: &CameraPlacement, m: &CameraModel, subject_height_m: f32) -> u64 {
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

/// Brings the counts up to date with the document: the region, the ladder of
/// detail levels under the current lighting, and every enabled camera's sight
/// lines, from the `results` that came back and new jobs on `backend`.
pub fn update(
    state: &mut CoverageState,
    scene: &Document,
    models: &Models,
    layers: &StaticLayers,
    results: Vec<JobResult>,
    backend: &mut CoverageBackend,
) {
    let params = scene.params;

    // The region first: every count is limited to it.
    let usable = scene.region.as_ref().filter(|r| r.problem().is_none());
    let mut region_changed = false;
    if state.region.as_ref().map(|(p, _)| p) != usable.map(|r| &r.points) {
        state.region = usable.map(|r| (r.points.clone(), RegionMask::new(layers, r)));
        region_changed = true;
    }
    let on = usable.is_some_and(|r| r.enabled);
    if on != state.region_on {
        state.region_on = on;
        region_changed = true;
    }
    // Out of the state while it is borrowed below; put back at the end.
    let region = state.region.take();
    let scope = Scope {
        layers,
        region: region.as_ref().filter(|_| on).map(|(_, m)| m),
    };

    // One rung per ladder level, keeping those already counted.
    let ladder = params.ladder();
    let mut old = std::mem::take(&mut state.rungs);
    state.rungs = ladder
        .iter()
        .map(
            |&(level, _)| match old.iter().position(|r| r.level == level) {
                Some(i) => {
                    let mut r = old.swap_remove(i);
                    if region_changed {
                        r.tally.limit_to(layers, scope.region);
                        r.changed = true;
                    }
                    r
                }
                None => Rung::new(level, scope),
            },
        )
        .collect();
    let selected = ladder
        .iter()
        .position(|(l, _)| *l == params.dori)
        .unwrap_or(0);
    let mut repaint = selected != state.selected;
    state.selected = selected;

    state.ranges.clear();
    // Enabled cameras with a known model: their keys, how far their sight
    // lines must reach (and how far to compute them when they fall short),
    // and their range at each ladder level.
    let mut current = Vec::new();
    let mut keys = BTreeMap::new();
    for c in scene.cameras.iter().filter(|c| c.enabled) {
        let Some(m) = models.get(&c.model_id) else {
            continue;
        };
        let ranges: Vec<f32> = ladder
            .iter()
            .map(|(_, ppm)| m.effective_range_m(*ppm, params.lighting))
            .collect();
        state.ranges.insert(c.id, ranges[selected]);
        let k = key(c, m, params.subject_height_m);
        keys.insert(c.id, k);
        let reach = (params.sight_range_m(m), params.sight_compute_range_m(m));
        current.push((c, m, k, reach, ranges));
    }
    let gone: Vec<u32> = state
        .cams
        .keys()
        .filter(|id| !keys.contains_key(*id))
        .copied()
        .collect();
    for id in gone {
        state.forget(scope, id);
    }
    for r in results {
        apply(state, scope, keys.get(&r.camera_id).copied(), r);
    }
    for (c, m, k, (needed, compute_to), _) in &current {
        // One job per camera at a time: a camera dragged faster than its jobs
        // finish gets its latest pose computed when the previous job is back.
        let fresh = state
            .cams
            .get(&c.id)
            .is_some_and(|(old, sight)| old == k && sight.covers(*needed));
        if fresh || backend.busy(c.id) {
            continue;
        }
        let cone = ViewCone::new(c, m, *compute_to);
        if let Some(r) = backend.submit(c.id, *k, cone, params.subject_height_m, layers) {
            apply(state, scope, Some(*k), r);
        }
    }
    // Count each camera's cells at every level. While a camera's sight lines
    // are being recomputed for a longer range, it counts what they reach.
    for (c, _, _, _, ranges) in &current {
        let Some((_, sight)) = state.cams.get(&c.id) else {
            continue;
        };
        for (rung, &r) in state.rungs.iter_mut().zip(ranges) {
            rung.count(scope, c.id, sight, r);
        }
    }
    for (i, r) in state.rungs.iter_mut().enumerate() {
        if r.changed {
            r.stats = r.tally.stats(layers);
            r.changed = false;
            repaint |= i == selected;
        }
    }
    state.region = region;
    if repaint || state.painted_mode.is_none() {
        state.stats = state.rungs[selected].stats;
        state.paint = true;
    }
}

/// Keeps a camera's computed sight lines, given its current key (`None` once
/// it is deleted, disabled or without a model). A result for an earlier pose
/// still replaces older sight lines, so the overlay follows a dragged camera
/// one job behind, but never up-to-date ones. The rungs count the new sight
/// lines afterwards.
fn apply(state: &mut CoverageState, scope: Scope, current: Option<u64>, r: JobResult) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_overlay_is_painted_again_after_a_change_of_mode_colours_or_counts() {
        let mut state = CoverageState::default();
        let overlap = (OverlayMode::Overlap, true);
        assert!(!state.is_painted(overlap, 0), "never painted");
        assert!(!state.counted() && state.selected_counts().is_none());
        state.mark_painted(overlap, 7);
        assert!(state.is_painted(overlap, 7));
        assert_eq!(state.painted_colors(), 7);
        assert!(
            !state.is_painted((OverlayMode::Overlap, false), 7),
            "façades"
        );
        assert!(!state.is_painted((OverlayMode::PerCamera, true), 7));
        assert!(!state.is_painted(overlap, 8), "camera colours");
        state.repaint();
        assert!(!state.is_painted(overlap, 7), "counts or theme");
        state.mark_painted(overlap, 7);
        assert!(state.is_painted(overlap, 7));
    }
}
