//! Camera commands shared by the inspector, the camera list's context menu
//! and the floating toolbar next to the selected camera.

use super::UiData;
use crate::icons;
use crate::state::{TrackScene, terrain_of};
pub use observation_client_model::commands::CameraAction;
use observation_client_model::commands::{self, EditCtx};

/// The icon on a camera command's button.
pub fn icon(action: CameraAction) -> &'static str {
    match action {
        CameraAction::Select => icons::CURSOR,
        CameraAction::Focus => icons::CROSSHAIR_SIMPLE,
        CameraAction::Aim => icons::CROSSHAIR,
        CameraAction::Move => icons::ARROWS_OUT_CARDINAL,
        CameraAction::Duplicate => icons::COPY,
        CameraAction::ToggleEnabled => icons::EYE,
        CameraAction::Delete => icons::TRASH,
    }
}

/// Runs `action` on camera `id`. Actions other than Delete and
/// ToggleEnabled also select it.
pub fn apply(d: &mut UiData, id: u32, action: CameraAction) {
    let mut ctx = EditCtx {
        doc: &mut TrackScene(&mut d.scene),
        editor: &mut d.editor,
        panels: &mut d.panels,
        toasts: &mut d.toasts,
    };
    if commands::camera_action(&mut ctx, terrain_of(d.area.as_deref()), id, action) {
        d.ui.focus_request = true;
    }
}
