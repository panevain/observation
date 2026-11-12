//! Orbit camera, pointer/touch handling and tool actions.

use crate::addresses::address_label;
use crate::cameras::CameraBody;
use crate::state::{
    AddressJump, Area, Editor, Hover, Models, Panels, Scene, Toasts, Tool, TrackScene, UiLayout,
    UiState, terrain_of,
};
use crate::world::{AreaLoadSet, BuildingMesh, GROUND_MARGIN_M};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::math::Affine3A;
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings, RayCastVisibility};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::input::EguiWantsInput;
use observation_client_model::commands::{self, EditCtx, Hit, HitTarget, Key};
use observation_core::address::{Address, ON_OUTLINE_M};
use observation_core::terrain::Terrain;

/// Pixels a press may travel and still count as a click/tap.
const CLICK_SLOP_PX: f32 = 5.0;
/// How far from a camera body's centre a click still picks it, in pixels.
/// Bodies are only a few pixels across at street zoom.
const PICK_RADIUS_MOUSE_PX: f32 = 14.0;
/// As [`PICK_RADIUS_MOUSE_PX`], for a tap: about a 44 px fingertip target.
const PICK_RADIUS_TOUCH_PX: f32 = 22.0;
const MIN_DISTANCE: f32 = 8.0;
const MAX_DISTANCE: f32 = 1200.0;
const ORBIT_SPEED: f32 = 0.006;
/// Near clip distance, metres. WebGL remaps depth to [-1, 1], which costs most
/// of the precision of reverse-Z, so the near plane is kept as far out as the
/// view allows: up to 1 m, which keeps the ground layers apart at range, and
/// down to 0.1 m when the camera is low enough that 1 m would clip the ground.
const NEAR_PLANE_M: std::ops::RangeInclusive<f32> = 0.1..=1.0;

/// Orbit pitch limits, radians: from nearly straight down to just above level.
const MIN_PITCH: f32 = -1.52;
const MAX_PITCH: f32 = -0.05;
/// Least height of the orbit camera above the ground under it, metres.
const EYE_CLEARANCE_M: f32 = 1.5;
/// Farthest a pointer ray is followed to the ground, metres.
const MAX_GROUND_RAY_M: f32 = 5_000.0;

/// Spherical orbit around a focus point, which stays on the ground.
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
    /// Half the camera's height above the ground at the focus, within
    /// [`NEAR_PLANE_M`]: the ground at the bottom of the frame is never closer
    /// than the height.
    fn near_plane(&self) -> f32 {
        let height = -self.distance * self.pitch.sin();
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
    /// What the Address tool's card was last looked up for, and the address
    /// found, so a click on a settled pointer need not cast again.
    address_key: Option<AddressKey>,
    address_found: Option<Address>,
}

/// Everything the address under the pointer depends on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AddressKey {
    pos: Vec2,
    view: Affine3A,
}

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PointerState>()
            .add_systems(Startup, spawn_camera)
            // After an area load, so the pointer never reads the old area
            // once the address state has been cleared for the new one.
            .add_systems(
                Update,
                (
                    keyboard,
                    pointer.after(AreaLoadSet),
                    focus_on_ground,
                    apply_orbit,
                )
                    .chain(),
            )
            // Undo and import replace the custom models (and the inspector
            // deletes them, after the frame's input): a click must never place
            // a camera with a model that is gone.
            .add_systems(
                Update,
                crate::state::forget_missing_place_model
                    .run_if(resource_changed::<Models>)
                    .after(crate::history::record)
                    .after(crate::persist::receive_imports)
                    .before(pointer),
            );
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

/// Keeps the orbit focus on the ground as it pans over the terrain, and the
/// eye above the ground: looking along a slope, the lowest pitch would put
/// it inside the hill behind it, so the view tilts down instead.
fn focus_on_ground(area: Option<Res<Area>>, mut q: Query<&mut OrbitCamera>) {
    let Some(area) = area else { return };
    let terrain = &area.bundle.terrain;
    for mut o in &mut q {
        let f = o.focus;
        let y = terrain.height_at(Vec2::new(f.x, f.z));
        if (f.y - y).abs() > 1e-3 {
            o.focus.y = y;
        }
        // The ground under the eye moves as the pitch changes, so settle it
        // in a few rounds.
        let mut pitch = o.pitch;
        for _ in 0..3 {
            let eye = OrbitCamera { pitch, ..*o }.transform().translation;
            let floor = terrain.height_at(Vec2::new(eye.x, eye.z)) + EYE_CLEARANCE_M;
            if eye.y >= floor - 1e-3 {
                break;
            }
            let sin = ((o.focus.y - floor) / o.distance).clamp(-1.0, 1.0);
            pitch = sin.asin().clamp(MIN_PITCH, MAX_PITCH);
        }
        if pitch != o.pitch {
            o.pitch = pitch;
        }
    }
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
    mut editor: ResMut<Editor>,
    mut panels: ResMut<Panels>,
    mut toasts: ResMut<Toasts>,
    mut scene: ResMut<Scene>,
    mut orbit: Query<&mut OrbitCamera>,
) {
    if wants.wants_any_keyboard_input() {
        return;
    }
    let pressed = |key| key_codes(key).iter().any(|k| keys.just_pressed(*k));
    let mut ctx = EditCtx {
        doc: &mut TrackScene(&mut scene),
        editor: &mut editor,
        panels: &mut panels,
        toasts: &mut toasts,
    };
    if commands::keys(&mut ctx, &pressed, browser_modifier(&keys)) {
        ui.focus_request = true;
    }
    if std::mem::take(&mut ui.focus_request)
        && let Some(c) = editor.selected.and_then(|id| scene.camera(id))
        && let Ok(mut o) = orbit.single_mut()
    {
        // `focus_on_ground` puts it on the ground under the camera.
        o.focus = Vec3::from_array(c.position).with_y(0.0);
        o.distance = o.distance.min(60.0);
    }
}

/// Whether Ctrl, Cmd (Super) or Alt is held: the tool and panel shortcuts
/// then leave the key to the browser. Shift is not one of them.
fn browser_modifier(keys: &ButtonInput<KeyCode>) -> bool {
    keys.any_pressed([
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
        KeyCode::AltLeft,
        KeyCode::AltRight,
    ])
}

/// The keys of a shortcut.
fn key_codes(key: Key) -> &'static [KeyCode] {
    match key {
        Key::Tool(Tool::Select) => &[KeyCode::KeyV],
        Key::Tool(Tool::Place) => &[KeyCode::KeyP],
        Key::Tool(Tool::Aim) => &[KeyCode::KeyA],
        Key::Tool(Tool::Move) => &[KeyCode::KeyM],
        Key::Tool(Tool::Region) => &[KeyCode::KeyR],
        Key::Tool(Tool::Address) => &[KeyCode::KeyL],
        Key::Escape => &[KeyCode::Escape],
        Key::Finish => &[KeyCode::Enter, KeyCode::NumpadEnter],
        Key::Erase => &[KeyCode::Backspace, KeyCode::Delete],
        Key::Help => &[KeyCode::Slash, KeyCode::F1],
        Key::Focus => &[KeyCode::KeyF],
        Key::Overlay => &[KeyCode::KeyN],
    }
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
    bodies: Query<'w, 's, (&'static CameraBody, &'static GlobalTransform)>,
    area: Option<Res<'w, Area>>,
}

#[allow(clippy::too_many_arguments)]
fn pointer(
    input: RawInput,
    window: Single<&Window, With<PrimaryWindow>>,
    mut cam: Query<(&Camera, &GlobalTransform, &mut OrbitCamera)>,
    mut ray_cast: MeshRayCast,
    targets: Targets,
    mut st: ResMut<PointerState>,
    mut ui: ResMut<UiState>,
    mut editor: ResMut<Editor>,
    mut panels: ResMut<Panels>,
    mut hovering: ResMut<Hover>,
    mut toasts: ResMut<Toasts>,
    mut scene: ResMut<Scene>,
    models: Res<Models>,
    jump: Res<AddressJump>,
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
        bodies,
        area,
    } = targets;
    let terrain = terrain_of(area.as_deref());
    // The ground the tiles cover; a ray past it meets nothing.
    let ground_rect = area.as_deref().map(|a| {
        (
            a.layers.grid.min - GROUND_MARGIN_M,
            a.layers.grid.max() + GROUND_MARGIN_M,
        )
    });
    let Ok((camera, cam_t, mut orbit)) = cam.single_mut() else {
        return;
    };

    // `bodies_only` tests just the camera bodies: a cheap first pass for
    // hover, where testing every building triangle on each mouse move is not
    // worth it unless a body is under the pointer at all. The ground is the
    // terrain itself rather than a mesh, out to the edge of the tiles.
    let mut cast = |screen: Vec2, bodies_only: bool| -> Option<Hit> {
        // Takes window coordinates: it maps the camera's viewport rect itself.
        let ray = camera.viewport_to_world(cam_t, screen).ok()?;
        let filter = |e: Entity| bodies.contains(e) || (!bodies_only && buildings.contains(e));
        let settings = MeshRayCastSettings::default()
            .with_filter(&filter)
            .with_visibility(RayCastVisibility::Any);
        let mesh = ray_cast.cast_ray(ray, &settings).first().map(|(e, hit)| {
            let target = match bodies.get(*e) {
                Ok((b, _)) => HitTarget::Camera(b.0),
                Err(_) => HitTarget::Building,
            };
            Hit {
                point: hit.point,
                normal: hit.normal.normalize_or(Vec3::Y),
                target,
            }
        });
        let ground = (!bodies_only)
            .then(|| ground_hit(terrain, ground_rect, ray.origin, *ray.direction))
            .flatten()
            .map(|point| Hit {
                point,
                normal: terrain.normal_at(Vec2::new(point.x, point.z)),
                target: HitTarget::Ground,
            });
        match (mesh, ground) {
            (Some(m), Some(g)) => {
                let d = |h: &Hit| h.point.distance_squared(ray.origin);
                Some(if d(&g) < d(&m) { g } else { m })
            }
            (m, g) => m.or(g),
        }
    };

    // The camera body nearest `screen` whose centre is within `radius` px. Its
    // centre is cast to again, so a body behind a building is not picked.
    let near_body = |cast: &mut dyn FnMut(Vec2, bool) -> Option<Hit>,
                     screen: Vec2,
                     radius: f32|
     -> Option<Hit> {
        let (id, centre) = bodies
            .iter()
            .filter_map(|(b, t)| {
                let p = camera.world_to_viewport(cam_t, t.translation()).ok()?;
                (p.distance_squared(screen) <= radius * radius).then_some((b.0, p))
            })
            .min_by(|a, b| {
                a.1.distance_squared(screen)
                    .total_cmp(&b.1.distance_squared(screen))
            })?;
        cast(centre, false).filter(|h| h.target == HitTarget::Camera(id))
    };

    // The address under `screen`. The ray meets the buildings, so pointing
    // at a roof or a wall gives that building's own address, not the ground
    // behind it or a neighbour's.
    let address_under =
        |cast: &mut dyn FnMut(Vec2, bool) -> Option<Hit>, screen: Vec2| -> Option<Address> {
            let area = area.as_deref()?;
            let at = cast(screen, false)
                .map(|h| [h.point.x, h.point.z])
                .or_else(|| ground_point(camera, cam_t, screen, terrain))?;
            area.bundle.address_pointed_at(at, ON_OUTLINE_M).cloned()
        };
    if area.as_ref().is_some_and(|a| a.is_changed()) {
        st.address_key = None;
    }

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
    let touch = !active.is_empty() || st.touch_id.is_some();
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

    if editor.tool != Tool::Region {
        if !editor.region_draft.is_empty() || editor.region_hover.is_some() {
            editor.region_draft.clear();
            editor.region_hover = None;
        }
    } else {
        let hover = pos
            .filter(|_| !over_ui)
            .and_then(|p| ground_point(camera, cam_t, p, terrain))
            .map(|p| clamp_to_area(p, area.as_deref()));
        if editor.region_hover != hover {
            editor.region_hover = hover;
        }
        let snap = region_snap_m(orbit.distance);
        if editor.region_snap_m != snap {
            editor.region_snap_m = snap;
        }
    }

    if editor.tool != Tool::Address && editor.selected_address.is_some() {
        editor.selected_address = None;
    }

    let Some(pos) = pos else {
        if hovering.hover_scene.is_some() {
            hovering.hover_scene = None;
        }
        if hovering.hover_address.is_some() {
            hovering.hover_address = None;
        }
        st.address_key = None;
        return;
    };
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);

    // The camera under an idle mouse, so its row in the list lights up. A
    // body hit is confirmed with the full cast, so one hidden behind a
    // building does not light up.
    if !pressed && active.is_empty() && delta != Vec2::ZERO {
        let camera_at = |h: Option<Hit>| match h?.target {
            HitTarget::Camera(id) => Some(id),
            _ => None,
        };
        let hover = if over_ui {
            None
        } else if camera_at(cast(pos, true)).is_some() {
            camera_at(cast(pos, false))
        } else if editor.tool != Tool::Place {
            camera_at(near_body(&mut cast, pos, PICK_RADIUS_MOUSE_PX))
        } else {
            None
        };
        if hovering.hover_scene != hover {
            hovering.hover_scene = hover;
        }
    }

    // The address under an idle mouse, looked up again only when the
    // pointer, the view or the area changes. Hidden while dragging and over
    // the panels.
    if editor.tool == Tool::Address && active.is_empty() && !pressed && !over_ui {
        let key = AddressKey {
            pos,
            view: cam_t.affine(),
        };
        if st.address_key != Some(key) {
            st.address_key = Some(key);
            st.address_found = address_under(&mut cast, pos);
            let hover = Some(st.address_found.as_ref().map(address_label));
            if hovering.hover_address != hover {
                hovering.hover_address = hover;
            }
        }
    } else {
        st.address_key = None;
        if hovering.hover_address.is_some() {
            hovering.hover_address = None;
        }
    }

    if just_pressed && !over_ui {
        st.gesture = Gesture::Pending { start: pos, button };
        if button == MouseButton::Left && editor.tool.needs_selection() && editor.selected.is_some()
        {
            st.gesture = Gesture::Tool;
            if let Some(hit) = cast(pos, false) {
                commands::apply_tool(&mut editor, &mut scene, &models, hit);
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
                orbit.pitch = (orbit.pitch - delta.y * ORBIT_SPEED).clamp(MIN_PITCH, MAX_PITCH);
            }
            Gesture::Pan => pan(&mut orbit, delta, window.height()),
            Gesture::Tool => {
                if delta != Vec2::ZERO
                    && let Some(hit) = cast(pos, false)
                {
                    commands::apply_tool(&mut editor, &mut scene, &models, hit);
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
            if editor.tool == Tool::Region {
                if let Some(p) = ground_point(camera, cam_t, pos, terrain) {
                    commands::region_click(
                        &mut EditCtx {
                            doc: &mut **scene,
                            editor: &mut editor,
                            panels: &mut panels,
                            toasts: &mut toasts,
                        },
                        clamp_to_area(p, area.as_deref()),
                    );
                }
            } else if editor.tool == Tool::Address {
                // A click reuses the card's lookup for the same spot; a tap
                // looks up afresh. A click away from any address clears
                // the selection.
                let here = AddressKey {
                    pos,
                    view: cam_t.affine(),
                };
                let picked = if st.address_key == Some(here) {
                    st.address_found.clone()
                } else {
                    address_under(&mut cast, pos)
                };
                ui.jump_to_address = jump.0 && picked.is_some();
                editor.selected_address = picked;
            } else {
                let mut hit = cast(pos, false);
                // Placing near a camera should still place, so only the
                // other tools reach for a body the press just missed.
                if editor.tool != Tool::Place
                    && !matches!(
                        hit,
                        Some(Hit {
                            target: HitTarget::Camera(_),
                            ..
                        })
                    )
                {
                    let radius = if touch {
                        PICK_RADIUS_TOUCH_PX
                    } else {
                        PICK_RADIUS_MOUSE_PX
                    };
                    hit = near_body(&mut cast, pos, radius).or(hit);
                }
                commands::click(
                    &mut EditCtx {
                        doc: &mut **scene,
                        editor: &mut editor,
                        panels: &mut panels,
                        toasts: &mut toasts,
                    },
                    &models,
                    hit,
                );
            }
        }
        st.gesture = Gesture::None;
    }
}

/// Where the pointer's ray meets the ground, world `[x, z]`, however far out
/// (beyond the terrain grid the ground keeps its edge height).
fn ground_point(
    camera: &Camera,
    cam_t: &GlobalTransform,
    window_pos: Vec2,
    terrain: &Terrain,
) -> Option<[f32; 2]> {
    let ray = camera.viewport_to_world(cam_t, window_pos).ok()?;
    let p = terrain.ray_hit(ray.origin, *ray.direction, MAX_GROUND_RAY_M)?;
    Some([p.x, p.z])
}

/// Where a ray meets the ground inside `rect` (`x, z` min and max), if it does.
fn ground_hit(
    terrain: &Terrain,
    rect: Option<(Vec2, Vec2)>,
    origin: Vec3,
    dir: Vec3,
) -> Option<Vec3> {
    let (lo, hi) = rect?;
    let p = terrain.ray_hit(origin, dir, MAX_GROUND_RAY_M)?;
    let xz = Vec2::new(p.x, p.z);
    (xz.cmpge(lo).all() && xz.cmple(hi).all()).then_some(p)
}

/// How close to the first corner a click closes the region, metres: it grows
/// with the zoom so the corner stays easy to hit, and is drawn around it.
pub fn region_snap_m(view_distance: f32) -> f32 {
    (view_distance * 0.02).clamp(1.0, 10.0)
}

/// Keeps a region corner on the loaded area, so a click near the horizon
/// cannot put one kilometres away.
fn clamp_to_area(p: [f32; 2], area: Option<&Area>) -> [f32; 2] {
    match area {
        Some(a) => {
            let r = a.bounds();
            Vec2::from_array(p).clamp(r.min, r.max).to_array()
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use observation_client_model::commands::Key;

    #[test]
    fn ctrl_cmd_and_alt_count_as_browser_modifiers_but_shift_does_not() {
        for (key, expected) in [
            (KeyCode::ControlLeft, true),
            (KeyCode::ControlRight, true),
            (KeyCode::SuperLeft, true),
            (KeyCode::SuperRight, true),
            (KeyCode::AltLeft, true),
            (KeyCode::AltRight, true),
            (KeyCode::ShiftLeft, false),
            (KeyCode::ShiftRight, false),
        ] {
            let mut keys = ButtonInput::<KeyCode>::default();
            keys.press(key);
            keys.press(KeyCode::KeyR);
            assert_eq!(browser_modifier(&keys), expected, "{key:?}");
        }
    }

    /// The page hands these keys to the browser while a modifier is held;
    /// its list must match the keys of the shortcuts that then do nothing.
    #[test]
    fn the_page_leaves_exactly_the_yielding_shortcut_keys_to_the_browser() {
        let page = include_str!("../../../web/index.html");
        let line = page
            .lines()
            .find(|l| l.contains("const BROWSER_KEYS"))
            .expect("web/index.html lists BROWSER_KEYS");
        let mut listed: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
        let mut expected: Vec<String> = Tool::ALL
            .into_iter()
            .map(Key::Tool)
            .chain([
                Key::Escape,
                Key::Finish,
                Key::Erase,
                Key::Help,
                Key::Focus,
                Key::Overlay,
            ])
            .filter(|k| k.yields_to_modifiers())
            .flat_map(|k| key_codes(k).iter().map(|c| format!("{c:?}")))
            .collect();
        listed.sort_unstable();
        expected.sort_unstable();
        assert_eq!(listed, expected);
    }
}
