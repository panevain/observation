//! Working scenes: scenes the server holds for the agent, addressed by a
//! short id (`s1`, `s2`, ...), so that a small model passes a few characters
//! to each scene tool instead of the whole scene file and its reply.
//!
//! - **The store** ([`SceneStore`]) keeps at most [`MAX_SCENES`] scenes, in
//!   memory only. Opening one more closes the least recently used, and an id
//!   closed that way says so when it is used again. Each scene keeps its
//!   latest version and up to [`MAX_UNDO`] earlier ones, each at most
//!   [`MAX_SCENE_BYTES`] of JSON (the agent bridge's limit for a shared
//!   state), and all versions together at most [`MAX_STORE_BYTES`]: past
//!   that the oldest earlier versions go first, then the least recently used
//!   scenes. Ids are never reused within a store, nor version numbers within
//!   a scene: undo goes back to an earlier version and drops the later ones,
//!   and the next edit gets a new number.
//! - **Scoping.** The store belongs to an [`ObservationMcp`] and its clones,
//!   as the connection to the app does: one per stdio process, and over HTTP
//!   one per bearer token ([`crate::http`]), so one client never sees
//!   another's scenes.
//! - **Any scene tool takes `scene_id`** ([`ObservationMcp::call_with_scenes`]).
//!   Before a tool with a `scene` argument runs, a `scene_id` in its
//!   arguments is replaced by the stored scene; after it, an edited `scene`
//!   in its result is stored as the next version and replaced in the result
//!   by `scene_id` and `version` (unless the call asked for `return_scene`).
//!   [`document_scene_ids`] adds the two arguments to the tool list, so a
//!   tool added later with a `scene` argument gets them without a change here.
//! - **The tools** here open, list, export, close and undo working scenes,
//!   compare two scenes or versions in plain words, and run several edits as
//!   one version, all or nothing.

use crate::ObservationMcp;
use crate::ToolError;
use crate::app::AppStateOut;
use crate::edit;
use crate::propose::model_name;
use crate::schema::JsonObject;
use crate::server::fail;
use observation_core::camera::Lighting;
use observation_core::scene::changes::ChangeSet;
use observation_core::scene::{AreaRef, SceneFile};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, tool, tool_router};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Most working scenes a store keeps; opening another closes the least
/// recently used.
pub const MAX_SCENES: usize = 32;
/// Most earlier versions kept per scene, for undo.
pub const MAX_UNDO: usize = 20;
/// Largest version kept, bytes of compact JSON: the agent bridge's limit for
/// a shared state, so any working scene can also be shared or proposed.
pub const MAX_SCENE_BYTES: usize = observation_agent_api::MAX_STATE_BYTES;
/// Most bytes one store keeps over all its versions. Past it the oldest
/// earlier versions are dropped first, then the least recently used scenes
/// are closed.
pub const MAX_STORE_BYTES: usize = 64 << 20;
/// Most edits one apply_edits call runs.
pub const MAX_EDITS: usize = 50;
/// Most change lines diff_scenes and undo return.
pub const MAX_LINES: usize = 40;
/// How many closed-to-make-room ids are remembered, to say so when one is used.
const EVICTED_REMEMBERED: usize = 256;

/// A working scene's id: `s` and a number from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SceneId(u64);

impl fmt::Display for SceneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.0)
    }
}

impl SceneId {
    /// Reads `s1` (or `S1`, or `1`), ignoring surrounding spaces.
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let digits = t
            .strip_prefix('s')
            .or_else(|| t.strip_prefix('S'))
            .unwrap_or(t);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok().filter(|&n| n > 0).map(Self)
    }
}

/// Reads a reference to a stored version: `s1` (the latest) or `s1@3`.
fn parse_ref(text: &str) -> Option<(SceneId, Option<u32>)> {
    match text.split_once('@') {
        None => Some((SceneId::parse(text)?, None)),
        Some((id, version)) => {
            let v = version.trim();
            let v = v.strip_prefix('v').unwrap_or(v);
            Some((SceneId::parse(id)?, Some(v.parse().ok()?)))
        }
    }
}

/// One version of a scene.
#[derive(Debug)]
struct Version {
    number: u32,
    text: Arc<str>,
    /// When it was stored, on the store's clock: the budget drops the
    /// oldest earlier versions first.
    made: u64,
}

/// One stored scene: its versions, oldest first.
#[derive(Debug)]
struct Working {
    versions: VecDeque<Version>,
    /// The number the next version gets; numbers are never reused.
    next_version: u32,
    /// When it was last used, on the store's clock.
    used: u64,
}

impl Working {
    fn latest(&self) -> &Version {
        self.versions.back().expect("a working scene has a version")
    }

    fn numbers(&self) -> String {
        let n: Vec<String> = self.versions.iter().map(|v| v.number.to_string()).collect();
        n.join(", ")
    }
}

#[derive(Debug, Default)]
struct Inner {
    last_id: u64,
    clock: u64,
    /// Bytes over all versions of all scenes.
    bytes: usize,
    scenes: HashMap<SceneId, Working>,
    /// Ids closed to make room, oldest eviction first.
    evicted: VecDeque<SceneId>,
}

impl Inner {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// The open ids, most recently used first.
    fn ids(&self) -> Vec<SceneId> {
        let mut ids: Vec<(u64, SceneId)> =
            self.scenes.iter().map(|(id, w)| (w.used, *id)).collect();
        ids.sort_by(|a, b| b.cmp(a));
        ids.into_iter().map(|(_, id)| id).collect()
    }

    fn open_list(&self) -> String {
        let ids = self.ids();
        if ids.is_empty() {
            "No scenes are open: open_scene makes one".to_owned()
        } else {
            let list: Vec<String> = ids.iter().map(ToString::to_string).collect();
            format!(
                "Open scenes: {} (open_scene makes another)",
                list.join(", ")
            )
        }
    }

    /// The scene `id`, marked as used, or why there is none.
    fn working(&mut self, id: SceneId) -> Result<&mut Working, ToolError> {
        if !self.scenes.contains_key(&id) {
            let why = if self.evicted.contains(&id) {
                format!(
                    "scene {id} was closed to make room: at most {MAX_SCENES} working scenes \
                     ({} MiB in all) are kept, and it was the least recently used",
                    MAX_STORE_BYTES >> 20
                )
            } else if id.0 <= self.last_id {
                format!("scene {id} was closed")
            } else {
                format!("there is no scene {id}")
            };
            return Err(ToolError::invalid(format!("{why}. {}.", self.open_list())));
        }
        let now = self.tick();
        let w = self.scenes.get_mut(&id).expect("checked above");
        w.used = now;
        Ok(w)
    }

    /// Closes `id` to make room, remembering that it was.
    fn evict(&mut self, id: SceneId) {
        if let Some(w) = self.scenes.remove(&id) {
            self.bytes -= w.versions.iter().map(|v| v.text.len()).sum::<usize>();
        }
        self.evicted.push_back(id);
        while self.evicted.len() > EVICTED_REMEMBERED {
            self.evicted.pop_front();
        }
    }

    /// Brings the store within `budget` bytes: drops the oldest earlier
    /// version of any scene first, then closes the least recently used
    /// scenes other than `keep`. Returns the scenes closed.
    fn make_room(&mut self, budget: usize, keep: SceneId) -> Vec<SceneId> {
        let mut closed = Vec::new();
        while self.bytes > budget {
            let oldest = self
                .scenes
                .iter()
                .filter(|(_, w)| w.versions.len() > 1)
                .filter_map(|(id, w)| w.versions.front().map(|v| (v.made, *id)))
                .min();
            if let Some((_, id)) = oldest {
                if let Some(v) = self
                    .scenes
                    .get_mut(&id)
                    .and_then(|w| w.versions.pop_front())
                {
                    self.bytes -= v.text.len();
                }
                continue;
            }
            let Some(&lru) = self.ids().iter().rev().find(|&&id| id != keep) else {
                break;
            };
            self.evict(lru);
            closed.push(lru);
        }
        closed
    }
}

/// The working scenes of one client (see the module documentation).
#[derive(Debug)]
pub struct SceneStore {
    inner: Mutex<Inner>,
    /// [`MAX_STORE_BYTES`], or less in tests.
    budget: usize,
}

impl Default for SceneStore {
    fn default() -> Self {
        Self::with_budget(MAX_STORE_BYTES)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `scene` as stored: compact JSON within the size limit.
fn to_text(scene: &Value) -> Result<Arc<str>, ToolError> {
    let text = scene.to_string();
    if text.len() > MAX_SCENE_BYTES {
        return Err(ToolError::invalid(format!(
            "the scene is {} KiB of JSON, but a working scene may be at most {} KiB",
            text.len().div_ceil(1024),
            MAX_SCENE_BYTES / 1024
        )));
    }
    Ok(Arc::from(text))
}

fn from_text(text: &str) -> Result<Value, ToolError> {
    serde_json::from_str(text).map_err(|e| ToolError::invalid(format!("a stored scene: {e}")))
}

/// A scene as one store entry lists it.
#[derive(Debug, Clone)]
pub struct Listed {
    pub id: SceneId,
    pub version: u32,
    /// How many earlier versions undo can go back to.
    pub undo_steps: usize,
    pub scene: Value,
}

/// A stored version: the scene and its number.
pub type Versioned = (Value, u32);

/// A scene just stored: its version, and the scenes closed to make room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub id: SceneId,
    pub version: u32,
    pub closed: Vec<SceneId>,
}

impl Stored {
    /// "; closed s2 to make room", or nothing.
    pub fn closed_note(&self) -> String {
        if self.closed.is_empty() {
            return String::new();
        }
        let ids: Vec<String> = self.closed.iter().map(ToString::to_string).collect();
        format!("; closed {} to make room", ids.join(", "))
    }
}

impl SceneStore {
    /// A store that keeps at most `budget` bytes over all its versions.
    pub fn with_budget(budget: usize) -> Self {
        Self {
            inner: Mutex::default(),
            budget,
        }
    }

    /// Stores `scene` (a checked scene file) as a new working scene, closing
    /// the least recently used ones if the store is full.
    pub fn open(&self, scene: &Value) -> Result<Stored, ToolError> {
        let text = to_text(scene)?;
        let mut inner = lock(&self.inner);
        let mut closed = Vec::new();
        while inner.scenes.len() >= MAX_SCENES {
            let Some(&oldest) = inner.ids().last() else {
                break;
            };
            inner.evict(oldest);
            closed.push(oldest);
        }
        inner.last_id += 1;
        let id = SceneId(inner.last_id);
        let now = inner.tick();
        inner.bytes += text.len();
        inner.scenes.insert(
            id,
            Working {
                versions: VecDeque::from([Version {
                    number: 1,
                    text,
                    made: now,
                }]),
                next_version: 2,
                used: now,
            },
        );
        closed.extend(inner.make_room(self.budget, id));
        Ok(Stored {
            id,
            version: 1,
            closed,
        })
    }

    /// The latest version of `id`, and its number.
    pub fn latest(&self, id: SceneId) -> Result<(Value, u32), ToolError> {
        self.get(id, None)
    }

    /// Version `version` of `id` (the latest if `None`), and its number.
    pub fn get(&self, id: SceneId, version: Option<u32>) -> Result<(Value, u32), ToolError> {
        let (number, text) = {
            let mut inner = lock(&self.inner);
            let w = inner.working(id)?;
            let found = match version {
                None => Some(w.latest()),
                Some(wanted) => w.versions.iter().find(|v| v.number == wanted),
            };
            match found {
                Some(v) => (v.number, Arc::clone(&v.text)),
                None => {
                    return Err(ToolError::invalid(format!(
                        "scene {id} has no version {}: it keeps versions {}",
                        version.unwrap_or_default(),
                        w.numbers()
                    )));
                }
            }
        };
        Ok((from_text(&text)?, number))
    }

    /// The latest version of `id` and the one before it, each with its
    /// number: what its last edit changed.
    pub fn last_edit(&self, id: SceneId) -> Result<(Versioned, Versioned), ToolError> {
        let (before, after) = {
            let mut inner = lock(&self.inner);
            let w = inner.working(id)?;
            let n = w.versions.len();
            if n < 2 {
                return Err(ToolError::invalid(format!(
                    "scene {id} keeps no version before {} to compare it with",
                    w.latest().number
                )));
            }
            let pick = |v: &Version| (v.number, Arc::clone(&v.text));
            (pick(&w.versions[n - 2]), pick(&w.versions[n - 1]))
        };
        Ok((
            (from_text(&before.1)?, before.0),
            (from_text(&after.1)?, after.0),
        ))
    }

    /// Stores `scene` as the version after `based_on`, which must still be
    /// the latest; returns the new version (an unchanged scene adds none).
    pub fn commit(&self, id: SceneId, based_on: u32, scene: &Value) -> Result<Stored, ToolError> {
        let text = to_text(scene)?;
        let mut inner = lock(&self.inner);
        let now = inner.tick();
        let w = inner.working(id)?;
        let latest = w.latest();
        if latest.number != based_on {
            return Err(ToolError::invalid(format!(
                "scene {id} changed while this call ran (it is now at version {}), so this \
                 edit was not kept: make it again",
                latest.number
            )));
        }
        if *latest.text == *text {
            return Ok(Stored {
                id,
                version: based_on,
                closed: Vec::new(),
            });
        }
        let number = w.next_version;
        w.next_version += 1;
        let mut freed = 0;
        let added = text.len();
        w.versions.push_back(Version {
            number,
            text,
            made: now,
        });
        while w.versions.len() > MAX_UNDO + 1 {
            freed += w.versions.pop_front().map_or(0, |v| v.text.len());
        }
        inner.bytes = inner.bytes + added - freed;
        let closed = inner.make_room(self.budget, id);
        Ok(Stored {
            id,
            version: number,
            closed,
        })
    }

    /// Goes back `steps` versions, dropping the later ones (their numbers are
    /// not used again); returns the version left latest, as
    /// `(version, scene)`, and the one undone.
    pub fn undo(&self, id: SceneId, steps: usize) -> Result<((u32, Value), Value), ToolError> {
        let (undone, (number, text)) = {
            let mut inner = lock(&self.inner);
            let w = inner.working(id)?;
            let earlier = w.versions.len() - 1;
            if steps == 0 || steps > earlier {
                return Err(ToolError::invalid(match earlier {
                    0 => format!("scene {id} has no earlier version to go back to"),
                    n => format!("scene {id} can go back 1 to {n} steps"),
                }));
            }
            let undone = Arc::clone(&w.latest().text);
            let freed: usize = w
                .versions
                .drain(w.versions.len() - steps..)
                .map(|v| v.text.len())
                .sum();
            let latest = w.latest();
            let kept = (latest.number, Arc::clone(&latest.text));
            inner.bytes -= freed;
            (undone, kept)
        };
        Ok(((number, from_text(&text)?), from_text(&undone)?))
    }

    /// Closes `id`.
    pub fn close(&self, id: SceneId) -> Result<(), ToolError> {
        let mut inner = lock(&self.inner);
        inner.working(id)?;
        if let Some(w) = inner.scenes.remove(&id) {
            inner.bytes -= w.versions.iter().map(|v| v.text.len()).sum::<usize>();
        }
        Ok(())
    }

    /// The open scenes, most recently used first.
    pub fn list(&self) -> Result<Vec<Listed>, ToolError> {
        let handles: Vec<(SceneId, u32, usize, Arc<str>)> = {
            let inner = lock(&self.inner);
            inner
                .ids()
                .into_iter()
                .filter_map(|id| {
                    let w = inner.scenes.get(&id)?;
                    let latest = w.latest();
                    Some((
                        id,
                        latest.number,
                        w.versions.len() - 1,
                        Arc::clone(&latest.text),
                    ))
                })
                .collect()
        };
        handles
            .into_iter()
            .map(|(id, version, undo_steps, text)| {
                Ok(Listed {
                    id,
                    version,
                    undo_steps,
                    scene: from_text(&text)?,
                })
            })
            .collect()
    }

    /// The open ids, most recently used first.
    pub fn ids(&self) -> Vec<SceneId> {
        lock(&self.inner).ids()
    }

    /// Bytes over all versions kept.
    pub fn bytes(&self) -> usize {
        lock(&self.inner).bytes
    }
}

// ---------------------------------------------------------------------------
// `scene_id` on every scene tool
// ---------------------------------------------------------------------------

const SCENE_ID: &str = "scene_id";
const RETURN_SCENE: &str = "return_scene";

fn properties(schema: &Map<String, Value>) -> Option<&Map<String, Value>> {
    schema.get("properties")?.as_object()
}

/// Whether `tool` takes a scene (a `scene` argument).
pub fn takes_scene(tool: &Tool) -> bool {
    properties(&tool.input_schema).is_some_and(|p| p.contains_key("scene"))
}

/// Whether `tool` takes a scene and returns it edited.
pub fn edits_scene(tool: &Tool) -> bool {
    takes_scene(tool)
        && tool
            .output_schema
            .as_ref()
            .and_then(|s| properties(s))
            .is_some_and(|p| p.contains_key("scene"))
}

/// Drops `name` from a schema's `required` list.
fn not_required(schema: &mut Map<String, Value>, name: &str) {
    if let Some(Value::Array(required)) = schema.get_mut("required") {
        required.retain(|r| r != name);
    }
}

fn add_property(schema: &mut Map<String, Value>, name: &str, property: Value) {
    if let Some(Value::Object(p)) = schema.get_mut("properties") {
        p.insert(name.to_owned(), property);
    }
}

/// Documents `scene_id` on every tool with a `scene` argument, and
/// `return_scene` on those that return the scene edited, with `scene` no
/// longer required; an editing tool's result may then hold `scene_id` and
/// `version` in place of `scene`. Editing tools change the stored scene
/// when given a `scene_id`, so they are no longer marked read-only or
/// idempotent.
pub fn document_scene_ids(mut router: ToolRouter<ObservationMcp>) -> ToolRouter<ObservationMcp> {
    for route in router.map.values_mut() {
        let tool = &mut route.attr;
        if !takes_scene(tool) {
            continue;
        }
        let edits = edits_scene(tool);
        let input = Arc::make_mut(&mut tool.input_schema);
        not_required(input, "scene");
        add_property(
            input,
            SCENE_ID,
            json!({
                "type": "string",
                "description": "The id open_scene gave, such as \"s1\". Pass it instead of \
                                `scene`."
            }),
        );
        if !edits {
            continue;
        }
        add_property(
            input,
            RETURN_SCENE,
            json!({
                "type": "boolean",
                "description": "With scene_id: also return the whole edited scene. Default \
                                false: the result has scene_id and version instead."
            }),
        );
        if let Some(output) = tool.output_schema.as_mut() {
            let output = Arc::make_mut(output);
            not_required(output, "scene");
            add_property(
                output,
                SCENE_ID,
                json!({"type": "string", "description": "The working scene edited."}),
            );
            add_property(
                output,
                "version",
                json!({"type": "integer", "description": "Its version after the edit."}),
            );
        }
        let annotations = tool
            .annotations
            .get_or_insert_with(ToolAnnotations::default);
        annotations.read_only_hint = Some(false);
        annotations.destructive_hint = Some(false);
        annotations.idempotent_hint = Some(false);
    }
    router
}

/// What a call's `scene_id` asked for, for after the call.
#[derive(Debug)]
pub struct SceneCall {
    id: SceneId,
    based_on: u32,
    return_scene: bool,
    edits: bool,
}

/// Puts the stored scene in place of a `scene_id` among the arguments of a
/// call to `tool`, if it takes a scene; returns what to do with its result.
pub fn prepare(
    store: &SceneStore,
    tool: Option<&Tool>,
    arguments: Option<&mut Map<String, Value>>,
) -> Result<Option<SceneCall>, ToolError> {
    let (Some(tool), Some(args)) = (tool, arguments) else {
        return Ok(None);
    };
    if !takes_scene(tool) {
        return Ok(None);
    }
    let return_scene = match args.remove(RETURN_SCENE) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => b,
        Some(_) => return Err(ToolError::invalid("return_scene is true or false")),
    };
    // A whole tool result passed as the scene. One of a working scene (with a
    // `scene_id`, as create_scene's and every edit by id are) stands for that
    // scene, so the edit is stored; any other, for the value of its `scene`
    // field, put in place here so that the call's area is found from it too.
    let result_id = args
        .get("scene")
        .and_then(edit::result_fields)
        .and_then(|fields| fields.get(SCENE_ID).cloned());
    if let Some(id) = result_id {
        args.remove("scene");
        args.entry(SCENE_ID).or_insert(id);
    } else if let Some(inner) = args.get("scene").and_then(edit::unwrap_result) {
        args.insert("scene".to_owned(), inner);
    }
    // A small model may put the id where the scene goes.
    if let Some(Value::String(text)) = args.get("scene")
        && !args.contains_key(SCENE_ID)
        && let Some(id) = SceneId::parse(text)
    {
        args.insert(SCENE_ID.to_owned(), Value::String(id.to_string()));
        args.remove("scene");
    }
    let id = match args.remove(SCENE_ID) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(text)) => SceneId::parse(&text).ok_or_else(|| {
            ToolError::invalid(format!(
                "scene_id `{text}` is not a working scene's id, such as s1"
            ))
        })?,
        Some(other) => SceneId::parse(&other.to_string()).ok_or_else(|| {
            ToolError::invalid("scene_id is a working scene's id, such as \"s1\"")
        })?,
    };
    if args.get("scene").is_some_and(|s| !s.is_null()) {
        return Err(ToolError::invalid(
            "pass either scene_id or scene, not both",
        ));
    }
    let (scene, version) = store.latest(id)?;
    args.insert("scene".to_owned(), scene);
    Ok(Some(SceneCall {
        id,
        based_on: version,
        return_scene,
        edits: edits_scene(tool),
    }))
}

/// Arguments that name only the area of `scene`, for
/// [`ObservationMcp::in_tool_scope`], without copying the whole scene.
fn area_arguments(scene: Option<&Value>) -> Option<Map<String, Value>> {
    let scene = match scene? {
        // Rare: a scene sent as text. The scope reads its area from the text.
        text @ Value::String(_) => text.clone(),
        other => json!({ "area": other.get("area")? }),
    };
    Some(Map::from_iter([("scene".to_owned(), scene)]))
}

/// A failed call's result, with `e` for the agent.
fn tool_error(e: &ToolError) -> CallToolResponse {
    CallToolResult::error(vec![ContentBlock::text(e.to_string())]).into()
}

/// Stores the edited scene in a call's result as the next version, and puts
/// `scene_id` and `version` in the result in its place (beside it with
/// `return_scene`).
pub fn finish(
    store: &SceneStore,
    call: &SceneCall,
    response: CallToolResponse,
) -> CallToolResponse {
    let CallToolResponse::Complete(mut result) = response else {
        return response;
    };
    if !call.edits || result.is_error == Some(true) {
        return result.into();
    }
    let Some(Value::Object(out)) = result.structured_content.as_mut() else {
        return result.into();
    };
    let Some(scene) = out.get("scene") else {
        return result.into();
    };
    let stored = match store.commit(call.id, call.based_on, scene) {
        Ok(stored) => stored,
        Err(e) => return tool_error(&e),
    };
    if !call.return_scene {
        out.remove("scene");
    }
    out.insert(SCENE_ID.to_owned(), Value::String(call.id.to_string()));
    out.insert("version".to_owned(), Value::from(stored.version));
    if !stored.closed.is_empty() {
        let note = stored.closed_note();
        if let Some(Value::String(summary)) = out.get_mut("summary") {
            summary.push_str(&note);
        } else {
            out.insert(
                "note".to_owned(),
                Value::String(note.trim_start_matches("; ").to_owned()),
            );
        }
    }
    let text = Value::Object(out.clone()).to_string();
    result.content = vec![ContentBlock::text(text)];
    result.into()
}

impl ObservationMcp {
    /// Runs a tool call as [`ServerHandler::call_tool`](rmcp::ServerHandler::call_tool)
    /// does: with a `scene_id` read from and written back to the store (see
    /// the module documentation), and in the scope of the scene's area
    /// ([`Self::in_tool_scope`]).
    pub async fn call_with_scenes(
        &self,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tools = Self::all_tools();
        let call = match prepare(
            &self.scenes,
            tools.get(&request.name),
            request.arguments.as_mut(),
        ) {
            Ok(call) => call,
            Err(e) => return Ok(tool_error(&e)),
        };
        let scope = area_arguments(request.arguments.as_ref().and_then(|a| a.get("scene")));
        let tcc = ToolCallContext::new(self, request, context);
        let response = self.in_tool_scope(scope.as_ref(), tools.call(tcc)).await?;
        Ok(match call {
            Some(call) => finish(&self.scenes, &call, response),
            None => response,
        })
    }

    /// The working scenes.
    pub fn scenes(&self) -> &Arc<SceneStore> {
        &self.scenes
    }
}

// ---------------------------------------------------------------------------
// Summaries
// ---------------------------------------------------------------------------

fn count(n: usize, one: &str, many: &str) -> String {
    match n {
        0 => format!("no {many}"),
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// What a scene holds, in one line.
pub fn describe(scene: &SceneFile) -> String {
    let enabled = scene.cameras.iter().filter(|c| c.enabled).count();
    let mut parts = vec![count(scene.cameras.len(), "camera", "cameras")];
    if enabled < scene.cameras.len() {
        parts[0] = format!("{} ({enabled} enabled)", parts[0]);
    }
    if !scene.custom_models.is_empty() {
        parts.push(count(
            scene.custom_models.len(),
            "custom model",
            "custom models",
        ));
    }
    if !scene.areas.is_empty() {
        parts.push(count(scene.areas.len(), "labelled area", "labelled areas"));
    }
    if !scene.targets.is_empty() {
        parts.push(count(scene.targets.len(), "target", "targets"));
    }
    let area = match scene.area {
        AreaRef::Demo => "the demo street",
        AreaRef::Osm { .. } => "an OpenStreetMap area",
    };
    let light = match scene.params.lighting {
        Lighting::Day => "by day",
        Lighting::Night => "at night",
    };
    format!(
        "{} on {area}; detail level {} {light}",
        parts.join(", "),
        scene.params.dori.label()
    )
}

/// `scene`'s one-line summary as working scene `id` at `version`.
fn line(id: SceneId, version: u32, scene: &Value) -> String {
    match edit::parse(scene) {
        Ok(s) => format!("{id} version {version}: {}", describe(&s)),
        Err(e) => format!("{id} version {version}: could not be read ({e})"),
    }
}

/// The changes from `from` to `to`, in plain lines (at most [`MAX_LINES`]),
/// and how many there are; `None` if they are on different areas.
fn change_lines(from: &SceneFile, to: &SceneFile) -> Option<(Vec<String>, usize)> {
    let changes = ChangeSet::between(from, to).ok()?;
    let mut lines = changes.lines(&|id| model_name(from, to, id));
    let total = lines.len();
    lines.truncate(MAX_LINES);
    Some((lines, total))
}

// ---------------------------------------------------------------------------
// Tool inputs and outputs
// ---------------------------------------------------------------------------

/// Where open_scene takes the scene from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenFrom {
    /// A new, empty scene on the demo street.
    New,
    /// The scene open in the connected app (as get_app_state returns it).
    App,
    /// The scene given in `json`.
    Json,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenArgs {
    /// new (an empty demo scene), app (the connected app's open scene) or
    /// json (the scene in `json`). Default: json if `json` is given, else new.
    pub from: Option<OpenFrom>,
    /// A scene file, for from = json.
    #[schemars(with = "Option<JsonObject>")]
    pub json: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SceneIdArgs {
    /// The id open_scene gave, such as "s1".
    pub scene_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportArgs {
    /// The id open_scene gave, such as "s1", or "s1@3" for its version 3.
    pub scene_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoArgs {
    /// The id open_scene gave, such as "s1".
    pub scene_id: String,
    /// How many versions to go back (default 1).
    pub steps: Option<usize>,
}

/// Schema of a scene reference: a stored version's id or an inline scene.
#[derive(Debug)]
pub struct SceneRefSchema;

impl JsonSchema for SceneRefSchema {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("SceneRef")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "anyOf": [{ "type": "string" }, { "type": "object" }] })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiffArgs {
    /// The earlier scene: a working scene's id (s1, its latest version), a
    /// version (s1@2) or a scene file.
    #[schemars(with = "SceneRefSchema")]
    pub a: Value,
    /// The later scene, given the same way. Default: the latest version of
    /// a's scene, or, if a is a plain id such as s1, the version a's latest
    /// came from, so a alone shows what its last edit changed.
    #[schemars(with = "Option<SceneRefSchema>")]
    pub b: Option<Value>,
}

/// One edit for apply_edits.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditStep {
    /// A scene-editing tool, such as place_camera, aim_camera or add_target.
    pub tool: String,
    /// Its arguments, without scene or scene_id.
    #[serde(default)]
    #[schemars(with = "Option<JsonObject>")]
    pub args: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyArgs {
    /// The id open_scene gave, such as "s1".
    pub scene_id: String,
    /// The edits, run in order (at most 50). If one fails, none is kept.
    pub edits: Vec<EditStep>,
}

/// A working scene and its version.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SceneHandle {
    /// Pass it as scene_id to the scene tools.
    pub scene_id: String,
    pub version: u32,
    /// What the scene holds, in one line.
    pub summary: String,
}

/// A new scene from create_scene, kept as a working scene too.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Created {
    /// Pass it as scene_id to the scene tools (preferred to passing `scene`).
    pub scene_id: String,
    pub version: u32,
    pub summary: String,
    /// The scene file, for passing as `scene` instead.
    #[schemars(with = "JsonObject")]
    pub scene: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Exported {
    pub scene_id: String,
    pub version: u32,
    /// The scene file: import it in the app, or pass it to propose_changes.
    #[schemars(with = "JsonObject")]
    pub scene: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListedOut {
    pub scene_id: String,
    pub version: u32,
    /// How many versions undo can go back.
    pub undo_steps: usize,
    pub summary: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SceneList {
    /// The open working scenes, most recently used first.
    pub scenes: Vec<ListedOut>,
    /// How many can be open; opening another closes the least recently used.
    pub max_scenes: usize,
    pub summary: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Closed {
    pub closed: String,
    /// The working scenes still open, most recently used first.
    pub open: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Undone {
    pub scene_id: String,
    /// The version the scene is back at; later versions are dropped.
    pub version: u32,
    /// What the undone edits had changed, one line each.
    pub undone: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Diffed {
    /// Which scenes were compared, such as "s1 version 2 to s1 version 4".
    pub compared: String,
    /// One line per change, in plain words (at most 40).
    pub changes: Vec<String>,
    /// How many changes there are in all.
    pub total: usize,
    pub summary: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StepOut {
    pub tool: String,
    /// The tool's result, without the scene (camera ids and so on).
    #[schemars(with = "JsonObject")]
    pub result: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Applied {
    pub scene_id: String,
    /// The version all the edits made together.
    pub version: u32,
    /// Each edit's result, in order.
    pub steps: Vec<StepOut>,
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn not_an_id(text: &str) -> ToolError {
    ToolError::invalid(format!(
        "`{text}` is not a working scene's id, such as s1 (list_scenes lists them)"
    ))
}

fn id_of(text: &str) -> Result<SceneId, ToolError> {
    SceneId::parse(text).ok_or_else(|| not_an_id(text))
}

/// A side of diff_scenes, resolved: the scene and its name in the output.
struct Side {
    scene: SceneFile,
    name: String,
}

impl Side {
    fn stored(id: SceneId, (scene, version): (Value, u32)) -> Result<Self, ToolError> {
        Ok(Self {
            scene: edit::parse(&scene)?,
            name: format!("{id} version {version}"),
        })
    }
}

/// An argument of the edit `n` (`name`) of apply_edits, as an object.
fn step_args(n: usize, name: &str, args: Option<Value>) -> Result<Map<String, Value>, ToolError> {
    let parsed = match args {
        None | Some(Value::Null) => return Ok(Map::new()),
        Some(Value::String(text)) => serde_json::from_str(&text).ok(),
        other => other,
    };
    match parsed {
        Some(Value::Object(m)) => Ok(m),
        _ => Err(ToolError::invalid(format!(
            "edit {n} ({name}): args is an object of the tool's arguments"
        ))),
    }
}

impl ObservationMcp {
    /// A side given as a stored version's reference (`s1`, `s1@2`), with
    /// its id and version, or as a scene file.
    fn side(&self, given: &Value, label: &str) -> Result<(Side, Option<SceneId>), ToolError> {
        match given {
            Value::String(text) if !text.trim_start().starts_with('{') => {
                let (id, version) = parse_ref(text).ok_or_else(|| {
                    ToolError::invalid(format!(
                        "{label}: `{text}` is neither a working scene's id (s1), a version \
                         (s1@2) nor a scene file"
                    ))
                })?;
                Ok((Side::stored(id, self.scenes.get(id, version)?)?, Some(id)))
            }
            other => {
                let (scene, _) = edit::finish(&edit::parse(other)?)?;
                let name = format!("the scene given as {label}");
                Ok((Side { scene, name }, None))
            }
        }
    }

    fn diff(&self, a: &DiffArgs) -> Result<Diffed, ToolError> {
        let (from, to) = match (&a.a, &a.b) {
            (first, Some(second)) => (self.side(first, "a")?.0, self.side(second, "b")?.0),
            // a alone as a plain id: what its last edit changed.
            (Value::String(text), None) if !text.contains('@') => {
                let id = id_of(text)?;
                let (before, after) = self.scenes.last_edit(id)?;
                (Side::stored(id, before)?, Side::stored(id, after)?)
            }
            // a alone as a version: from it to the latest.
            (first, None) => {
                let (from, id) = self.side(first, "a")?;
                let id = id.ok_or_else(|| {
                    ToolError::invalid("give b too: a is a scene file, not a working scene")
                })?;
                (from, Side::stored(id, self.scenes.latest(id)?)?)
            }
        };
        let compared = format!("{} to {}", from.name, to.name);
        let Some((changes, total)) = change_lines(&from.scene, &to.scene) else {
            return Ok(Diffed {
                summary: format!(
                    "{compared}: the scenes are on different areas, so they are not compared \
                     item by item"
                ),
                compared,
                changes: Vec::new(),
                total: 0,
            });
        };
        let summary = match total {
            0 => format!("{compared}: no changes"),
            1 => format!("{compared}: 1 change"),
            n if n > changes.len() => {
                format!(
                    "{compared}: {n} changes, the first {} listed",
                    changes.len()
                )
            }
            n => format!("{compared}: {n} changes"),
        };
        Ok(Diffed {
            compared,
            changes,
            total,
            summary,
        })
    }

    /// create_scene: a new demo scene, stored, with its file.
    pub(crate) fn create(&self) -> Result<Created, ToolError> {
        let (_, scene) = edit::finish(&edit::new_demo_scene()?)?;
        let stored = self.scenes.open(&scene)?;
        Ok(Created {
            scene_id: stored.id.to_string(),
            version: stored.version,
            summary: format!(
                "created an empty scene on the demo street as {}{}",
                stored.id,
                stored.closed_note()
            ),
            scene,
        })
    }

    async fn open(&self, a: OpenArgs) -> Result<SceneHandle, ToolError> {
        let from = a.from.unwrap_or(if a.json.is_some() {
            OpenFrom::Json
        } else {
            OpenFrom::New
        });
        let scene = match from {
            OpenFrom::New => edit::finish(&edit::new_demo_scene()?)?.1,
            OpenFrom::Json => {
                let json = a
                    .json
                    .as_ref()
                    .ok_or_else(|| ToolError::invalid("from = json needs the scene in `json`"))?;
                edit::finish(&edit::parse(json)?)?.1
            }
            OpenFrom::App => {
                if !self.link.is_connected() {
                    return Err(ToolError::NotConnected);
                }
                let state = AppStateOut::of(self.link.refresh().await?);
                match (state.scene, state.scene_problem) {
                    (Some(scene), _) => scene,
                    (None, Some(problem)) => {
                        return Err(ToolError::invalid(format!(
                            "the app's scene could not be read: {problem}"
                        )));
                    }
                    (None, None) => {
                        return Err(ToolError::invalid(
                            "the app has not shared a scene yet: wait until an area has loaded",
                        ));
                    }
                }
            }
        };
        let stored = self.scenes.open(&scene)?;
        Ok(SceneHandle {
            scene_id: stored.id.to_string(),
            version: stored.version,
            summary: format!(
                "{}{}",
                line(stored.id, stored.version, &scene),
                stored.closed_note()
            ),
        })
    }

    async fn apply(
        &self,
        a: ApplyArgs,
        context: RequestContext<RoleServer>,
    ) -> Result<Applied, ToolError> {
        let id = id_of(&a.scene_id)?;
        if a.edits.is_empty() {
            return Err(ToolError::invalid("give at least one edit"));
        }
        if a.edits.len() > MAX_EDITS {
            return Err(ToolError::invalid(format!(
                "{} edits, but one call runs at most {MAX_EDITS}",
                a.edits.len()
            )));
        }
        let (scene, based_on) = self.scenes.latest(id)?;
        // The scene's area is fetched once, and in view for every edit.
        let scope = area_arguments(Some(&scene));
        let (scene, steps) = self
            .in_tool_scope(scope.as_ref(), self.run_edits(scene, a.edits, context))
            .await?;
        let stored = self.scenes.commit(id, based_on, &scene)?;
        let summary = format!(
            "applied {}; now {}{}",
            count(steps.len(), "edit", "edits"),
            line(id, stored.version, &scene),
            stored.closed_note()
        );
        Ok(Applied {
            scene_id: id.to_string(),
            version: stored.version,
            steps,
            summary,
        })
    }

    /// Runs `edits` on `scene` in order through the tool router; returns the
    /// scene they leave and each edit's result, or why one failed.
    async fn run_edits(
        &self,
        mut scene: Value,
        edits: Vec<EditStep>,
        context: RequestContext<RoleServer>,
    ) -> Result<(Value, Vec<StepOut>), ToolError> {
        let tools = Self::all_tools();
        let allowed = || {
            let names: Vec<String> = tools
                .list_all()
                .iter()
                .filter(|t| edits_scene(t))
                .map(|t| t.name.to_string())
                .collect();
            names.join(", ")
        };
        let mut steps = Vec::with_capacity(edits.len());
        for (i, step) in edits.into_iter().enumerate() {
            let n = i + 1;
            let name = step.tool.trim().to_owned();
            let failed = |why: &str| {
                ToolError::invalid(format!(
                    "edit {n} ({name}) failed, so no edit was kept: {why}"
                ))
            };
            match tools.get(&name) {
                None => {
                    return Err(ToolError::invalid(format!(
                        "edit {n}: there is no tool `{name}`; apply_edits runs these: {}",
                        allowed()
                    )));
                }
                Some(tool) if !edits_scene(tool) => {
                    return Err(ToolError::invalid(format!(
                        "edit {n}: {name} does not edit a scene; apply_edits runs these: {}",
                        allowed()
                    )));
                }
                Some(_) => {}
            }
            let mut args = step_args(n, &name, step.args)?;
            for key in ["scene", SCENE_ID, RETURN_SCENE] {
                args.remove(key);
            }
            args.insert("scene".to_owned(), scene);
            let request = CallToolRequestParams::new(name.clone()).with_arguments(args);
            let tcc = ToolCallContext::new(self, request, context.clone());
            let response = tools.call(tcc).await.map_err(|e| failed(&e.message))?;
            let CallToolResponse::Complete(result) = response else {
                return Err(failed("it did not finish"));
            };
            let mut out = match (result.is_error, result.structured_content) {
                (Some(true), _) | (_, None) => {
                    let text: Vec<&str> = result
                        .content
                        .iter()
                        .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
                        .collect();
                    return Err(failed(&text.join(" ")));
                }
                (_, Some(Value::Object(out))) => out,
                (_, Some(_)) => return Err(failed("it returned no scene")),
            };
            scene = out
                .remove("scene")
                .ok_or_else(|| failed("it returned no scene"))?;
            steps.push(StepOut {
                tool: name,
                result: Value::Object(out),
            });
        }
        Ok((scene, steps))
    }
}

#[tool_router(router = store_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Open a working scene the server keeps for you, and get its id (such as s1) to pass as scene_id to the scene tools instead of the whole scene. from: new (an empty demo scene, the default), app (the connected app's open scene) or json (a scene file given in json).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn open_scene(
        &self,
        Parameters(a): Parameters<OpenArgs>,
    ) -> Result<Json<SceneHandle>, String> {
        self.open(a).await.map(Json).map_err(fail)
    }

    #[tool(
        description = "Get a working scene's file (scene_id such as s1, or s1@3 for version 3), ready to import in the app or to pass to propose_changes.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn export_scene(
        &self,
        Parameters(a): Parameters<ExportArgs>,
    ) -> Result<Json<Exported>, String> {
        let (id, version) = parse_ref(&a.scene_id).ok_or_else(|| fail(not_an_id(&a.scene_id)))?;
        let (scene, version) = self.scenes.get(id, version).map_err(fail)?;
        Ok(Json(Exported {
            scene_id: id.to_string(),
            version,
            scene,
        }))
    }

    #[tool(
        description = "List the open working scenes, most recently used first, each with its version and a one-line summary.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn list_scenes(&self) -> Result<Json<SceneList>, String> {
        let scenes: Vec<ListedOut> = self
            .scenes
            .list()
            .map_err(fail)?
            .into_iter()
            .map(|l| ListedOut {
                scene_id: l.id.to_string(),
                version: l.version,
                undo_steps: l.undo_steps,
                summary: line(l.id, l.version, &l.scene),
            })
            .collect();
        let summary = match scenes.len() {
            0 => "no working scenes are open: open_scene makes one".to_owned(),
            n => format!(
                "{} open, of at most {MAX_SCENES}",
                count(n, "working scene", "working scenes")
            ),
        };
        Ok(Json(SceneList {
            scenes,
            max_scenes: MAX_SCENES,
            summary,
        }))
    }

    #[tool(
        description = "Close a working scene; its id stops working. Export it first to keep it.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn close_scene(
        &self,
        Parameters(a): Parameters<SceneIdArgs>,
    ) -> Result<Json<Closed>, String> {
        let id = id_of(&a.scene_id).map_err(fail)?;
        self.scenes.close(id).map_err(fail)?;
        Ok(Json(Closed {
            closed: id.to_string(),
            open: self.scenes.ids().iter().map(ToString::to_string).collect(),
        }))
    }

    #[tool(
        description = "Undo the last edit of a working scene, or the last `steps` edits (up to 20 are kept). Returns what was undone.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) fn undo(&self, Parameters(a): Parameters<UndoArgs>) -> Result<Json<Undone>, String> {
        let id = id_of(&a.scene_id).map_err(fail)?;
        let steps = a.steps.unwrap_or(1);
        let ((version, scene), undone) = self.scenes.undo(id, steps).map_err(fail)?;
        let now = edit::parse(&scene).map_err(fail)?;
        let before = edit::parse(&undone).map_err(fail)?;
        let lines = change_lines(&now, &before).map_or_else(Vec::new, |(l, _)| l);
        Ok(Json(Undone {
            scene_id: id.to_string(),
            version,
            undone: lines,
            summary: format!(
                "undid {}; {} is back at version {version}: {}",
                count(steps, "edit", "edits"),
                id,
                describe(&now)
            ),
        }))
    }

    #[tool(
        description = "Compare two scenes and list each change in plain words. a and b are each a working scene's id (s1), a version (s1@2) or a scene file; with a alone as s1, shows what its last edit changed.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn diff_scenes(
        &self,
        Parameters(a): Parameters<DiffArgs>,
    ) -> Result<Json<Diffed>, String> {
        self.diff(&a).map(Json).map_err(fail)
    }

    #[tool(
        description = "Run several scene edits on a working scene in order, as one new version, all or nothing. Each edit is {tool, args}: a scene-editing tool (place_camera, aim_camera, add_target, ...) and its arguments without scene. Returns each edit's result, such as the camera ids.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn apply_edits(
        &self,
        Parameters(a): Parameters<ApplyArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<Applied>, String> {
        self.apply(a, context).await.map(Json).map_err(fail)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
pub(crate) mod tests;
