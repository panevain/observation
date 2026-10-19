//! Orbit camera, pointer/touch handling and tool actions.

use crate::cameras::CameraBody;
use crate::state::{Area, MOUNT_OFFSET_M, Models, Scene, Tool, UiLayout, UiState};
use crate::world::{BuildingMesh, GroundMesh};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings, RayCastVisibility};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::input::EguiWantsInput;
use observation_core::camera::{CameraPlacement, yaw_pitch_from_forward};
use observation_core::coverage::{MAX_REGION_POINTS, Region};

/// Pixels a press may travel and still count as a click/tap.
const CLICK_SLOP_PX: f32 = 5.0;
const MIN_DISTANCE: f32 = 8.0;
const MAX_DISTANCE: f32 = 1200.0;
const ORBIT_SPEED: f32 = 0.006;
/// Near clip distance, metres. WebGL remaps depth to [-1, 1], which costs most
/// of the precision of reverse-Z, so the near plane is kept as far out as the
/// view allows: up to 1 m, which keeps the ground layers apart at range, and
/// down to 0.1 m when the camera is low enough that 1 m would clip the ground.
const NEAR_PLANE_M: std::ops::RangeInclusive<f32> = 0.1..=1.0;

/// Spherical orbit around a focus point.
#[derive(Debug, Component)]
pub struct OrbitCamera {
    pub focus: Vec3,
    /// Radians around +Y.
    pub yaw: f32,
    /// Radians; negative looks down.
    pub pitch: f32,
    pub distance: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            focus: Vec3::ZERO,
            yaw: 0.35,
            pitch: -0.75,
            distance: 300.0,
        }
    }
}

impl OrbitCamera {
    /// Half the camera's height above the ground, within [`NEAR_PLANE_M`]: the
    /// ground at the bottom of the frame is never closer than the height.
    fn near_plane(&self) -> f32 {
        let height = self.focus.y - self.distance * self.pitch.sin();
        (height * 0.5).clamp(*NEAR_PLANE_M.start(), *NEAR_PLANE_M.end())
    }

    fn transform(&self) -> Transform {
        let rot = Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0);
        let pos = self.focus + rot * Vec3::new(0.0, 0.0, self.distance);
        Transform::from_translation(pos).looking_at(self.focus, Vec3::Y)
    }
}

/// What an in-progress press is doing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Gesture {
    #[default]
    None,
    /// Pressed but not yet moved beyond the click slop.
    Pending {
        start: Vec2,
        button: MouseButton,
    },
    Orbit,
    Pan,
    /// Aim/Move tool drag.
    Tool,
}

#[derive(Debug, Default, Resource)]
struct PointerState {
    gesture: Gesture,
    last_touch_mid: Option<Vec2>,
    last_touch_dist: Option<f32>,
    /// Whether the current two-finger gesture started on the UI; decided
    /// once when it starts so it cannot change hands half-way.
    pinch_on_ui: Option<bool>,
    /// Touch id driving a one-finger gesture.
    touch_id: Option<u64>,
    last_pos: Option<Vec2>,
}

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PointerState>()
            .add_systems(Startup, spawn_camera)
            .add_systems(Update, (keyboard, pointer, apply_orbit).chain());
    }
}

fn spawn_camera(mut commands: Commands, mut egui: ResMut<bevy_egui::EguiGlobalSettings>) {
    let orbit = OrbitCamera::default();
    // Khronos PBR Neutral keeps base colours close to the design tokens and,
    // unlike the default, needs no LUT assets (and their zstd decoder).
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            near: orbit.near_plane(),
            ..default()
        }),
        bevy::core_pipeline::tonemapping::Tonemapping::KhronosPbrNeutral,
        Msaa::Sample4,
        orbit.transform(),
        orbit,
    ));
    // egui gets its own full-window overlay camera so that shrinking the 3D
    // viewport to the space between panels doesn't clip the UI.
    egui.auto_create_primary_context = false;
    commands.spawn((
        bevy_egui::PrimaryEguiContext,
        // Hyperlinks (attribution, spec sheets) open in a new tab, keeping the app.
        bevy_egui::EguiContextSettings {
            default_open_url_target: Some("_blank".to_owned()),
            ..default()
        },
        Camera2d,
        bevy::camera::visibility::RenderLayers::none(),
        Camera {
            order: 1,
            output_mode: bevy::camera::CameraOutputMode::Write {
                blend_state: Some(bevy::render::render_resource::BlendState::ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
    ));
}

fn apply_orbit(
    mut q: Query<(&OrbitCamera, &mut Transform, &mut Projection), Changed<OrbitCamera>>,
) {
    for (o, mut t, mut proj) in &mut q {
        *t = o.transform();
        if let Projection::Perspective(p) = &mut *proj {
            let near = o.near_plane();
            if p.near != near {
                p.near = near;
            }
        }
    }
}

fn keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    wants: Res<EguiWantsInput>,
    mut ui: ResMut<UiState>,
    mut scene: ResMut<Scene>,
    mut orbit: Query<&mut OrbitCamera>,
) {
    if wants.wants_any_keyboard_input() {
        return;
    }
    for (key, tool) in [
        (KeyCode::KeyV, Tool::Select),
        (KeyCode::KeyP, Tool::Place),
        (KeyCode::KeyA, Tool::Aim),
        (KeyCode::KeyM, Tool::Move),
        (KeyCode::KeyR, Tool::Region),
    ] {
        if keys.just_pressed(key) {
            ui.tool = tool;
        }
    }
    if keys.just_pressed(KeyCode::Escape) {
        if ui.tool == Tool::Select {
            ui.selected = None;
        }
        ui.tool = Tool::Select;
        ui.map_open = false;
        ui.help_open = false;
    }
    if ui.tool == Tool::Region {
        // Drawing a region: Enter closes it, Backspace undoes a corner.
        if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
            finish_region(&mut ui, &mut scene);
        }
        if keys.just_pressed(KeyCode::Backspace) || keys.just_pressed(KeyCode::Delete) {
            ui.region_draft.pop();
        }
    } else if (keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace))
        && let Some(id) = ui.selected.take()
    {
        scene.remove_camera(id);
    }
    if keys.just_pressed(KeyCode::Slash) || keys.just_pressed(KeyCode::F1) {
        ui.help_open = !ui.help_open;
    }
    if keys.just_pressed(KeyCode::KeyF)
        && let Some(c) = ui.selected.and_then(|id| scene.camera(id))
        && let Ok(mut o) = orbit.single_mut()
    {
        o.focus = Vec3::from_array(c.position).with_y(0.0);
        o.distance = o.distance.min(60.0);
    }
    if keys.just_pressed(KeyCode::KeyN) {
        ui.overlay = match ui.overlay {
            crate::state::OverlayMode::Overlap => crate::state::OverlayMode::PerCamera,
            crate::state::OverlayMode::PerCamera => crate::state::OverlayMode::Overlap,
        };
    }
}

/// A ray-cast hit on something the tools care about.
#[derive(Debug, Clone, Copy)]
struct Hit {
    point: Vec3,
    normal: Vec3,
    target: HitTarget,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum HitTarget {
    Building,
    Ground,
    Camera(u32),
}

/// Raw input sources for [`pointer`].
#[derive(bevy::ecs::system::SystemParam)]
struct RawInput<'w> {
    mouse: Res<'w, ButtonInput<MouseButton>>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    motion: Res<'w, AccumulatedMouseMotion>,
    scroll: Res<'w, AccumulatedMouseScroll>,
    touches: Res<'w, Touches>,
    wants: Res<'w, EguiWantsInput>,
    layout: Res<'w, UiLayout>,
}

/// Entities the tools can hit.
#[derive(bevy::ecs::system::SystemParam)]
struct Targets<'w, 's> {
    buildings: Query<'w, 's, (), With<BuildingMesh>>,
    grounds: Query<'w, 's, (), With<GroundMesh>>,
    bodies: Query<'w, 's, &'static CameraBody>,
    area: Option<Res<'w, Area>>,
}

fn pointer(
    input: RawInput,
    window: Single<&Window, With<PrimaryWindow>>,
    mut cam: Query<(&Camera, &GlobalTransform, &mut OrbitCamera)>,
    mut ray_cast: MeshRayCast,
    targets: Targets,
    mut st: ResMut<PointerState>,
    mut ui: ResMut<UiState>,
    mut scene: ResMut<Scene>,
    models: Res<Models>,
) {
    let RawInput {
        mouse,
        keys,
        motion,
        scroll,
        touches,
        wants,
        layout,
    } = input;
    let Targets {
        buildings,
        grounds,
        bodies,
        area,
    } = targets;
    let Ok((camera, cam_t, mut orbit)) = cam.single_mut() else {
        return;
    };

    let mut cast = |screen: Vec2| -> Option<Hit> {
        // Takes window coordinates: it maps the camera's viewport rect itself.
        let ray = camera.viewport_to_world(cam_t, screen).ok()?;
        let filter = |e: Entity| buildings.contains(e) || grounds.contains(e) || bodies.contains(e);
        let settings = MeshRayCastSettings::default()
            .with_filter(&filter)
            .with_visibility(RayCastVisibility::Any);
        let (e, hit) = ray_cast.cast_ray(ray, &settings).first()?;
        let target = if let Ok(b) = bodies.get(*e) {
            HitTarget::Camera(b.0)
        } else if buildings.contains(*e) {
            HitTarget::Building
        } else {
            HitTarget::Ground
        };
        Some(Hit {
            point: hit.point,
            normal: hit.normal.normalize_or(Vec3::Y),
            target,
        })
    };

    // --- Two-finger touch: pinch zoom + pan (always camera navigation).
    let active: Vec<_> = touches.iter().collect();
    if active.len() >= 2 {
        let (a, b) = (active[0].position(), active[1].position());
        let mid = (a + b) / 2.0;
        let dist = a.distance(b);
        // A pinch with either finger on a panel belongs to the panel.
        let on_ui = *st.pinch_on_ui.get_or_insert_with(|| {
            wants.wants_any_pointer_input() || layout.is_ui(a) || layout.is_ui(b)
        });
        if on_ui {
            st.gesture = Gesture::None;
            st.touch_id = None;
            return;
        }
        if let (Some(lm), Some(ld)) = (st.last_touch_mid, st.last_touch_dist) {
            pan(&mut orbit, mid - lm, window.height());
            if dist > 1.0 {
                orbit.distance = (orbit.distance * ld / dist).clamp(MIN_DISTANCE, MAX_DISTANCE);
            }
        }
        st.last_touch_mid = Some(mid);
        st.last_touch_dist = Some(dist);
        st.gesture = Gesture::None;
        st.touch_id = None;
        return;
    }
    st.last_touch_mid = None;
    st.last_touch_dist = None;
    st.pinch_on_ui = None;

    // --- Unified single pointer (mouse or one finger).
    let (pos, just_pressed, pressed, just_released, button) = if let Some(t) = active.first() {
        let first = st.touch_id != Some(t.id());
        st.touch_id = Some(t.id());
        (Some(t.position()), first, true, false, MouseButton::Left)
    } else if st.touch_id.take().is_some() {
        (st.last_pos, false, false, true, MouseButton::Left)
    } else {
        let button = [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
            .into_iter()
            .find(|b| mouse.pressed(*b) || mouse.just_released(*b))
            .unwrap_or(MouseButton::Left);
        (
            window.cursor_position(),
            mouse.just_pressed(button),
            mouse.pressed(button),
            mouse.just_released(button),
            button,
        )
    };
    let delta = match (pos, st.last_pos) {
        (Some(p), Some(l)) if active.len() == 1 => p - l,
        _ => motion.delta,
    };
    st.last_pos = pos;

    let over_ui = wants.wants_any_pointer_input() || pos.is_some_and(|p| layout.is_ui(p));

    // Scroll zoom.
    if !over_ui && scroll.delta.y != 0.0 {
        let lines = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
        };
        orbit.distance = (orbit.distance * (1.0 - lines * 0.1)).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    if ui.tool != Tool::Region {
        if !ui.region_draft.is_empty() || ui.region_hover.is_some() {
            ui.region_draft.clear();
            ui.region_hover = None;
        }
    } else {
        let hover = pos
            .filter(|_| !over_ui)
            .and_then(|p| ground_point(camera, cam_t, p))
            .map(|p| clamp_to_area(p, area.as_deref()));
        if ui.region_hover != hover {
            ui.region_hover = hover;
        }
        let snap = region_snap_m(orbit.distance);
        if ui.region_snap_m != snap {
            ui.region_snap_m = snap;
        }
    }

    let Some(pos) = pos else { return };
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);

    if just_pressed && !over_ui {
        st.gesture = Gesture::Pending { start: pos, button };
        if button == MouseButton::Left && ui.tool.needs_selection() && ui.selected.is_some() {
            st.gesture = Gesture::Tool;
            if let Some(hit) = cast(pos) {
                apply_tool(&mut ui, &mut scene, &models, hit);
            }
        }
    }

    if pressed {
        match st.gesture {
            Gesture::Pending { start, button } if pos.distance(start) > CLICK_SLOP_PX => {
                st.gesture = match button {
                    MouseButton::Right => Gesture::Pan,
                    _ if shift => Gesture::Pan,
                    _ => Gesture::Orbit,
                };
            }
            Gesture::Orbit => {
                orbit.yaw -= delta.x * ORBIT_SPEED;
                orbit.pitch = (orbit.pitch - delta.y * ORBIT_SPEED).clamp(-1.52, -0.05);
            }
            Gesture::Pan => pan(&mut orbit, delta, window.height()),
            Gesture::Tool => {
                if delta != Vec2::ZERO
                    && let Some(hit) = cast(pos)
                {
                    apply_tool(&mut ui, &mut scene, &models, hit);
                }
            }
            _ => {}
        }
    }

    if just_released {
        if let Gesture::Pending {
            button: MouseButton::Left,
            ..
        } = st.gesture
        {
            if ui.tool == Tool::Region {
                if let Some(p) = ground_point(camera, cam_t, pos) {
                    region_click(&mut ui, &mut scene, clamp_to_area(p, area.as_deref()));
                }
            } else {
                let hit = cast(pos);
                click(&mut ui, &mut scene, &models, hit);
            }
        }
        st.gesture = Gesture::None;
    }
}

/// Where the pointer's ray meets the ground plane, world `[x, z]`.
fn ground_point(camera: &Camera, cam_t: &GlobalTransform, window_pos: Vec2) -> Option<[f32; 2]> {
    let ray = camera.viewport_to_world(cam_t, window_pos).ok()?;
    let dy = ray.direction.y;
    if dy > -1e-4 {
        return None;
    }
    let p = ray.origin + *ray.direction * (-ray.origin.y / dy);
    Some([p.x, p.z])
}

/// How close to the first corner a click closes the region, metres: it grows
/// with the zoom so the corner stays easy to hit, and is drawn around it.
pub fn region_snap_m(view_distance: f32) -> f32 {
    (view_distance * 0.02).clamp(1.0, 10.0)
}

/// Adds a region corner, or closes the region when the first corner is clicked.
fn region_click(ui: &mut UiState, scene: &mut Scene, p: [f32; 2]) {
    let closes = ui.region_draft.len() >= 3
        && ui.region_draft.first().is_some_and(|f| {
            Vec2::from_array(*f).distance(Vec2::from_array(p)) <= ui.region_snap_m
        });
    if closes {
        finish_region(ui, scene);
    } else if ui.region_draft.len() < MAX_REGION_POINTS {
        ui.region_draft.push(p);
    } else {
        ui.error(format!(
            "A region has at most {MAX_REGION_POINTS} corners; click the first corner to close it"
        ));
    }
}

/// Turns the draft into the scene's region (nothing to do without corners).
fn finish_region(ui: &mut UiState, scene: &mut Scene) {
    if ui.region_draft.is_empty() {
        return;
    }
    let region = Region {
        points: std::mem::take(&mut ui.region_draft),
        enabled: true,
    };
    match region.problem() {
        None => {
            ui.info(format!(
                "Coverage now counts only the {:.0} m² region",
                region.area_m2()
            ));
            scene.region = Some(region);
            scene.touch();
            ui.tool = Tool::Select;
        }
        Some(problem) => {
            ui.region_draft = region.points;
            ui.error(problem.message());
        }
    }
}

/// Keeps a region corner on the loaded area, so a click near the horizon
/// cannot put one kilometres away.
fn clamp_to_area(p: [f32; 2], area: Option<&Area>) -> [f32; 2] {
    match area {
        Some(a) => Vec2::from_array(p)
            .clamp(a.layers.grid.min, a.layers.grid.max())
            .to_array(),
        None => p,
    }
}

fn pan(orbit: &mut OrbitCamera, delta: Vec2, window_h: f32) {
    let scale = orbit.distance / window_h.max(1.0) * 1.2;
    let (s, c) = orbit.yaw.sin_cos();
    let right = Vec3::new(c, 0.0, -s);
    let fwd = Vec3::new(-s, 0.0, -c);
    orbit.focus += (-right * delta.x + fwd * delta.y) * scale;
}

/// A click/tap (press + release without dragging).
fn click(ui: &mut UiState, scene: &mut Scene, models: &Models, hit: Option<Hit>) {
    match ui.tool {
        Tool::Select => {
            ui.selected = match hit {
                Some(Hit {
                    target: HitTarget::Camera(id),
                    ..
                }) => Some(id),
                _ => None,
            };
        }
        Tool::Place => {
            let Some(hit) = hit else { return };
            if let HitTarget::Camera(id) = hit.target {
                ui.selected = Some(id);
                return;
            }
            let kind = models.kind_of(&ui.place_model);
            let (position, yaw) = mount(hit, kind.default_mount_height_m());
            let id = scene.add_camera(CameraPlacement {
                id: 0,
                label: String::new(),
                model_id: ui.place_model.clone(),
                position: position.to_array(),
                yaw_deg: yaw.unwrap_or(0.0),
                pitch_deg: kind.default_pitch_deg(),
                roll_deg: 0.0,
                color: None,
                enabled: true,
            });
            ui.selected = Some(id);
            if yaw.is_none() {
                // Placed on the ground: switch to Aim so the next click points it.
                ui.tool = Tool::Aim;
                ui.info("Click where the new camera should point");
            }
        }
        // Handled by `region_click`.
        Tool::Region => {}
        Tool::Aim | Tool::Move => {
            if ui.selected.is_none()
                && let Some(Hit {
                    target: HitTarget::Camera(id),
                    ..
                }) = hit
            {
                ui.selected = Some(id);
            }
        }
    }
}

fn apply_tool(ui: &mut UiState, scene: &mut Scene, models: &Models, hit: Hit) {
    let Some(id) = ui.selected else { return };
    let tool = ui.tool;
    let Some(cam) = scene.camera_mut(id) else {
        return;
    };
    match tool {
        Tool::Aim => {
            if !matches!(hit.target, HitTarget::Camera(c) if c == id) {
                cam.look_at(hit.point);
            }
        }
        Tool::Move => {
            if matches!(hit.target, HitTarget::Camera(_)) {
                return;
            }
            let kind = models.kind_of(&cam.model_id);
            let (position, yaw) = mount(hit, kind.default_mount_height_m());
            cam.position = position.to_array();
            if let Some(y) = yaw {
                cam.yaw_deg = y;
            }
        }
        _ => {}
    }
}

/// Mount position for a hit: on a wall, just in front of the surface facing
/// outward; on the ground or a roof, raised to the mount height.
fn mount(hit: Hit, mount_height: f32) -> (Vec3, Option<f32>) {
    let wall = hit.target == HitTarget::Building && hit.normal.y.abs() < 0.5;
    if wall {
        let n = hit.normal.with_y(0.0).normalize_or(Vec3::X);
        let yaw = yaw_pitch_from_forward(n).map(|(y, _)| y);
        (hit.point + n * MOUNT_OFFSET_M, yaw)
    } else {
        (hit.point + Vec3::Y * mount_height, None)
    }
}
