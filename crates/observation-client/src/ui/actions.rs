//! Camera commands shared by the inspector, the camera list's context menu
//! and the floating toolbar next to the selected camera.

use super::UiData;
use crate::icons;
use crate::state::{Tool, terrain_of};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraAction {
    Select,
    Focus,
    Aim,
    Move,
    Duplicate,
    ToggleEnabled,
    Delete,
}

impl CameraAction {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Select => icons::CURSOR,
            Self::Focus => icons::CROSSHAIR_SIMPLE,
            Self::Aim => icons::CROSSHAIR,
            Self::Move => icons::ARROWS_OUT_CARDINAL,
            Self::Duplicate => icons::COPY,
            Self::ToggleEnabled => icons::EYE,
            Self::Delete => icons::TRASH,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Focus => "Focus",
            Self::Aim => "Aim",
            Self::Move => "Move",
            Self::Duplicate => "Duplicate",
            Self::ToggleEnabled => "Include in coverage",
            Self::Delete => "Delete",
        }
    }

    pub fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::Focus => Some("F"),
            Self::Aim => Some("A"),
            Self::Move => Some("M"),
            Self::Delete => Some("Del"),
            _ => None,
        }
    }
}

/// Runs `action` on camera `id`. Actions other than Delete and
/// ToggleEnabled also select it.
pub fn apply(d: &mut UiData, id: u32, action: CameraAction) {
    let Some(c) = d.scene.camera(id).cloned() else {
        return;
    };
    match action {
        CameraAction::Select => d.ui.select(id),
        CameraAction::Focus => {
            d.ui.select(id);
            d.ui.focus_request = true;
        }
        CameraAction::Aim => {
            d.ui.select(id);
            d.ui.tool = Tool::Aim;
        }
        CameraAction::Move => {
            d.ui.select(id);
            d.ui.tool = Tool::Move;
        }
        CameraAction::Duplicate => {
            let mut copy = c;
            copy.label = String::new();
            // A metre east, at the same height above the ground.
            let [x, _, z] = copy.position;
            copy.position = terrain_of(d.area.as_deref()).moved_to(copy.position, x + 1.0, z);
            let new = d.scene.add_camera(copy);
            d.ui.select(new);
        }
        CameraAction::ToggleEnabled => {
            if let Some(c) = d.scene.camera_mut(id) {
                c.enabled = !c.enabled;
            }
        }
        CameraAction::Delete => {
            d.scene.remove_camera(id);
            if d.ui.selected == Some(id) {
                d.ui.selected = None;
                if d.ui.tool.needs_selection() {
                    d.ui.tool = Tool::Select;
                }
            }
        }
    }
}
