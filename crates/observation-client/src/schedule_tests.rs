//! Checks how the client's systems are ordered in the app's schedules.
//!
//! On the web the executor is single-threaded: `Update` runs its systems one
//! after another, in an order Bevy resolves from the declared edges. Where
//! two systems use the same data (one of them mutably) and no edge orders
//! them, Bevy's tie-breaking picks the order, and any new system or edge
//! anywhere in the schedule can flip it. So every pair whose order the user
//! could see is ordered by an edge ([`ORDERED`]), and the pairs left to
//! tie-breaking are exactly the ones whose order does not matter
//! ([`COMMUTES`]).
//!
//! The app is the binary's own ([`crate::app`] with [`crate::default_plugins`]),
//! built headless: `WinitPlugin` is disabled (no event loop, no real window)
//! and the render plugin gets no graphics backend, so the render sub-app is
//! not created. What this leaves out: the render sub-app's schedules, winit's
//! systems, and the branches Bevy takes only on the web (this is a native
//! build). None of them adds a system to the client's schedules.
//!
//! A system is the client's own when its type name is in this crate
//! (`observation_client::…`); the model crate has no systems. Bevy's systems
//! and the sync points it inserts (`bevy_ecs::apply_deferred`) are left out.

use bevy::ecs::schedule::graph::Direction;
use bevy::ecs::schedule::{
    InternedScheduleLabel, NodeId, ScheduleBuildSettings, ScheduleLabel, Schedules, SystemKey,
};
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::winit::WinitPlugin;
use std::collections::{BTreeMap, BTreeSet};

const CRATE: &str = "observation_client::";

/// What a system that draws shows after an edit made before it in the frame.
const EDIT_DRAWN: &str = "what this draws (cones, coverage, overlay, region, address highlight) \
    shows the edit (a key, an import, an undo or redo, an area load) in its frame";
/// What a system that draws shows after a click: the pointer runs after it.
const CLICK_DRAWN: &str = "a click edit is drawn (and, without a worker, its coverage \
    computed) one frame later";
const EXPORT: &str = "an Export includes the edits of its frame (keys, clicks, an undo or redo)";

/// Pairs of `Update` systems that use the same data and whose order the user
/// can see: `first` must run before `second`, through an edge or a chain of
/// edges. The text says what that order gives.
const ORDERED: &[(&str, &str, &str)] = &[
    (
        "input::keyboard",
        "cameras::sync_bodies",
        "a camera deleted with the Delete key loses its body in the frame its cone goes",
    ),
    (
        "persist::receive_imports",
        "cameras::sync_bodies",
        "imported cameras get their bodies in the frame their cones appear",
    ),
    (
        "cameras::sync_bodies",
        "world::on_area_loaded",
        "a new area is drawn for one frame with the old cameras' bodies",
    ),
    (
        "cameras::sync_bodies",
        "history::record",
        "after an undo or redo the bodies show the old cameras for one frame while the cones show the restored ones",
    ),
    (
        "input::keyboard",
        "history::record",
        "a key edit is recorded in its own frame, so a click or panel edit in the next frame is a step of its own, and an undo from the toolbar in that frame reverts the key edit",
    ),
    (
        "persist::receive_imports",
        "history::record",
        "an import is recorded as its own undo step in its frame, before its area starts loading",
    ),
    (
        "world::on_area_loaded",
        "history::record",
        "an area load is recorded as its own step in its frame; an undo asked for in that frame reverts it and clears the data notice",
    ),
    (
        "history::record",
        "net::start_area_loads",
        "an undo asked for in the frame after a Load click still runs, and the load starts after it",
    ),
    (
        "history::record",
        "net::drain_replies",
        "`record` stays busy in the frame an area reply arrives, so an edit made while the area loads is recorded together with the load, and an undo in that frame is dropped",
    ),
    (
        "persist::receive_imports",
        "net::start_area_loads",
        "the import's area request and loading card come in the frame of the \"Imported\" toast",
    ),
    (
        "world::on_area_loaded",
        "net::drain_replies",
        "the loading card goes in the frame the reply arrives and the area shows in the next",
    ),
    (
        "world::apply_ground_tiles",
        "net::drain_replies",
        "an arrived tile shows in the map panel one frame before it textures the 3D ground",
    ),
    ("history::record", "persist::handle_actions", EXPORT),
    ("input::keyboard", "persist::handle_actions", EXPORT),
    ("input::pointer", "persist::handle_actions", EXPORT),
    ("coverage::update_coverage", "input::pointer", CLICK_DRAWN),
    ("coverage::draw_region", "input::pointer", CLICK_DRAWN),
    ("coverage::draw_outside_area", "input::pointer", CLICK_DRAWN),
    ("world::highlight_address", "input::pointer", CLICK_DRAWN),
    ("cameras::draw_frustums", "input::pointer", CLICK_DRAWN),
    ("history::record", "cameras::draw_frustums", EDIT_DRAWN),
    ("input::keyboard", "cameras::draw_frustums", EDIT_DRAWN),
    (
        "persist::receive_imports",
        "cameras::draw_frustums",
        EDIT_DRAWN,
    ),
    (
        "world::on_area_loaded",
        "cameras::draw_frustums",
        EDIT_DRAWN,
    ),
    (
        "cameras::sync_bodies",
        "input::pointer",
        "the bodies follow a click edit one frame later, and the pointer picks against this frame's bodies",
    ),
    ("history::record", "coverage::update_coverage", EDIT_DRAWN),
    ("input::keyboard", "coverage::update_coverage", EDIT_DRAWN),
    (
        "persist::receive_imports",
        "coverage::update_coverage",
        EDIT_DRAWN,
    ),
    ("history::record", "coverage::paint_overlay", EDIT_DRAWN),
    ("input::keyboard", "coverage::paint_overlay", EDIT_DRAWN),
    (
        "persist::receive_imports",
        "coverage::paint_overlay",
        EDIT_DRAWN,
    ),
    ("history::record", "coverage::draw_region", EDIT_DRAWN),
    ("input::keyboard", "coverage::draw_region", EDIT_DRAWN),
    (
        "persist::receive_imports",
        "coverage::draw_region",
        EDIT_DRAWN,
    ),
    ("history::record", "coverage::draw_outside_area", EDIT_DRAWN),
    ("input::keyboard", "coverage::draw_outside_area", EDIT_DRAWN),
    (
        "persist::receive_imports",
        "coverage::draw_outside_area",
        EDIT_DRAWN,
    ),
    ("history::record", "world::highlight_address", EDIT_DRAWN),
    ("input::keyboard", "world::highlight_address", EDIT_DRAWN),
    (
        "persist::receive_imports",
        "world::highlight_address",
        EDIT_DRAWN,
    ),
];

const SCENE: &str = "state::Scene";
const MODELS: &str = "state::Models";
const EDITOR: &str = "state::Editor";
const TOASTS: &str = "state::Toasts";
const TILES: &str = "net::Tiles";

const TOAST_LIFETIME: &str = "toast lifetime only: `tick_toasts` ages toasts by the frame's \
    time, so a toast pushed before it in the same frame lives one frame less, out of 4 s or more";
const AUTOSAVE: &str = "autosave runs on a timer of arbitrary phase: either way an edit of \
    its frame is stored at this save or the next, as if made a frame earlier or later";
const PLACE_MODEL: &str = "disjoint fields: `forget_missing_place_model` writes only the \
    Place tool's model, which only a pointer click reads";
const REPLY_TOASTS: &str = "replies are picked up from the network: only the place of their \
    toasts among this frame's moves, which arrival timing moves anyway";

/// Pairs of `Update` systems that use the same data with no edge between
/// them, and why their order does not matter: `(one, other, data, why)`,
/// with `one` before `other` alphabetically. `data` is what Bevy reports
/// they share, sorted.
const COMMUTES: &[(&str, &str, &[&str], &str)] = &[
    (
        "cameras::draw_frustums",
        "state::forget_missing_place_model",
        &[EDITOR],
        PLACE_MODEL,
    ),
    (
        "coverage::draw_outside_area",
        "state::forget_missing_place_model",
        &[EDITOR],
        PLACE_MODEL,
    ),
    (
        "coverage::draw_region",
        "state::forget_missing_place_model",
        &[EDITOR],
        PLACE_MODEL,
    ),
    (
        "state::forget_missing_place_model",
        "world::highlight_address",
        &[EDITOR],
        PLACE_MODEL,
    ),
    (
        "history::record",
        "persist::autosave",
        &[MODELS, SCENE],
        AUTOSAVE,
    ),
    ("input::keyboard", "persist::autosave", &[SCENE], AUTOSAVE),
    ("input::pointer", "persist::autosave", &[SCENE], AUTOSAVE),
    (
        "persist::autosave",
        "persist::receive_imports",
        &[MODELS, SCENE],
        AUTOSAVE,
    ),
    (
        "persist::autosave",
        "world::on_area_loaded",
        &[SCENE],
        AUTOSAVE,
    ),
    (
        "history::record",
        "ui::tick_toasts",
        &[TOASTS],
        TOAST_LIFETIME,
    ),
    (
        "input::keyboard",
        "ui::tick_toasts",
        &[TOASTS],
        TOAST_LIFETIME,
    ),
    (
        "input::pointer",
        "ui::tick_toasts",
        &[TOASTS],
        TOAST_LIFETIME,
    ),
    (
        "net::drain_replies",
        "ui::tick_toasts",
        &[TOASTS],
        TOAST_LIFETIME,
    ),
    (
        "persist::handle_actions",
        "ui::tick_toasts",
        &[TOASTS],
        TOAST_LIFETIME,
    ),
    (
        "persist::receive_imports",
        "ui::tick_toasts",
        &[TOASTS],
        TOAST_LIFETIME,
    ),
    (
        "ui::tick_toasts",
        "world::on_area_loaded",
        &[TOASTS],
        TOAST_LIFETIME,
    ),
    (
        "input::pointer",
        "net::drain_replies",
        &[TOASTS],
        REPLY_TOASTS,
    ),
    (
        "net::drain_replies",
        "persist::handle_actions",
        &[TOASTS],
        REPLY_TOASTS,
    ),
    (
        "input::keyboard",
        "persist::receive_imports",
        &[EDITOR, SCENE, TOASTS],
        "the imported file is picked up from the file dialog: whether a key edit of its frame \
         lands on the old or the new scene already depends on when the file arrives",
    ),
    (
        "net::pump_tiles",
        "world::apply_ground_tiles",
        &[TILES],
        "only the frame a tile request is sent in moves; when the tile arrives depends on the \
         network",
    ),
];

/// The binary's app, headless (see the module documentation).
fn headless_app() -> App {
    crate::app(
        crate::default_plugins()
            .disable::<WinitPlugin>()
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    backends: None,
                    ..default()
                })),
                ..default()
            }),
    )
}

fn client_name(name: &str) -> Option<String> {
    let name = name.trim_prefix('<');
    name.strip_prefix(CRATE).map(str::to_owned)
}

/// One schedule, built: the client's systems in run order, which client
/// system reaches which through declared edges, and the conflicting pairs
/// among the client's systems with the data they share.
struct Built {
    order: Vec<String>,
    before: BTreeSet<(String, String)>,
    unordered: BTreeMap<(String, String), Vec<String>>,
}

/// Builds `label`. With `sync_points` off, Bevy inserts no automatic sync
/// points, so the conflicting pairs are all those no declared edge orders:
/// a sync point orders the systems on either side of it, but where Bevy puts
/// one depends on the whole graph, much like tie-breaking.
fn build(app: &mut App, label: InternedScheduleLabel, sync_points: bool) -> Built {
    let mut built = Built {
        order: Vec::new(),
        before: BTreeSet::new(),
        unordered: BTreeMap::new(),
    };
    app.world_mut().schedule_scope(label, |world, schedule| {
        if !sync_points {
            schedule.set_build_settings(ScheduleBuildSettings {
                auto_insert_apply_deferred: false,
                ..schedule.get_build_settings()
            });
        }
        // Fails on a cycle.
        schedule.initialize(world).expect("the schedule builds");
        let names: BTreeMap<SystemKey, String> = schedule
            .systems()
            .expect("initialized")
            .filter_map(|(key, system)| Some((key, client_name(&system.name().to_string())?)))
            .collect();
        built.order = schedule
            .systems()
            .expect("initialized")
            .filter_map(|(key, _)| names.get(&key).cloned())
            .collect();
        let graph = schedule.graph();

        // The systems under each node: itself for a system, its members for
        // a set (including the set of each function's systems that
        // `.before(f)` and `.after(f)` name).
        let hierarchy = graph.hierarchy().graph();
        let members = |node: NodeId| -> Vec<SystemKey> {
            let mut out = Vec::new();
            let mut stack = vec![node];
            let mut seen = BTreeSet::new();
            while let Some(n) = stack.pop() {
                if !seen.insert(n) {
                    continue;
                }
                match n {
                    NodeId::System(key) => out.push(key),
                    NodeId::Set(_) => {
                        stack.extend(hierarchy.neighbors_directed(n, Direction::Outgoing))
                    }
                }
            }
            out
        };
        let mut next: BTreeMap<SystemKey, BTreeSet<SystemKey>> = BTreeMap::new();
        for (a, b) in graph.dependency().graph().all_edges() {
            let to = members(b);
            for from in members(a) {
                next.entry(from).or_default().extend(to.iter().copied());
            }
        }
        for (&start, first) in &names {
            let mut stack: Vec<SystemKey> =
                next.get(&start).into_iter().flatten().copied().collect();
            let mut seen = BTreeSet::new();
            while let Some(n) = stack.pop() {
                if seen.insert(n) {
                    stack.extend(next.get(&n).into_iter().flatten().copied());
                }
            }
            for n in seen {
                if let Some(second) = names.get(&n) {
                    built.before.insert((first.clone(), second.clone()));
                }
            }
        }

        for (a, b, on) in &graph.conflicting_systems().0 {
            let (Some(a), Some(b)) = (names.get(a), names.get(b)) else {
                continue;
            };
            let mut on: Vec<String> = on
                .iter()
                .map(|id| {
                    world
                        .components()
                        .get_name(*id)
                        .expect("a registered component")
                        .to_string()
                        .replace(CRATE, "")
                })
                .collect();
            on.sort();
            let key = if a <= b {
                (a.clone(), b.clone())
            } else {
                (b.clone(), a.clone())
            };
            built.unordered.insert(key, on);
        }
    });
    built
}

fn update(sync_points: bool) -> Built {
    build(&mut headless_app(), Update.intern(), sync_points)
}

#[test]
fn order_sensitive_pairs_are_ordered_by_edges() {
    let built = update(true);
    let mut problems = Vec::new();
    for &(first, second, what) in ORDERED {
        for name in [first, second] {
            if !built.order.iter().any(|n| n == name) {
                problems.push(format!(
                    "`{name}` is no longer in `Update`: update `ORDERED`"
                ));
            }
        }
        if built
            .before
            .contains(&(first.to_owned(), second.to_owned()))
        {
            continue;
        }
        let actual = if built
            .before
            .contains(&(second.to_owned(), first.to_owned()))
        {
            "an edge orders them the other way round"
        } else {
            "no edge orders them, so Bevy's tie-breaking does"
        };
        problems.push(format!(
            "`{first}` must run before `{second}` ({what}), but {actual}: \
             add `.before`/`.after` edges that order them this way"
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn unordered_pairs_in_update_are_the_commuting_ones() {
    let built = update(false);
    let mut problems = Vec::new();
    let mut listed = BTreeMap::new();
    for &(one, other, data, why) in COMMUTES {
        let key = if one <= other {
            (one, other)
        } else {
            (other, one)
        };
        assert!(
            listed.insert(key, (data, why)).is_none(),
            "{one} / {other} listed twice"
        );
    }
    for ((a, b), on) in &built.unordered {
        match listed.remove(&(a.as_str(), b.as_str())) {
            None => problems.push(format!(
                "`{a}` / `{b}` share {on:?} with no edge between them, so Bevy's \
                 tie-breaking orders them. If the user could see their order, add \
                 `.before`/`.after` edges and list the pair in `ORDERED`; if not, list it \
                 in `COMMUTES` with the data and why the order does not matter"
            )),
            Some((data, why)) if data != on.as_slice() => problems.push(format!(
                "`{a}` / `{b}` now share {on:?}, listed as {data:?}: check that \
                 \"{why}\" still holds, then update the data in `COMMUTES`"
            )),
            Some(_) => {}
        }
    }
    for ((a, b), _) in listed {
        problems.push(format!(
            "`{a}` / `{b}` is listed in `COMMUTES` but is no longer an unordered \
             conflicting pair (an edge orders it now, or the two share no data): \
             remove it, and if an edge now orders it on purpose, list it in `ORDERED`"
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn order_sensitive_pairs_survive_an_unrelated_system() {
    // An unrelated system changes how Bevy breaks ties; the declared edges
    // must keep every order-sensitive pair in place regardless.
    fn unrelated() {}
    let mut app = headless_app();
    app.add_systems(Update, unrelated);
    let order = build(&mut app, Update.intern(), true).order;
    let position = |name: &str| order.iter().position(|n| n == name);
    for &(first, second, what) in ORDERED {
        let (Some(a), Some(b)) = (position(first), position(second)) else {
            panic!("`{first}` or `{second}` is not in `Update`");
        };
        assert!(a < b, "`{first}` must run before `{second}` ({what})");
    }
}

#[test]
fn other_schedules_have_no_unordered_client_pairs() {
    let mut app = headless_app();
    let labels: Vec<InternedScheduleLabel> = app
        .world()
        .resource::<Schedules>()
        .iter()
        .map(|(_, schedule)| schedule.label())
        .filter(|label| *label != Update.intern())
        .collect();
    let mut problems = Vec::new();
    for label in labels {
        for ((a, b), on) in build(&mut app, label, false).unordered {
            problems.push(format!(
                "{label:?}: `{a}` / `{b}` share {on:?} with no edge between them: \
                 order them with `.before`/`.after`"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
