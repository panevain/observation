//! Phosphor icons (MIT), subset at compile time to the glyphs the UI uses.
//!
//! Regular glyphs render inline in any label. Fill glyphs (the active tool)
//! need [`fill::rich`].

use crate::state::Tool;
use bevy_egui::egui::FontDefinitions;
use observation_core::camera::CameraKind;

egui_phosphor::subset! {
    pub mod set {
        use regular::{
            CURSOR, MAP_PIN_PLUS, CROSSHAIR, ARROWS_OUT_CARDINAL, POLYGON,
            TRIANGLE_DASHED, WALL,
            BELL, LIGHTBULB, FLASHLIGHT, SECURITY_CAMERA, WEBCAM, CIRCLE_HALF, HOUSE,
            MAP_TRIFOLD, HOUSE_LINE, UPLOAD_SIMPLE, DOWNLOAD_SIMPLE, QUESTION, GEAR_SIX,
            ARROW_COUNTER_CLOCKWISE, ARROW_CLOCKWISE, EYE, EYE_SLASH, TRASH, COPY,
            DOTS_THREE_VERTICAL, CHECK, WARNING, WARNING_CIRCLE, X, MAGNIFYING_GLASS,
            CLOUD_CHECK, CLOUD_WARNING, INFO, PLUS, KEYBOARD, CROSSHAIR_SIMPLE,
        };
        use fill::{CURSOR, MAP_PIN_PLUS, CROSSHAIR, ARROWS_OUT_CARDINAL, POLYGON};
    }
}

pub use set::fill;
pub use set::regular::*;

/// Registers both icon fonts. Call after the text fonts are in place: the
/// fill family copies the proportional list as it stands.
pub fn install(fonts: &mut FontDefinitions) {
    set::regular::add_to_fonts(fonts);
    set::fill::add_as_family(fonts);
}

/// Glyph for an editing tool (the same codepoint in both weights).
pub fn tool(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => CURSOR,
        Tool::Place => MAP_PIN_PLUS,
        Tool::Aim => CROSSHAIR,
        Tool::Move => ARROWS_OUT_CARDINAL,
        Tool::Region => POLYGON,
    }
}

/// Glyph shown next to a camera of this type.
pub fn kind(kind: CameraKind) -> &'static str {
    match kind {
        CameraKind::Doorbell => BELL,
        CameraKind::Floodlight => LIGHTBULB,
        CameraKind::Spotlight => FLASHLIGHT,
        CameraKind::Bullet => SECURITY_CAMERA,
        CameraKind::Turret => WEBCAM,
        CameraKind::Dome => CIRCLE_HALF,
        CameraKind::Indoor => HOUSE,
    }
}
