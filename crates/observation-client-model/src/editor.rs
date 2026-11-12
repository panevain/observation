//! What the user is editing with (the active tool, the selection, the model
//! the Place tool places, the region being drawn and the address picked),
//! which panels are open, and what the pointer is over.

use crate::document::Models;
use observation_core::address::Address;

/// Editing tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Place,
    Aim,
    Move,
    /// Draw the coverage region.
    Region,
    /// Show the street address under the pointer; click to select it.
    Address,
}

impl Tool {
    pub const ALL: [Self; 6] = [
        Self::Select,
        Self::Place,
        Self::Aim,
        Self::Move,
        Self::Region,
        Self::Address,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Place => "Place",
            Self::Aim => "Aim",
            Self::Move => "Move",
            Self::Region => "Region",
            Self::Address => "Address",
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Select => "V",
            Self::Place => "P",
            Self::Aim => "A",
            Self::Move => "M",
            Self::Region => "R",
            Self::Address => "L",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Select => {
                "Click a camera to select it · drag to orbit · right-drag to pan · scroll to zoom"
            }
            Self::Place => {
                "Click a wall to mount a camera there, or the ground to place it on a pole"
            }
            Self::Aim => "Click or drag on the scene to point the selected camera",
            Self::Move => "Click or drag on a wall or the ground to move the selected camera",
            Self::Region => {
                "Click to add corners · click the first corner or press Enter to finish · Backspace undoes a corner"
            }
            Self::Address => {
                "Point at a building to see its address · click to select it and the buildings at it"
            }
        }
    }

    /// Short instruction for the mode pill.
    pub fn banner(self) -> &'static str {
        match self {
            Self::Select => "Click a camera to select it",
            Self::Place => "Click a wall or the ground to place a camera",
            Self::Aim => "Click where the selected camera should look",
            Self::Move => "Click a wall or the ground to move the selected camera",
            Self::Region => "Click corners of the area to measure, then the first corner",
            Self::Address => "Point at a building to see its address",
        }
    }

    /// Keys and gestures for the status bar: (key, what it does).
    pub fn keymap(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Select => &[
                ("Click", "select"),
                ("Drag", "orbit"),
                ("Right-drag", "pan"),
                ("Scroll", "zoom"),
                ("F", "focus"),
                ("Del", "delete"),
            ],
            Self::Place => &[
                ("Click wall", "mount"),
                ("Click ground", "pole"),
                ("Esc", "done"),
            ],
            Self::Aim => &[
                ("Click/drag", "point camera"),
                ("Right-drag", "pan"),
                ("Esc", "done"),
            ],
            Self::Move => &[
                ("Click/drag", "move camera"),
                ("Right-drag", "pan"),
                ("Esc", "done"),
            ],
            Self::Region => &[
                ("Click", "add corner"),
                ("Enter", "finish"),
                ("Backspace", "remove corner"),
                ("Esc", "cancel"),
            ],
            Self::Address => &[
                ("Click", "select address"),
                ("Drag", "orbit"),
                ("Right-drag", "pan"),
                ("Del", "clear"),
                ("Esc", "done"),
            ],
        }
    }

    pub fn needs_selection(self) -> bool {
        matches!(self, Self::Aim | Self::Move)
    }
}

/// The editing state: which tool is active and what it works on.
#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    pub tool: Tool,
    pub selected: Option<u32>,
    /// Model used by the Place tool.
    pub place_model: String,
    /// Corners placed so far with the Region tool, world `[x, z]`.
    pub region_draft: Vec<[f32; 2]>,
    /// Ground point under the pointer while drawing a region.
    pub region_hover: Option<[f32; 2]>,
    /// How close to the first corner a click closes the region, metres.
    pub region_snap_m: f32,
    /// The address picked with the Address tool; its buildings are
    /// highlighted.
    pub selected_address: Option<Address>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            tool: Tool::Select,
            selected: None,
            place_model: "generic-doorbell-160".to_owned(),
            region_draft: Vec::new(),
            region_hover: None,
            region_snap_m: 1.0,
            selected_address: None,
        }
    }
}

impl Editor {
    /// Goes back to the default model for the Place tool if its model is
    /// gone (deleted, undone, or left out of a restored or imported scene).
    pub fn forget_missing_place_model(&mut self, models: &Models) {
        if models.get(&self.place_model).is_none() {
            self.place_model = Self::default().place_model;
        }
    }
}

/// How the coverage overlay is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayMode {
    /// Heat ramp by number of cameras that see a spot.
    #[default]
    Overlap,
    /// Each camera's own colour.
    PerCamera,
}

/// The inspector's tabs below the coverage summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InspectorTab {
    /// Model picker, Place, the camera list and custom models.
    #[default]
    Cameras,
    /// The selected camera's settings.
    Camera,
    /// What the coverage figures count (detail level, lighting, subject
    /// height) and how the overlay colours the ground.
    Analysis,
}

/// The panels and dialogs that are open, and how the scene is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Panels {
    pub overlay: OverlayMode,
    pub show_facades: bool,
    pub show_frustums: bool,
    pub map_open: bool,
    pub help_open: bool,
    /// The local map-data regions dialog.
    pub regions_open: bool,
    pub custom_model_open: bool,
    pub inspector_tab: InspectorTab,
    /// The selection the inspector last saw, so it can open the Camera tab
    /// when the selection changes some other way than [`select`].
    pub inspector_seen: Option<u32>,
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            overlay: OverlayMode::Overlap,
            show_facades: true,
            show_frustums: true,
            map_open: false,
            help_open: false,
            regions_open: false,
            custom_model_open: false,
            inspector_tab: InspectorTab::Cameras,
            inspector_seen: None,
        }
    }
}

/// Selects a camera and shows its settings in the inspector, even when it
/// was already selected.
pub fn select(editor: &mut Editor, panels: &mut Panels, id: u32) {
    editor.selected = Some(id);
    panels.inspector_tab = InspectorTab::Camera;
}

/// What the pointer is over.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hover {
    /// Camera under the pointer in the camera list.
    pub hover_list: Option<u32>,
    /// Camera under the pointer in the 3D view.
    pub hover_scene: Option<u32>,
    /// What the Address tool shows by the pointer: `None` off the scene,
    /// `Some(None)` where there is no address.
    pub hover_address: Option<Option<String>>,
}

impl Hover {
    /// The camera to highlight: hovered in the list or in the 3D view.
    pub fn hovered(&self) -> Option<u32> {
        self.hover_list.or(self.hover_scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::camera::{CameraKind, CameraModel, Catalog};

    #[test]
    fn the_editor_starts_on_select_with_nothing_picked() {
        assert_eq!(
            Editor::default(),
            Editor {
                tool: Tool::Select,
                selected: None,
                place_model: "generic-doorbell-160".to_owned(),
                region_draft: vec![],
                region_hover: None,
                region_snap_m: 1.0,
                selected_address: None,
            }
        );
    }

    #[test]
    fn a_missing_place_model_falls_back_to_the_default() {
        let mut models = Models::new(Catalog::default());
        let mut editor = Editor {
            place_model: "custom-1-gone".to_owned(),
            ..Editor::default()
        };
        editor.forget_missing_place_model(&models);
        assert_eq!(editor.place_model, "generic-doorbell-160");

        models.custom.push(CameraModel {
            id: "custom-2-kept".to_owned(),
            vendor: String::new(),
            name: "Kept".to_owned(),
            kind: CameraKind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 50.0,
            resolution: [1920, 1080],
            ir_range_m: None,
            source: None,
            notes: None,
        });
        editor.place_model = "custom-2-kept".to_owned();
        editor.forget_missing_place_model(&models);
        assert_eq!(editor.place_model, "custom-2-kept");
    }

    #[test]
    fn aim_and_move_need_a_selection() {
        let needs: Vec<Tool> = [
            Tool::Select,
            Tool::Place,
            Tool::Aim,
            Tool::Move,
            Tool::Region,
            Tool::Address,
        ]
        .into_iter()
        .filter(|t| t.needs_selection())
        .collect();
        assert_eq!(needs, [Tool::Aim, Tool::Move]);
    }

    #[test]
    fn selecting_opens_the_camera_tab_even_when_already_selected() {
        let mut editor = Editor::default();
        let mut panels = Panels::default();
        select(&mut editor, &mut panels, 3);
        assert_eq!(
            (editor.selected, panels.inspector_tab),
            (Some(3), InspectorTab::Camera)
        );
        panels.inspector_tab = InspectorTab::Analysis;
        select(&mut editor, &mut panels, 3);
        assert_eq!(panels.inspector_tab, InspectorTab::Camera);
    }

    #[test]
    fn the_list_hover_wins_over_the_scene_hover() {
        let mut hover = Hover::default();
        assert_eq!(hover.hovered(), None);
        hover.hover_scene = Some(2);
        assert_eq!(hover.hovered(), Some(2));
        hover.hover_list = Some(5);
        assert_eq!(hover.hovered(), Some(5));
    }

    #[test]
    fn panels_start_with_both_overlays_shown_and_the_camera_list() {
        let p = Panels::default();
        assert!(p.show_facades && p.show_frustums);
        assert!(!(p.map_open || p.help_open || p.regions_open || p.custom_model_open));
        assert_eq!(
            (p.overlay, p.inspector_tab, p.inspector_seen),
            (OverlayMode::Overlap, InspectorTab::Cameras, None)
        );
    }
}
