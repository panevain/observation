//! The whole client, headless, over fake ports: what a visit does from
//! start-up to the next autosave, across the plugins.
//!
//! The app is [`ClientPlugins`] on `MinimalPlugins` and assets, with no
//! window, rendering or egui plugin (the panels are not drawn; the input
//! and egui resources the systems read are inserted empty). The server is a
//! [`FakeHttp`] that holds each request until the test answers it, the
//! browser's storage is a [`MemoryStore`], and coverage jobs go through a
//! [`LoopbackJobChannel`], which answers as the worker does, a frame later
//! (for a narrower cone, see [`NarrowWorker`]).
//!
//! Each frame advances real time by more than `Time<Virtual>`'s largest
//! step, so the two clocks differ: the autosave interval runs on virtual
//! time, and the save is stamped with real time.

use super::*;
use crate::coverage::{CoverageBackend, CoverageState};
use crate::net::area_url;
use crate::persist::Autosave;
use crate::platform::{KeyValueStore, Platform};
use crate::state::{Area, Editor, Models, Panels, Scene, Toasts};
use bevy::ecs::schedule::{ScheduleLabel, SingleThreadedExecutor};
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::{ButtonState, InputPlugin};
use bevy::time::TimeUpdateStrategy;
use bevy_egui::{EguiGlobalSettings, input::EguiWantsInput};
use observation_client_model::coverage::backend;
use observation_client_model::persist::{STORAGE_KEY, SaveState};
use observation_client_model::platform::{JobChannel, Reply};
use observation_client_model::testing::{
    FakeClock, FakeColorScheme, FakeFiles, FakeHttp, LoopbackJobChannel, MemoryStore, StoreOp,
};
use observation_client_model::toasts::{ToastAction, ToastKind};
use observation_client_model::vm::StatusVm;
use observation_core::bundle::{AreaBundle, OsmRoad, build_bundle};
use observation_core::camera::{CameraKind, CameraModel, CameraPlacement};
use observation_core::coverage_job::JobRequest;
use observation_core::geo::{BBox, LatLon};
use observation_core::scene::{AreaRef, SceneFile};
use observation_core::terrain::Terrain;
use std::sync::Arc;
use std::time::Duration;

/// Real time per frame: above the 250 ms `Time<Virtual>` advances at most.
const FRAME: Duration = Duration::from_millis(400);

/// Frames of virtual time (250 ms each) in one autosave interval (1.5 s).
const INTERVAL_FRAMES: usize = 6;

/// The loopback worker, but each job is run for a cone half as wide (its
/// range unchanged, so the answer still counts as covering it): its answers
/// differ from what the backend computes in place when it falls back.
#[derive(Debug, Default)]
struct NarrowWorker(LoopbackJobChannel);

impl JobChannel for NarrowWorker {
    fn post(&mut self, bytes: &[u8]) -> Result<(), String> {
        match JobRequest::decode(bytes) {
            Ok(JobRequest::Compute(mut job)) => {
                job.cone.tan_half_h *= 0.5;
                self.0.post(&JobRequest::Compute(job).encode())
            }
            _ => self.0.post(bytes),
        }
    }

    fn receive(&mut self) -> Option<Reply> {
        self.0.receive()
    }
}

struct Harness {
    app: App,
    http: Arc<FakeHttp>,
    store: Arc<MemoryStore>,
}

impl Harness {
    /// The app with `stored` as the autosave in the browser, before its
    /// first frame.
    fn new(stored: &str) -> Self {
        let http = Arc::new(FakeHttp::default());
        let store = Arc::new(MemoryStore::default());
        store.set(STORAGE_KEY, stored).expect("stored");
        store.take_ops();
        let platform = Platform {
            http: http.clone(),
            local: store.clone(),
            session: Arc::new(MemoryStore::default()),
            files: Arc::new(FakeFiles::default()),
            clock: Arc::new(FakeClock::at(0.0)),
            scheme: Arc::new(FakeColorScheme::new(false)),
        };
        let mut app = App::new();
        app.insert_resource(platform)
            .add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_asset::<GizmoAsset>()
            .add_plugins(InputPlugin)
            .init_gizmo_group::<DefaultGizmoConfigGroup>()
            .init_resource::<EguiWantsInput>()
            .init_resource::<EguiGlobalSettings>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(FRAME))
            .add_plugins(ClientPlugins);
        // Single-threaded, as on the web: systems with no order between
        // them (only commuting pairs are left so) run in one fixed order,
        // not the parallel executor's of the moment.
        for label in [
            Startup.intern(),
            First.intern(),
            PreUpdate.intern(),
            Update.intern(),
            PostUpdate.intern(),
            Last.intern(),
        ] {
            app.edit_schedule(label, |s| {
                s.set_executor(SingleThreadedExecutor::new());
            });
        }
        // The worker path, as in the browser (the native build has none).
        **app.world_mut().non_send_mut::<CoverageBackend>() = backend::CoverageBackend::new(
            Some(Box::new(NarrowWorker::default())),
            Arc::new(FakeClock::at(0.0)),
        );
        Self { app, http, store }
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.app.update();
        }
    }

    /// Runs frames until storage is written, at most one autosave interval.
    fn until_written(&mut self) -> Vec<StoreOp> {
        for _ in 0..INTERVAL_FRAMES {
            self.app.update();
            let ops = self.store.take_ops();
            if !ops.is_empty() {
                return ops;
            }
        }
        panic!("nothing written within an autosave interval");
    }

    /// Presses and lets go of `key` (one frame each), as the window reports it.
    fn press(&mut self, key_code: KeyCode, logical_key: Key) {
        for state in [ButtonState::Pressed, ButtonState::Released] {
            self.app.world_mut().write_message(KeyboardInput {
                key_code,
                logical_key: logical_key.clone(),
                state,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
            self.app.update();
        }
    }

    fn requests(&self) -> Vec<String> {
        self.http
            .sent()
            .into_iter()
            .map(|r| r.url)
            .filter(|u| u != "/api/config")
            .collect()
    }

    fn stored(&self) -> SceneFile {
        let text = self.store.get(STORAGE_KEY).expect("an autosave");
        SceneFile::from_json(&text).expect("a scene")
    }

    fn camera_ids(&self) -> Vec<u32> {
        let scene = self.app.world().resource::<Scene>();
        scene.cameras.iter().map(|c| c.id).collect()
    }

    /// The status bar's save status and figures, as `draw_ui` builds them.
    fn status(&self) -> StatusVm {
        let w = self.app.world();
        let now = w.resource::<Time<Real>>().elapsed_secs_f64();
        let stats = w.resource::<CoverageState>().stats;
        StatusVm::new(None, "", self.save_state(), now, &stats)
    }

    fn street(&self) -> String {
        let [.., (label, street)] = self.status().figures;
        assert_eq!(label, "street");
        street
    }

    fn submitted(&self) -> usize {
        self.app.world().non_send::<CoverageBackend>().submitted()
    }

    fn save_state(&self) -> SaveState {
        self.app.world().resource::<Autosave>().status()
    }

    /// The scene as autosave writes it now.
    fn scene_json(&self) -> String {
        let w = self.app.world();
        let file = w.resource::<Scene>().to_file(w.resource::<Models>());
        file.expect("an area is set").to_json()
    }

    /// The `Saved` stamp, checked against both clocks: it is the real
    /// time of this frame, which the virtual clock lags behind.
    fn saved_at(&self) -> f64 {
        let SaveState::Saved(at) = self.save_state() else {
            panic!("saved, got {:?}", self.save_state());
        };
        let w = self.app.world();
        let real = w.resource::<Time<Real>>().elapsed_secs_f64();
        let virt = w.resource::<Time<Virtual>>().elapsed_secs_f64();
        assert_ne!(real, virt, "the two clocks differ");
        assert_eq!(at, real, "stamped with the real time of the write");
        at
    }

    /// Each toast's kind, text and whether it stays until dismissed.
    fn toasts(&self) -> Vec<(ToastKind, String, bool)> {
        let toasts = self.app.world().resource::<Toasts>();
        toasts
            .iter()
            .map(|t| (t.kind, t.text.clone(), t.ttl.is_infinite()))
            .collect()
    }
}

fn bbox() -> BBox {
    BBox::new(51.5, -0.001, 51.5009, 0.001).expect("valid box")
}

/// A street along the middle of the box.
fn bundle() -> AreaBundle {
    let road = OsmRoad {
        osm_id: 1,
        highway: "residential".into(),
        name: None,
        width_m: 8.0,
        points: vec![LatLon::new(51.50045, -0.001), LatLon::new(51.50045, 0.001)],
    };
    build_bundle(
        bbox(),
        &[],
        &[road],
        &[],
        Terrain::default(),
        "Test",
        0,
        None,
    )
}

/// A saved scene over the box's area: two cameras on a custom model, at
/// either end of the street, looking along it.
fn saved_scene() -> String {
    let mut file = SceneFile::new(AreaRef::Osm { bbox: bbox() }, bbox().center());
    file.custom_models.push(CameraModel {
        id: "custom-1-test".into(),
        vendor: String::new(),
        name: "Test".into(),
        kind: CameraKind::Bullet,
        hfov_deg: 90.0,
        vfov_deg: 55.0,
        resolution: [1920, 1080],
        ir_range_m: Some(12.0),
        source: None,
        notes: None,
    });
    let camera = |id, label: &str, x: f32, yaw_deg| CameraPlacement {
        id,
        label: label.into(),
        model_id: "custom-1-test".into(),
        position: [x, 3.0, 4.0],
        yaw_deg,
        pitch_deg: -10.0,
        roll_deg: 0.0,
        color: None,
        enabled: true,
        group: None,
    };
    file.cameras = vec![
        camera(1, "West", -60.0, 90.0),
        camera(2, "East", 60.0, 270.0),
    ];
    file.to_json()
}

#[test]
fn a_saved_scene_is_restored_loaded_covered_edited_and_saved_again() {
    let mut h = Harness::new(&saved_scene());
    let area = AreaRef::Osm { bbox: bbox() };
    assert!(h.camera_ids().is_empty());
    assert!(h.requests().is_empty());

    // Start-up restores the saved cameras and asks the server for their
    // area; restoring writes nothing.
    h.frames(1);
    assert_eq!(h.camera_ids(), [1, 2]);
    assert_eq!(h.requests(), [area_url(&area)]);
    assert_eq!(h.store.ops(), []);

    // Unanswered, the area stays unloaded and is not asked for again.
    h.frames(1);
    assert!(!h.app.world().contains_resource::<Area>());
    assert_eq!(h.requests(), [area_url(&area)]);
    assert_eq!(h.submitted(), 0);

    // The answer becomes the scene's ground, keeping the saved cameras, and
    // one coverage job per camera goes to the worker.
    assert!(h.http.respond(&area_url(&area), Ok(bundle().encode())));
    h.frames(2);
    assert_eq!(h.app.world().resource::<Area>().area, area);
    assert_eq!(h.camera_ids(), [1, 2]);
    let loaded = (
        ToastKind::Info,
        "Loaded 0 buildings and 1 street segments".to_owned(),
        false,
    );
    assert_eq!(h.toasts(), [loaded]);
    assert_eq!(h.submitted(), 2);
    assert_eq!(h.street(), "0%", "the worker has not answered yet");

    // The worker answers on the next frame. Its answers are for narrower
    // cones: computed in place the figure would be 49% (and would have shown
    // with the submission).
    h.frames(1);
    assert_eq!(h.street(), "43%", "the worker's answers");
    assert_eq!(h.submitted(), 2);
    assert_eq!(h.status().save, None, "not saved yet");

    // The next autosave writes the loaded scene.
    let ops = h.until_written();
    assert_eq!(ops, [StoreOp::Set(STORAGE_KEY.to_owned(), h.scene_json())]);
    // What restore put back is the saved scene; the load only adds where the
    // area's data comes from.
    let mut written = h.stored();
    assert!(written.provenance.is_some());
    written.provenance = None;
    assert_eq!(
        written.to_json(),
        saved_scene(),
        "the restored scene, unchanged"
    );
    let first = h.saved_at();
    assert_eq!(h.status().save.expect("saved").text, "Saved");

    // Unchanged, the scene is not written again, and the stamp stays.
    h.frames(INTERVAL_FRAMES);
    assert_eq!(h.store.ops(), []);
    assert_eq!(h.save_state(), SaveState::Saved(first));

    // An edit: Delete removes the selected camera, and its coverage with
    // it, without a new job.
    h.app.world_mut().resource_mut::<Editor>().selected = Some(2);
    h.press(KeyCode::Delete, Key::Delete);
    assert_eq!(h.camera_ids(), [1]);
    let toasts = h.app.world().resource::<Toasts>();
    let last = toasts.iter().last().expect("a toast");
    assert_eq!(
        (last.text.as_str(), &last.action),
        ("Deleted East", &Some(ToastAction::Undo))
    );
    // The two cameras mirror each other at either end of the street and
    // their stretches do not overlap, so the one left covers half of the
    // 43% (each figure is rounded).
    assert_eq!(h.street(), "22%");
    assert_eq!(h.submitted(), 2);

    // The next autosave writes the edited scene.
    let ops = h.until_written();
    assert_eq!(ops, [StoreOp::Set(STORAGE_KEY.to_owned(), h.scene_json())]);
    assert_eq!(h.stored().cameras.len(), 1);
    assert!(h.saved_at() > first);
}

#[test]
fn an_unreadable_autosave_is_reported_and_kept_until_the_demo_street_replaces_it() {
    let mut h = Harness::new("{ not json");
    assert!(h.toasts().is_empty());
    h.frames(1);
    assert_eq!(
        h.toasts(),
        [(
            ToastKind::Error,
            "Your saved scene could not be restored: not a valid scene file: key must be a \
             string at line 1 column 3. Starting with the demo street instead"
                .to_owned(),
            true,
        )]
    );
    assert!(!h.app.world().resource::<Panels>().help_open);
    let demo = area_url(&AreaRef::Demo);
    assert_eq!(h.requests(), std::slice::from_ref(&demo));

    // Until the demo street is there, the unreadable text stays.
    h.frames(2 * INTERVAL_FRAMES);
    assert_eq!(h.store.ops(), []);
    assert_eq!(h.status().save, None);

    assert!(h.http.respond(&demo, Ok(bundle().encode())));
    let ops = h.until_written();
    assert_eq!(ops, [StoreOp::Set(STORAGE_KEY.to_owned(), h.scene_json())]);
    assert_eq!(h.stored().area, AreaRef::Demo);
    h.saved_at();
    let toasts = h.toasts();
    assert_eq!(toasts.len(), 2, "{toasts:?}");
    assert!(toasts[0].2, "the error stays until dismissed: {toasts:?}");
}
