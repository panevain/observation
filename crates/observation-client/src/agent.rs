//! The link to a local agent (see [`observation_client_model::agent`]): the
//! resource, keeping it up to date, and Share with agent (the `S` key over
//! the ground). The Connect agent dialog is `ui::agent`.
//!
//! The systems run in `Last`, after everything that edits the scene in the
//! frame, so what is posted is the frame's final state, and no system in
//! `Update` has to be ordered against them.

use crate::input::OrbitCamera;
use crate::net::ServerConfig;
use crate::persist::SceneSlots;
use crate::platform::Platform;
use crate::state::{Area, Editor, Models, Scene, UiLayout};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::input::EguiWantsInput;
use observation_client_model::agent::{AgentLink, shared_state};

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

/// Offers Connect agent when the server says the bridge is there for this
/// browser.
fn offer(config: Res<ServerConfig>, mut link: ResMut<AgentLinkRes>) {
    if link.available != config.agent_bridge {
        link.available = config.agent_bridge;
    }
}

/// Takes in the bridge's replies and events, and posts the state when it
/// changed.
fn keep_up(
    time: Res<Time<Real>>,
    mut link: ResMut<AgentLinkRes>,
    scene: Res<Scene>,
    models: Res<Models>,
    editor: Res<Editor>,
    slots: Res<SceneSlots>,
    platform: Res<Platform>,
) {
    if !link.is_open() {
        return;
    }
    let name = slots.index.active_name();
    link.update(&*platform.http, time.delta_secs(), |pinned| {
        shared_state(&scene, &models, name, editor.selected, pinned)
    });
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
