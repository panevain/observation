//! Pins how [`ClientPlugins`] are scheduled.
//!
//! On the web the executor is single-threaded, so `Update` runs its systems
//! one after another in the order Bevy resolves from the declared edges.
//! Where two systems that touch the same data have no edge between them,
//! Bevy's tie-breaking picks their order, and an unrelated change (a new
//! system, a new set, a new `Commands` parameter) can flip it. These tests
//! make such a flip visible: [`update_runs_in_the_pinned_order`] pins the
//! whole order, and [`update_conflicts_are_the_known_ones`] pins which pairs
//! are left to tie-breaking and why that is acceptable or not.
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

/// Whether the order of an unordered pair matters to the user.
#[derive(Debug, Clone, Copy)]
enum Order {
    /// Flipping it changes what the user sees or gets: what.
    Observable(&'static str),
    /// Flipping it changes nothing the user can tell apart: why.
    Commutes(&'static str),
}

/// Two systems in `Update` that use the same data, at least one mutably,
/// with no edge between them: `first` runs before `second` today (see
/// [`UPDATE_ORDER`]). `on` is the data they share, as Bevy reports it.
struct Pair {
    first: &'static str,
    second: &'static str,
    on: &'static [&'static str],
    order: Order,
}

use Order::{Commutes, Observable};

const SCENE: &str = "state::Scene";
const MODELS: &str = "state::Models";
const EDITOR: &str = "state::Editor";
const TOASTS: &str = "state::Toasts";
const PANELS: &str = "state::Panels";
const HOVER: &str = "state::Hover";
const TILES: &str = "net::Tiles";
const LOADING: &str = "state::Loading";
const NOTICE: &str = "state::DataChangeNotice";
const ORBIT: &str = "input::OrbitCamera";
const MESHES: &str = "bevy_asset::assets::Assets<bevy_mesh::mesh::Mesh>";
const MATERIALS: &str = "bevy_asset::assets::Assets<bevy_pbr::pbr_material::StandardMaterial>";
const LOAD_AREA: &str = "bevy_ecs::message::messages::Messages<state::LoadArea>";
const AREA_LOADED: &str = "bevy_ecs::message::messages::Messages<state::AreaLoaded>";

// Reasons use these terms:
// - "picked up": the system's only input arrives from outside the frame (a
//   network reply, a picked file, the autosave timer). Flipping it with a
//   system that does not draw or derive from its output is the same as the
//   input arriving one frame earlier or later, which already varies.
// - "one frame": for one frame, something drawn lags the input or disagrees
//   with something else drawn; the next frame agrees again.
// - "rare": the outcome differs, but only when both act in the same frame.
// - toast lifetime: `tick_toasts` ages toasts by the frame's time, so a toast
//   pushed before it in the same frame lives one frame shorter, out of 4 s or
//   more.
const UPDATE_CONFLICTS: &[Pair] = &[
    Pair {
        first: "persist::handle_actions",
        second: "history::record",
        on: &[MODELS, SCENE, TOASTS],
        order: Observable(
            "rare: an Export and an undo or redo in the same frame export the scene before the step; flipped, after it",
        ),
    },
    Pair {
        first: "persist::handle_actions",
        second: "input::keyboard",
        on: &[SCENE, TOASTS],
        order: Observable(
            "rare: Export leaves out a key edit made in the frame after the click; flipped, the file includes it",
        ),
    },
    Pair {
        first: "persist::handle_actions",
        second: "input::pointer",
        on: &[SCENE, TOASTS],
        order: Observable(
            "rare: Export leaves out a scene click made in the frame after the click; flipped, the file includes it",
        ),
    },
    Pair {
        first: "persist::handle_actions",
        second: "persist::receive_imports",
        on: &[MODELS, SCENE, TOASTS],
        order: Commutes(
            "the imported file is picked up: whether an Export in the same frame sees it is already a matter of timing",
        ),
    },
    Pair {
        first: "persist::handle_actions",
        second: "ui::tick_toasts",
        on: &[TOASTS],
        order: Commutes("toast lifetime only"),
    },
    Pair {
        first: "persist::handle_actions",
        second: "world::on_area_loaded",
        on: &[SCENE, TOASTS],
        order: Commutes(
            "the area reply is picked up: whether an Export in the same frame sees it is already a matter of timing",
        ),
    },
    Pair {
        first: "persist::handle_actions",
        second: "net::drain_replies",
        on: &[TOASTS],
        order: Commutes("replies are picked up: only their toasts' place among this frame's moves"),
    },
    Pair {
        first: "ui::tick_toasts",
        second: "persist::receive_imports",
        on: &[TOASTS],
        order: Commutes("toast lifetime only"),
    },
    Pair {
        first: "ui::tick_toasts",
        second: "input::keyboard",
        on: &[TOASTS],
        order: Commutes("toast lifetime only"),
    },
    Pair {
        first: "ui::tick_toasts",
        second: "world::on_area_loaded",
        on: &[TOASTS],
        order: Commutes("toast lifetime only"),
    },
    Pair {
        first: "ui::tick_toasts",
        second: "history::record",
        on: &[TOASTS],
        order: Commutes("toast lifetime only"),
    },
    Pair {
        first: "ui::tick_toasts",
        second: "input::pointer",
        on: &[TOASTS],
        order: Commutes("toast lifetime only"),
    },
    Pair {
        first: "ui::tick_toasts",
        second: "net::drain_replies",
        on: &[TOASTS],
        order: Commutes("toast lifetime only"),
    },
    Pair {
        first: "cameras::sync_bodies",
        second: "persist::receive_imports",
        on: &[MODELS, SCENE],
        order: Observable(
            "one frame: the imported cameras' bodies appear a frame after their cones; flipped, together",
        ),
    },
    Pair {
        first: "cameras::sync_bodies",
        second: "input::keyboard",
        on: &[SCENE],
        order: Observable(
            "one frame: a camera deleted with the Delete key keeps its body a frame after its cone is gone; flipped, both go together",
        ),
    },
    Pair {
        first: "cameras::sync_bodies",
        second: "world::on_area_loaded",
        on: &[MATERIALS, SCENE],
        order: Observable(
            "one frame: a new area is drawn with the old cameras' bodies; flipped, with its own",
        ),
    },
    Pair {
        first: "cameras::sync_bodies",
        second: "history::record",
        on: &[MODELS, SCENE],
        order: Observable(
            "one frame: after an undo or redo the bodies show the old cameras while the cones show the restored ones; flipped, they agree",
        ),
    },
    Pair {
        first: "persist::receive_imports",
        second: "input::keyboard",
        on: &[EDITOR, SCENE, TOASTS],
        order: Commutes(
            "the imported file is picked up: a key edit in its frame lands on the old or the new scene depending on timing already",
        ),
    },
    Pair {
        first: "persist::receive_imports",
        second: "persist::autosave",
        on: &[MODELS, SCENE],
        order: Commutes(
            "autosave runs on a timer of arbitrary phase: flipped, the import is stored at the next save, as if it came a frame later",
        ),
    },
    Pair {
        first: "persist::receive_imports",
        second: "world::on_area_loaded",
        on: &[EDITOR, MODELS, SCENE, TOASTS],
        order: Commutes(
            "both are picked up (a file and an area reply): which lands first in a frame is already a matter of timing",
        ),
    },
    Pair {
        first: "persist::receive_imports",
        second: "history::record",
        on: &[EDITOR, LOAD_AREA, MODELS, SCENE, TOASTS],
        order: Observable(
            "an import is recorded as its own undo step in its frame; flipped, `record` next runs while its area loads, so the import folds into the step taken after the load",
        ),
    },
    Pair {
        first: "persist::receive_imports",
        second: "net::start_area_loads",
        on: &[LOAD_AREA],
        order: Observable(
            "one frame: the import's area request and loading card come a frame after the \"Imported\" toast; flipped, together",
        ),
    },
    Pair {
        first: "persist::receive_imports",
        second: "net::drain_replies",
        on: &[TOASTS],
        order: Commutes(
            "both are picked up: only the place of their toasts in the stack moves, as timing already moves it",
        ),
    },
    Pair {
        first: "input::keyboard",
        second: "persist::autosave",
        on: &[SCENE],
        order: Commutes(
            "autosave runs on a timer of arbitrary phase: flipped, a key edit is stored at the next save, as if pressed a frame later",
        ),
    },
    Pair {
        first: "input::keyboard",
        second: "world::on_area_loaded",
        on: &[EDITOR, ORBIT, SCENE, TOASTS],
        order: Commutes(
            "the area reply is picked up: a key edit or focus in its frame acts on the old or the new area depending on timing already",
        ),
    },
    Pair {
        first: "input::keyboard",
        second: "history::record",
        on: &[EDITOR, SCENE, TOASTS],
        order: Observable(
            "a key edit is recorded in its own frame; flipped, a frame later, so a click or UI edit in that frame joins its undo step, and an undo from the toolbar in the same frame reverts the step before the key edit instead of the key edit",
        ),
    },
    Pair {
        first: "input::keyboard",
        second: "state::forget_missing_place_model",
        on: &[EDITOR],
        order: Commutes(
            "disjoint fields: `forget_missing_place_model` writes only the Place tool's model, which the key commands neither read nor write",
        ),
    },
    Pair {
        first: "input::keyboard",
        second: "net::drain_replies",
        on: &[TOASTS],
        order: Commutes("replies are picked up: only their toasts' place among this frame's moves"),
    },
    Pair {
        first: "persist::autosave",
        second: "world::on_area_loaded",
        on: &[SCENE],
        order: Commutes(
            "autosave runs on a timer of arbitrary phase: flipped, the loaded area is stored a save earlier, as if the reply came a frame earlier",
        ),
    },
    Pair {
        first: "persist::autosave",
        second: "history::record",
        on: &[MODELS, SCENE],
        order: Commutes(
            "autosave runs on a timer of arbitrary phase: flipped, an undo is stored a save earlier, as if asked a frame earlier",
        ),
    },
    Pair {
        first: "persist::autosave",
        second: "input::pointer",
        on: &[SCENE],
        order: Commutes(
            "autosave runs on a timer of arbitrary phase: flipped, a click edit is stored a save earlier, as if made a frame earlier",
        ),
    },
    Pair {
        first: "world::on_area_loaded",
        second: "history::record",
        on: &[EDITOR, MODELS, NOTICE, SCENE, TOASTS],
        order: Observable(
            "an area load is recorded as its own undo step in its frame, then an undo asked for in that frame reverts it and clears the data notice; flipped, the undo runs first and the load sets the notice, and a click in that frame joins the load's step",
        ),
    },
    Pair {
        first: "world::on_area_loaded",
        second: "state::forget_missing_place_model",
        on: &[EDITOR],
        order: Commutes(
            "disjoint fields: an area load never touches the Place tool's model, the only field `forget_missing_place_model` writes",
        ),
    },
    Pair {
        first: "world::on_area_loaded",
        second: "net::pump_tiles",
        on: &[TILES],
        order: Commutes(
            "disjoint fields: the load only pins the ground's tiles, which only the map panel's queue trimming reads; `pump_tiles` only sends queued requests",
        ),
    },
    Pair {
        first: "world::on_area_loaded",
        second: "net::drain_replies",
        on: &[AREA_LOADED, TILES, TOASTS],
        order: Observable(
            "one frame: the loading card goes in the frame the reply arrives and the area shows a frame later; flipped, together",
        ),
    },
    Pair {
        first: "history::record",
        second: "net::start_area_loads",
        on: &[LOAD_AREA, LOADING],
        order: Observable(
            "rare: an undo asked for in the frame after a Load click still runs, and the load starts after it; flipped, the load starts first and the undo is dropped as busy",
        ),
    },
    Pair {
        first: "history::record",
        second: "net::drain_replies",
        on: &[LOADING, TOASTS],
        order: Commutes(
            "replies are picked up: flipped, `record` stops being busy a frame earlier, as if the area reply came a frame earlier",
        ),
    },
    Pair {
        first: "coverage::update_coverage",
        second: "input::pointer",
        on: &[SCENE],
        order: Observable(
            "one frame: a click edit is computed (on the main thread, without a worker) and painted a frame later; flipped, in its own frame",
        ),
    },
    Pair {
        first: "coverage::paint_overlay",
        second: "input::pointer",
        on: &[MESHES, PANELS, SCENE],
        order: Commutes(
            "the painting follows the coverage results, which `update_coverage` (before both) makes; the pointer changes no colour or overlay setting, and a recoloured mesh does not change what a ray hits",
        ),
    },
    Pair {
        first: "coverage::draw_region",
        second: "input::pointer",
        on: &[EDITOR, SCENE],
        order: Observable(
            "one frame: the region outline and draft are drawn before the click that changes them; flipped, after",
        ),
    },
    Pair {
        first: "world::apply_ground_tiles",
        second: "net::pump_tiles",
        on: &[TILES],
        order: Commutes(
            "only the frame a tile request is sent moves; the reply's arrival is network-bound",
        ),
    },
    Pair {
        first: "world::apply_ground_tiles",
        second: "net::drain_replies",
        on: &[TILES],
        order: Observable(
            "one frame: an arrived tile is shown in the map panel a frame before it textures the 3D ground; flipped, together",
        ),
    },
    Pair {
        first: "world::highlight_address",
        second: "input::pointer",
        on: &[MESHES, EDITOR],
        order: Observable(
            "one frame: the highlight follows an Address tool click a frame later; flipped, in its frame",
        ),
    },
    Pair {
        first: "input::pointer",
        second: "cameras::draw_frustums",
        on: &[EDITOR, HOVER, PANELS, SCENE],
        order: Observable(
            "one frame: the cones show this frame's clicks, drags and hover; flipped, the cones lag them by a frame",
        ),
    },
    Pair {
        first: "input::pointer",
        second: "coverage::draw_outside_area",
        on: &[EDITOR],
        order: Observable(
            "one frame: a click that ends the Region tool hides the outside-area hatching at once; flipped, a frame later",
        ),
    },
    Pair {
        first: "input::pointer",
        second: "net::drain_replies",
        on: &[TOASTS],
        order: Commutes("replies are picked up: only their toasts' place among this frame's moves"),
    },
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

#[test]
fn update_conflicts_are_the_known_ones() {
    let mut expected: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for p in UPDATE_CONFLICTS {
        let (a, b) = (p.first.to_owned(), p.second.to_owned());
        let key = if a <= b { (a, b) } else { (b, a) };
        let mut on: Vec<String> = p.on.iter().map(|s| (*s).to_owned()).collect();
        on.sort();
        assert!(
            expected.insert(key, on).is_none(),
            "{} / {} is listed twice",
            p.first,
            p.second
        );
        let reason = match p.order {
            Observable(r) | Commutes(r) => r,
        };
        assert!(
            !reason.is_empty(),
            "{} / {}: give a reason",
            p.first,
            p.second
        );
    }
    let found: BTreeMap<_, _> = resolve(&mut client_app(), Update.intern())
        .conflicts
        .into_iter()
        .collect();
    let mut problems = Vec::new();
    for (pair, on) in &found {
        match expected.get(pair) {
            None => problems.push(format!(
                "new unordered pair {} / {} on {on:?}: order it with an edge, or list it with what flipping it would change",
                pair.0, pair.1
            )),
            Some(listed) if listed != on => problems.push(format!(
                "{} / {} now share {on:?}, listed as {listed:?}: check its reason still holds, then update it",
                pair.0, pair.1
            )),
            Some(_) => {}
        }
    }
    for pair in expected.keys().filter(|p| !found.contains_key(*p)) {
        problems.push(format!(
            "{} / {} is no longer an unordered conflicting pair (now ordered or independent): remove it from the list, and say in the pull request why",
            pair.0, pair.1
        ));
    }
    assert!(problems.is_empty(), "{ADVICE}\n\n{}", problems.join("\n"));
    // Each pair is listed in the order it runs today.
    for p in UPDATE_CONFLICTS {
        let at = |s: &str| UPDATE_ORDER.iter().position(|o| *o == s);
        assert!(
            at(p.first) < at(p.second),
            "{} runs after {}: swap them in the list",
            p.first,
            p.second
        );
    }
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
