//! What the user is editing with (the active tool, the selection, the model
//! the Place tool places, the outline being drawn, the area or target picked
//! and the address picked), which panels are open, and what the pointer is
//! over.

use crate::document::Models;
use observation_core::address::Address;
use observation_core::evidence::Tier;

/// Editing tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Place,
    Aim,
    Move,
    /// Draw labelled areas (and the one the figures are limited to).
    Areas,
    /// Mark a named target: a patch of wall or of ground.
    Target,
    /// Show the street address under the pointer; click to select it.
    Address,
}

impl Tool {
    pub const ALL: [Self; 7] = [
        Self::Select,
        Self::Place,
        Self::Aim,
        Self::Move,
        Self::Areas,
        Self::Target,
        Self::Address,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Place => "Place",
            Self::Aim => "Aim",
            Self::Move => "Move",
            Self::Areas => "Areas",
            Self::Target => "Target",
            Self::Address => "Address",
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Select => "V",
            Self::Place => "P",
            Self::Aim => "A",
            Self::Move => "M",
            Self::Areas => "R",
            Self::Target => "T",
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
            Self::Areas => {
                "Outline your land, a neighbour's or public ground: click to add corners · click the first corner or press Enter to finish · Backspace undoes a corner"
            }
            Self::Target => {
                "Mark a door, a gate or a path: click a wall or the ground, or outline it (choose under Places)"
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
            Self::Areas => "Click corners of the area, then the first corner",
            Self::Target => "Click a wall or the ground to mark a target",
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
            Self::Areas => &[
                ("Click", "add corner"),
                ("Enter", "finish"),
                ("Backspace", "remove corner"),
                ("Esc", "cancel"),
            ],
            Self::Target => &[
                ("Click wall", "wall target"),
                ("Click ground", "ground target"),
                ("Enter", "finish outline"),
                ("Esc", "done"),
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

/// What finishing the outline drawn with the Areas tool does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AreaDraw {
    /// Adds a new unlabelled area.
    #[default]
    New,
    /// Gives the area with this id the new outline, keeping its label, name
    /// and switch.
    Redraw(u32),
    /// Gives the area the figures are limited to the new outline, or adds
    /// one to limit them to (the Coverage card's menu, as the single region
    /// did before areas had labels).
    Limit,
}

/// How the Target tool marks a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetShape {
    /// A click on a wall marks a patch of it; on the ground, a small square.
    #[default]
    Point,
    /// Corners clicked on the ground, closed like an area.
    Outline,
}

/// The editing state: which tool is active and what it works on.
#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    pub tool: Tool,
    pub selected: Option<u32>,
    /// Model used by the Place tool.
    pub place_model: String,
    /// Corners placed so far of the outline being drawn (with the Areas
    /// tool, or the Target tool's outline), world `[x, z]`.
    pub region_draft: Vec<[f32; 2]>,
    /// Ground point under the pointer while drawing an outline.
    pub region_hover: Option<[f32; 2]>,
    /// How close to the first corner a click closes the outline, metres.
    pub region_snap_m: f32,
    /// What finishing an outline with the Areas tool does.
    pub area_draw: AreaDraw,
    /// How the Target tool marks a target.
    pub target_shape: TargetShape,
    /// The area picked in the Places tab, highlighted in the 3D view.
    pub selected_area: Option<u32>,
    /// The target picked in the Places tab, highlighted in the 3D view.
    pub selected_target: Option<u32>,
    /// The address picked with the Address tool; its buildings are
    /// highlighted.
    pub selected_address: Option<Address>,
    /// One-shot request: frame the selected camera in the 3D view. Consumed
    /// by the input systems the next time they read the keyboard.
    pub focus_request: bool,
    /// One-shot request: scroll the camera list to the picked address's group
    /// once. Consumed by the inspector; an area load drops it.
    pub jump_to_address: bool,
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
            area_draw: AreaDraw::New,
            target_shape: TargetShape::Point,
            selected_area: None,
            selected_target: None,
            selected_address: None,
            focus_request: false,
            jump_to_address: false,
        }
    }
}

impl Editor {
    /// Switches to `tool`. Leaving a tool drops the outline it was drawing
    /// (and what finishing it would have done).
    pub fn set_tool(&mut self, tool: Tool) {
        if self.tool != tool {
            self.region_draft.clear();
            self.region_hover = None;
            self.area_draw = AreaDraw::New;
        }
        self.tool = tool;
    }

    /// Whether the active tool draws an outline on the ground corner by
    /// corner (Enter closes it, Backspace removes a corner).
    pub fn draws_outline(&self) -> bool {
        match self.tool {
            Tool::Areas => true,
            Tool::Target => self.target_shape == TargetShape::Outline,
            _ => false,
        }
    }

    /// Drops the picked area and target if the document no longer has them
    /// (deleted, undone, or another scene or area loaded), and a redraw of an
    /// area that is gone.
    pub fn forget_missing_places(&mut self, doc: &crate::document::Document) {
        let has_area = |id: Option<u32>| id.is_some_and(|id| doc.areas.iter().any(|a| a.id == id));
        if self.selected_area.is_some() && !has_area(self.selected_area) {
            self.selected_area = None;
        }
        if let AreaDraw::Redraw(id) = self.area_draw
            && !has_area(Some(id))
        {
            self.area_draw = AreaDraw::New;
        }
        if self
            .selected_target
            .is_some_and(|id| !doc.targets.iter().any(|t| t.id == id))
        {
            self.selected_target = None;
        }
    }

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
    /// height), how the overlay colours the ground, and where every camera's
    /// footage lands.
    Analysis,
    /// The labelled areas and the targets, with each target's verdicts.
    Places,
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
    /// The Scenes dialog (the scenes saved in this browser).
    pub scenes_open: bool,
    pub custom_model_open: bool,
    /// The Evidence sheet dialog, and the tier chosen in it (Aggregate
    /// unless the user picks Exact; not remembered across visits).
    pub sheet_open: bool,
    pub sheet_tier: Tier,
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
            scenes_open: false,
            custom_model_open: false,
            sheet_open: false,
            sheet_tier: Tier::default(),
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
                area_draw: AreaDraw::New,
                target_shape: TargetShape::Point,
                selected_area: None,
                selected_target: None,
                selected_address: None,
                focus_request: false,
                jump_to_address: false,
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
            Tool::Areas,
            Tool::Target,
            Tool::Address,
        ]
        .into_iter()
        .filter(|t| t.needs_selection())
        .collect();
        assert_eq!(needs, [Tool::Aim, Tool::Move]);
    }

    #[test]
    fn the_outline_tools_and_leaving_them() {
        let mut editor = Editor::default();
        assert!(!editor.draws_outline());
        editor.set_tool(Tool::Areas);
        assert!(editor.draws_outline());
        editor.set_tool(Tool::Target);
        assert!(!editor.draws_outline(), "a click marks a point");
        editor.target_shape = TargetShape::Outline;
        assert!(editor.draws_outline());
        // Leaving a tool drops its outline and what finishing it would do;
        // picking the same tool again keeps them.
        editor.set_tool(Tool::Areas);
        editor.region_draft = vec![[0.0, 0.0]];
        editor.area_draw = AreaDraw::Redraw(3);
        editor.set_tool(Tool::Areas);
        assert_eq!(editor.region_draft.len(), 1);
        editor.set_tool(Tool::Select);
        assert!(editor.region_draft.is_empty());
        assert_eq!(editor.area_draw, AreaDraw::New);
    }

    #[test]
    fn a_picked_area_or_target_that_is_gone_is_forgotten() {
        use observation_core::footage::{LabelledArea, Target, TargetPlace};
        let mut doc = crate::document::Document::default();
        doc.areas.push(LabelledArea {
            id: 2,
            label: None,
            name: None,
            points: Vec::new(),
            enabled: true,
        });
        doc.targets.push(Target {
            id: 5,
            name: String::new(),
            at: TargetPlace::Wall {
                point: [0.0; 3],
                normal: [1.0, 0.0],
            },
        });
        let mut editor = Editor {
            selected_area: Some(2),
            selected_target: Some(5),
            area_draw: AreaDraw::Redraw(2),
            ..Editor::default()
        };
        editor.forget_missing_places(&doc);
        assert_eq!(
            (
                editor.selected_area,
                editor.selected_target,
                editor.area_draw
            ),
            (Some(2), Some(5), AreaDraw::Redraw(2))
        );
        doc.areas.clear();
        doc.targets.clear();
        editor.forget_missing_places(&doc);
        assert_eq!(
            (
                editor.selected_area,
                editor.selected_target,
                editor.area_draw
            ),
            (None, None, AreaDraw::New)
        );
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
    fn nothing_starts_hovered() {
        assert_eq!(
            Hover::default(),
            Hover {
                hover_list: None,
                hover_scene: None,
                hover_address: None,
            }
        );
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
        assert!(
            !(p.map_open || p.help_open || p.regions_open || p.scenes_open || p.custom_model_open)
        );
        assert_eq!(
            (p.overlay, p.inspector_tab, p.inspector_seen),
            (OverlayMode::Overlap, InspectorTab::Cameras, None)
        );
    }
}
