//! Tool groups, profiles and trimmed schemas.

use super::*;
use crate::store::tests::{call, failed, ok};
use serde_json::json;
use std::collections::HashMap;

fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |k| vars.get(k).cloned()
}

#[test]
fn the_selection_reads_the_flags_before_the_environment() {
    let none = ToolSelection::from_lookup(None, None, lookup(&[])).unwrap();
    assert_eq!(none, ToolSelection::default());
    let env = lookup(&[
        ("OBS_MCP_PROFILE", "Small_App"),
        ("OBS_MCP_TOOLSETS", "scenes, cameras,scenes"),
    ]);
    let s = ToolSelection::from_lookup(None, None, &env).unwrap();
    assert_eq!(s.profile, Profile::SmallApp);
    assert_eq!(s.groups, Some(vec![Group::Scenes, Group::Cameras]));
    let flags = ToolSelection::from_lookup(Some("full".into()), Some("all".into()), &env).unwrap();
    assert_eq!(flags.profile, Profile::Full);
    assert_eq!(flags.groups.as_deref(), Some(&Group::ALL[..]));

    assert_eq!(
        ToolSelection::from_lookup(Some("tiny".into()), None, lookup(&[])),
        Err(SelectionError::Profile("tiny".into()))
    );
    assert_eq!(
        ToolSelection::from_lookup(None, Some("scenes,maps".into()), lookup(&[])),
        Err(SelectionError::Group("maps".into()))
    );
    assert_eq!(
        ToolSelection::from_lookup(None, Some(" , ".into()), lookup(&[])),
        Err(SelectionError::NoGroups)
    );
}

#[test]
fn every_tool_has_a_group_and_every_listed_name_a_tool() {
    let all = ObservationMcp::all_tools().list_all();
    for t in &all {
        assert!(ObservationMcp::tool_group(&t.name).is_some(), "{}", t.name);
    }
    // The view tools come with their own router; until it is there they are
    // simply not offered.
    let view = [
        "focus_view",
        "select_cameras",
        "highlight",
        "set_view_options",
        "request_load_area",
        "search_places",
    ];
    for name in SMALL_TOOLS.iter().chain(APP_TOOLS) {
        assert!(
            all.iter().any(|t| t.name == *name) || view.contains(name),
            "{name} is in a profile but is no tool"
        );
    }
    assert_eq!(
        ObservationMcp::tool_group("check_point"),
        Some(Group::Analysis)
    );
    assert_eq!(
        ObservationMcp::tool_group("get_started"),
        Some(Group::Guide)
    );
}

/// The serialised tools/list of a selection, as a client receives it.
fn listed(selection: ToolSelection) -> (Vec<Tool>, String) {
    let toolbox = Toolbox::new(selection);
    let tools = toolbox.tools().to_vec();
    let text = serde_json::to_string(&json!({ "tools": tools })).unwrap();
    (tools, text)
}

/// Tokens by the bytes / 4 rule, which overestimates Qwen's tokenizer on
/// this JSON slightly.
fn tokens(text: &str) -> usize {
    text.len() / 4
}

/// A profile's tool list must stay within its budget: a small model's
/// context holds it with every turn.
#[test]
fn each_profile_stays_within_its_token_budget() {
    for (profile, budget) in [
        (Profile::Small, 4_000),
        (Profile::SmallApp, 5_500),
        (Profile::Full, 38_000),
    ] {
        let (tools, text) = listed(ToolSelection::profile(profile));
        let n = tokens(&text);
        println!(
            "{}: {} tools, {} bytes, about {n} tokens",
            profile.name(),
            tools.len(),
            text.len()
        );
        assert!(
            n <= budget,
            "the {} profile lists {} tools in about {n} tokens, over its budget of {budget}",
            profile.name(),
            tools.len()
        );
    }
}

fn walk<'a>(v: &'a Value, path: &str, found: &mut Vec<(String, &'a Value)>) {
    match v {
        Value::Object(m) => {
            for (k, sub) in m {
                let here = format!("{path}/{k}");
                found.push((here.clone(), sub));
                walk(sub, &here, found);
            }
        }
        Value::Array(items) => {
            for (i, sub) in items.iter().enumerate() {
                let here = format!("{path}/{i}");
                found.push((here.clone(), sub));
                walk(sub, &here, found);
            }
        }
        _ => {}
    }
}

#[test]
fn small_profiles_list_plain_schemas() {
    let (tools, _) = listed(ToolSelection::profile(Profile::Small));
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for name in SMALL_TOOLS {
        assert!(names.contains(name), "{name} in {names:?}");
    }
    assert!(!names.contains(&"address_at_point"));
    for t in &tools {
        assert!(t.output_schema.is_none(), "{}", t.name);
        let input = Value::Object((*t.input_schema).clone());
        let mut found = Vec::new();
        walk(&input, &t.name, &mut found);
        for (path, v) in found {
            let key = path.rsplit('/').next().unwrap_or_default();
            if path.ends_with(&format!("/properties/{key}")) {
                continue;
            }
            assert!(
                !matches!(key, "$schema" | "format" | "$ref" | "$defs" | "default"),
                "{path}"
            );
            assert!(*v != json!("null"), "{path} allows null");
            if key == "description" {
                assert!(!v.as_str().unwrap().contains('\n'), "{path}");
            }
        }
        let props = input["properties"].as_object().unwrap();
        assert!(!props.contains_key("scene"), "{}", t.name);
        assert!(!props.contains_key("return_scene"), "{}", t.name);
        if props.contains_key("scene_id") {
            let required = input["required"].as_array().unwrap();
            assert!(required.contains(&json!("scene_id")), "{}", t.name);
        }
    }
    let count = &tools
        .iter()
        .find(|t| t.name == "add_example_cameras")
        .unwrap()
        .input_schema["properties"]["count"];
    // The description gives 1 to 20, so the bounds are left to it.
    assert_eq!(count["type"], "integer");
    assert!(count.get("minimum").is_none(), "{count}");
}

#[test]
fn the_full_profile_keeps_whole_schemas_without_their_dialect() {
    let (tools, _) = listed(ToolSelection::default());
    assert_eq!(tools.len(), ObservationMcp::all_tools().list_all().len());
    for t in &tools {
        assert!(!t.input_schema.contains_key("$schema"), "{}", t.name);
        if let Some(out) = &t.output_schema {
            assert!(!out.contains_key("$schema"), "{}", t.name);
        }
    }
    let place = tools.iter().find(|t| t.name == "place_camera").unwrap();
    let props = place.input_schema["properties"].as_object().unwrap();
    assert!(props.contains_key("scene") && props.contains_key("scene_id"));
    let count = &tools
        .iter()
        .find(|t| t.name == "add_example_cameras")
        .unwrap()
        .input_schema["properties"]["count"];
    assert_eq!(
        (&count["minimum"], &count["maximum"]),
        (&json!(1), &json!(20))
    );
}

#[test]
fn toolsets_choose_the_groups_and_always_offer_the_guide() {
    let s = ToolSelection {
        profile: Profile::Small,
        groups: Some(vec![Group::Places]),
    };
    let (tools, _) = listed(s);
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "add_target",
            "get_started",
            "label_area",
            "limit_figures_to",
            "remove_area",
            "remove_target",
            "update_target"
        ]
    );
}

/// The MCP server over HTTP with `selection`, without authentication.
fn app_with(selection: ToolSelection) -> axum::Router {
    use crate::http::config::{Auth, HttpConfig};
    crate::http::router_for(
        &HttpConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            auth: Auth::Disabled,
            allowed_hosts: vec!["localhost".into()],
            allowed_origins: Vec::new(),
        },
        &selection,
        &tokio_util::sync::CancellationToken::new(),
    )
}

#[tokio::test]
async fn a_tool_outside_the_profile_is_refused_by_name() {
    let app = app_with(ToolSelection::profile(Profile::Small));
    ok(&call(&app, "open_scene", json!({})).await);
    let e = failed(
        &call(
            &app,
            "address_at_point",
            json!({"scene_id": "s1", "x": 0, "z": 0}),
        )
        .await,
    );
    assert!(e.contains("small profile"), "{e}");
    let e = failed(&call(&app, "who_sees_point", json!({})).await);
    assert!(e.contains("no tool who_sees_point"), "{e}");
    // A scene tool without a scene says how to get one.
    let e = failed(&call(&app, "compute_coverage", json!({})).await);
    assert!(e.contains("call open_scene first"), "{e}");
    // apply_edits is not offered here, and runs only offered tools where it is.
    let app = app_with(ToolSelection {
        profile: Profile::Full,
        groups: Some(vec![Group::Scenes, Group::Places]),
    });
    ok(&call(&app, "open_scene", json!({})).await);
    let edits =
        json!({"scene_id": "s1", "edits": [{"tool": "add_example_cameras", "args": {"count": 1}}]});
    let e = failed(&call(&app, "apply_edits", edits).await);
    assert!(e.contains("edit 1") && e.contains("not offered"), "{e}");
}
