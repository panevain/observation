//! Incremental coverage computation and overlay painting.
//!
//! Each camera's visibility is cached under a key derived from everything
//! that affects it, so moving one camera only recomputes that camera.

use crate::state::{Area, Models, OverlayMode, Scene, Tool, UiState};
use crate::theme::{Tokens, to_bevy};
use crate::world::{CLEAR, FacadeOverlay, Overlay, texel, write_overlay};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use observation_core::camera::{CameraModel, CameraPlacement};
use observation_core::coverage::{
    CoverageCounts, CoverageParams, CoverageStats, Region, RegionMask, Visibility as CellVisibility,
};
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
    /// The active region and its cells, if the figures are limited to one.
    region: Option<(Region, RegionMask)>,
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

fn key(c: &CameraPlacement, m: &CameraModel, p: &CoverageParams) -> u64 {
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
        m.resolution,
        m.ir_range_m.map(f32::to_bits),
    )
        .hash(&mut h);
    (p.dori, p.lighting, p.subject_height_m.to_bits()).hash(&mut h);
    h.finish()
}

/// The view cone used for analysis and drawing.
pub fn cone_for(c: &CameraPlacement, m: &CameraModel, p: &CoverageParams) -> ViewCone {
    ViewCone::new(c, m, m.effective_range_m(p.dori, p.lighting))
}

fn update_coverage(
    scene: Res<Scene>,
    models: Res<Models>,
    area: Option<Res<Area>>,
    mut state: ResMut<CoverageState>,
) {
    let Some(area) = area else { return };
    if !scene.is_changed() && !models.is_changed() && !area.is_changed() && state.counts.is_some() {
        return;
    }
    let layers = &area.layers;
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
    for c in scene.cameras.iter().filter(|c| c.enabled) {
        let Some(m) = models.get(&c.model_id) else {
            continue;
        };
        state.ranges.insert(
            c.id,
            m.effective_range_m(scene.params.dori, scene.params.lighting),
        );
        let k = key(c, m, &scene.params);
        if state.cams.get(&c.id).is_some_and(|(old, _)| *old == k) {
            continue;
        }
        let vis = layers.visibility(
            &cone_for(c, m, &scene.params),
            scene.params.subject_height_m,
        );
        if let Some((_, old)) = state.cams.insert(c.id, (k, vis)) {
            counts.remove(&old);
        }
        if let Some((_, new)) = state.cams.get(&c.id) {
            counts.add(new);
        }
        changed = true;
    }
    let wanted = scene.region.as_ref().filter(|r| r.is_active());
    if state.region.as_ref().map(|(r, _)| r) != wanted {
        state.region = wanted.map(|r| (r.clone(), RegionMask::new(layers, r)));
        changed = true;
    }
    if changed || state.painted_mode.is_none() {
        let mask = state.region.as_ref().map(|(_, m)| m);
        state.stats = CoverageStats::compute_in(layers, counts, mask);
        state.paint = true;
    }
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
    // Per-camera colours can change without any visibility changing.
    let recolor =
        ui.overlay == OverlayMode::PerCamera && (scene.is_changed() || models.is_changed());
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
            let mut col = texel(crate::cameras::camera_color(c, &models, &tokens));
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
    let color = to_bevy(tokens.tools.region);
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
        // The first corner is drawn larger once clicking it would close the region.
        let radius = if i == 0 && draft.len() >= 3 { 0.9 } else { 0.4 };
        gizmos.circle(Isometry3d::new(at(*p), flat), radius, color);
    }
}
