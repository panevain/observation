//! View commands: the agent points the user at something in the open app,
//! never changing the scene.
//!
//! - The agent posts a [`CommandPost`] to [`paths::COMMAND`](crate::paths::COMMAND)
//!   and gets its id ([`CommandSent`], answered `202 Accepted`). The server
//!   checks it ([`AgentCommand::checked`]) and passes it to the browser's open
//!   event streams as a [`BrowserEventKind::Command`](crate::BrowserEventKind::Command)
//!   event. Commands are not stored: a stream opened later does not get
//!   them, and with no stream open the post is refused (`409`).
//! - The browser applies one as the user would: it moves the 3D view,
//!   changes the selection, marks something, or changes a view setting (the
//!   overlay colours, the cones, the walls). No command changes the scene:
//!   an agent that wants another lighting or detail level proposes it as a
//!   scene change. A [`AgentCommand::RequestLoadArea`] never loads anything
//!   by itself: the
//!   app asks the user, who loads the area or not. With the user's "Let the
//!   agent move the view and selection" off, every other command is ignored.
//! - A pairing may send [`COMMANDS_PER_SECOND`] commands a second, in bursts
//!   of up to [`COMMAND_BURST`].
//!
//! Notes and reasons are untrusted text: tidied here, drawn as text, never
//! as markup. A kind of command, of highlight or of overlay this build does
//! not know reads as `Unknown`, so a later kind is skipped by an older
//! browser rather than breaking its event stream.
//!
//! With the `schema` feature, [`ItemRef`] and [`OverlayColours`] derive
//! `schemars::JsonSchema` (without `Unknown`), so the MCP tools take them
//! as they are.

use observation_core::footage::MAX_NAME_CHARS;
use observation_core::geo::BBox;
use serde::{Deserialize, Serialize};

use crate::{MAX_SELECTION, tidy_line};

/// Largest [`CommandPost`] body accepted, bytes.
pub const MAX_COMMAND_BYTES: usize = 16 * 1024;
/// Commands a pairing may send a second, on average.
pub const COMMANDS_PER_SECOND: u32 = 10;
/// Commands a pairing may send at once.
pub const COMMAND_BURST: u32 = 20;
/// Farthest a point in a command may be from the area's origin along `x` or
/// `z`, metres. Areas are at most 500 m across.
pub const MAX_COORD_M: f32 = 2_000.0;
/// Closest the view may be brought to the spot it looks at, metres.
pub const MIN_FOCUS_DISTANCE_M: f32 = 8.0;
/// Farthest the view may be taken from the spot it looks at, metres.
pub const MAX_FOCUS_DISTANCE_M: f32 = 1_200.0;
/// Longest note on a highlight kept, characters.
pub const MAX_NOTE_CHARS: usize = 80;
/// Longest reason for loading an area kept, characters.
pub const MAX_LOAD_REASON_CHARS: usize = 200;

/// Body of `POST` [`paths::COMMAND`](crate::paths::COMMAND).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandPost {
    pub v: u32,
    pub command: AgentCommand,
}

/// The server passed a [`CommandPost`] on to the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSent {
    pub v: u32,
    /// The command's id within the pairing (from 1).
    pub id: u64,
}

/// What the agent asks the app to show, by its `kind`. Positions are metres
/// from the area's origin, `[x, z]`: x east, z south.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentCommand {
    /// Moves the 3D view to look at `at`, from `distance_m` away (or from
    /// as far as it is now, at most 60 m).
    FocusView {
        at: [f32; 2],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        distance_m: Option<f32>,
    },
    /// Selects these cameras (none: clears the selection). The app selects
    /// one camera at a time: the first of them in its scene.
    SelectCameras { ids: Vec<u32> },
    /// Marks something in the 3D view, with an optional note drawn by it.
    /// A new highlight replaces the one before.
    Highlight {
        what: Highlight,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Changes the view settings given; the rest stay as they are.
    SetViewOptions(ViewOptions),
    /// Asks the user to load this area, saying why. The app never loads it
    /// without the user's click.
    RequestLoadArea { bbox: BBox, reason: String },
    /// A kind this build does not know.
    #[serde(other)]
    Unknown,
}

/// What a [`AgentCommand::Highlight`] marks, by its `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Highlight {
    /// A camera of the open scene.
    Camera { id: u32 },
    /// A target of the open scene, by id or name.
    Target { item: ItemRef },
    /// A labelled area of the open scene, by id or name.
    Area { item: ItemRef },
    /// A spot on the ground, `[x, z]`.
    Point { at: [f32; 2] },
    /// Removes the highlight.
    Clear,
    /// A kind this build does not know.
    #[serde(other)]
    Unknown,
}

/// An area or target named by its id, or by its name (matched without
/// regard to case or to how the spaces in it run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ItemRef {
    Id(u32),
    Name(String),
}

/// `name` as names are compared: its spaces made single, trimmed, in lower
/// case.
fn comparable(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl ItemRef {
    /// Whether this names the item with `id` and `name`.
    pub fn matches(&self, id: u32, name: Option<&str>) -> bool {
        match self {
            Self::Id(i) => *i == id,
            Self::Name(n) => name.is_some_and(|name| comparable(name) == comparable(n)),
        }
    }
}

/// An item in words, as the app and the agent name it: "target 3 ‘Front
/// door’", or "target 3" without a name. The name is quoted as it is.
pub fn item_words(what: &str, id: u32, name: Option<&str>) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => format!("{what} {id} ‘{n}’"),
        None => format!("{what} {id}"),
    }
}

/// The item `r` names among `items` (`(id, name)`), with its words (see
/// [`item_words`]), if there is one.
pub fn find_item<'a>(
    mut items: impl Iterator<Item = (u32, Option<&'a str>)>,
    r: &ItemRef,
    what: &str,
) -> Option<(u32, String)> {
    items
        .find(|(id, name)| r.matches(*id, *name))
        .map(|(id, name)| (id, item_words(what, id, name)))
}

/// The view settings a [`AgentCommand::SetViewOptions`] changes; `None`
/// leaves one as it is. Only settings of the view, never of the scene: the
/// lighting and the detail level are the scene's, changed by a proposal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewOptions {
    /// How the coverage overlay colours the ground.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<OverlayColours>,
    /// Every camera's view cone shown (the selected one always shows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cones: Option<bool>,
    /// Coverage shown on building walls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walls: Option<bool>,
}

impl ViewOptions {
    /// Whether it changes nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How the coverage overlay colours the ground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OverlayColours {
    /// By how many cameras see each spot.
    Overlap,
    /// Each camera's coverage in its own colour.
    PerCamera,
    /// A colouring this build does not know.
    #[serde(other)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    Unknown,
}

/// A point that is finite and within [`MAX_COORD_M`] of the origin.
fn check_point(at: [f32; 2], what: &str) -> Result<(), String> {
    if at.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORD_M) {
        Ok(())
    } else {
        Err(format!(
            "{what} must be [x, z] in metres, each within {MAX_COORD_M} of the area's origin"
        ))
    }
}

impl AgentCommand {
    /// The command tidied (notes and reasons made one short line, ids
    /// without repeats) and checked: every number finite and within its
    /// limits, an area of the size the app loads, and nothing this build
    /// does not know. The error is worded for the agent.
    pub fn checked(self) -> Result<Self, String> {
        match self {
            Self::FocusView { at, distance_m } => {
                check_point(at, "`at`")?;
                if let Some(d) = distance_m
                    && !(d.is_finite()
                        && (MIN_FOCUS_DISTANCE_M..=MAX_FOCUS_DISTANCE_M).contains(&d))
                {
                    return Err(format!(
                        "`distance_m` must be {MIN_FOCUS_DISTANCE_M} to {MAX_FOCUS_DISTANCE_M} m"
                    ));
                }
                Ok(Self::FocusView { at, distance_m })
            }
            Self::SelectCameras { ids } => {
                if ids.len() > MAX_SELECTION {
                    return Err(format!("select at most {MAX_SELECTION} cameras"));
                }
                let mut unique = Vec::with_capacity(ids.len());
                for id in ids {
                    if !unique.contains(&id) {
                        unique.push(id);
                    }
                }
                Ok(Self::SelectCameras { ids: unique })
            }
            Self::Highlight { what, note } => {
                let what = match what {
                    Highlight::Point { at } => {
                        check_point(at, "the point")?;
                        Highlight::Point { at }
                    }
                    Highlight::Target { item } => Highlight::Target {
                        item: item.checked()?,
                    },
                    Highlight::Area { item } => Highlight::Area {
                        item: item.checked()?,
                    },
                    Highlight::Unknown => {
                        return Err(
                            "highlight a camera, a target, an area or a point, or clear".into()
                        );
                    }
                    other @ (Highlight::Camera { .. } | Highlight::Clear) => other,
                };
                let note = match what {
                    Highlight::Clear => None,
                    _ => note.and_then(|n| tidy_line(&n, MAX_NOTE_CHARS)),
                };
                Ok(Self::Highlight { what, note })
            }
            Self::SetViewOptions(o) => {
                if o.is_empty() {
                    return Err("say which view setting to change".into());
                }
                if o.overlay == Some(OverlayColours::Unknown) {
                    return Err("`overlay` is overlap or per_camera".into());
                }
                Ok(Self::SetViewOptions(o))
            }
            Self::RequestLoadArea { bbox, reason } => {
                let bbox = BBox::new(bbox.south, bbox.west, bbox.north, bbox.east)
                    .map_err(|e| format!("the area's bbox is not usable: {e}"))?;
                bbox.validate_size()
                    .map_err(|e| format!("the app cannot load that area: {e}"))?;
                let reason = tidy_line(&reason, MAX_LOAD_REASON_CHARS).ok_or(
                    "say why the area should be loaded: the user reads the reason before deciding",
                )?;
                Ok(Self::RequestLoadArea { bbox, reason })
            }
            Self::Unknown => Err("unknown kind of command".into()),
        }
    }
}

impl ItemRef {
    fn checked(self) -> Result<Self, String> {
        match self {
            Self::Id(id) => Ok(Self::Id(id)),
            Self::Name(n) => tidy_line(&n, MAX_NAME_CHARS)
                .map(Self::Name)
                .ok_or_else(|| "give the item's id or its name".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrowserEvent, BrowserEventKind, PROTOCOL_VERSION};
    use serde_json::json;

    fn read(v: serde_json::Value) -> AgentCommand {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn commands_read_and_write_by_kind() {
        let cases = [
            (
                json!({"kind": "focus_view", "at": [12.5, -3.0]}),
                AgentCommand::FocusView {
                    at: [12.5, -3.0],
                    distance_m: None,
                },
            ),
            (
                json!({"kind": "select_cameras", "ids": [3, 1]}),
                AgentCommand::SelectCameras { ids: vec![3, 1] },
            ),
            (
                json!({"kind": "highlight", "what": {"kind": "target", "item": "Front door"},
                       "note": "seen by no camera"}),
                AgentCommand::Highlight {
                    what: Highlight::Target {
                        item: ItemRef::Name("Front door".into()),
                    },
                    note: Some("seen by no camera".into()),
                },
            ),
            (
                json!({"kind": "highlight", "what": {"kind": "area", "item": 4}}),
                AgentCommand::Highlight {
                    what: Highlight::Area {
                        item: ItemRef::Id(4),
                    },
                    note: None,
                },
            ),
            (
                json!({"kind": "highlight", "what": {"kind": "clear"}}),
                AgentCommand::Highlight {
                    what: Highlight::Clear,
                    note: None,
                },
            ),
            (
                json!({"kind": "set_view_options", "overlay": "per_camera", "cones": false}),
                AgentCommand::SetViewOptions(ViewOptions {
                    overlay: Some(OverlayColours::PerCamera),
                    cones: Some(false),
                    walls: None,
                }),
            ),
            (
                json!({"kind": "request_load_area", "reason": "your street",
                       "bbox": {"south": 51.5, "west": -0.13, "north": 51.502, "east": -0.127}}),
                AgentCommand::RequestLoadArea {
                    bbox: BBox::new(51.5, -0.13, 51.502, -0.127).unwrap(),
                    reason: "your street".into(),
                },
            ),
        ];
        for (text, command) in cases {
            assert_eq!(read(text.clone()), command, "{text}");
            assert_eq!(serde_json::to_value(&command).unwrap(), text);
        }
        // Scene settings are not view options: a `night` field is
        // ignored, which leaves nothing to change.
        let night = read(json!({"kind": "set_view_options", "night": true}));
        assert_eq!(night, AgentCommand::SetViewOptions(ViewOptions::default()));
        assert!(night.checked().is_err());
    }

    #[test]
    fn later_kinds_read_as_unknown() {
        assert_eq!(
            read(json!({"kind": "fly_through", "speed": 3})),
            AgentCommand::Unknown
        );
        assert_eq!(
            read(json!({"kind": "highlight", "what": {"kind": "tree", "id": 1}})),
            AgentCommand::Highlight {
                what: Highlight::Unknown,
                note: None
            }
        );
        let o = read(json!({"kind": "set_view_options", "overlay": "rainbow"}));
        assert_eq!(
            o,
            AgentCommand::SetViewOptions(ViewOptions {
                overlay: Some(OverlayColours::Unknown),
                ..ViewOptions::default()
            })
        );
        // And none of them passes the check.
        for unknown in [
            o,
            AgentCommand::Unknown,
            read(json!({"kind": "highlight", "what": {"kind": "tree"}})),
        ] {
            assert!(unknown.checked().is_err());
        }
    }

    #[test]
    fn a_command_event_carries_its_id_and_command() {
        let event = BrowserEvent::new(BrowserEventKind::Command {
            id: 2,
            command: AgentCommand::FocusView {
                at: [1.0, 2.0],
                distance_m: Some(40.0),
            },
        });
        let text = serde_json::to_string(&event).unwrap();
        assert_eq!(
            text,
            r#"{"v":1,"type":"command","id":2,"command":{"kind":"focus_view","at":[1.0,2.0],"distance_m":40.0}}"#
        );
        assert_eq!(serde_json::from_str::<BrowserEvent>(&text).unwrap(), event);
        // An event with a command kind this build does not know still reads.
        let later: BrowserEvent = serde_json::from_value(
            json!({"v": 1, "type": "command", "id": 3, "command": {"kind": "orbit"}}),
        )
        .unwrap();
        assert_eq!(
            later.kind,
            BrowserEventKind::Command {
                id: 3,
                command: AgentCommand::Unknown
            }
        );
        let post = CommandPost {
            v: PROTOCOL_VERSION,
            command: AgentCommand::SelectCameras { ids: vec![] },
        };
        let text = serde_json::to_string(&post).unwrap();
        assert_eq!(serde_json::from_str::<CommandPost>(&text).unwrap(), post);
    }

    #[test]
    fn numbers_must_be_finite_and_within_limits() {
        let focus = |at: [f32; 2], distance_m| AgentCommand::FocusView { at, distance_m };
        assert!(focus([0.0, 0.0], None).checked().is_ok());
        assert!(focus([-2000.0, 2000.0], Some(8.0)).checked().is_ok());
        assert!(focus([0.0, 0.0], Some(1200.0)).checked().is_ok());
        for bad in [
            focus([f32::NAN, 0.0], None),
            focus([0.0, f32::INFINITY], None),
            focus([2000.5, 0.0], None),
            focus([0.0, 0.0], Some(7.9)),
            focus([0.0, 0.0], Some(1201.0)),
            focus([0.0, 0.0], Some(f32::NAN)),
        ] {
            assert!(bad.clone().checked().is_err(), "{bad:?}");
        }
        let point = |at| AgentCommand::Highlight {
            what: Highlight::Point { at },
            note: None,
        };
        assert!(point([5.0, 5.0]).checked().is_ok());
        assert!(point([5.0, -1e9]).checked().is_err());
        // A huge number in JSON reads as infinity in an f32, and is refused.
        let huge = read(json!({"kind": "focus_view", "at": [1e300, 0]}));
        assert!(huge.checked().is_err());
    }

    #[test]
    fn selections_are_capped_and_without_repeats() {
        let ids = AgentCommand::SelectCameras {
            ids: vec![4, 2, 4, 2, 9],
        };
        assert_eq!(
            ids.checked().unwrap(),
            AgentCommand::SelectCameras { ids: vec![4, 2, 9] }
        );
        let too_many = AgentCommand::SelectCameras {
            ids: (0..=MAX_SELECTION as u32).collect(),
        };
        assert!(too_many.checked().is_err());
    }

    #[test]
    fn notes_and_reasons_are_tidied_and_a_reason_is_needed() {
        let note = AgentCommand::Highlight {
            what: Highlight::Camera { id: 1 },
            note: Some(format!("  blind\n spot {}", "x".repeat(200))),
        };
        let AgentCommand::Highlight { note: Some(n), .. } = note.checked().unwrap() else {
            panic!("a note");
        };
        assert!(n.starts_with("blind spot x"), "{n}");
        assert_eq!(n.chars().count(), MAX_NOTE_CHARS);
        // Clearing keeps no note; an empty name is no name.
        let clear = AgentCommand::Highlight {
            what: Highlight::Clear,
            note: Some("x".into()),
        };
        assert_eq!(
            clear.checked().unwrap(),
            AgentCommand::Highlight {
                what: Highlight::Clear,
                note: None
            }
        );
        let nameless = AgentCommand::Highlight {
            what: Highlight::Target {
                item: ItemRef::Name("  ".into()),
            },
            note: None,
        };
        assert!(nameless.checked().is_err());

        let bbox = BBox::new(51.5, -0.13, 51.502, -0.127).unwrap();
        let load = |reason: &str| AgentCommand::RequestLoadArea {
            bbox,
            reason: reason.into(),
        };
        assert!(load(" \n ").checked().is_err());
        let AgentCommand::RequestLoadArea { reason, .. } =
            load("  to see\nyour street ").checked().unwrap()
        else {
            panic!("a request");
        };
        assert_eq!(reason, "to see your street");
    }

    #[test]
    fn an_area_to_load_must_be_one_the_app_loads() {
        let load = |south, west, north, east| AgentCommand::RequestLoadArea {
            bbox: BBox {
                south,
                west,
                north,
                east,
            },
            reason: "r".into(),
        };
        assert!(load(51.5, -0.13, 51.502, -0.127).checked().is_ok());
        for bad in [
            // Too large, too small, upside down, off the map.
            load(51.0, -1.0, 52.0, 0.0),
            load(51.5, -0.13, 51.50001, -0.12999),
            load(51.502, -0.13, 51.5, -0.127),
            load(89.0, -0.13, 89.002, -0.127),
            load(f64::NAN, -0.13, 51.502, -0.127),
        ] {
            assert!(bad.clone().checked().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn view_options_must_change_something() {
        assert!(
            AgentCommand::SetViewOptions(ViewOptions::default())
                .checked()
                .is_err()
        );
        let walls = AgentCommand::SetViewOptions(ViewOptions {
            walls: Some(true),
            ..ViewOptions::default()
        });
        assert!(walls.checked().is_ok());
    }

    #[test]
    fn items_match_by_id_or_by_name() {
        assert!(ItemRef::Id(3).matches(3, None));
        assert!(!ItemRef::Id(3).matches(4, Some("3")));
        let door = ItemRef::Name(" front door".into());
        assert!(door.matches(9, Some("Front Door ")));
        assert!(!door.matches(9, None));
        assert!(!door.matches(9, Some("Back door")));
        // Spaces run as they may, on either side; a checked name still matches.
        let spaced = ItemRef::Name("Front  door".into()).checked().unwrap();
        assert!(spaced.matches(9, Some("Front  door")));
        assert!(spaced.matches(9, Some(" front\tdoor")));
        assert!(ItemRef::Name("front door".into()).matches(9, Some("Front   Door")));
    }

    #[test]
    fn items_are_found_and_put_in_words() {
        let items = [(1, Some("Front door")), (2, None), (3, Some("  "))];
        assert_eq!(
            find_item(
                items.into_iter(),
                &ItemRef::Name("FRONT door".into()),
                "target"
            ),
            Some((1, "target 1 ‘Front door’".to_owned()))
        );
        assert_eq!(
            find_item(items.into_iter(), &ItemRef::Id(2), "area"),
            Some((2, "area 2".to_owned()))
        );
        assert_eq!(item_words("camera", 3, Some("  ")), "camera 3");
        assert_eq!(find_item(items.into_iter(), &ItemRef::Id(9), "area"), None);
    }
}
