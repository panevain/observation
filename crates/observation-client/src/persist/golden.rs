//! Golden files for what the client persists: the `localStorage` autosave and
//! the exported scene file, which are the same JSON, written from the
//! [`Scene`] and [`Models`] resources, and what restoring and importing make
//! of saved text, through the same systems and ports as the app.
//!
//! The JSON format itself (`SceneFile`, its versions and validation) has its
//! own golden files in `observation-core`; these pin the client's side of it:
//! which document fields are saved, how a loaded file becomes the document
//! again, and what the user is told when it cannot. A failure means one of
//! them changed: if that is intended, regenerate the files with
//!
//! ```sh
//! OBS_BLESS=1 cargo nextest run -p observation-client golden
//! ```
//!
//! and review the diff. `autosave_v1_flat.json` is an input written the way
//! builds before terrain saved scenes, and is never rewritten.

use super::{ADDRESS_JUMP_KEY, FONT_KEY, PersistAction, PersistPlugin, STORAGE_KEY};
use crate::platform::{KeyValueStore, Platform, PlatformPlugin};
use crate::state::{LoadArea, Models, Scene, Snapshot, ToastKind, UiState};
use bevy::ecs::message::Messages;
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use observation_client_model::testing::{
    FakeClock, FakeColorScheme, FakeFiles, FakeHttp, MemoryStore,
};
use observation_core::camera::{CameraKind, CameraModel, CameraPlacement, Catalog};
use observation_core::coverage::Region;
use observation_core::geo::{BBox, LatLon};
use observation_core::provenance::Provenance;
use observation_core::scene::{AreaRef, SceneFile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

// ----- harness ---------------------------------------------------------------

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Whether to rewrite the golden files (`OBS_BLESS=1`). Refused under CI,
/// which must compare against the committed files rather than rewrite them.
fn blessing() -> bool {
    let bless = std::env::var_os("OBS_BLESS").is_some_and(|v| v == "1");
    assert!(
        !bless || std::env::var_os("CI").is_none(),
        "OBS_BLESS is for local use: CI must compare against the committed golden files"
    );
    bless
}

/// Reads a committed fixture (an input, never rewritten).
fn read(name: &str) -> String {
    let path = golden_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Compares `actual` with the golden file `name`, or with `OBS_BLESS=1`
/// writes it there. Returns the golden text.
fn check(name: &str, actual: &str) -> String {
    let path = golden_dir().join(name);
    if blessing() {
        std::fs::create_dir_all(golden_dir()).expect("create the golden directory");
        std::fs::write(&path, actual).expect("write the golden file");
        return actual.to_owned();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden file {}: {e}\nRun with OBS_BLESS=1 to create it.",
            path.display()
        )
    });
    if let Some((line, (e, a))) = expected
        .lines()
        .zip(actual.lines())
        .enumerate()
        .find(|(_, (e, a))| e != a)
    {
        panic!(
            "output differs from the golden file {} at line {}: {a:?}, expected {e:?}.\n\
             If the change is intended, run with OBS_BLESS=1 and review the diff.",
            path.display(),
            line + 1
        );
    }
    assert_eq!(
        expected,
        actual,
        "output differs from the golden file {} in length.\n\
         If the change is intended, run with OBS_BLESS=1 and review the diff.",
        path.display()
    );
    expected
}

// ----- fixtures --------------------------------------------------------------

/// A non-demo area, written out so the bytes depend on no `cos`.
fn fixture_area() -> AreaRef {
    AreaRef::Osm {
        bbox: BBox {
            south: 51.5,
            west: -0.125,
            north: 51.5015,
            east: -0.1225,
        },
    }
}

fn fixture_custom_model() -> CameraModel {
    CameraModel {
        id: "custom-1-porch-cam".to_owned(),
        vendor: String::new(),
        name: "Porch cam".to_owned(),
        kind: CameraKind::Doorbell,
        hfov_deg: 150.0,
        vfov_deg: 120.0,
        resolution: [1536, 1536],
        ir_range_m: Some(5.0),
        source: Some("https://example.com/porch-cam".to_owned()),
        notes: Some("Bought 2025".to_owned()),
    }
}

fn camera(model_id: &str, position: [f32; 3], yaw_deg: f32) -> CameraPlacement {
    CameraPlacement {
        id: 0,
        label: String::new(),
        model_id: model_id.to_owned(),
        position,
        yaw_deg,
        pitch_deg: -20.0,
        roll_deg: 0.0,
        color: None,
        enabled: true,
        group: None,
    }
}

/// A document with something in every saved field: cameras of several
/// kinds, labels, colours and groups (one disabled, one on a custom model),
/// a region, non-default analysis settings and a provenance.
fn fixture_scene() -> (Scene, Models) {
    let mut models = Models::new(Catalog::default());
    models.custom.push(fixture_custom_model());
    let mut scene = Scene::default();
    scene.area = Some(fixture_area());
    scene.origin = LatLon::new(51.50075, -0.12375);
    scene.add_camera(camera("generic-bullet", [12.5, 3.25, -4.0], 90.0));
    scene.add_camera(CameraPlacement {
        label: "Doorbell".to_owned(),
        group: Some("Number 12".to_owned()),
        color: Some([200, 40, 40]),
        ..camera("custom-1-porch-cam", [-3.0, 1.25, 7.5], 180.0)
    });
    scene.add_camera(CameraPlacement {
        enabled: false,
        roll_deg: 5.0,
        group: Some("Number 12".to_owned()),
        ..camera("generic-dome", [0.0, 4.5, 0.0], 270.5)
    });
    scene.params.dori = observation_core::camera::DoriLevel::Recognise;
    scene.params.lighting = observation_core::camera::Lighting::Night;
    scene.params.subject_height_m = 1.5;
    scene.region = Some(Region {
        points: vec![[-20.0, -20.0], [25.0, -18.5], [22.0, 30.0], [-15.0, 28.0]],
        enabled: true,
    });
    scene.provenance = Some(Provenance {
        osm_data_unix: Some(1_793_475_782),
        bundle_built_unix: Some(1_793_479_382),
        osm_fingerprint: Some("00000000000000aa".to_owned()),
        app_version: "0.1.0".to_owned(),
        catalog_version: "0123456789abcdef".to_owned(),
        caveats_version: 1,
    });
    (scene, models)
}

/// Everything about a document that is saved or undone, for comparing two.
fn saved_state(scene: &Scene, models: &Models) -> (Snapshot, Option<Provenance>, bool) {
    (
        scene.snapshot(models),
        scene.provenance.clone(),
        scene.on_flat_ground,
    )
}

/// The fixture's autosave, checked against (or blessed into) `autosave.json`.
/// Every test that starts from the golden text goes through here, so they
/// all compare the same bytes, and blessing does not depend on test order.
fn golden_autosave() -> String {
    let (scene, models) = fixture_scene();
    let json = scene.to_file(&models).expect("an area is set").to_json();
    check("autosave.json", &json)
}

// ----- the document and its JSON --------------------------------------------

#[test]
fn autosave_json_is_unchanged() {
    let (scene, models) = fixture_scene();
    let golden = golden_autosave();

    // It loads back to the same document, which saves the same bytes.
    let mut loaded = Scene::default();
    let mut loaded_models = Models::new(Catalog::default());
    loaded.load_file(
        SceneFile::from_json(&golden).expect("the golden file loads"),
        &mut loaded_models,
    );
    assert_eq!(
        saved_state(&loaded, &loaded_models),
        saved_state(&scene, &models)
    );
    assert_eq!(loaded.saved_provenance, scene.provenance);
    let again = loaded
        .to_file(&loaded_models)
        .expect("an area is set")
        .to_json();
    assert_eq!(again, golden);
    // A new camera gets the next id after the loaded ones.
    assert_eq!(
        loaded.add_camera(camera("generic-bullet", [0.0; 3], 0.0)),
        4
    );
}

#[test]
fn a_scene_without_an_area_saves_nothing() {
    let models = Models::new(Catalog::default());
    assert!(Scene::default().to_file(&models).is_none());
}

#[test]
fn a_flat_ground_autosave_keeps_its_version_until_the_area_loads() {
    let mut scene = Scene::default();
    let mut models = Models::new(Catalog::default());
    let file = SceneFile::from_json(&read("autosave_v1_flat.json")).expect("the v1 file loads");
    scene.load_file(file, &mut models);
    assert!(scene.on_flat_ground);
    assert_eq!(scene.cameras.len(), 2);
    let json = scene.to_file(&models).expect("an area is set").to_json();
    check("autosave_v1_flat.resaved.json", &json);
}

#[test]
fn undo_snapshots_put_the_saved_document_back() {
    let (mut scene, mut models) = fixture_scene();
    let before = scene.snapshot(&models);
    let json = scene.to_file(&models).expect("an area is set").to_json();
    scene.remove_camera(2);
    models.custom.clear();
    scene.region = None;
    assert_ne!(scene.snapshot(&models), before);
    scene.restore(&before, &mut models);
    assert_eq!(scene.snapshot(&models), before);
    assert_eq!(
        scene.to_file(&models).expect("an area is set").to_json(),
        json
    );
}

// ----- restore, autosave, export and import through the app -----------------

struct Harness {
    app: App,
    store: Arc<MemoryStore>,
    files: Arc<FakeFiles>,
}

/// The persistence half of the app over fake ports, with `stored` as the
/// autosave in the browser, run up to the end of its first frame.
fn harness(stored: Option<&str>) -> Harness {
    let store = Arc::new(MemoryStore::default());
    if let Some(text) = stored {
        store.set(STORAGE_KEY, text).expect("stored");
    }
    let files = Arc::new(FakeFiles::default());
    let platform = Platform {
        http: Arc::new(FakeHttp::default()),
        local: store.clone(),
        session: Arc::new(MemoryStore::default()),
        files: files.clone(),
        clock: Arc::new(FakeClock::at(0.0)),
        scheme: Arc::new(FakeColorScheme::new(true)),
    };
    let mut app = App::new();
    app.insert_resource(platform)
        .add_plugins(MinimalPlugins)
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            250,
        )))
        .add_message::<LoadArea>()
        .add_plugins((PlatformPlugin, crate::ResourcesPlugin, PersistPlugin));
    app.update();
    Harness { app, store, files }
}

impl Harness {
    fn loads(&mut self) -> Vec<(AreaRef, bool)> {
        self.app
            .world_mut()
            .resource_mut::<Messages<LoadArea>>()
            .drain()
            .map(|l| (l.area, l.keep_cameras))
            .collect()
    }

    fn toasts(&self) -> Vec<(ToastKind, String, bool)> {
        self.app
            .world()
            .resource::<UiState>()
            .toasts
            .iter()
            .map(|t| (t.kind, t.text.clone(), t.ttl.is_infinite()))
            .collect()
    }

    fn saved_json(&self) -> Option<String> {
        let w = self.app.world();
        w.resource::<Scene>()
            .to_file(w.resource::<Models>())
            .map(|f| f.to_json())
    }

    /// Runs frames until the autosave interval has passed.
    fn run_past_autosave(&mut self) {
        for _ in 0..8 {
            self.app.update();
        }
    }
}

#[test]
fn restoring_the_autosave_loads_its_area_with_its_cameras() {
    let golden = golden_autosave();
    let mut h = harness(Some(&golden));
    assert_eq!(h.loads(), [(fixture_area(), true)]);
    assert_eq!(h.toasts(), []);
    assert!(!h.app.world().resource::<UiState>().help_open);
    assert_eq!(h.saved_json().as_deref(), Some(golden.as_str()));
    let (scene, models) = fixture_scene();
    let w = h.app.world();
    assert_eq!(
        saved_state(w.resource::<Scene>(), w.resource::<Models>()),
        saved_state(&scene, &models)
    );
    // Nothing changed, but the first autosave writes the same text back.
    h.store.remove(STORAGE_KEY);
    h.run_past_autosave();
    assert_eq!(h.store.get(STORAGE_KEY).as_deref(), Some(golden.as_str()));
}

#[test]
fn a_first_visit_opens_help_over_the_demo_street() {
    let mut h = harness(None);
    assert_eq!(h.loads(), [(AreaRef::Demo, false)]);
    assert_eq!(h.toasts(), []);
    assert!(h.app.world().resource::<UiState>().help_open);
    // Nothing to save until an area is set.
    h.run_past_autosave();
    assert_eq!(h.store.get(STORAGE_KEY), None);
}

#[test]
fn an_unreadable_autosave_falls_back_to_the_demo_street_and_says_why() {
    let too_many = {
        let mut file = SceneFile::new(AreaRef::Demo, LatLon::new(0.0, 0.0));
        file.cameras = (1..=257)
            .map(|id| CameraPlacement {
                id,
                ..camera("generic-bullet", [0.0; 3], 0.0)
            })
            .collect();
        file.to_json()
    };
    let too_new = golden_autosave().replace("\"version\": 2", "\"version\": 3");
    assert_ne!(too_new, golden_autosave(), "the version field was found");
    for (stored, why) in [
        (
            "{ not json",
            "not a valid scene file: key must be a string at line 1 column 3",
        ),
        (
            too_many.as_str(),
            "scene has invalid values: 257 cameras, but at most 256 are supported",
        ),
        (
            too_new.as_str(),
            "scene version 3 is newer than this app reads",
        ),
    ] {
        let mut h = harness(Some(stored));
        assert_eq!(h.loads(), [(AreaRef::Demo, false)], "{why}");
        let toasts = h.toasts();
        assert_eq!(toasts.len(), 1, "{toasts:?}");
        let (kind, text, sticky) = &toasts[0];
        assert_eq!(*kind, ToastKind::Error);
        assert!(*sticky, "a lost scene is never missed");
        assert!(
            text.starts_with("Your saved scene could not be restored: ")
                && text.ends_with(". Starting with the demo street instead"),
            "{text}"
        );
        assert!(text.contains(why), "{text}");
        assert!(!h.app.world().resource::<UiState>().help_open);
        // The unreadable text is left alone until something replaces it.
        assert_eq!(h.store.get(STORAGE_KEY).as_deref(), Some(stored));
    }
}

#[test]
fn export_downloads_the_autosave_json() {
    let golden = golden_autosave();
    let mut h = harness(Some(&golden));
    h.app.world_mut().write_message(PersistAction::Export);
    h.app.update();
    assert_eq!(
        h.files.downloads(),
        [(
            "observation-scene.json".to_owned(),
            "application/json".to_owned(),
            golden
        )]
    );
}

#[test]
fn export_before_an_area_says_there_is_nothing_yet() {
    let mut h = harness(None);
    h.app.world_mut().write_message(PersistAction::Export);
    h.app.update();
    assert_eq!(h.files.downloads(), []);
    assert_eq!(
        h.toasts(),
        [(
            ToastKind::Error,
            "Nothing to export yet; wait for an area to finish loading".to_owned(),
            false
        )]
    );
}

#[test]
fn import_replaces_the_document_and_loads_its_area() {
    let golden = golden_autosave();
    let mut h = harness(None);
    h.loads();
    h.app.world_mut().write_message(PersistAction::Import);
    h.app.update();
    assert_eq!(h.files.open_pickers(), [".json,application/json"]);
    assert!(h.files.pick(Ok(golden.clone())));
    h.app.update();
    assert_eq!(h.loads(), [(fixture_area(), true)]);
    assert_eq!(
        h.toasts(),
        [(
            ToastKind::Info,
            "Imported scene with 3 camera(s)".to_owned(),
            false
        )]
    );
    assert_eq!(h.saved_json().as_deref(), Some(golden.as_str()));
    h.run_past_autosave();
    assert_eq!(h.store.get(STORAGE_KEY).as_deref(), Some(golden.as_str()));
}

#[test]
fn a_failed_import_keeps_the_document_and_says_why() {
    let golden = golden_autosave();
    let mut h = harness(Some(&golden));
    h.loads();
    for (picked, toast) in [
        (
            Err("it is not text".to_owned()),
            "Import failed: could not read the file: it is not text",
        ),
        (
            Ok("{ not json".to_owned()),
            "Import failed: not a valid scene file: key must be a string at line 1 column 3",
        ),
    ] {
        h.app.world_mut().write_message(PersistAction::Import);
        h.app.update();
        assert!(h.files.pick(picked));
        h.app.update();
        assert_eq!(h.loads(), []);
        assert!(
            h.toasts()
                .contains(&(ToastKind::Error, toast.to_owned(), true)),
            "{:?}",
            h.toasts()
        );
        assert_eq!(h.saved_json().as_deref(), Some(golden.as_str()));
    }
}

#[test]
fn preferences_are_saved_only_once_changed() {
    let mut h = harness(None);
    h.run_past_autosave();
    assert_eq!(
        h.store.get(FONT_KEY),
        None,
        "the value read at startup is not written back"
    );
    assert_eq!(h.store.get(ADDRESS_JUMP_KEY), None);
    *h.app.world_mut().resource_mut::<crate::theme::UiFont>() = crate::theme::UiFont::Iosevka;
    *h.app
        .world_mut()
        .resource_mut::<crate::state::AddressJump>() = crate::state::AddressJump(false);
    h.app.update();
    assert_eq!(h.store.get(FONT_KEY).as_deref(), Some("iosevka"));
    assert_eq!(h.store.get(ADDRESS_JUMP_KEY).as_deref(), Some("off"));
}
