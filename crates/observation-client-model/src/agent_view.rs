//! The agent's view commands (see [`observation_agent_api::command`]): what
//! each one does in the app, worked out against the document.
//!
//! [`plan`] checks a command again (the server did, but the browser trusts
//! nothing that came over the wire) and finds what it names in the
//! document: a camera, a target or an area by id or name. It never changes
//! the document. What it returns is either a [`ViewAction`] for the client
//! to carry out (move the 3D view, change the selection, the analysis
//! settings or the view toggles), a mark to draw, or a request to load an
//! area for the user to decide on. Each comes with the words of a toast, so
//! the user always hears what the agent did.
//!
//! Night and the detail level are analysis settings saved in the scene: the
//! client applies them as the Analysis tab does, so they are an undoable
//! edit like any other.

use crate::document::Document;
use crate::editor::OverlayMode;
use observation_agent_api::{AgentCommand, Highlight, ItemRef, OverlayColours};
use observation_core::camera::Lighting;
use observation_core::coverage::CoverageParams;
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
    /// now, at most 60 m).
    Focus {
        at: [f32; 2],
        distance_m: Option<f32>,
    },
    /// Select this camera, or nothing.
    Select(Option<u32>),
    /// The analysis settings to set (as the Analysis tab does).
    Params(CoverageParams),
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
    /// What the client carries out, in order, and the toast that says so.
    Act(Vec<ViewAction>, String),
    /// The mark to draw (`None`: remove it), and the toast.
    Mark(Option<AgentMark>, String),
    /// A request for the user to decide on.
    AskToLoad(LoadRequest),
}

/// A camera's words: "camera 3 ‘Porch’".
fn camera_words(doc: &Document, id: u32) -> String {
    match doc
        .camera(id)
        .map(|c| c.label.trim())
        .filter(|l| !l.is_empty())
    {
        Some(label) => format!("camera {id} ‘{label}’"),
        None => format!("camera {id}"),
    }
}

/// The id and words of the item `r` names among `(id, name)`.
fn find<'a>(
    mut items: impl Iterator<Item = (u32, Option<&'a str>)>,
    r: &ItemRef,
    what: &str,
) -> Result<(u32, String), String> {
    items
        .find(|(id, name)| r.matches(*id, *name))
        .map(
            |(id, name)| match name.map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) => (id, format!("{what} ‘{n}’")),
                None => (id, format!("{what} {id}")),
            },
        )
        .ok_or_else(|| {
            let named = match r {
                ItemRef::Id(id) => id.to_string(),
                ItemRef::Name(n) => format!("‘{n}’"),
            };
            format!("The agent asked to mark {what} {named}, which is not in the scene.")
        })
}

/// What `command` does in the app with document `doc`, or why it does
/// nothing (worded for a toast).
pub fn plan(command: AgentCommand, doc: &Document) -> Result<Planned, String> {
    let command = command
        .checked()
        .map_err(|why| format!("The agent sent a command the app could not use: {why}."))?;
    Ok(match command {
        AgentCommand::FocusView { at, distance_m } => Planned::Act(
            vec![ViewAction::Focus { at, distance_m }],
            "Agent moved the view".to_owned(),
        ),
        AgentCommand::SelectCameras { ids } => {
            if ids.is_empty() {
                Planned::Act(
                    vec![ViewAction::Select(None)],
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
                    vec![ViewAction::Select(Some(id))],
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
                // `checked` refuses it.
                Highlight::Unknown => return Err("The agent sent an unknown mark.".into()),
            };
            Planned::Mark(
                Some(AgentMark { at, note }),
                format!("Agent marked {words}"),
            )
        }
        AgentCommand::SetViewOptions(o) => {
            let mut params = doc.params;
            let mut said = Vec::new();
            if let Some(night) = o.night {
                params.lighting = if night {
                    Lighting::Night
                } else {
                    Lighting::Day
                };
                said.push(if night { "night" } else { "day" }.to_owned());
            }
            if let Some(level) = o.detail_level.and_then(|l| l.dori()) {
                params.dori = level;
                said.push(format!("{} detail", level.label()));
            }
            let overlay = o.overlay.and_then(|c| match c {
                OverlayColours::Overlap => Some(OverlayMode::Overlap),
                OverlayColours::PerCamera => Some(OverlayMode::PerCamera),
                OverlayColours::Unknown => None,
            });
            if let Some(mode) = overlay {
                said.push(
                    match mode {
                        OverlayMode::Overlap => "overlap colours",
                        OverlayMode::PerCamera => "a colour per camera",
                    }
                    .to_owned(),
                );
            }
            if let Some(c) = o.cones {
                said.push(
                    if c {
                        "every cone"
                    } else {
                        "only the selected cone"
                    }
                    .to_owned(),
                );
            }
            if let Some(w) = o.walls {
                said.push(
                    if w {
                        "coverage on walls"
                    } else {
                        "no coverage on walls"
                    }
                    .to_owned(),
                );
            }
            let mut actions = Vec::new();
            if o.night.is_some() || o.detail_level.is_some() {
                actions.push(ViewAction::Params(params));
            }
            if overlay.is_some() || o.cones.is_some() || o.walls.is_some() {
                actions.push(ViewAction::Panels {
                    overlay,
                    cones: o.cones,
                    walls: o.walls,
                });
            }
            Planned::Act(
                actions,
                format!("Agent set the view to {}", said.join(", ")),
            )
        }
        AgentCommand::RequestLoadArea { bbox, reason } => {
            Planned::AskToLoad(LoadRequest { bbox, reason })
        }
        // `checked` refuses it.
        AgentCommand::Unknown => return Err("The agent sent an unknown command.".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_agent_api::{DetailLevel, ViewOptions};
    use observation_core::camera::{CameraPlacement, DoriLevel};
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
                vec![ViewAction::Focus {
                    at: [3.0, -4.0],
                    distance_m: Some(40.0)
                }],
                "Agent moved the view".into()
            ))
        );
        // One camera at a time: the first that is in the scene.
        let select = AgentCommand::SelectCameras { ids: vec![9, 1, 2] };
        assert_eq!(
            plan(select, &doc),
            Ok(Planned::Act(
                vec![ViewAction::Select(Some(1))],
                "Agent selected camera 1 ‘Porch’".into()
            ))
        );
        let none = AgentCommand::SelectCameras { ids: vec![] };
        assert!(matches!(
            plan(none, &doc),
            Ok(Planned::Act(a, _)) if a == [ViewAction::Select(None)]
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
        assert_eq!(notice, "Agent marked target ‘Front door’");
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
    fn view_options_set_the_analysis_settings_and_the_toggles() {
        let doc = doc();
        let both = AgentCommand::SetViewOptions(ViewOptions {
            night: Some(true),
            detail_level: Some(DetailLevel::Recognise),
            overlay: Some(OverlayColours::PerCamera),
            cones: Some(true),
            walls: None,
        });
        let Ok(Planned::Act(actions, notice)) = plan(both, &doc) else {
            panic!("actions");
        };
        let mut params = doc.params;
        params.lighting = Lighting::Night;
        params.dori = DoriLevel::Recognise;
        assert_eq!(
            actions,
            [
                ViewAction::Params(params),
                ViewAction::Panels {
                    overlay: Some(OverlayMode::PerCamera),
                    cones: Some(true),
                    walls: None
                }
            ]
        );
        assert_eq!(
            notice,
            "Agent set the view to night, Recognise detail, a colour per camera, every cone"
        );
        let walls = AgentCommand::SetViewOptions(ViewOptions {
            walls: Some(false),
            ..ViewOptions::default()
        });
        assert!(matches!(
            plan(walls, &doc),
            Ok(Planned::Act(a, _)) if a.len() == 1 && matches!(a[0], ViewAction::Panels { .. })
        ));
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
        assert!(plan(AgentCommand::Unknown, &doc).is_err());
    }
}
