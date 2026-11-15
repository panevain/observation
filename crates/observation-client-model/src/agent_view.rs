//! The agent's view commands (see [`observation_agent_api::command`]): what
//! each one does in the app, worked out against the document.
//!
//! [`plan`] checks a command again (the server did, but the browser trusts
//! nothing that came over the wire) and finds what it names in the
//! document: a camera, a target or an area by id or name. What it returns
//! is either a [`ViewAction`] for the client to carry out (move the 3D
//! view, change the selection or the view toggles), a mark to draw, or a
//! request to load an area for the user to decide on. Each comes with the
//! words of a toast, so the user always hears what the agent did.
//!
//! No command changes the document: the lighting and the detail level are
//! the scene's, and an agent changes them only by a proposal the user
//! accepts.

use crate::document::Document;
use crate::editor::OverlayMode;
use observation_agent_api::{
    AgentCommand, Highlight, ItemRef, OverlayColours, find_item, item_words,
};
use observation_core::footage::TargetPlace;
use observation_core::geo::BBox;

/// What the agent's mark is on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarkAt {
    Camera(u32),
    Target(u32),
    Area(u32),
    /// A spot on the ground, world `[x, z]`.
    Point([f32; 2]),
}

/// The agent's mark in the 3D view, with its note.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentMark {
    pub at: MarkAt,
    /// Drawn as plain text by the mark.
    pub note: Option<String>,
}

impl AgentMark {
    /// Where the mark is, world `[x, z]`, with the height of what it marks
    /// when that is known (a camera or a wall target): `None` once the item
    /// it is on has gone from the document.
    pub fn anchor(&self, doc: &Document) -> Option<([f32; 2], Option<f32>)> {
        match self.at {
            MarkAt::Camera(id) => doc
                .camera(id)
                .map(|c| ([c.position[0], c.position[2]], Some(c.position[1]))),
            MarkAt::Target(id) => {
                doc.targets
                    .iter()
                    .find(|t| t.id == id)
                    .and_then(|t| match &t.at {
                        TargetPlace::Ground { points } => centre(points).map(|c| (c, None)),
                        TargetPlace::Wall { point, .. } => {
                            Some(([point[0], point[2]], Some(point[1])))
                        }
                    })
            }
            MarkAt::Area(id) => doc
                .areas
                .iter()
                .find(|a| a.id == id)
                .and_then(|a| centre(&a.points))
                .map(|c| (c, None)),
            MarkAt::Point(p) => Some((p, None)),
        }
    }
}

/// The mean of `points`.
fn centre(points: &[[f32; 2]]) -> Option<[f32; 2]> {
    let n = points.len() as f32;
    (!points.is_empty()).then(|| {
        let (x, z) = points
            .iter()
            .fold((0.0, 0.0), |(x, z), p| (x + p[0], z + p[1]));
        [x / n, z / n]
    })
}

/// The agent asks the user to load an area.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadRequest {
    pub bbox: BBox,
    pub reason: String,
}

impl LoadRequest {
    /// The area in words: its size and centre.
    pub fn area_words(&self) -> String {
        let c = self.bbox.center();
        format!(
            "an area {:.0} × {:.0} m around {:.5}, {:.5}",
            self.bbox.width_m(),
            self.bbox.height_m(),
            c.lat,
            c.lon
        )
    }

    /// What the card says.
    pub fn text(&self) -> String {
        format!(
            "The agent asks to load {} because {}",
            self.area_words(),
            self.reason
        )
    }
}

/// What the client carries out for a command.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewAction {
    /// Look at world `[x, z]` from `distance_m` away (else from as far as
    /// now, at most as far as framing a camera does).
    Focus {
        at: [f32; 2],
        distance_m: Option<f32>,
    },
    /// Select this camera, or nothing.
    Select(Option<u32>),
    /// The view toggles to change; `None` leaves one as it is.
    Panels {
        overlay: Option<OverlayMode>,
        cones: Option<bool>,
        walls: Option<bool>,
    },
}

/// What a command comes to.
#[derive(Debug, Clone, PartialEq)]
pub enum Planned {
    /// What the client carries out, and the toast that says so.
    Act(ViewAction, String),
    /// The mark to draw (`None`: remove it), and the toast.
    Mark(Option<AgentMark>, String),
    /// A request for the user to decide on.
    AskToLoad(LoadRequest),
    /// A kind of command this build does not know (from a newer agent):
    /// skipped.
    Unknown,
}

/// A camera of the document in words: "camera 3 ‘Porch’".
fn camera_words(doc: &Document, id: u32) -> String {
    item_words("camera", id, doc.camera(id).map(|c| c.label.as_str()))
}

/// The id and words of the item `r` names among `items`, or why not.
fn find<'a>(
    items: impl Iterator<Item = (u32, Option<&'a str>)>,
    r: &ItemRef,
    what: &str,
) -> Result<(u32, String), String> {
    find_item(items, r, what).ok_or_else(|| {
        let named = match r {
            ItemRef::Id(id) => id.to_string(),
            ItemRef::Name(n) => format!("‘{n}’"),
        };
        format!("The agent asked to mark {what} {named}, which is not in the scene.")
    })
}

/// Whether `command` holds a kind this build does not know.
fn unknown(command: &AgentCommand) -> bool {
    match command {
        AgentCommand::Unknown => true,
        AgentCommand::Highlight { what, .. } => *what == Highlight::Unknown,
        AgentCommand::SetViewOptions(o) => o.overlay == Some(OverlayColours::Unknown),
        _ => false,
    }
}

/// What `command` does in the app with document `doc`, or why it does
/// nothing (worded for a toast).
pub fn plan(command: AgentCommand, doc: &Document) -> Result<Planned, String> {
    if unknown(&command) {
        return Ok(Planned::Unknown);
    }
    let command = command
        .checked()
        .map_err(|why| format!("The agent sent a command the app could not use: {why}."))?;
    Ok(match command {
        AgentCommand::FocusView { at, distance_m } => Planned::Act(
            ViewAction::Focus { at, distance_m },
            "Agent moved the view".to_owned(),
        ),
        AgentCommand::SelectCameras { ids } => {
            if ids.is_empty() {
                Planned::Act(
                    ViewAction::Select(None),
                    "Agent cleared the selection".to_owned(),
                )
            } else {
                // One camera at a time: the first that is in the scene.
                let id = ids
                    .iter()
                    .copied()
                    .find(|id| doc.camera(*id).is_some())
                    .ok_or_else(|| {
                        format!(
                            "The agent asked to select camera {}, which is not in the scene.",
                            ids[0]
                        )
                    })?;
                Planned::Act(
                    ViewAction::Select(Some(id)),
                    format!("Agent selected {}", camera_words(doc, id)),
                )
            }
        }
        AgentCommand::Highlight { what, note } => {
            let (at, words) = match what {
                Highlight::Clear => {
                    return Ok(Planned::Mark(None, "Agent removed its mark".to_owned()));
                }
                Highlight::Camera { id } => {
                    if doc.camera(id).is_none() {
                        return Err(format!(
                            "The agent asked to mark camera {id}, which is not in the scene."
                        ));
                    }
                    (MarkAt::Camera(id), camera_words(doc, id))
                }
                Highlight::Target { item } => {
                    let items = doc.targets.iter().map(|t| (t.id, Some(t.name.as_str())));
                    let (id, words) = find(items, &item, "target")?;
                    (MarkAt::Target(id), words)
                }
                Highlight::Area { item } => {
                    let items = doc.areas.iter().map(|a| (a.id, a.name.as_deref()));
                    let (id, words) = find(items, &item, "area")?;
                    (MarkAt::Area(id), words)
                }
                Highlight::Point { at } => (MarkAt::Point(at), "a spot".to_owned()),
                // Taken as unknown above.
                Highlight::Unknown => return Ok(Planned::Unknown),
            };
            Planned::Mark(
                Some(AgentMark { at, note }),
                format!("Agent marked {words}"),
            )
        }
        AgentCommand::SetViewOptions(o) => {
            let overlay = o.overlay.and_then(|c| match c {
                OverlayColours::Overlap => Some(OverlayMode::Overlap),
                OverlayColours::PerCamera => Some(OverlayMode::PerCamera),
                OverlayColours::Unknown => None,
            });
            let mut said = Vec::new();
            match overlay {
                Some(OverlayMode::Overlap) => said.push("overlap colours"),
                Some(OverlayMode::PerCamera) => said.push("a colour per camera"),
                None => {}
            }
            if let Some(c) = o.cones {
                said.push(if c {
                    "every cone"
                } else {
                    "only the selected cone"
                });
            }
            if let Some(w) = o.walls {
                said.push(if w {
                    "coverage on walls"
                } else {
                    "no coverage on walls"
                });
            }
            Planned::Act(
                ViewAction::Panels {
                    overlay,
                    cones: o.cones,
                    walls: o.walls,
                },
                format!("Agent set the view to {}", said.join(", ")),
            )
        }
        AgentCommand::RequestLoadArea { bbox, reason } => {
            Planned::AskToLoad(LoadRequest { bbox, reason })
        }
        // Taken as unknown above.
        AgentCommand::Unknown => Planned::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_agent_api::ViewOptions;
    use observation_core::camera::CameraPlacement;
    use observation_core::footage::{LabelledArea, Target};

    /// A document with cameras 1 ‘Porch’ and 2, target 5 ‘Front door’ on a
    /// wall and area 8 ‘Our garden’.
    fn doc() -> Document {
        let mut doc = Document::default();
        for (id, label) in [(1, "Porch"), (2, "")] {
            doc.cameras.push(CameraPlacement {
                id,
                label: label.into(),
                model_id: "m".into(),
                position: [id as f32, 3.0, -6.0],
                yaw_deg: 180.0,
                pitch_deg: -10.0,
                roll_deg: 0.0,
                color: None,
                enabled: true,
                group: None,
            });
        }
        doc.targets.push(Target {
            id: 5,
            name: "Front door".into(),
            at: TargetPlace::Wall {
                point: [4.0, 1.5, 2.0],
                normal: [0.0, 1.0],
            },
        });
        doc.areas.push(LabelledArea {
            id: 8,
            label: None,
            name: Some("Our garden".into()),
            points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            enabled: true,
        });
        doc
    }

    fn mark(what: Highlight) -> AgentCommand {
        AgentCommand::Highlight {
            what,
            note: Some(" seen by\nnobody ".into()),
        }
    }

    #[test]
    fn focus_and_selection_become_actions_with_a_toast() {
        let doc = doc();
        let focus = AgentCommand::FocusView {
            at: [3.0, -4.0],
            distance_m: Some(40.0),
        };
        assert_eq!(
            plan(focus, &doc),
            Ok(Planned::Act(
                ViewAction::Focus {
                    at: [3.0, -4.0],
                    distance_m: Some(40.0)
                },
                "Agent moved the view".into()
            ))
        );
        // One camera at a time: the first that is in the scene.
        let select = AgentCommand::SelectCameras { ids: vec![9, 1, 2] };
        assert_eq!(
            plan(select, &doc),
            Ok(Planned::Act(
                ViewAction::Select(Some(1)),
                "Agent selected camera 1 ‘Porch’".into()
            ))
        );
        let none = AgentCommand::SelectCameras { ids: vec![] };
        assert!(matches!(
            plan(none, &doc),
            Ok(Planned::Act(ViewAction::Select(None), _))
        ));
        let missing = AgentCommand::SelectCameras { ids: vec![9] };
        assert!(plan(missing, &doc).unwrap_err().contains("camera 9"));
    }

    #[test]
    fn marks_find_what_they_name_and_keep_a_tidy_note() {
        let doc = doc();
        let Ok(Planned::Mark(Some(m), notice)) = plan(
            mark(Highlight::Target {
                item: ItemRef::Name("FRONT DOOR".into()),
            }),
            &doc,
        ) else {
            panic!("a mark");
        };
        assert_eq!(m.at, MarkAt::Target(5));
        assert_eq!(m.note.as_deref(), Some("seen by nobody"));
        assert_eq!(notice, "Agent marked target 5 ‘Front door’");
        assert_eq!(m.anchor(&doc), Some(([4.0, 2.0], Some(1.5))));

        let Ok(Planned::Mark(Some(m), _)) = plan(
            mark(Highlight::Area {
                item: ItemRef::Id(8),
            }),
            &doc,
        ) else {
            panic!("a mark");
        };
        assert_eq!(m.anchor(&doc), Some(([5.0, 5.0], None)));
        let Ok(Planned::Mark(Some(m), notice)) = plan(mark(Highlight::Camera { id: 2 }), &doc)
        else {
            panic!("a mark");
        };
        assert_eq!(notice, "Agent marked camera 2");
        assert_eq!(m.anchor(&doc), Some(([2.0, -6.0], Some(3.0))));
        // What is not in the scene is said so; clearing removes the mark.
        let why = plan(
            mark(Highlight::Area {
                item: ItemRef::Name("Their drive".into()),
            }),
            &doc,
        )
        .unwrap_err();
        assert!(why.contains("‘Their drive’"), "{why}");
        assert!(plan(mark(Highlight::Camera { id: 7 }), &doc).is_err());
        assert_eq!(
            plan(mark(Highlight::Clear), &doc),
            Ok(Planned::Mark(None, "Agent removed its mark".into()))
        );
        // A mark on something since removed has nowhere to go.
        let gone = AgentMark {
            at: MarkAt::Camera(7),
            note: None,
        };
        assert_eq!(gone.anchor(&doc), None);
    }

    #[test]
    fn view_options_set_only_the_view_toggles() {
        let doc = doc();
        let options = AgentCommand::SetViewOptions(ViewOptions {
            overlay: Some(OverlayColours::PerCamera),
            cones: Some(true),
            walls: None,
        });
        assert_eq!(
            plan(options, &doc),
            Ok(Planned::Act(
                ViewAction::Panels {
                    overlay: Some(OverlayMode::PerCamera),
                    cones: Some(true),
                    walls: None
                },
                "Agent set the view to a colour per camera, every cone".into()
            ))
        );
    }

    #[test]
    fn a_request_to_load_is_for_the_user_and_bad_commands_are_said_so() {
        let doc = doc();
        let bbox = BBox::new(51.5, -0.13, 51.502, -0.127).unwrap();
        let Ok(Planned::AskToLoad(r)) = plan(
            AgentCommand::RequestLoadArea {
                bbox,
                reason: " it is\nyour street".into(),
            },
            &doc,
        ) else {
            panic!("a request");
        };
        assert_eq!(r.reason, "it is your street");
        assert!(
            r.text().starts_with("The agent asks to load an area "),
            "{}",
            r.text()
        );
        assert!(
            r.text().contains("around 51.50100, -0.12850"),
            "{}",
            r.text()
        );
        assert!(r.text().ends_with("because it is your street"));
        // Checked again here: out of range, unknown.
        let far = AgentCommand::FocusView {
            at: [1e6, 0.0],
            distance_m: None,
        };
        assert!(plan(far, &doc).unwrap_err().contains("could not use"));
        // Kinds from a newer agent are skipped, not refused.
        assert_eq!(plan(AgentCommand::Unknown, &doc), Ok(Planned::Unknown));
        assert_eq!(plan(mark(Highlight::Unknown), &doc), Ok(Planned::Unknown));
        let rainbow = AgentCommand::SetViewOptions(ViewOptions {
            overlay: Some(OverlayColours::Unknown),
            ..ViewOptions::default()
        });
        assert_eq!(plan(rainbow, &doc), Ok(Planned::Unknown));
    }
}
