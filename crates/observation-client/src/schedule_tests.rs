//! Pins how [`ClientPlugins`] are scheduled.
//!
//! On the web the executor is single-threaded, so `Update` runs its systems
//! one after another in the order Bevy resolves from the declared edges.
//! Where two systems that touch the same data have no edge between them,
//! Bevy's tie-breaking picks their order, and an unrelated change (a new
//! system, a new set, a new `Commands` parameter) can flip it. These tests
//! make such a flip visible: [`update_runs_in_the_pinned_order`] pins the
//! whole order.
//!
//! The app is built headless (`MinimalPlugins` and assets, no rendering, no
//! egui plugin, no window), so the pins hold for the client's own systems;
//! the systems `DefaultPlugins` add to `Update` in the real app are not in it.

use super::*;
use crate::platform::Platform;
use bevy::ecs::schedule::{InternedScheduleLabel, ScheduleLabel, Schedules};
use observation_client_model::testing::{
    FakeClock, FakeColorScheme, FakeFiles, FakeHttp, MemoryStore,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// What to do when a pin fails.
const ADVICE: &str = "The schedule changed. Either restore the old order \
    (add or move `.before`/`.after` edges until this test passes), or update \
    the pin and explain in the pull request why the new order is safe: on the \
    web, systems run one at a time in this order.";

/// The run order of `Update`, as the single-threaded executor runs it.
/// `bevy_ecs::apply_deferred` is a sync point Bevy inserts (commands are
/// applied there).
const UPDATE_ORDER: &[&str] = &[
    "persist::handle_actions",
    "history::shortcuts",
    "ui::tick_toasts",
    "cameras::sync_bodies",
    "persist::receive_imports",
    "input::keyboard",
    "persist::autosave",
    "persist::save_font",
    "world::on_area_loaded",
    "persist::save_address_jump",
    "history::record",
    "state::forget_missing_place_model",
    "bevy_ecs::apply_deferred",
    "coverage::update_coverage",
    "coverage::paint_overlay",
    "coverage::draw_region",
    "world::apply_ground_tiles",
    "bevy_ecs::apply_deferred",
    "world::follow_theme",
    "world::highlight_address",
    "input::pointer",
    "input::focus_on_ground",
    "input::apply_orbit",
    "cameras::draw_frustums",
    "coverage::draw_outside_area",
    "net::start_area_loads",
    "net::pump_tiles",
    "net::send_region_requests",
    "net::drain_replies",
];

/// The client's plugins over fake ports, headless.
fn client_app() -> App {
    let platform = Platform {
        http: Arc::new(FakeHttp::default()),
        local: Arc::new(MemoryStore::default()),
        session: Arc::new(MemoryStore::default()),
        files: Arc::new(FakeFiles::default()),
        clock: Arc::new(FakeClock::at(0.0)),
        scheme: Arc::new(FakeColorScheme::new(false)),
    };
    let mut app = App::new();
    app.insert_resource(platform)
        .add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Image>()
        .add_plugins(ClientPlugins);
    app
}

/// A type or system name without this crate's prefix.
fn short(name: &str) -> String {
    name.replace("observation_client::", "")
}

/// An unordered conflicting pair: the two systems by name, and the data
/// they share.
type Conflict = ((String, String), Vec<String>);

/// A schedule as it runs: its systems in run order, and its unordered
/// conflicting pairs, each sorted by name.
struct Resolved {
    order: Vec<String>,
    conflicts: Vec<Conflict>,
}

fn resolve(app: &mut App, label: InternedScheduleLabel) -> Resolved {
    let mut resolved = Resolved {
        order: Vec::new(),
        conflicts: Vec::new(),
    };
    app.world_mut().schedule_scope(label, |world, schedule| {
        schedule.initialize(world).expect("the schedule builds");
        let names: BTreeMap<_, _> = schedule
            .systems()
            .expect("initialised")
            .map(|(key, system)| (key, short(&system.name().to_string())))
            .collect();
        resolved.order = schedule
            .systems()
            .expect("initialised")
            .map(|(key, _)| names[&key].clone())
            .collect();
        resolved.conflicts = schedule
            .graph()
            .conflicting_systems()
            .iter()
            .map(|(a, b, on)| {
                let mut on: Vec<String> = on
                    .iter()
                    .map(|id| {
                        let name = world.components().get_name(*id).expect("registered");
                        short(&name.to_string())
                    })
                    .collect();
                on.sort();
                let (a, b) = (names[a].clone(), names[b].clone());
                (if a <= b { (a, b) } else { (b, a) }, on)
            })
            .collect();
        resolved.conflicts.sort();
    });
    resolved
}

#[test]
fn update_runs_in_the_pinned_order() {
    let order = resolve(&mut client_app(), Update.intern()).order;
    assert!(
        order == UPDATE_ORDER,
        "{ADVICE}\n\npinned:\n{UPDATE_ORDER:#?}\nnow:\n{order:#?}"
    );
}

/// Unordered conflicting pairs outside `Update`, by schedule: only Bevy's
/// own systems, which it adds for each gizmo group the client configures.
/// Each writes only its own group's buffer and asset, so they commute.
const OTHER_CONFLICTS: &[(&str, &str, &str, &[&str])] = &[(
    "Last",
    "bevy_gizmos::update_gizmo_meshes<coverage::HatchGizmos>",
    "bevy_gizmos::update_gizmo_meshes<coverage::RegionGizmos>",
    &["bevy_asset::assets::Assets<bevy_gizmos::GizmoAsset>"],
)];

/// Every other schedule leaves no conflicting pair of the client's to
/// tie-breaking.
#[test]
fn other_schedules_have_no_unordered_conflicts() {
    let mut app = client_app();
    let labels: Vec<InternedScheduleLabel> = app
        .world()
        .resource::<Schedules>()
        .iter()
        .map(|(_, schedule)| schedule.label())
        .filter(|label| *label != Update.intern())
        .collect();
    assert!(labels.len() > 4, "the schedules are found: {labels:?}");
    for label in labels {
        let name = format!("{label:?}");
        let expected: Vec<Conflict> = OTHER_CONFLICTS
            .iter()
            .filter(|(schedule, ..)| *schedule == name)
            .map(|(_, a, b, on)| {
                let on = on.iter().map(|s| (*s).to_owned()).collect();
                (((*a).to_owned(), (*b).to_owned()), on)
            })
            .collect();
        let conflicts = resolve(&mut app, label).conflicts;
        assert!(
            conflicts == expected,
            "{ADVICE}\n\n{name} has unordered conflicting pairs: {conflicts:#?}\nexpected: {expected:#?}"
        );
    }
}
