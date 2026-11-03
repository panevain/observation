//! Incremental coverage computation and overlay painting.
//!
//! Each camera's sight lines are cached under a key derived from everything
//! that affects them, so moving one camera only recomputes that camera. They
//! are computed once, out to the camera's longest range over the detail
//! levels, with each cell's distance, so every detail level and Day/Night
//! follows by distance alone: switching them recomputes nothing. The
//! computing itself happens in [`backend`]: a Web Worker in the browser.

mod backend;

use crate::input::OrbitCamera;
use crate::state::{Area, Models, OverlayMode, Scene, Tool, UiState, terrain_of};
use crate::theme::{Tokens, rgba, to_bevy};
use crate::world::{AreaLoadSet, CLEAR, FacadeOverlay, OutsideArea, Overlay, texel, write_overlay};
use backend::CoverageBackend;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use observation_core::camera::{CameraModel, CameraPlacement, DoriLevel};
use observation_core::coverage::{
    CoverageParams, CoverageStats, RegionMask, Sight, StaticLayers, Tally,
};
use observation_core::coverage_job::JobResult;
use observation_core::frustum::ViewCone;
use std::hash::{Hash, Hasher};

#[derive(Default, Resource)]
pub struct CoverageState {
    /// Each camera's cache key and sight lines.
    cams: HashMap<u32, (u64, Sight)>,
    /// One per [`CoverageParams::ladder`] level, under the current lighting.
    rungs: Vec<Rung>,
    /// Index into `rungs` of the selected detail level.
    selected: usize,
    /// The figures at the selected detail level.
    pub stats: CoverageStats,
    /// Range used for each camera at the selected detail level (for the
    /// inspector), metres.
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

/// The cells seen at one detail level, under the current lighting.
struct Rung {
    level: DoriLevel,
    tally: Tally,
    /// The range each camera's cells were counted out to, metres.
    counted: HashMap<u32, f32>,
    stats: CoverageStats,
    /// The counts changed since `stats` were taken.
    changed: bool,
}

/// What the counts are limited to: the loaded area's layers, and the region
/// when the figures are limited to it.
#[derive(Clone, Copy)]
struct Scope<'a> {
    layers: &'a StaticLayers,
    region: Option<&'a RegionMask>,
}

impl Rung {
    fn new(level: DoriLevel, scope: Scope) -> Self {
        Self {
            level,
            tally: Tally::new(scope.layers, scope.region),
            counted: HashMap::default(),
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

    /// Stops counting a camera everywhere and forgets its sight lines.
    fn forget(&mut self, scope: Scope, id: u32) {
        if let Some((_, sight)) = self.cams.remove(&id) {
            for r in &mut self.rungs {
                r.uncount(scope, id, &sight);
            }
        }
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
    if results.is_empty() && !scene.is_changed() && !models.is_changed() && !state.rungs.is_empty()
    {
        return;
    }
    let state = &mut *state;
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
    let mut keys = HashMap::new();
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
    let Some(counts) = state.rungs.get(state.selected).map(|r| &r.tally.counts) else {
        return;
    };
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
            let Some((seen_ground, seen_facade)) = state.seen(c.id) else {
                continue;
            };
            let mut col = texel(crate::cameras::camera_color(&scene, c, &models, &tokens));
            col[3] = 150;
            for i in seen_ground {
                ground[i as usize] = col;
            }
            for i in seen_facade {
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

    use crate::state::{FACADE_CELL_M, GROUND_CELL_M, Models};
    use observation_core::bundle::{OsmRoad, build_bundle};
    use observation_core::camera::{CameraKind, Catalog, Lighting};
    use observation_core::coverage::{CoverageCounts, Region};
    use observation_core::geo::{BBox, LatLon};
    use observation_core::scene::AreaRef;
    use observation_core::terrain::Terrain;

    /// An app running [`update_coverage`] on a straight street with two
    /// cameras looking along it, computing inline.
    fn app() -> App {
        let bbox = BBox::new(51.5, -0.001, 51.5009, 0.001).unwrap();
        let road = OsmRoad {
            osm_id: 1,
            highway: "residential".into(),
            name: None,
            width_m: 8.0,
            points: vec![LatLon::new(51.50045, -0.001), LatLon::new(51.50045, 0.001)],
        };
        let bundle = build_bundle(bbox, &[], &[road], &[], Terrain::default(), "", 0, None);
        let layers = StaticLayers::new(&bundle, GROUND_CELL_M, FACADE_CELL_M);
        let model = CameraModel {
            id: "test".into(),
            vendor: String::new(),
            name: "Test".into(),
            kind: CameraKind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 55.0,
            resolution: [1920, 1080],
            ir_range_m: Some(12.0),
            source: None,
            notes: None,
        };
        let mut models = Models::new(Catalog::default());
        models.custom.push(model);
        let camera = |id, x: f32, yaw_deg| CameraPlacement {
            id,
            label: String::new(),
            model_id: "test".into(),
            position: [x, 3.0, 4.0],
            yaw_deg,
            pitch_deg: -10.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        };
        let mut scene = Scene::default();
        scene.cameras = vec![camera(1, -60.0, 90.0), camera(2, 60.0, 270.0)];
        let mut app = App::new();
        app.insert_resource(scene)
            .insert_resource(models)
            .insert_resource(Area {
                area: AreaRef::Demo,
                bundle,
                layers,
            })
            .init_resource::<CoverageState>()
            .init_non_send::<CoverageBackend>()
            .add_systems(Update, update_coverage);
        app.update();
        app
    }

    fn submitted(app: &App) -> usize {
        app.world().non_send::<CoverageBackend>().submitted
    }

    /// The figures for the current settings, computed from scratch.
    fn direct(app: &App) -> CoverageStats {
        let w = app.world();
        let (scene, models, layers) = (
            w.resource::<Scene>(),
            w.resource::<Models>(),
            &w.resource::<Area>().layers,
        );
        let mut counts = CoverageCounts::new(layers);
        for c in &scene.cameras {
            let m = models.get(&c.model_id).unwrap();
            counts.add(&layers.visibility(
                &cone_for(c, m, &scene.params),
                scene.params.subject_height_m,
            ));
        }
        let mask = scene
            .region
            .as_ref()
            .filter(|r| r.is_active())
            .map(|r| RegionMask::new(layers, r));
        CoverageStats::compute_in(layers, &counts, mask.as_ref())
    }

    fn set(app: &mut App, f: impl FnOnce(&mut CoverageParams)) {
        f(&mut app.world_mut().resource_mut::<Scene>().params);
        app.update();
    }

    #[test]
    fn levels_and_lighting_switch_without_recomputing() {
        let mut app = app();
        assert_eq!(submitted(&app), 2);
        let stats = |app: &App| app.world().resource::<CoverageState>().stats;
        let first = stats(&app);
        assert!(first.street_fraction() > 0.1, "{first:?}");
        assert_eq!(first, direct(&app));

        let mut ladder = Vec::new();
        for lighting in [Lighting::Day, Lighting::Night] {
            for level in [
                DoriLevel::Identify,
                DoriLevel::Observe,
                DoriLevel::Recognise,
                DoriLevel::Detect,
            ] {
                set(&mut app, |p| {
                    p.dori = level;
                    p.lighting = lighting;
                });
                assert_eq!(stats(&app), direct(&app), "{level:?} {lighting:?}");
                // The ladder gives the same figures without selecting a level.
                let rung = app
                    .world()
                    .resource::<CoverageState>()
                    .ladder()
                    .find(|(l, _)| *l == level)
                    .map(|(_, s)| *s);
                assert_eq!(rung, Some(stats(&app)));
                ladder.push(stats(&app).street_fraction());
            }
        }
        assert_eq!(submitted(&app), 2, "a level or lighting change recomputed");
        // Detect by day sees the most, and coverage falls as the level rises.
        assert!(ladder[3] > ladder[2] && ladder[2] > ladder[0], "{ladder:?}");

        // A Custom density within the computed range needs no recompute
        // either; a lower one, reaching farther, does (once per camera).
        set(&mut app, |p| {
            p.dori = DoriLevel::Custom;
            p.lighting = Lighting::Day;
            p.custom_ppm = 40.0;
        });
        assert_eq!(submitted(&app), 2);
        assert_eq!(stats(&app), direct(&app));
        assert_eq!(app.world().resource::<CoverageState>().ladder().count(), 5);
        set(&mut app, |p| p.custom_ppm = 10.0);
        assert_eq!(submitted(&app), 4);
        assert_eq!(stats(&app), direct(&app));
        // That computed down to 5 px/m, so lowering it on to 7 does not.
        set(&mut app, |p| p.custom_ppm = 7.0);
        assert_eq!(submitted(&app), 4);
        assert_eq!(stats(&app), direct(&app));

        // The subject height moves the sample points: every camera again.
        set(&mut app, |p| p.subject_height_m = 1.6);
        assert_eq!(submitted(&app), 6);
        assert_eq!(stats(&app), direct(&app));

        // Limiting the figures to a region, or lifting the limit, recounts
        // every level without recomputing.
        let whole = stats(&app);
        app.world_mut().resource_mut::<Scene>().region = Some(Region {
            points: vec![[-80.0, -50.0], [0.0, -50.0], [0.0, 50.0], [-80.0, 50.0]],
            enabled: true,
        });
        app.update();
        let west = stats(&app);
        assert_eq!(west, direct(&app));
        assert!(west.street_m2 < whole.street_m2 && west.street_seen_m2 > 0.0);
        set(&mut app, |p| p.dori = DoriLevel::Identify);
        assert_eq!(stats(&app), direct(&app));
        app.world_mut()
            .resource_mut::<Scene>()
            .region
            .as_mut()
            .unwrap()
            .enabled = false;
        set(&mut app, |p| p.dori = DoriLevel::Custom);
        assert_eq!(stats(&app), whole);
        assert_eq!(submitted(&app), 6);
    }

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
