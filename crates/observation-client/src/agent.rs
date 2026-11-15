//! The link to a local agent (see [`observation_client_model::agent`]): the
//! resource, keeping it up to date, Share with agent (the `S` key over the
//! ground), carrying out the agent's view commands (see
//! [`observation_client_model::agent_view`]), and the ghost of the agent's
//! proposal and the agent's mark in the 3D view. The Connect agent dialog,
//! the proposal card, the mark's note and the request to load an area are
//! `ui::agent`.
//!
//! The link's systems run in `Last`, after everything that edits the scene
//! in the frame, so what is posted and checked is the frame's final state,
//! and no system in `Update` has to be ordered against them. The ghost is
//! drawn in `PostUpdate`, after every edit too and before the gizmos are
//! collected at the end of the frame.

use crate::coverage::cone_for;
use crate::input::{OrbitCamera, look_at};
use crate::net::ServerConfig;
use crate::persist::SceneSlots;
use crate::platform::Platform;
use crate::state::{Area, Editor, Models, Panels, Scene, Toasts, UiLayout, terrain_of};
use crate::theme::{Tokens, to_bevy};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::input::EguiWantsInput;
use observation_client_model::agent::{AgentLink, SceneKey, StateKey, shared_state};
use observation_client_model::agent_view::{MarkAt, ViewAction};
use observation_client_model::editor::select;
use observation_core::footage::TargetPlace;
use observation_core::scene::changes::Change;

/// Farthest a Share with agent ray looks for the ground, metres.
const MAX_SHARE_RAY_M: f32 = 5_000.0;
/// Where the dialog's "Let the agent move the view and selection" is kept
/// in this browser: `on` or `off` (on when unset).
pub const VIEW_COMMANDS_KEY: &str = "observation.agent_view";

/// The link to a local agent, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct AgentLinkRes(AgentLink);

pub struct AgentPlugin;

impl Plugin for AgentPlugin {
    fn build(&self, app: &mut App) {
        let allow_view = Platform::of(app.world())
            .local
            .get(VIEW_COMMANDS_KEY)
            .as_deref()
            != Some("off");
        let mut link = AgentLink::default();
        link.allow_view = allow_view;
        app.insert_resource(AgentLinkRes(link)).add_systems(
            Last,
            (
                offer.run_if(resource_changed::<ServerConfig>),
                share_pointed_ground,
                keep_up,
                follow_commands,
                remember_view_choice,
            )
                .chain(),
        );
    }
}

/// The ghost of the agent's proposal in the 3D view. Apart from
/// [`AgentPlugin`] because it needs Bevy's gizmos.
pub struct ProposalGhostPlugin;

impl Plugin for ProposalGhostPlugin {
    fn build(&self, app: &mut App) {
        app.insert_gizmo_config(
            ProposalGizmos,
            GizmoConfig {
                line: GizmoLineConfig {
                    width: 2.5,
                    ..default()
                },
                // Over buildings, like the region outline: a ghost behind
                // a house still shows where it would go.
                depth_bias: -1.0,
                ..default()
            },
        )
        .add_systems(PostUpdate, (draw_proposal, draw_mark));
    }
}

/// Offers Connect agent when the server says the bridge is there for this
/// browser.
fn offer(config: Res<ServerConfig>, mut link: ResMut<AgentLinkRes>) {
    if link.available != config.agent_bridge {
        link.available = config.agent_bridge;
    }
}

/// Takes in the bridge's replies and events, posts the state when it
/// changed, and keeps the agent's proposal checked against the scene.
#[allow(clippy::too_many_arguments)]
fn keep_up(
    time: Res<Time<Real>>,
    mut link: ResMut<AgentLinkRes>,
    scene: Res<Scene>,
    models: Res<Models>,
    editor: Res<Editor>,
    slots: Res<SceneSlots>,
    platform: Res<Platform>,
    mut toasts: ResMut<Toasts>,
) {
    if !link.is_open() {
        return;
    }
    let name = slots.index.active_name();
    let http = &*platform.http;
    // Cheap to compare: the state is built only when one of these changed.
    let key = StateKey {
        revision: scene.revision,
        models: u64::from(models.last_changed().get()),
        scene_name: name.to_owned(),
        selected: editor.selected,
    };
    link.update(http, time.delta_secs(), key, |pinned| {
        shared_state(&scene, &models, name, editor.selected, pinned)
    });
    link.follow_proposal(http, &scene, &models);
    for notice in link.take_notices() {
        toasts.info(notice);
    }
}

/// Carries out the agent's view commands that arrived this frame, after
/// every edit of the frame, one at a time in the order the agent sent them:
/// moves the 3D view (as framing a camera does), changes the selection and
/// the view toggles, and tells the user what the agent did. None of them
/// changes the scene. First the mark and a request to load an area are
/// dropped if another scene was opened or another area loaded.
fn follow_commands(
    mut link: ResMut<AgentLinkRes>,
    scene: Res<Scene>,
    slots: Res<SceneSlots>,
    mut editor: ResMut<Editor>,
    mut panels: ResMut<Panels>,
    mut orbit: Query<&mut OrbitCamera>,
    mut toasts: ResMut<Toasts>,
) {
    link.follow_scene(SceneKey {
        slot: slots.index.active,
        area: scene.area,
    });
    if !link.is_open() {
        return;
    }
    while let Some(action) = link.next_view_action(&scene) {
        match action {
            None => {}
            Some(ViewAction::Focus { at, distance_m }) => {
                if let Ok(mut o) = orbit.single_mut() {
                    look_at(&mut o, Vec2::from_array(at), distance_m);
                }
            }
            Some(ViewAction::Select(Some(id))) => select(&mut editor, &mut panels, id),
            Some(ViewAction::Select(None)) => editor.selected = None,
            Some(ViewAction::Panels {
                overlay,
                cones,
                walls,
            }) => {
                if let Some(o) = overlay {
                    panels.overlay = o;
                }
                if let Some(c) = cones {
                    panels.show_frustums = c;
                }
                if let Some(w) = walls {
                    panels.show_facades = w;
                }
            }
        }
    }
    for notice in link.take_notices() {
        toasts.info(notice);
    }
}

/// Keeps the dialog's "Let the agent move the view and selection" in this
/// browser when the user changes it.
fn remember_view_choice(
    link: Res<AgentLinkRes>,
    platform: Res<Platform>,
    mut last: Local<Option<bool>>,
) {
    let now = link.allow_view;
    match *last {
        Some(was) if was == now => {}
        None => *last = Some(now),
        Some(_) => {
            *last = Some(now);
            if let Err(why) = platform
                .local
                .set(VIEW_COMMANDS_KEY, if now { "on" } else { "off" })
            {
                bevy::log::warn!("could not store the agent view setting: {why:?}");
            }
        }
    }
}

/// Gizmo group for the proposal's ghost: thicker than the view cones, and
/// drawn over buildings.
#[derive(Default, Reflect, GizmoConfigGroup)]
struct ProposalGizmos;

/// Height of the ghost's outlines above the ground, metres.
const GHOST_Y_M: f32 = 0.2;
/// Opacity of a ghost camera's cone.
const GHOST_ALPHA: f32 = 0.7;

/// While the proposal card is open and the proposal can be accepted, the
/// ghost of what it would do: new and moved cameras translucent with their
/// cones (a moved one with a line from where it is), removed cameras
/// crossed out, and new or changed areas and targets outlined.
fn draw_proposal(
    mut gizmos: Gizmos<ProposalGizmos>,
    link: Res<AgentLinkRes>,
    models: Res<Models>,
    tokens: Res<Tokens>,
    area: Option<Res<Area>>,
) {
    if !link.card_open {
        return;
    }
    let Some((p, (_, proposed))) = link.proposal().and_then(|p| p.proposed().map(|s| (p, s)))
    else {
        return;
    };
    let terrain = terrain_of(area.as_deref());
    let ghost = to_bevy(tokens.accent);
    let removed = to_bevy(tokens.danger);
    let at = |p: [f32; 2]| {
        let v = Vec2::from_array(p);
        Vec3::new(v.x, terrain.height_at(v) + GHOST_Y_M, v.y)
    };
    let outline = |gizmos: &mut Gizmos<ProposalGizmos>, points: &[[f32; 2]], color: Color| {
        let ring: Vec<Vec3> = points
            .iter()
            .chain(points.first())
            .map(|p| at(*p))
            .collect();
        gizmos.linestrip(ring, color);
    };
    let cross = |gizmos: &mut Gizmos<ProposalGizmos>, centre: Vec3, color: Color| {
        let r = 0.8;
        gizmos.line(
            centre - Vec3::new(r, r, 0.0),
            centre + Vec3::new(r, r, 0.0),
            color,
        );
        gizmos.line(
            centre - Vec3::new(r, -r, 0.0),
            centre + Vec3::new(r, -r, 0.0),
            color,
        );
    };
    for change in &p.changes.changes {
        match change {
            Change::Camera(c) => {
                let Some(new) = c.new.as_ref() else {
                    if let Some(base) = &c.base {
                        cross(&mut gizmos, Vec3::from_array(base.position), removed);
                    }
                    continue;
                };
                let model = proposed
                    .custom_models
                    .iter()
                    .find(|m| m.id == new.model_id)
                    .or_else(|| models.get(&new.model_id));
                let eye = Vec3::from_array(new.position);
                gizmos.sphere(Isometry3d::from_translation(eye), 0.35, ghost);
                if let Some(base) = &c.base {
                    gizmos.line(Vec3::from_array(base.position), eye, ghost.with_alpha(0.5));
                }
                let Some(m) = model.filter(|_| new.enabled) else {
                    continue;
                };
                let cone = cone_for(new, m, &proposed.params);
                let color = ghost.with_alpha(GHOST_ALPHA);
                let r = cone.range;
                for d in cone.corner_rays() {
                    gizmos.line(cone.eye, cone.eye + d * r, color);
                }
                let rim = cone.outline_rays(12);
                for w in rim.windows(2) {
                    gizmos.line(cone.eye + w[0] * r, cone.eye + w[1] * r, color);
                }
                if let (Some(a), Some(b)) = (rim.last(), rim.first()) {
                    gizmos.line(cone.eye + *a * r, cone.eye + *b * r, color);
                }
            }
            Change::Area(a) => match (&a.base, &a.new) {
                (_, Some(new)) => outline(&mut gizmos, &new.points, ghost),
                (Some(base), None) => outline(&mut gizmos, &base.points, removed),
                (None, None) => {}
            },
            Change::Target(t) => {
                let (place, color) = match (&t.base, &t.new) {
                    (_, Some(new)) => (&new.at, ghost),
                    (Some(base), None) => (&base.at, removed),
                    (None, None) => continue,
                };
                match place {
                    TargetPlace::Ground { points } => outline(&mut gizmos, points, color),
                    TargetPlace::Wall { point, normal } => {
                        let centre = Vec3::from_array(*point);
                        let n = Vec3::new(normal[0], 0.0, normal[1]).normalize_or_zero();
                        gizmos.sphere(Isometry3d::from_translation(centre), 0.5, color);
                        gizmos.line(centre, centre + n, color);
                    }
                }
            }
            Change::CustomModel(_) | Change::Params { .. } | Change::LimitFiguresTo { .. } => {}
        }
    }
}

/// Radius of the ring around the agent's mark, metres.
const MARK_RING_M: f32 = 1.2;
/// Height of the pin above the agent's mark, metres.
const MARK_PIN_M: f32 = 6.0;

/// The agent's mark: a ring and a pin over a camera or a spot, the outline
/// of a target or an area, in the accent colour over a dark casing.
fn draw_mark(
    mut gizmos: Gizmos<ProposalGizmos>,
    link: Res<AgentLinkRes>,
    scene: Res<Scene>,
    tokens: Res<Tokens>,
    area: Option<Res<Area>>,
) {
    let Some(mark) = link.mark() else {
        return;
    };
    let Some((at, height)) = mark.anchor(&scene) else {
        return;
    };
    let terrain = terrain_of(area.as_deref());
    let color = to_bevy(tokens.accent);
    let halo = to_bevy(tokens.world.halo);
    let ground = |p: [f32; 2]| {
        let v = Vec2::from_array(p);
        Vec3::new(v.x, terrain.height_at(v) + GHOST_Y_M, v.y)
    };
    let flat =
        |centre: Vec3| Isometry3d::new(centre, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2));
    let outline = |gizmos: &mut Gizmos<ProposalGizmos>, points: &[[f32; 2]]| {
        let ring: Vec<Vec3> = points
            .iter()
            .chain(points.first())
            .map(|p| ground(*p))
            .collect();
        gizmos.linestrip(ring, color);
    };
    match mark.at {
        MarkAt::Area(id) => {
            if let Some(a) = scene.areas.iter().find(|a| a.id == id) {
                outline(&mut gizmos, &a.points);
            }
        }
        MarkAt::Target(id) => {
            if let Some(observation_core::footage::TargetPlace::Ground { points }) =
                scene.targets.iter().find(|t| t.id == id).map(|t| &t.at)
            {
                outline(&mut gizmos, points);
            }
        }
        MarkAt::Camera(_) | MarkAt::Point(_) => {}
    }
    let foot = ground(at);
    let centre = height.map_or(foot, |h| Vec3::new(at[0], h, at[1]));
    gizmos.circle(flat(centre), MARK_RING_M + 0.08, halo);
    gizmos.circle(flat(centre), MARK_RING_M, color);
    gizmos.line(foot, centre.max(foot) + Vec3::Y * MARK_PIN_M, color);
}

/// Share with agent: `S` with the pointer over the ground of the loaded
/// area shares that point, while a pairing is open.
fn share_pointed_ground(
    keys: Res<ButtonInput<KeyCode>>,
    wants: Res<EguiWantsInput>,
    layout: Res<UiLayout>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    area: Option<Res<Area>>,
    mut link: ResMut<AgentLinkRes>,
) {
    if !keys.just_pressed(KeyCode::KeyS) || wants.wants_any_keyboard_input() || !link.is_open() {
        return;
    }
    let modifier = keys.any_pressed([
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
        KeyCode::AltLeft,
        KeyCode::AltRight,
    ]);
    let (Some(area), Some(pos), Ok((cam, gt))) = (area, window.cursor_position(), camera.single())
    else {
        return;
    };
    if modifier || layout.is_ui(pos) {
        return;
    }
    let Some(p) = cam
        .viewport_to_world(gt, pos)
        .ok()
        .and_then(|ray| {
            area.bundle
                .terrain
                .ray_hit(ray.origin, *ray.direction, MAX_SHARE_RAY_M)
        })
        .filter(|p| area.bounds().contains(Vec2::new(p.x, p.z)))
    else {
        return;
    };
    let at = area
        .bundle
        .projection()
        .to_latlon([f64::from(p.x), f64::from(p.z)]);
    link.share_point(p.x, p.z, Some((at.lat, at.lon)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Platform, PlatformPlugin};
    use observation_agent_api::paths;
    use observation_client_model::agent::LinkPhase;
    use observation_client_model::platform::StreamEvent;
    use observation_client_model::testing::{
        FakeClock, FakeColorScheme, FakeFiles, FakeHttp, MemoryStore,
    };
    use std::sync::Arc;

    /// The link's systems over a held-reply fake server.
    fn app(http: &Arc<FakeHttp>) -> App {
        let platform = Platform {
            http: http.clone(),
            local: Arc::new(MemoryStore::default()),
            session: Arc::new(MemoryStore::default()),
            files: Arc::new(FakeFiles::default()),
            sheet: Arc::new(observation_client_model::testing::FakeSheetOutput::default()),
            clock: Arc::new(FakeClock::at(0.0)),
            scheme: Arc::new(FakeColorScheme::new(false)),
        };
        let mut app = App::new();
        app.insert_resource(platform)
            .add_plugins(MinimalPlugins)
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_millis(600),
            ))
            .add_plugins(PlatformPlugin)
            .init_resource::<Scene>()
            .insert_resource(Models::new(Default::default()))
            .init_resource::<Editor>()
            .init_resource::<Panels>()
            .init_resource::<SceneSlots>()
            .init_resource::<ServerConfig>()
            .init_resource::<UiLayout>()
            .init_resource::<EguiWantsInput>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<Toasts>()
            .add_plugins(AgentPlugin);
        app.world_mut()
            .spawn(Window::default())
            .insert(PrimaryWindow);
        app
    }

    #[test]
    fn the_link_is_offered_by_the_server_and_posts_the_open_scene() {
        let http = Arc::new(FakeHttp::default());
        let mut app = app(&http);
        app.update();
        assert!(!app.world().resource::<AgentLinkRes>().available);
        app.world_mut().resource_mut::<ServerConfig>().agent_bridge = true;
        app.update();
        assert!(app.world().resource::<AgentLinkRes>().available);

        let platform = app.world().resource::<Platform>().clone();
        app.world_mut()
            .resource_mut::<AgentLinkRes>()
            .connect(&*platform.http);
        assert!(http.respond_to(
            "POST",
            paths::PAIRINGS,
            Ok(br#"{"v":1,"code":"ABCDEFGH","browser_token":"t","code_expires_in_s":600,"idle_timeout_s":1800}"#.to_vec()),
        ));
        app.update();
        assert_eq!(
            app.world().resource::<AgentLinkRes>().phase(),
            &LinkPhase::Waiting {
                code: "ABCDEFGH".into()
            }
        );
        // No area yet: nothing to post.
        app.update();
        assert!(!http.sent().iter().any(|r| r.method == "PUT"));
        app.world_mut().resource_mut::<Scene>().area = Some(observation_core::scene::AreaRef::Demo);
        app.world_mut().resource_mut::<Editor>().selected = Some(4);
        app.update();
        let put = http
            .sent()
            .into_iter()
            .find(|r| r.method == "PUT")
            .expect("the state is posted");
        let body: serde_json::Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["state"]["selection"], serde_json::json!([4]));
        assert_eq!(body["state"]["area"]["kind"], "demo");
        assert_eq!(put.bearer.as_deref(), Some("t"));
    }

    fn command(id: u64, command: serde_json::Value) -> StreamEvent {
        let event = serde_json::json!({"v": 1, "type": "command", "id": id, "command": command});
        StreamEvent::Data(event.to_string().into_bytes())
    }

    #[test]
    fn the_agents_commands_move_the_view_and_selection_unless_turned_off() {
        let http = Arc::new(FakeHttp::default());
        let mut app = app(&http);
        app.world_mut().resource_mut::<ServerConfig>().agent_bridge = true;
        let orbit = app.world_mut().spawn(OrbitCamera::default()).id();
        app.world_mut().resource_mut::<Scene>().cameras.push(
            observation_core::camera::CameraPlacement {
                id: 4,
                label: "Porch".into(),
                model_id: "m".into(),
                position: [1.0, 3.0, 2.0],
                yaw_deg: 0.0,
                pitch_deg: 0.0,
                roll_deg: 0.0,
                color: None,
                enabled: true,
                group: None,
            },
        );
        app.update();
        assert!(
            app.world().resource::<AgentLinkRes>().allow_view,
            "on by default"
        );
        let platform = app.world().resource::<Platform>().clone();
        app.world_mut()
            .resource_mut::<AgentLinkRes>()
            .connect(&*platform.http);
        assert!(http.respond_to(
            "POST",
            paths::PAIRINGS,
            Ok(br#"{"v":1,"code":"ABCDEFGH","browser_token":"t","code_expires_in_s":600,"idle_timeout_s":1800}"#.to_vec()),
        ));
        app.update();
        let events = format!("{}?token=t", paths::EVENTS);
        http.emit_follow(
            &events,
            &StreamEvent::Data(br#"{"v":1,"type":"status","phase":"claimed"}"#.to_vec()),
        );
        http.emit_follow(
            &events,
            &command(
                1,
                serde_json::json!({"kind": "focus_view", "at": [10.0, -20.0], "distance_m": 40.0}),
            ),
        );
        http.emit_follow(
            &events,
            &command(2, serde_json::json!({"kind": "select_cameras", "ids": [4]})),
        );
        http.emit_follow(
            &events,
            &command(
                3,
                serde_json::json!({"kind": "set_view_options", "overlay": "per_camera", "walls": false}),
            ),
        );
        let revision = app.world().resource::<Scene>().revision;
        app.update();
        let o = app.world().get::<OrbitCamera>(orbit).unwrap();
        assert_eq!((o.focus, o.distance), (Vec3::new(10.0, 0.0, -20.0), 40.0));
        assert_eq!(app.world().resource::<Editor>().selected, Some(4));
        let panels = app.world().resource::<Panels>();
        assert!(!panels.show_facades);
        assert_eq!(
            panels.overlay,
            observation_client_model::editor::OverlayMode::PerCamera
        );
        // The scene is untouched.
        assert_eq!(app.world().resource::<Scene>().revision, revision);
        let toasts: Vec<String> = app
            .world()
            .resource::<Toasts>()
            .iter()
            .map(|t| t.text.clone())
            .collect();
        assert!(
            toasts.iter().any(|t| t == "Agent moved the view"),
            "{toasts:?}"
        );
        assert!(
            toasts
                .iter()
                .any(|t| t == "Agent selected camera 4 ‘Porch’"),
            "{toasts:?}"
        );

        // Turned off: the view stays, and the choice is remembered.
        app.world_mut().resource_mut::<AgentLinkRes>().allow_view = false;
        http.emit_follow(
            &events,
            &command(
                4,
                serde_json::json!({"kind": "focus_view", "at": [0.0, 0.0]}),
            ),
        );
        app.update();
        let o = app.world().get::<OrbitCamera>(orbit).unwrap();
        assert_eq!(o.focus, Vec3::new(10.0, 0.0, -20.0));
        assert_eq!(
            platform.local.get(VIEW_COMMANDS_KEY).as_deref(),
            Some("off")
        );
    }
}
