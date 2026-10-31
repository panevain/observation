//! Incremental coverage computation and overlay painting.
//!
//! Each camera's visibility is cached under a key derived from everything
//! that affects it, so moving one camera only recomputes that camera. The
//! computing itself happens in [`backend`]: a Web Worker in the browser.

mod backend;

use crate::state::{Area, Models, OverlayMode, Scene, Tool, UiState};
use crate::theme::{Tokens, rgba, to_bevy};
use crate::world::{CLEAR, FacadeOverlay, Overlay, texel, write_overlay};
use backend::CoverageBackend;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use observation_core::camera::{CameraModel, CameraPlacement};
use observation_core::coverage::{
    CoverageCounts, CoverageParams, CoverageStats, RegionMask, Visibility as CellVisibility,
};
use observation_core::coverage_job::JobResult;
use observation_core::frustum::ViewCone;
use std::hash::{Hash, Hasher};

#[derive(Default, Resource)]
pub struct CoverageState {
    cams: HashMap<u32, (u64, CellVisibility)>,
    counts: Option<CoverageCounts>,
    pub stats: CoverageStats,
    /// Range used for each camera (for the inspector), metres.
    pub ranges: HashMap<u32, f32>,
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

impl CoverageState {
    pub fn visibility(&self, id: u32) -> Option<&CellVisibility> {
        self.cams.get(&id).map(|(_, v)| v)
    }
}

pub struct CoveragePlugin;

impl Plugin for CoveragePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CoverageState>()
            .init_non_send::<CoverageBackend>()
            .insert_gizmo_config(
                RegionGizmos,
                GizmoConfig {
                    line: GizmoLineConfig {
                        width: 4.0,
                        joints: GizmoLineJoint::Round(4),
                        ..default()
                    },
                    // Drawn over buildings so the whole outline stays visible.
                    depth_bias: -1.0,
                    ..default()
                },
            )
            .add_systems(
                Update,
                (update_coverage, paint_overlay, draw_region).chain(),
            );
    }
}

/// Everything a camera's visibility depends on: its pose, the fields of view
/// and effective range that shape its cone, and the subject height. Hashing
/// the resolved range (not the density, lighting and IR range behind it) means
/// a settings change that leaves the range alone, such as a density change
/// while the IR range caps it at night, recomputes nothing.
fn key(c: &CameraPlacement, m: &CameraModel, range: f32, subject_height_m: f32) -> u64 {
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
        range.to_bits(),
        subject_height_m.to_bits(),
    )
        .hash(&mut h);
    h.finish()
}

/// The view cone used for analysis and drawing.
pub fn cone_for(c: &CameraPlacement, m: &CameraModel, p: &CoverageParams) -> ViewCone {
    ViewCone::new(c, m, m.effective_range_m(p.pixels_per_metre(), p.lighting))
}

fn update_coverage(
    scene: Res<Scene>,
    models: Res<Models>,
    area: Option<Res<Area>>,
    mut state: ResMut<CoverageState>,
    mut backend: NonSendMut<CoverageBackend>,
) {
    let Some(area) = area else { return };
    if area.is_changed() {
        // `Area` is only ever inserted, so this is a new area. on_area_loaded
        // resets the state too, but may run before the new `Area` arrives.
        backend.load_area(&area.bundle);
        *state = CoverageState::default();
    }
    let layers = &area.layers;
    let results = backend.drain(layers);
    if results.is_empty() && !scene.is_changed() && !models.is_changed() && state.counts.is_some() {
        return;
    }
    let state = &mut *state;
    let counts = state
        .counts
        .get_or_insert_with(|| CoverageCounts::new(layers));
    let mut changed = false;

    let live: Vec<u32> = scene
        .cameras
        .iter()
        .filter(|c| c.enabled)
        .map(|c| c.id)
        .collect();
    state.cams.retain(|id, (_, vis)| {
        let keep = live.contains(id);
        if !keep {
            counts.remove(vis);
            changed = true;
        }
        keep
    });
    state.ranges.clear();
    let mut current = Vec::new();
    for c in scene.cameras.iter().filter(|c| c.enabled) {
        let Some(m) = models.get(&c.model_id) else {
            continue;
        };
        let range = m.effective_range_m(scene.params.pixels_per_metre(), scene.params.lighting);
        state.ranges.insert(c.id, range);
        current.push((c, m, range, key(c, m, range, scene.params.subject_height_m)));
    }
    let key_of = |id: u32| current.iter().find(|(c, ..)| c.id == id).map(|e| e.3);
    for r in results {
        changed |= apply(&mut state.cams, counts, key_of(r.camera_id), r);
    }
    for &(c, m, range, k) in &current {
        // One job per camera at a time: a camera dragged faster than its jobs
        // finish gets its latest pose computed when the previous job is back.
        if state.cams.get(&c.id).is_some_and(|(old, _)| *old == k) || backend.busy(c.id) {
            continue;
        }
        let cone = ViewCone::new(c, m, range);
        if let Some(r) = backend.submit(c.id, k, cone, scene.params.subject_height_m, layers) {
            changed |= apply(&mut state.cams, counts, Some(k), r);
        }
    }
    let usable = scene.region.as_ref().filter(|r| r.problem().is_none());
    if state.region.as_ref().map(|(p, _)| p) != usable.map(|r| &r.points) {
        state.region = usable.map(|r| (r.points.clone(), RegionMask::new(layers, r)));
        changed = true;
    }
    let on = usable.is_some_and(|r| r.enabled);
    if on != state.region_on {
        state.region_on = on;
        changed = true;
    }
    if changed || state.painted_mode.is_none() {
        let mask = state.region.as_ref().filter(|_| on).map(|(_, m)| m);
        state.stats = CoverageStats::compute_in(layers, counts, mask);
        state.paint = true;
    }
}

/// Puts a computed visibility into the cache and the counts, given the
/// camera's current key (`None` once it is deleted or disabled). A result for
/// an earlier pose still replaces an older visibility, so the overlay follows
/// a dragged camera one job behind, but never an up-to-date one. Returns
/// whether anything changed.
fn apply(
    cams: &mut HashMap<u32, (u64, CellVisibility)>,
    counts: &mut CoverageCounts,
    current: Option<u64>,
    r: JobResult,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    if r.key != current && cams.get(&r.camera_id).is_some_and(|(k, _)| *k == current) {
        return false;
    }
    if let Some((_, old)) = cams.insert(r.camera_id, (r.key, r.visibility)) {
        counts.remove(&old);
    }
    if let Some((_, new)) = cams.get(&r.camera_id) {
        counts.add(new);
    }
    true
}

fn paint_overlay(
    mut state: ResMut<CoverageState>,
    ui: Res<UiState>,
    scene: Res<Scene>,
    models: Res<Models>,
    tokens: Res<Tokens>,
    overlay: Option<Res<Overlay>>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut facade_vis: Query<&mut Visibility, With<FacadeOverlay>>,
) {
    let Some(overlay) = overlay else { return };
    for mut v in &mut facade_vis {
        let want = if ui.show_facades {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
    let mode = (ui.overlay, ui.show_facades);
    // Per-camera colours can change without any visibility changing; repaint
    // only when one actually did.
    let colors =
        if ui.overlay == OverlayMode::PerCamera && (scene.is_changed() || models.is_changed()) {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for c in &scene.cameras {
                let color = crate::cameras::camera_color(&scene, c, &models, &tokens);
                (c.id, rgba(color)).hash(&mut h);
            }
            h.finish()
        } else {
            state.painted_colors
        };
    let recolor = colors != state.painted_colors;
    if !state.paint && !recolor && state.painted_mode == Some(mode) && !overlay.is_changed() {
        return;
    }
    let Some(counts) = &state.counts else { return };
    let (mut ground, mut facade): (Vec<[u8; 4]>, Vec<[u8; 4]>) = match ui.overlay {
        OverlayMode::Overlap => (
            counts
                .ground
                .iter()
                .map(|&c| {
                    if c == 0 {
                        CLEAR
                    } else {
                        texel(tokens.coverage_color(c))
                    }
                })
                .collect(),
            counts
                .facade
                .iter()
                .map(|&c| {
                    if c == 0 {
                        CLEAR
                    } else {
                        texel(tokens.coverage_color(c))
                    }
                })
                .collect(),
        ),
        OverlayMode::PerCamera => (
            vec![CLEAR; counts.ground.len()],
            vec![CLEAR; counts.facade.len()],
        ),
    };
    if ui.overlay == OverlayMode::PerCamera {
        for c in &scene.cameras {
            let Some(vis) = state.visibility(c.id) else {
                continue;
            };
            let mut col = texel(crate::cameras::camera_color(&scene, c, &models, &tokens));
            col[3] = 150;
            for &i in &vis.ground {
                ground[i as usize] = col;
            }
            for &i in &vis.facade {
                facade[i as usize] = col;
            }
        }
    }
    write_overlay(&overlay, &mut images, &mut meshes, &ground, &facade);
    state.paint = false;
    state.painted_mode = Some(mode);
    state.painted_colors = colors;
}

/// Height of the region outline above the ground, metres.
const REGION_Y_M: f32 = 0.15;

/// Gizmo group for the region outline: thicker than the view cones.
#[derive(Default, Reflect, GizmoConfigGroup)]
struct RegionGizmos;

/// The region outline, and the region being drawn.
fn draw_region(
    mut gizmos: Gizmos<RegionGizmos>,
    scene: Res<Scene>,
    ui: Res<UiState>,
    tokens: Res<Tokens>,
) {
    let color = to_bevy(tokens.world.region);
    let at = |p: [f32; 2]| Vec3::new(p[0], REGION_Y_M, p[1]);
    if let Some(r) = &scene.region
        && ui.region_draft.is_empty()
    {
        let c = if r.is_active() {
            color
        } else {
            color.with_alpha(0.35)
        };
        gizmos.linestrip(r.points.iter().chain(r.points.first()).map(|p| at(*p)), c);
    }
    if ui.tool != Tool::Region {
        return;
    }
    let draft = &ui.region_draft;
    gizmos.linestrip(draft.iter().chain(&ui.region_hover).map(|p| at(*p)), color);
    let flat = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
    for (i, p) in draft.iter().enumerate() {
        // Once clicking it would close the region, the first corner shows
        // the whole area a click closes it from.
        let radius = if i == 0 && draft.len() >= 3 {
            ui.region_snap_m
        } else {
            0.4
        };
        gizmos.circle(Isometry3d::new(at(*p), flat), radius, color);
    }
}
