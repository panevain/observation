//! Incremental coverage computation and overlay painting.
//!
//! Each camera's visibility is cached under a key derived from everything
//! that affects it, so moving one camera only recomputes that camera. The
//! computing itself happens in [`backend`]: a Web Worker in the browser.

mod backend;

use crate::input::OrbitCamera;
use crate::state::{Area, Models, OverlayMode, Scene, Tool, UiState, terrain_of};
use crate::theme::{Tokens, rgba, to_bevy};
use crate::world::{AreaLoadSet, CLEAR, FacadeOverlay, OutsideArea, Overlay, texel, write_overlay};
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
            .insert_gizmo_config(
                HatchGizmos,
                GizmoConfig {
                    line: GizmoLineConfig {
                        width: 1.5,
                        ..default()
                    },
                    ..default()
                },
            )
            .add_systems(
                Update,
                (
                    // After a new area's state reset and `Area` insert, which
                    // Bevy applies in between (an automatic sync point).
                    update_coverage.after(AreaLoadSet),
                    paint_overlay,
                    draw_region,
                    draw_outside_area,
                )
                    .chain(),
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
        // `Area` is only ever inserted, so this is a new area (on_area_loaded
        // has reset the state).
        backend.load_area(&area.bundle);
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

    state.ranges.clear();
    // Enabled cameras with a known model, and their current keys.
    let mut current = Vec::new();
    let mut keys = HashMap::new();
    for c in scene.cameras.iter().filter(|c| c.enabled) {
        let Some(m) = models.get(&c.model_id) else {
            continue;
        };
        let range = m.effective_range_m(scene.params.pixels_per_metre(), scene.params.lighting);
        state.ranges.insert(c.id, range);
        let k = key(c, m, range, scene.params.subject_height_m);
        keys.insert(c.id, k);
        current.push((c, m, range, k));
    }
    state.cams.retain(|id, (_, vis)| {
        let keep = keys.contains_key(id);
        if !keep {
            counts.remove(vis);
            changed = true;
        }
        keep
    });
    for r in results {
        changed |= apply(&mut state.cams, counts, keys.get(&r.camera_id).copied(), r);
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
/// camera's current key (`None` once it is deleted, disabled or without a
/// model). A result for an earlier pose still replaces an older visibility, so
/// the overlay follows a dragged camera one job behind, but never an
/// up-to-date one. Returns whether anything changed.
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
    counts.add(&r.visibility);
    if let Some((_, old)) = cams.insert(r.camera_id, (r.key, r.visibility)) {
        counts.remove(&old);
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
    // A theme change can recolour either mode.
    if tokens.is_changed() {
        state.paint = true;
    }
    let mode = (ui.overlay, ui.show_facades);
    // Per-camera colours can change without any visibility changing; repaint
    // only when one actually did.
    let colors = if (ui.overlay == OverlayMode::PerCamera
        && (scene.is_changed() || models.is_changed()))
        || tokens.is_changed()
    {
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
    area: Option<Res<Area>>,
) {
    let terrain = terrain_of(area.as_deref());
    let color = to_bevy(tokens.world.region);
    let at = |p: [f32; 2]| {
        let p = Vec2::from_array(p);
        Vec3::new(p.x, terrain.height_at(p) + REGION_Y_M, p.y)
    };
    // The outline through `points`, laid over the ground between corners.
    let draped = |points: Vec<[f32; 2]>| -> Vec<Vec3> {
        let mut out: Vec<Vec3> = points
            .windows(2)
            .flat_map(|w| terrain.drape(Vec2::from_array(w[0]), Vec2::from_array(w[1]), REGION_Y_M))
            .collect();
        out.extend(points.last().map(|p| at(*p)));
        out
    };
    if let Some(r) = &scene.region
        && ui.region_draft.is_empty()
    {
        let c = if r.is_active() {
            color
        } else {
            color.with_alpha(0.35)
        };
        let ring = r.points.iter().chain(r.points.first()).copied().collect();
        gizmos.linestrip(draped(ring), c);
    }
    if ui.tool != Tool::Region {
        return;
    }
    let draft = &ui.region_draft;
    let path = draft.iter().chain(&ui.region_hover).copied().collect();
    gizmos.linestrip(draped(path), color);
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

/// Gizmo group for the hatching beyond the loaded area: thin, and hidden by
/// buildings like the ground it lies on.
#[derive(Default, Reflect, GizmoConfigGroup)]
struct HatchGizmos;

/// Opacity of the hatching beyond the loaded area.
const HATCH_ALPHA: f32 = 0.6;

/// Side of the squares the hatching is laid out in, metres: each square takes
/// its own line spacing from its distance to the camera.
const HATCH_PATCH_M: f32 = 64.0;

/// While the Region tool is on, marks where corners can go: a solid edge
/// around the loaded area, and the ground beyond it tinted and hatched.
fn draw_outside_area(
    mut edge: Gizmos<RegionGizmos>,
    mut hatching: Gizmos<HatchGizmos>,
    mut outside: Query<(&OutsideArea, &mut Visibility)>,
    camera: Query<&GlobalTransform, With<OrbitCamera>>,
    area: Option<Res<Area>>,
    ui: Res<UiState>,
    tokens: Res<Tokens>,
) {
    let on = ui.tool == Tool::Region;
    for (_, mut vis) in &mut outside {
        vis.set_if_neq(if on {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
    let (true, Some(area), Ok((outside, _)), Ok(eye)) =
        (on, area, outside.single(), camera.single())
    else {
        return;
    };
    let inner = area.bounds();
    let color = to_bevy(tokens.world.outside_area);
    let terrain = &area.bundle.terrain;
    let at = |p: Vec2| Vec3::new(p.x, terrain.height_at(p) + REGION_Y_M, p.y);
    // A line laid over the ground.
    let draped = |a: Vec2, b: Vec2| {
        let mut points = terrain.drape(a, b, REGION_Y_M);
        points.push(at(b));
        points
    };
    let corners = [
        inner.min,
        Vec2::new(inner.max.x, inner.min.y),
        inner.max,
        Vec2::new(inner.min.x, inner.max.y),
    ];
    for (i, &a) in corners.iter().enumerate() {
        edge.linestrip(draped(a, corners[(i + 1) % 4]), color);
    }
    let (eye, c) = (eye.translation(), color.with_alpha(HATCH_ALPHA));
    let outer = outside.outer;
    let first = (outer.min / HATCH_PATCH_M).floor();
    let last = (outer.max / HATCH_PATCH_M).ceil();
    for i in first.x as i32..last.x as i32 {
        for j in first.y as i32..last.y as i32 {
            let min = Vec2::new(i as f32, j as f32) * HATCH_PATCH_M;
            let patch = Rect::from_corners(min, min + HATCH_PATCH_M).intersect(outer);
            if patch.is_empty() || inner.contains(patch.min) && inner.contains(patch.max) {
                continue;
            }
            let spacing = hatch_spacing_m(eye.distance(at(patch.center())));
            hatch(inner, patch, spacing, |a, b| {
                hatching.linestrip(draped(a, b), c)
            });
        }
    }
}

/// Gap between hatch lines, metres: about 2% of the distance from the camera,
/// rounded to a power of two. Lines sit at multiples of the gap, so squares at
/// different gaps share every other line, and zooming adds or drops every
/// other line instead of sliding them all.
fn hatch_spacing_m(distance: f32) -> f32 {
    (distance * 0.02).clamp(2.0, 32.0).log2().round().exp2()
}

/// Diagonal lines `x + z = k * spacing` across `outer`, with `inner` cut out,
/// passed to `line` as segments.
fn hatch(inner: Rect, outer: Rect, spacing: f32, mut line: impl FnMut(Vec2, Vec2)) {
    // The span of `x` where the line `x + z = c` crosses `r`, if it does.
    let span = |r: Rect, c: f32| {
        let (lo, hi) = (r.min.x.max(c - r.max.y), r.max.x.min(c - r.min.y));
        (lo < hi).then_some((lo, hi))
    };
    let point = |c: f32, x: f32| Vec2::new(x, c - x);
    let first = ((outer.min.x + outer.min.y) / spacing).ceil() as i64;
    let last = ((outer.max.x + outer.max.y) / spacing).floor() as i64;
    for k in first..=last {
        let c = k as f32 * spacing;
        let Some((a0, a1)) = span(outer, c) else {
            continue;
        };
        match span(inner, c) {
            Some((b0, b1)) => {
                if a0 < b0 {
                    line(point(c, a0), point(c, b0.min(a1)));
                }
                if b1 < a1 {
                    line(point(c, b1.max(a0)), point(c, a1));
                }
            }
            None => line(point(c, a0), point(c, a1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(inner: Rect, outer: Rect, spacing: f32) -> Vec<(Vec2, Vec2)> {
        let mut out = Vec::new();
        hatch(inner, outer, spacing, |a, b| out.push((a, b)));
        out
    }

    #[test]
    fn hatching_stays_outside_the_area() {
        let inner = Rect::new(-10.0, -10.0, 10.0, 10.0);
        let outer = Rect::new(-30.0, -30.0, 30.0, 30.0);
        let segments = lines(inner, outer, 4.0);
        assert!(!segments.is_empty());
        for (a, b) in segments {
            for p in [a, b] {
                assert!(outer.inflate(1e-3).contains(p), "{p} outside the ground");
            }
            // Neither end nor the middle is strictly inside the area.
            for p in [a, b, (a + b) / 2.0] {
                assert!(!inner.inflate(-1e-3).contains(p), "{p} inside the area");
            }
        }
    }

    #[test]
    fn lines_through_the_area_are_split() {
        let inner = Rect::new(-10.0, -10.0, 10.0, 10.0);
        let outer = Rect::new(-30.0, -30.0, 30.0, 30.0);
        // `x + z = 0` crosses the area's middle: two pieces, each from the
        // ground's corner to the area's.
        let through: Vec<_> = lines(inner, outer, 4.0)
            .into_iter()
            .filter(|(a, b)| (a.x + a.y).abs() < 1e-3 && (b.x + b.y).abs() < 1e-3)
            .collect();
        assert_eq!(
            through,
            [
                (Vec2::new(-30.0, 30.0), Vec2::new(-10.0, 10.0)),
                (Vec2::new(10.0, -10.0), Vec2::new(30.0, -30.0)),
            ]
        );
    }

    #[test]
    fn spacing_follows_distance_in_powers_of_two() {
        assert_eq!(hatch_spacing_m(10.0), 2.0);
        assert_eq!(hatch_spacing_m(400.0), 8.0);
        assert_eq!(hatch_spacing_m(1500.0), 32.0);
        assert_eq!(hatch_spacing_m(5000.0), 32.0);
    }
}
