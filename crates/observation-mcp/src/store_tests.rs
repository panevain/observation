//! Working scenes: the store's limits, the `scene_id` arguments on every
//! scene tool, and the tools, end to end through the HTTP transport (the
//! only way to reach `call_tool` in process), on the demo street.

use super::*;
use crate::catalog::catalog;
use crate::http::config::{Auth, HttpConfig};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use http_body_util::BodyExt;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

fn empty() -> Value {
    edit::finish(&edit::new_demo_scene().unwrap()).unwrap().1
}

/// `scene` with its subject height set to `h`, as a different version.
fn with_height(scene: &Value, h: f64) -> Value {
    let mut s = scene.clone();
    s["params"]["subject_height_m"] = json!(h);
    s
}

/// The MCP server over HTTP without authentication: every request reaches
/// one server, so one store.
pub(crate) fn http_app() -> Router {
    crate::http::router(
        &HttpConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            auth: Auth::Disabled,
            allowed_hosts: vec!["localhost".into()],
            allowed_origins: Vec::new(),
        },
        &CancellationToken::new(),
    )
}

/// Calls `tool` with `arguments` through `app`; returns the call's result.
pub(crate) async fn call(app: &Router, tool: &str, arguments: Value) -> Value {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    });
    let req = Request::post("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let message: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(message.get("error").is_none(), "{message}");
    message["result"].clone()
}

/// The structured result of a call that succeeded.
pub(crate) fn ok(result: &Value) -> Value {
    assert_ne!(result["isError"], true, "{result}");
    result["structuredContent"].clone()
}

/// The text of a call that failed.
pub(crate) fn failed(result: &Value) -> String {
    assert_eq!(result["isError"], true, "{result}");
    result["content"][0]["text"].as_str().unwrap().to_owned()
}

fn model_id() -> String {
    catalog().unwrap().models[0].id.clone()
}

#[test]
fn ids_are_short_and_forgiving() {
    assert_eq!(SceneId(3).to_string(), "s3");
    for (text, n) in [("s1", 1), ("S12", 12), (" s7 ", 7), ("4", 4)] {
        assert_eq!(SceneId::parse(text), Some(SceneId(n)), "{text}");
    }
    for text in ["", "s", "s0", "x1", "s1a", "s-1", "scene 1"] {
        assert_eq!(SceneId::parse(text), None, "{text}");
    }
    assert_eq!(parse_ref("s2@3"), Some((SceneId(2), Some(3))));
    assert_eq!(parse_ref("s2@v3"), Some((SceneId(2), Some(3))));
    assert_eq!(parse_ref("s2"), Some((SceneId(2), None)));
    assert_eq!(parse_ref("s2@"), None);
}

#[test]
fn versions_are_kept_for_undo_up_to_the_limit() {
    let store = SceneStore::default();
    let base = empty();
    let opened = store.open(&base).unwrap();
    let id = opened.id;
    assert_eq!((id.to_string().as_str(), opened.version), ("s1", 1));
    // The same scene again adds no version.
    assert_eq!(store.commit(id, 1, &base).unwrap().version, 1);
    for i in 0..30 {
        let next = with_height(&base, 2.0 + f64::from(i) / 4.0);
        assert_eq!(store.commit(id, 1 + i, &next).unwrap().version, 2 + i);
    }
    let listed = store.list().unwrap();
    assert_eq!((listed[0].version, listed[0].undo_steps), (31, MAX_UNDO));
    // Versions before the 20 earlier ones kept are gone.
    let e = store.get(id, Some(10)).unwrap_err().to_string();
    assert!(
        e.contains("keeps versions 11, 12,") && e.ends_with(", 31"),
        "{e}"
    );
    assert_eq!(store.get(id, Some(11)).unwrap().1, 11);

    // An edit made from an older version is refused, not lost silently.
    let e = store.commit(id, 30, &base).unwrap_err().to_string();
    assert!(e.contains("now at version 31"), "{e}");

    let ((v, scene), undone) = store.undo(id, 2).unwrap();
    assert_eq!(v, 29);
    assert_eq!(scene["params"]["subject_height_m"], json!(8.75));
    assert_eq!(undone["params"]["subject_height_m"], json!(9.25));
    // Later versions are dropped, and their numbers are not used again.
    assert!(store.get(id, Some(31)).is_err());
    assert_eq!(store.commit(id, 29, &base).unwrap().version, 32);
    let e = store.get(id, Some(30)).unwrap_err().to_string();
    assert!(e.ends_with("28, 29, 32"), "{e}");
    let e = store.undo(id, 21).unwrap_err().to_string();
    assert!(e.contains("1 to 19"), "{e}");
    assert!(store.undo(id, 0).is_err());
    let other = store.open(&base).unwrap().id;
    let e = store.undo(other, 1).unwrap_err().to_string();
    assert!(e.contains("no earlier version"), "{e}");
}

/// An edit read from version 2, which undo then dropped and another edit
/// replaced, is refused: the replacement has a new number, so the edit
/// cannot land on a scene it was not made from.
#[test]
fn an_edit_from_an_undone_version_is_refused() {
    let store = SceneStore::default();
    let base = empty();
    let id = store.open(&base).unwrap().id;
    // A slow call reads version 2.
    assert_eq!(
        store
            .commit(id, 1, &with_height(&base, 2.0))
            .unwrap()
            .version,
        2
    );
    let (_, read) = store.latest(id).unwrap();
    assert_eq!(read, 2);
    // Meanwhile the agent undoes it and makes another edit.
    store.undo(id, 1).unwrap();
    let other = store.commit(id, 1, &with_height(&base, 3.0)).unwrap();
    assert_eq!(other.version, 3);
    // The slow call's edit no longer fits.
    let e = store
        .commit(id, read, &with_height(&base, 4.0))
        .unwrap_err()
        .to_string();
    assert!(e.contains("now at version 3"), "{e}");
    // And s1@2 does not quietly name the other edit.
    assert!(store.get(id, Some(2)).is_err());
}

#[test]
fn the_least_recently_used_scene_makes_room_and_says_so() {
    let store = SceneStore::default();
    let base = empty();
    let ids: Vec<SceneId> = (0..MAX_SCENES)
        .map(|_| store.open(&base).unwrap().id)
        .collect();
    // Using s1 makes s2 the least recently used.
    store.latest(ids[0]).unwrap();
    let newest = store.open(&base).unwrap();
    assert_eq!(newest.id.to_string(), "s33");
    assert_eq!(newest.closed, [ids[1]]);
    assert_eq!(newest.closed_note(), "; closed s2 to make room");
    assert_eq!(store.ids().len(), MAX_SCENES);
    assert_eq!(store.ids()[0], newest.id);
    let e = store.latest(ids[1]).unwrap_err().to_string();
    assert!(e.contains("s2 was closed to make room"), "{e}");
    assert!(e.contains("at most 32"), "{e}");
    assert!(e.contains("Open scenes: s33, s1, s32"), "{e}");
    assert!(store.latest(ids[0]).is_ok());

    store.close(ids[2]).unwrap();
    let e = store.latest(ids[2]).unwrap_err().to_string();
    assert!(e.starts_with("scene s3 was closed. Open scenes"), "{e}");
    let e = store.latest(SceneId(99)).unwrap_err().to_string();
    assert!(e.starts_with("there is no scene s99."), "{e}");
    assert!(store.close(ids[2]).is_err());
}

/// The ids closed to make room are forgotten oldest eviction first, not
/// lowest id first.
#[test]
fn evictions_are_remembered_in_order() {
    let store = SceneStore::default();
    let base = empty();
    let first: Vec<SceneId> = (0..MAX_SCENES)
        .map(|_| store.open(&base).unwrap().id)
        .collect();
    // Keep s1 in use while hundreds of others come and go.
    for _ in 0..EVICTED_REMEMBERED + 40 {
        store.latest(first[0]).unwrap();
        store.open(&base).unwrap();
    }
    // Now s1, the lowest id, is closed to make room, and more after it.
    for _ in 0..=MAX_SCENES {
        store.open(&base).unwrap();
    }
    let e = store.latest(first[0]).unwrap_err().to_string();
    assert!(e.contains("s1 was closed to make room"), "{e}");
    // s2 was evicted long before, so it is the one forgotten.
    let e = store.latest(first[1]).unwrap_err().to_string();
    assert!(e.starts_with("scene s2 was closed."), "{e}");
}

#[test]
fn a_version_is_at_most_the_bridges_limit() {
    let store = SceneStore::default();
    let mut big = empty();
    big["padding"] = json!("x".repeat(MAX_SCENE_BYTES));
    let e = store.open(&big).unwrap_err().to_string();
    assert!(e.contains("at most 1024 KiB"), "{e}");
    let id = store.open(&empty()).unwrap().id;
    assert!(store.commit(id, 1, &big).is_err());
    assert!(store.ids().len() == 1);
}

/// Past the store's byte budget, the oldest earlier versions go first, then
/// the least recently used scenes, never the one just written.
#[test]
fn the_store_keeps_within_its_byte_budget() {
    let base = empty();
    let size = base.to_string().len();
    // Room for five versions.
    let store = SceneStore::with_budget(size * 5 + size / 2);
    let a = store.open(&base).unwrap().id;
    for i in 0..3 {
        store
            .commit(a, 1 + i, &with_height(&base, 2.0 + f64::from(i)))
            .unwrap();
    }
    let b = store.open(&base).unwrap();
    assert!(b.closed.is_empty());
    assert_eq!(store.list().unwrap().len(), 2);
    // A sixth version drops a's oldest.
    let c = store.open(&base).unwrap();
    assert!(c.closed.is_empty());
    let e = store.get(a, Some(1)).unwrap_err().to_string();
    assert!(e.contains("keeps versions 2, 3, 4"), "{e}");
    assert!(store.bytes() <= size * 5 + size / 2);
    // Once no earlier versions are left, the least recently used scene goes.
    store.undo(a, 2).unwrap();
    store.latest(b.id).unwrap();
    store.latest(c.id).unwrap();
    for _ in 0..2 {
        store.open(&base).unwrap();
    }
    let d = store.open(&base).unwrap();
    assert_eq!(d.closed, [a], "{d:?}");
    assert!(
        store
            .latest(a)
            .unwrap_err()
            .to_string()
            .contains("closed to make room")
    );
    assert!(store.bytes() <= size * 5 + size / 2);
}

/// Every tool that takes a scene, the ones from other modules and any added
/// later included, documents `scene_id` and no longer requires `scene`; one
/// that returns the scene edited documents `return_scene`, and its result
/// may hold `scene_id` and `version` instead of `scene`.
#[test]
fn every_scene_tool_takes_a_scene_id() {
    let tools = ObservationMcp::all_tools().list_all();
    let mut taking = Vec::new();
    let mut editing = Vec::new();
    for tool in &tools {
        let input = &tool.input_schema;
        let props = input["properties"].as_object().unwrap();
        let required = |schema: &Map<String, Value>, name: &str| {
            schema
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|r| r.iter().any(|x| x == name))
        };
        if !props.contains_key("scene") {
            assert!(!props.contains_key(RETURN_SCENE), "{}", tool.name);
            continue;
        }
        taking.push(tool.name.to_string());
        assert_eq!(props[SCENE_ID]["type"], "string", "{}", tool.name);
        assert!(!required(input, "scene"), "{} requires scene", tool.name);
        let output = tool.output_schema.as_ref();
        let returns = output
            .and_then(|o| o.get("properties").and_then(Value::as_object))
            .is_some_and(|p| p.contains_key("scene"));
        if !returns {
            assert!(!props.contains_key(RETURN_SCENE), "{}", tool.name);
            continue;
        }
        editing.push(tool.name.to_string());
        assert_eq!(props[RETURN_SCENE]["type"], "boolean", "{}", tool.name);
        let output = output.unwrap();
        assert!(!required(output, "scene"), "{}", tool.name);
        assert_eq!(output["properties"][SCENE_ID]["type"], "string");
        assert_eq!(output["properties"]["version"]["type"], "integer");
        let hints = tool.annotations.as_ref().unwrap();
        assert_eq!(hints.read_only_hint, Some(false), "{}", tool.name);
        assert_eq!(hints.idempotent_hint, Some(false), "{}", tool.name);
    }
    for name in [
        "compute_coverage",
        "propose_changes",
        "list_camera_models",
        "find_gaps",
    ] {
        assert!(taking.iter().any(|t| t == name), "{name} in {taking:?}");
        assert!(!editing.iter().any(|t| t == name), "{name} in {editing:?}");
    }
    for name in [
        "place_camera",
        "add_target",
        "label_area",
        "set_coverage_params",
    ] {
        assert!(editing.iter().any(|t| t == name), "{name} in {editing:?}");
    }
    // The working-scene tools take ids, not scenes.
    for name in [
        "open_scene",
        "export_scene",
        "undo",
        "diff_scenes",
        "apply_edits",
    ] {
        assert!(tools.iter().any(|t| t.name == name), "{name}");
        assert!(!taking.iter().any(|t| t == name), "{name}");
    }
}

#[tokio::test]
async fn scene_tools_edit_a_working_scene_by_id() {
    let app = http_app();
    let opened = ok(&call(&app, "open_scene", json!({})).await);
    assert_eq!(opened["scene_id"], "s1");
    assert_eq!(opened["version"], 1);
    assert!(
        opened["summary"]
            .as_str()
            .unwrap()
            .starts_with("s1 version 1: no cameras on the demo street"),
        "{opened}"
    );

    let placed = call(
        &app,
        "place_camera",
        json!({"scene_id": "s1", "model_id": model_id(), "x": 0.0, "z": 15.0, "yaw_deg": 0.0}),
    )
    .await;
    let out = ok(&placed);
    assert_eq!(
        (out["scene_id"].clone(), out["version"].clone()),
        (json!("s1"), json!(2))
    );
    assert_eq!(out["camera_id"], 1);
    assert!(out.get("scene").is_none(), "{out}");
    // The text says the same as the structured result.
    let text: Value = serde_json::from_str(placed["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, out);

    // With return_scene the whole scene comes back too; the id may stand in for the scene.
    let aimed = ok(&call(
        &app,
        "aim_camera",
        json!({"scene": "s1", "id": 1, "yaw_deg": 90.0, "return_scene": true}),
    )
    .await);
    assert_eq!(aimed["version"], 3);
    assert_eq!(aimed["scene"]["cameras"][0]["yaw_deg"], 90.0);

    // A tool that only reads takes the id and stores nothing.
    let coverage = ok(&call(&app, "compute_coverage", json!({"scene_id": "s1"})).await);
    assert_eq!(coverage["levels"].as_array().unwrap().len(), 4);
    assert!(coverage.get("version").is_none());
    // Tidying an already tidy scene adds no version.
    let checked = ok(&call(&app, "validate_scene", json!({"scene_id": "s1"})).await);
    assert_eq!(checked["version"], 3);

    // Inline scenes work as before.
    let inline = ok(&call(&app, "validate_scene", json!({"scene": empty()})).await);
    assert!(inline["scene"].is_object() && inline.get("scene_id").is_none());

    let exported = ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await);
    assert_eq!(exported["version"], 3);
    assert_eq!(exported["scene"]["cameras"].as_array().unwrap().len(), 1);
    let first = ok(&call(&app, "export_scene", json!({"scene_id": "s1@1"})).await);
    assert_eq!(first["scene"]["cameras"].as_array().unwrap().len(), 0);

    // The last edit, then everything since version 1.
    let last = ok(&call(&app, "diff_scenes", json!({"a": "s1"})).await);
    assert_eq!(last["compared"], "s1 version 2 to s1 version 3");
    assert_eq!(last["total"], 1);
    assert!(
        last["changes"][0].as_str().unwrap().contains("aim at 90°"),
        "{last}"
    );
    let all = ok(&call(&app, "diff_scenes", json!({"a": "s1@1", "b": "s1"})).await);
    assert_eq!(all["total"], 1);
    assert!(
        all["changes"][0].as_str().unwrap().starts_with("Add"),
        "{all}"
    );
    let inline = ok(&call(&app, "diff_scenes", json!({"a": empty(), "b": "s1@1"})).await);
    assert_eq!(
        inline["summary"],
        "the scene given as a to s1 version 1: no changes"
    );

    let undone = ok(&call(&app, "undo", json!({"scene_id": "s1"})).await);
    assert_eq!(undone["version"], 2);
    assert!(
        undone["undone"][0].as_str().unwrap().contains("aim at 90°"),
        "{undone}"
    );
    let listed = ok(&call(&app, "list_scenes", json!({})).await);
    assert_eq!(listed["scenes"][0]["scene_id"], "s1");
    assert_eq!(listed["scenes"][0]["version"], 2);
    assert_eq!(listed["scenes"][0]["undo_steps"], 1);

    // Mistakes get a message that says what to do.
    let e = failed(&call(&app, "compute_coverage", json!({"scene_id": "s9"})).await);
    assert!(
        e.contains("no scene s9") && e.contains("Open scenes: s1"),
        "{e}"
    );
    let e = failed(
        &call(
            &app,
            "compute_coverage",
            json!({"scene_id": "s1", "scene": empty()}),
        )
        .await,
    );
    assert!(e.contains("not both"), "{e}");
    let e = failed(&call(&app, "remove_camera", json!({"scene_id": "s1", "id": 7})).await);
    assert!(e.contains("no camera with id 7"), "{e}");
    assert_eq!(
        ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await)["version"],
        2
    );

    let closed = ok(&call(&app, "close_scene", json!({"scene_id": "s1"})).await);
    assert_eq!(closed, json!({"closed": "s1", "open": []}));
    let e = failed(&call(&app, "export_scene", json!({"scene_id": "s1"})).await);
    assert!(e.contains("s1 was closed"), "{e}");
}

#[tokio::test]
async fn open_scene_takes_a_scene_file_and_refuses_a_bad_one() {
    let app = http_app();
    let mut scene = empty();
    scene["params"]["lighting"] = json!("Night");
    let opened = ok(&call(&app, "open_scene", json!({"json": scene})).await);
    assert!(
        opened["summary"].as_str().unwrap().ends_with("at night"),
        "{opened}"
    );
    // As a string holding the JSON, too.
    let text = ok(&call(
        &app,
        "open_scene",
        json!({"from": "json", "json": empty().to_string()}),
    )
    .await);
    assert_eq!(text["scene_id"], "s2");
    let e = failed(&call(&app, "open_scene", json!({"json": {"format": "nope"}})).await);
    assert!(!e.is_empty());
    let e = failed(&call(&app, "open_scene", json!({"from": "json"})).await);
    assert!(e.contains("needs the scene"), "{e}");
    let e = failed(&call(&app, "open_scene", json!({"from": "app"})).await);
    assert!(e.contains("connect_app"), "{e}");
}

#[tokio::test]
async fn apply_edits_is_all_or_nothing() {
    let app = http_app();
    ok(&call(&app, "open_scene", json!({})).await);
    let applied = ok(&call(
        &app,
        "apply_edits",
        json!({"scene_id": "s1", "edits": [
            {"tool": "place_camera", "args": {"model_id": model_id(), "x": 0.0, "z": 15.0}},
            {"tool": "aim_camera", "args": {"id": 1, "yaw_deg": 180.0, "scene_id": "s5"}},
            {"tool": "add_example_cameras", "args": {"count": 2}},
            {"tool": "set_coverage_params", "args": {"lighting": "Night"}},
        ]}),
    )
    .await);
    assert_eq!(applied["version"], 2, "{applied}");
    let steps = applied["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 4);
    assert_eq!(steps[0]["result"]["camera_id"], 1);
    assert_eq!(
        steps[2]["result"]["camera_ids"].as_array().unwrap().len(),
        2
    );
    assert!(steps.iter().all(|s| s["result"].get("scene").is_none()));
    assert!(
        applied["summary"].as_str().unwrap().contains("3 cameras"),
        "{applied}"
    );
    let scene = ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await)["scene"].clone();
    assert_eq!(scene["cameras"][0]["yaw_deg"], 180.0);

    // A failing edit keeps none of them.
    let e = failed(
        &call(
            &app,
            "apply_edits",
            json!({"scene_id": "s1", "edits": [
                {"tool": "remove_camera", "args": {"id": 1}},
                {"tool": "aim_camera", "args": {"id": 99, "yaw_deg": 0.0}},
            ]}),
        )
        .await,
    );
    assert!(
        e.starts_with("edit 2 (aim_camera) failed, so no edit was kept"),
        "{e}"
    );
    assert!(e.contains("no camera with id 99"), "{e}");
    let now = ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await);
    assert_eq!(now["version"], 2);
    assert_eq!(now["scene"]["cameras"].as_array().unwrap().len(), 3);

    // Only tools that edit a scene can be run.
    for tool in ["compute_coverage", "apply_edits", "nope"] {
        let e = failed(
            &call(
                &app,
                "apply_edits",
                json!({"scene_id": "s1", "edits": [{"tool": tool}]}),
            )
            .await,
        );
        assert!(
            e.contains("apply_edits runs these: add_example_cameras"),
            "{e}"
        );
        assert!(e.contains("place_camera"), "{e}");
    }
    let e = failed(&call(&app, "apply_edits", json!({"scene_id": "s1", "edits": []})).await);
    assert!(e.contains("at least one"), "{e}");
    // One undo step takes back the whole batch.
    let undone = ok(&call(&app, "undo", json!({"scene_id": "s1"})).await);
    assert_eq!(undone["version"], 1);
    assert_eq!(undone["undone"].as_array().unwrap().len(), 4, "{undone}");
}

/// An edit can use what an earlier edit in the batch returned, such as the
/// id of the camera it placed.
#[tokio::test]
async fn apply_edits_refers_to_earlier_edits() {
    let app = http_app();
    ok(&call(&app, "open_scene", json!({})).await);
    ok(&call(
        &app,
        "add_example_cameras",
        json!({"scene_id": "s1", "count": 1}),
    )
    .await);
    let applied = ok(&call(
        &app,
        "apply_edits",
        json!({"scene_id": "s1", "edits": [
            {"tool": "place_camera", "args": {"model_id": model_id(), "x": 0.0, "z": 15.0}},
            {"tool": "aim_camera", "args": {"id": "$1.camera_id", "yaw_deg": 90.0}},
            {"tool": "add_example_cameras", "args": {"count": 2}},
            {"tool": "update_camera", "args": {"id": "$3.camera_ids.1", "label": "Side"}},
        ]}),
    )
    .await);
    let steps = applied["steps"].as_array().unwrap();
    let placed = steps[0]["result"]["camera_id"].as_u64().unwrap();
    assert_eq!(placed, 2, "{applied}");
    assert_eq!(steps[1]["result"]["camera_id"], placed);
    let second = steps[2]["result"]["camera_ids"][1].clone();
    assert_eq!(steps[3]["result"]["camera_id"], second);
    let scene = ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await)["scene"].clone();
    let cams = scene["cameras"].as_array().unwrap();
    let aimed = cams.iter().find(|c| c["id"] == placed).unwrap();
    assert_eq!(aimed["yaw_deg"], 90.0);
    assert!(
        cams.iter()
            .any(|c| c["id"] == second && c["label"] == "Side")
    );

    // A reference to this edit or a later one, or to a field the edit did not
    // return, fails the batch and says what a reference is.
    for (args, why) in [
        (
            json!({"id": "$2.camera_id", "yaw_deg": 0}),
            "only edits 1 to 1 come before it",
        ),
        (
            json!({"id": "$1.model_id", "yaw_deg": 0}),
            "names nothing edit 1 (place_camera) returned",
        ),
    ] {
        let e = failed(
            &call(
                &app,
                "apply_edits",
                json!({"scene_id": "s1", "edits": [
                    {"tool": "place_camera", "args": {"x": 0.0, "z": 15.0}},
                    {"tool": "aim_camera", "args": args},
                ]}),
            )
            .await,
        );
        assert!(e.contains(why) && e.contains("\"$1.camera_id\""), "{e}");
    }
    let now = ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await);
    assert_eq!(now["version"], 3, "the failed batches kept nothing");

    // Only a whole string of that form is a reference: a label with a
    // dollar amount, or "$1" alone, is a label.
    let labelled = ok(&call(
        &app,
        "apply_edits",
        json!({"scene_id": "s1", "edits": [
            {"tool": "place_camera", "args": {"x": 0.0, "z": 15.0, "label": "$1.50 doorbell"}},
            {"tool": "update_camera", "args": {"id": "$1.camera_id", "label": "$1"}},
            {"tool": "place_camera", "args": {"x": 2.0, "z": 15.0, "label": "$1.camera_id!"}},
        ]}),
    )
    .await);
    let steps = labelled["steps"].as_array().unwrap();
    let scene = ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await)["scene"].clone();
    let label_of = |id: &Value| {
        scene["cameras"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == *id)
            .map(|c| c["label"].clone())
    };
    assert_eq!(
        label_of(&steps[0]["result"]["camera_id"]),
        Some(json!("$1"))
    );
    assert_eq!(
        label_of(&steps[2]["result"]["camera_id"]),
        Some(json!("$1.camera_id!"))
    );
}

#[test]
fn only_a_whole_reference_is_a_reference() {
    for (text, form) in [
        ("$1.camera_id", Some(("1", vec!["camera_id"]))),
        ("$12.camera_ids.0", Some(("12", vec!["camera_ids", "0"]))),
        ("$1.50", Some(("1", vec!["50"]))),
        ("$1.50 doorbell", None),
        ("$1", None),
        ("$1.", None),
        ("$x.camera_id", None),
        (" $1.camera_id", None),
        ("$1.2nd", None),
        ("costs $1.camera_id", None),
    ] {
        assert_eq!(super::reference_form(text), form, "{text}");
    }
}

#[test]
fn a_whole_tool_result_reads_as_its_scene() {
    let result = json!({"summary": "created", "scene_id": "s1", "scene": empty()});
    assert_eq!(edit::unwrap_result(&result), Some(empty()));
    assert!(edit::parse(&result).is_ok());
    assert!(edit::parse(&Value::String(result.to_string())).is_ok());
    // A scene file is never unwrapped, nor a result without a scene.
    assert_eq!(edit::unwrap_result(&empty()), None);
    assert_eq!(edit::unwrap_result(&json!({"summary": "x"})), None);
    assert!(edit::parse(&json!({"summary": "x"})).is_err());
}

#[tokio::test]
async fn create_scene_keeps_the_scene_and_a_whole_result_can_be_passed_on() {
    let app = http_app();
    let created = ok(&call(&app, "create_scene", json!({})).await);
    assert_eq!(created["scene_id"], "s1");
    assert_eq!(created["version"], 1);
    assert!(created["scene"].is_object());

    // The whole result where its `scene` field belongs: it names a working
    // scene, so the edit goes to that scene.
    let placed = ok(&call(
        &app,
        "place_camera",
        json!({"scene": created, "model_id": model_id(), "x": 0.0, "z": 15.0}),
    )
    .await);
    assert_eq!(placed["camera_id"], 1);
    assert_eq!(
        (placed["scene_id"].clone(), placed["version"].clone()),
        (json!("s1"), json!(2))
    );
    assert!(placed.get("scene").is_none(), "{placed}");
    // A result of an edit by id, with no scene in it at all, as text too.
    let moved = ok(&call(
        &app,
        "place_camera",
        json!({"scene": placed.to_string(), "model_id": model_id(), "x": 5.0, "z": 15.0}),
    )
    .await);
    assert_eq!(moved["version"], 3);
    let coverage = ok(&call(&app, "compute_coverage", json!({"scene": moved})).await);
    assert_eq!(coverage["cameras_counted"], json!([1, 2]));

    // A result without an id stands for the scene inside it.
    let checked = ok(&call(&app, "validate_scene", json!({"scene": empty()})).await);
    let inline = ok(&call(
        &app,
        "place_camera",
        json!({"scene": checked, "model_id": model_id(), "x": 0.0, "z": 15.0}),
    )
    .await);
    assert!(inline["scene"].is_object() && inline.get("scene_id").is_none());
    assert_eq!(
        ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await)["version"],
        3
    );
}

#[tokio::test]
async fn opening_a_scene_says_which_it_closed_to_make_room() {
    let app = http_app();
    for _ in 0..MAX_SCENES {
        let created = ok(&call(&app, "create_scene", json!({})).await);
        assert!(!created["summary"].as_str().unwrap().contains("closed"));
    }
    let created = ok(&call(&app, "create_scene", json!({})).await);
    assert!(
        created["summary"]
            .as_str()
            .unwrap()
            .ends_with("as s33; closed s1 to make room"),
        "{created}"
    );
    let opened = ok(&call(&app, "open_scene", json!({})).await);
    assert!(
        opened["summary"]
            .as_str()
            .unwrap()
            .ends_with("; closed s2 to make room"),
        "{opened}"
    );
}

/// Small models write "night" and "identify"; the schema's capitals are not
/// required.
#[tokio::test]
async fn analysis_settings_read_in_any_case() {
    let app = http_app();
    ok(&call(&app, "open_scene", json!({})).await);
    let set = ok(&call(
        &app,
        "set_coverage_params",
        json!({"scene_id": "s1", "lighting": "night", "dori": "IDENTIFY"}),
    )
    .await);
    assert_eq!(set["version"], 2);
    let scene = ok(&call(&app, "export_scene", json!({"scene_id": "s1"})).await)["scene"].clone();
    assert_eq!(scene["params"]["lighting"], "Night");
    assert_eq!(scene["params"]["dori"], "Identify");
    let us = ok(&call(
        &app,
        "set_coverage_params",
        json!({"scene_id": "s1", "dori": "recognize"}),
    )
    .await);
    assert_eq!(us["version"], 3);
    let e = failed(
        &call(
            &app,
            "set_coverage_params",
            json!({"scene_id": "s1", "lighting": "dusk"}),
        )
        .await,
    );
    assert!(
        e.contains("unknown variant `dusk`, expected `Day` or `Night`"),
        "{e}"
    );
    let kind: edit::Kind = serde_json::from_value(json!(" doorbell ")).unwrap();
    assert_eq!(kind, edit::Kind::Doorbell);
}

/// A writer that keeps what the log wrote, for the test below.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_tool_call_is_logged_without_its_values() {
    let args = json!({"scene_id": "s1", "label": "Mrs Smith's gate", "x": 4.5});
    let (keys, bytes) = argument_shape(args.as_object());
    assert_eq!(keys, "label,scene_id,x");
    assert_eq!(bytes, args.to_string().len());
    assert_eq!(argument_shape(None), (String::new(), 0));

    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let result: CallToolResponse =
        CallToolResult::structured(json!({"summary": "placed camera 1 at 12 Elm Road"})).into();
    tracing::subscriber::with_default(subscriber, || {
        log_call(
            "place_camera",
            &keys,
            bytes,
            std::time::Duration::from_millis(7),
            &Ok(result),
        );
    });
    let line = String::from_utf8(lock(&captured.0).clone()).unwrap();
    for part in [
        "tool call",
        "tool=\"place_camera\"",
        "arguments=\"label,scene_id,x\"",
        "ms=7",
        "outcome=\"ok\"",
        "result_bytes=",
    ] {
        assert!(line.contains(part), "{part} in {line}");
    }
    for value in ["Smith", "Elm Road", "s1", "4.5"] {
        assert!(!line.contains(value), "{value} logged in {line}");
    }
}

#[test]
fn logged_sizes_are_counted_without_a_buffer() {
    let args = json!({"scene_id": "s1", "label": "Porch ✓", "x": 1.5, "edits": [1, 2]});
    let compact = serde_json::to_vec(&args).unwrap().len();
    assert_eq!(super::json_bytes(&args), compact);
    let (keys, bytes) = super::argument_shape(args.as_object());
    assert_eq!(keys, "edits,label,scene_id,x");
    assert_eq!(bytes, compact);
}
