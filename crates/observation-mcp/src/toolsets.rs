//! Which tools a client is offered, and how: tool groups, profiles, and the
//! trimmed schemas small models read.
//!
//! - **Groups** follow the tool routers ([`ObservationMcp::all_tools`]
//!   names each router's group): a tool added to a router joins its group
//!   without a change here. `OBS_MCP_TOOLSETS` (or `--toolsets`) offers only
//!   the groups it names; `get_started` (group `guide`) is always offered.
//! - **Profiles** (`OBS_MCP_PROFILE` or `--profile`): `full` (the default)
//!   offers every tool with its whole schema and full results; `small` offers
//!   the few tools a small model needs offline, and `small-app` adds those
//!   that drive the connected app. Both small profiles trim the schemas
//!   ([`trim`]) and default the analysis tools to brief results. With
//!   toolsets as well, the groups choose the tools and the profile the
//!   schemas and the results.
//! - **Calls** to a tool the selection does not offer are refused with an
//!   error that names the profile, and `apply_edits` runs only offered tools.
//!
//! The server reads input with the tools' own types, whatever the listed
//! schema says, so a trimmed schema accepts nothing it did not before.

use crate::ObservationMcp;
use rmcp::model::Tool;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::Arc;
use thiserror::Error;

/// A group of tools, as `OBS_MCP_TOOLSETS` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// get_started.
    Guide,
    /// Working scenes (open_scene, export_scene, undo, apply_edits, ...),
    /// create_scene, validate_scene and load_demo_area.
    Scenes,
    /// Placing, moving, aiming and removing cameras.
    Cameras,
    /// The camera catalogue and custom models.
    Models,
    /// Labelled areas and targets.
    Places,
    /// Analysis settings, coverage, verdicts, splits, gaps and addresses.
    Analysis,
    /// The scene in words: surroundings, buildings, streets, mount points.
    Describe,
    /// render_map.
    Map,
    /// suggest_placement, optimise_aim and cover_target.
    Goals,
    /// The connection to the user's open app.
    App,
    /// Proposals to the app's scene and the user's decisions.
    Proposals,
    /// What the app shows: focus_view, select_cameras, highlight,
    /// set_view_options, request_load_area and search_places.
    View,
}

impl Group {
    pub const ALL: [Self; 12] = [
        Self::Guide,
        Self::Scenes,
        Self::Cameras,
        Self::Models,
        Self::Places,
        Self::Analysis,
        Self::Describe,
        Self::Map,
        Self::Goals,
        Self::App,
        Self::Proposals,
        Self::View,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Guide => "guide",
            Self::Scenes => "scenes",
            Self::Cameras => "cameras",
            Self::Models => "models",
            Self::Places => "places",
            Self::Analysis => "analysis",
            Self::Describe => "describe",
            Self::Map => "map",
            Self::Goals => "goals",
            Self::App => "app",
            Self::Proposals => "proposals",
            Self::View => "view",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|g| g.name() == t)
    }
}

/// The tools of the `small` profile: enough for a small model to open a
/// scene, look at it, place cameras where the server suggests, check them
/// and export the result.
pub const SMALL_TOOLS: &[&str] = &[
    "get_started",
    "open_scene",
    "export_scene",
    "scene_summary",
    "describe_surroundings",
    "render_map",
    "suggest_mount_points",
    "suggest_placement",
    "place_camera",
    "point_camera_at",
    "remove_camera",
    "add_example_cameras",
    "set_coverage_params",
    "compute_coverage",
    "find_gaps",
    "check_point",
];

/// What `small-app` adds to `small`: connecting to the app, reading it,
/// proposing a change and waiting for the user, and showing the result.
pub const APP_TOOLS: &[&str] = &[
    "connect_app",
    "get_app_state",
    "propose_changes",
    "wait_for_decision",
    "focus_view",
    "highlight",
    "select_cameras",
];

/// A tool profile, as `OBS_MCP_PROFILE` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// Every tool, whole schemas, full results.
    #[default]
    Full,
    /// [`SMALL_TOOLS`], trimmed schemas, brief results.
    Small,
    /// [`SMALL_TOOLS`] and [`APP_TOOLS`], trimmed schemas, brief results.
    SmallApp,
}

impl Profile {
    pub const ALL: [Self; 3] = [Self::Full, Self::Small, Self::SmallApp];

    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Small => "small",
            Self::SmallApp => "small-app",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().to_ascii_lowercase().replace('_', "-");
        Self::ALL.into_iter().find(|p| p.name() == t)
    }

    /// Whether schemas are trimmed and results brief by default.
    pub fn compact(self) -> bool {
        self != Self::Full
    }

    /// Whether the profile offers `tool` (without toolsets).
    pub fn offers(self, tool: &str) -> bool {
        match self {
            Self::Full => true,
            Self::Small => SMALL_TOOLS.contains(&tool),
            Self::SmallApp => SMALL_TOOLS.contains(&tool) || APP_TOOLS.contains(&tool),
        }
    }
}

/// The settings: a profile and, optionally, the groups to offer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolSelection {
    pub profile: Profile,
    /// The groups `OBS_MCP_TOOLSETS` named; `None` for the profile's tools.
    pub groups: Option<Vec<Group>>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SelectionError {
    #[error("OBS_MCP_PROFILE (or --profile) must be full, small or small-app, not `{0}`")]
    Profile(String),
    #[error(
        "OBS_MCP_TOOLSETS (or --toolsets) names no group `{0}`; the groups are all, guide, \
         scenes, cameras, models, places, analysis, describe, map, goals, app, proposals and view"
    )]
    Group(String),
    #[error("OBS_MCP_TOOLSETS (or --toolsets) names no groups")]
    NoGroups,
}

impl ToolSelection {
    /// A profile with its own tools.
    pub fn profile(profile: Profile) -> Self {
        Self {
            profile,
            groups: None,
        }
    }

    /// Reads the selection from the process environment; the flags
    /// (`--profile`, `--toolsets`) take precedence over the variables.
    pub fn from_env(
        profile: Option<String>,
        toolsets: Option<String>,
    ) -> Result<Self, SelectionError> {
        Self::from_lookup(profile, toolsets, |k| {
            std::env::var(k).ok().filter(|v| !v.trim().is_empty())
        })
    }

    /// As [`from_env`](Self::from_env), with the environment supplied by
    /// the caller.
    pub fn from_lookup(
        profile: Option<String>,
        toolsets: Option<String>,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, SelectionError> {
        let profile = match profile.or_else(|| get("OBS_MCP_PROFILE")) {
            None => Profile::Full,
            Some(text) => Profile::parse(&text).ok_or(SelectionError::Profile(text))?,
        };
        let groups = match toolsets.or_else(|| get("OBS_MCP_TOOLSETS")) {
            None => None,
            Some(list) => Some(parse_groups(&list)?),
        };
        Ok(Self { profile, groups })
    }

    /// Whether the selection offers `tool`, which is in `group`.
    pub fn offers(&self, tool: &str, group: Option<Group>) -> bool {
        if group == Some(Group::Guide) {
            return true;
        }
        match &self.groups {
            Some(groups) => group.is_some_and(|g| groups.contains(&g)),
            None => self.profile.offers(tool),
        }
    }

    /// "the small profile", or "the toolsets scenes, cameras (small profile)".
    pub fn describe(&self) -> String {
        match &self.groups {
            None => format!("the {} profile", self.profile.name()),
            Some(groups) => {
                let names: Vec<&str> = groups.iter().map(|g| g.name()).collect();
                format!(
                    "the toolsets {} ({} profile)",
                    names.join(", "),
                    self.profile.name()
                )
            }
        }
    }
}

fn parse_groups(list: &str) -> Result<Vec<Group>, SelectionError> {
    let mut groups = Vec::new();
    for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        if entry.eq_ignore_ascii_case("all") {
            groups.extend(Group::ALL);
            continue;
        }
        groups.push(Group::parse(entry).ok_or_else(|| SelectionError::Group(entry.to_owned()))?);
    }
    groups.sort_unstable();
    groups.dedup();
    if groups.is_empty() {
        return Err(SelectionError::NoGroups);
    }
    Ok(groups)
}

/// The tools one selection offers, with their schemas as listed.
#[derive(Debug)]
pub struct Toolbox {
    selection: ToolSelection,
    listed: Vec<Tool>,
    names: HashSet<String>,
}

impl Default for Toolbox {
    fn default() -> Self {
        Self::new(ToolSelection::default())
    }
}

impl Toolbox {
    pub fn new(selection: ToolSelection) -> Self {
        let compact = selection.profile.compact();
        let listed: Vec<Tool> = ObservationMcp::all_tools()
            .list_all()
            .into_iter()
            .filter(|t| selection.offers(&t.name, ObservationMcp::tool_group(&t.name)))
            .map(|t| {
                let required = ObservationMcp::scene_required(&t.name);
                trim(t, compact, required)
            })
            .collect();
        let names = listed.iter().map(|t| t.name.to_string()).collect();
        Self {
            selection,
            listed,
            names,
        }
    }

    pub fn selection(&self) -> &ToolSelection {
        &self.selection
    }

    pub fn profile(&self) -> Profile {
        self.selection.profile
    }

    /// The tools as tools/list gives them.
    pub fn tools(&self) -> &[Tool] {
        &self.listed
    }

    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.listed.iter().find(|t| t.name == name)
    }

    pub fn offers(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Why a call to `name` is refused: it is not offered, or there is no
    /// such tool.
    pub fn refusal(&self, name: &str) -> String {
        if ObservationMcp::all_tools().has_route(name) {
            format!(
                "{name} is not offered by {} (OBS_MCP_PROFILE, OBS_MCP_TOOLSETS); use the tools \
                 tools/list gives, or call get_started",
                self.selection.describe()
            )
        } else {
            format!("there is no tool {name}; call get_started for the workflow")
        }
    }
}

/// `tool` as a selection lists it. Every profile drops `$schema` and joins
/// multi-line descriptions into one line. A compact one (the small
/// profiles) also drops the output schema, `format`, null alternatives of
/// optional fields, bounds the type or description already gives, and
/// defaults; inlines `$ref`s; turns a choice of strings into a plain
/// `enum`; and hides `scene` and `return_scene`, so a scene is passed only
/// by `scene_id`, which is then required wherever a scene is.
pub fn trim(mut tool: Tool, compact: bool, scene_required: bool) -> Tool {
    let mut input = (*tool.input_schema).clone();
    input.remove("$schema");
    if compact {
        let defs = input.remove("$defs");
        let mut value = Value::Object(input);
        compact_schema(&mut value, defs.as_ref(), 0);
        input = match value {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        hide_scene(&mut input, scene_required);
        tool.output_schema = None;
    } else if let Some(output) = tool.output_schema.as_mut() {
        Arc::make_mut(output).remove("$schema");
    }
    one_line_descriptions(&mut input);
    tool.input_schema = Arc::new(input);
    if let Some(output) = tool.output_schema.as_mut() {
        one_line_descriptions(Arc::make_mut(output));
    }
    tool
}

fn one_line_descriptions(schema: &mut Map<String, Value>) {
    for (key, value) in schema.iter_mut() {
        match value {
            Value::String(text) if key == "description" && text.contains('\n') => {
                *text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            }
            Value::Object(m) => one_line_descriptions(m),
            Value::Array(items) => {
                for item in items {
                    if let Value::Object(m) = item {
                        one_line_descriptions(m);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Arguments the small profiles leave out of the schemas, for settings a
/// small model has no need of; the tools still accept them.
pub const HIDDEN_IN_COMPACT: [&str; 2] = ["roll_deg", "custom_ppm"];

/// Hides `scene`, `return_scene` and [`HIDDEN_IN_COMPACT`], and requires
/// `scene_id` where a scene was required.
fn hide_scene(input: &mut Map<String, Value>, scene_required: bool) {
    let Some(Value::Object(props)) = input.get_mut("properties") else {
        return;
    };
    for name in HIDDEN_IN_COMPACT {
        props.remove(name);
    }
    if props.remove("scene").is_none() {
        return;
    }
    props.remove("return_scene");
    let has_id = props.contains_key("scene_id");
    if scene_required && has_id {
        let required = input
            .entry("required")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(list) = required
            && !list.iter().any(|r| r == "scene_id")
        {
            list.insert(0, Value::from("scene_id"));
        }
    }
}

/// How deep `$ref`s are followed when inlined (the tools' types nest a few
/// levels; this only stops a cycle).
const MAX_INLINE_DEPTH: usize = 8;

fn compact_schema(value: &mut Value, defs: Option<&Value>, depth: usize) {
    let Value::Object(map) = value else {
        return;
    };
    // Inline a reference, keeping this schema's own description.
    if let Some(Value::String(r)) = map.get("$ref").cloned()
        && depth < MAX_INLINE_DEPTH
        && let Some(Value::Object(def)) = r
            .strip_prefix("#/$defs/")
            .and_then(|name| defs.and_then(|d| d.get(name)))
    {
        map.remove("$ref");
        for (k, v) in def {
            if k == "description" && map.contains_key("description") {
                continue;
            }
            map.entry(k.clone()).or_insert_with(|| v.clone());
        }
        compact_schema(value, defs, depth + 1);
        return;
    }
    // `["number", "null"]` → `"number"`.
    if let Some(Value::Array(types)) = map.get("type") {
        let kept: Vec<Value> = types.iter().filter(|t| *t != "null").cloned().collect();
        let replacement = match kept.len() {
            1 => kept.into_iter().next(),
            _ => Some(Value::Array(kept)),
        };
        if let Some(t) = replacement {
            map.insert("type".to_owned(), t);
        }
    }
    // `anyOf: [X, {"type": "null"}]` → X.
    for key in ["anyOf", "oneOf"] {
        let Some(Value::Array(options)) = map.get(key) else {
            continue;
        };
        let null = serde_json::json!({"type": "null"});
        let kept: Vec<Value> = options.iter().filter(|o| **o != null).cloned().collect();
        if kept.len() == options.len() {
            continue;
        }
        if let [only] = kept.as_slice() {
            map.remove(key);
            if let Value::Object(inner) = only {
                for (k, v) in inner {
                    if k == "description" && map.contains_key("description") {
                        continue;
                    }
                    map.insert(k.clone(), v.clone());
                }
            }
        } else {
            map.insert(key.to_owned(), Value::Array(kept));
        }
    }
    if map.contains_key("$ref") {
        compact_schema(value, defs, depth + 1);
        return;
    }
    let Value::Object(map) = value else {
        return;
    };
    // A choice of strings → a plain `enum`.
    if let Some(Value::Array(options)) = map.get("oneOf") {
        let names: Option<Vec<Vec<Value>>> = options
            .iter()
            .map(|o| match (o.get("const"), o.get("enum")) {
                (Some(c @ Value::String(_)), _) => Some(vec![c.clone()]),
                (None, Some(Value::Array(e))) if e.iter().all(Value::is_string) => Some(e.clone()),
                _ => None,
            })
            .collect();
        if let Some(names) = names {
            map.remove("oneOf");
            map.insert("type".to_owned(), Value::from("string"));
            map.insert("enum".to_owned(), Value::Array(names.concat()));
        }
    }
    map.remove("format");
    map.remove("default");
    let integer = map.get("type").is_some_and(|t| t == "integer");
    let words = map
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    for bound in ["minimum", "maximum"] {
        let implied = match map.get(bound) {
            None => continue,
            // An unsigned type's 0, which says nothing a model needs.
            Some(v) if integer && bound == "minimum" && v.as_f64() == Some(0.0) => true,
            Some(v) => mentions(&words, v),
        };
        if implied {
            map.remove(bound);
        }
    }
    for (key, sub) in map.iter_mut() {
        match key.as_str() {
            "properties" | "patternProperties" => {
                if let Value::Object(props) = sub {
                    for p in props.values_mut() {
                        compact_schema(p, defs, depth);
                    }
                }
            }
            "items" => {
                // The list's own description says what its items are.
                if let Value::Object(item) = sub {
                    item.remove("description");
                }
                compact_schema(sub, defs, depth);
            }
            "additionalProperties" | "not" => compact_schema(sub, defs, depth),
            "anyOf" | "oneOf" | "allOf" | "prefixItems" => {
                if let Value::Array(list) = sub {
                    for s in list {
                        compact_schema(s, defs, depth);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Whether `words` gives the number `v`.
fn mentions(words: &str, v: &Value) -> bool {
    let Some(n) = v.as_f64() else {
        return false;
    };
    let text = if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    };
    words
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .any(|w| w.trim_end_matches('.') == text)
}

#[cfg(test)]
#[path = "toolsets_tests.rs"]
mod tests;
