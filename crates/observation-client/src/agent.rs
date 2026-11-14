//! The link to a local agent (see [`observation_client_model::agent`]): the
//! resource, keeping it up to date, Share with agent (the `S` key over the
//! ground), and the ghost of the agent's proposal in the 3D view. The
//! Connect agent dialog and the proposal card are `ui::agent`.
//!
//! The link's systems run in `Last`, after everything that edits the scene
//! in the frame, so what is posted and checked is the frame's final state,
//! and no system in `Update` has to be ordered against them. The ghost is
//! drawn in `PostUpdate`, after every edit too and before the gizmos are
//! collected at the end of the frame.

use crate::coverage::cone_for;
use crate::input::OrbitCamera;
use crate::net::ServerConfig;
use crate::persist::SceneSlots;
use crate::platform::Platform;
use crate::state::{Area, Editor, Models, Scene, Toasts, UiLayout, terrain_of};
use crate::theme::{Tokens, to_bevy};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::input::EguiWantsInput;
use observation_client_model::agent::{AgentLink, shared_state};
use observation_core::footage::TargetPlace;
use observation_core::scene::changes::Change;

/// Farthest a Share with agent ray looks for the ground, metres.
const MAX_SHARE_RAY_M: f32 = 5_000.0;

/// The link to a local agent, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct AgentLinkRes(AgentLink);

pub struct AgentPlugin;

impl Plugin for AgentPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AgentLinkRes>().add_systems(
            Last,
            (
                offer.run_if(resource_changed::<ServerConfig>),
                share_pointed_ground,
                keep_up,
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
        .add_systems(PostUpdate, draw_proposal);
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
    link.update(http, time.delta_secs(), |pinned| {
        shared_state(&scene, &models, name, editor.selected, pinned)
    });
    link.follow_proposal(http, &scene, &models);
    for notice in link.take_notices() {
        toasts.info(notice);
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
}
