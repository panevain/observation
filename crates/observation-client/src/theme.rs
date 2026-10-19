//! Design tokens: the single source of colours, spacing, radii and type scale.
//!
//! No other module may contain colour literals (enforced in CI by
//! `scripts/check-design-tokens.sh`). The same values are mirrored as CSS
//! custom properties in `web/index.html` for the loading screen and the
//! small-viewport gate.

use bevy::prelude::*;
use bevy_egui::egui::{self, Color32, CornerRadius, FontId, Margin, Stroke, TextStyle};
use observation_core::camera::CameraKind;

/// Smallest supported viewport (CSS px). Below this the app shows a gate.
pub const MIN_VIEWPORT: Vec2 = Vec2::new(768.0, 600.0);

/// Spacing scale, px.
#[derive(Debug, Clone, Copy)]
pub struct Space {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
}

/// Corner radii, px.
#[derive(Debug, Clone, Copy)]
pub struct Radius {
    pub sm: u8,
    pub md: u8,
    pub lg: u8,
}

/// Type scale, px.
#[derive(Debug, Clone, Copy)]
pub struct TypeScale {
    pub caption: f32,
    pub body: f32,
    pub title: f32,
    pub heading: f32,
}

/// Layout sizes, px.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// Minimum interactive target (touch friendly).
    pub hit_target: f32,
    pub toolbar_width: f32,
    pub inspector_width: f32,
    pub topbar_height: f32,
    pub statusbar_height: f32,
}

/// Colour roles for the 3D scene (sRGB, straight alpha).
#[derive(Debug, Clone, Copy)]
pub struct WorldColors {
    pub sky: Color32,
    pub ground: Color32,
    pub building: Color32,
    pub selection: Color32,
    pub aim_target: Color32,
}

/// Colour coding of the editing tools (banner, tool rail).
#[derive(Debug, Clone, Copy)]
pub struct ToolColors {
    pub select: Color32,
    pub place: Color32,
    pub aim: Color32,
    pub move_: Color32,
    /// Also the colour of the region outline in the scene.
    pub region: Color32,
    /// Text on any of the above.
    pub on_tool: Color32,
}

#[derive(Debug, Clone, Copy, Resource)]
pub struct Tokens {
    pub dark: bool,
    // UI colour roles
    pub bg: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub surface_sunken: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub scrim: Color32,
    /// Quick picks offered for camera colours.
    pub camera_palette: [Color32; 8],
    /// Default camera colour per type, in [`CameraKind::ALL`] order.
    pub kind_colors: [Color32; 7],
    pub tools: ToolColors,
    /// Overlap ramp: seen by 1, 2, 3, 4+ cameras.
    pub coverage_ramp: [Color32; 4],
    /// Bounding box colour in the map picker.
    pub selection_box: Color32,
    pub world: WorldColors,
    pub space: Space,
    pub radius: Radius,
    pub font: TypeScale,
    pub layout: Layout,
    pub stroke_width: f32,
}

const SPACE: Space = Space {
    xs: 4.0,
    sm: 8.0,
    md: 12.0,
    lg: 16.0,
    xl: 24.0,
};
const RADIUS: Radius = Radius {
    sm: 4,
    md: 8,
    lg: 12,
};
const FONT: TypeScale = TypeScale {
    caption: 12.0,
    body: 14.0,
    title: 16.0,
    heading: 20.0,
};
const LAYOUT: Layout = Layout {
    hit_target: 32.0,
    toolbar_width: 76.0,
    inspector_width: 320.0,
    topbar_height: 48.0,
    statusbar_height: 28.0,
};

const CAMERA_PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x4E, 0x79, 0xA7),
    Color32::from_rgb(0xF2, 0x8E, 0x2B),
    Color32::from_rgb(0x59, 0xA1, 0x4F),
    Color32::from_rgb(0xE1, 0x57, 0x59),
    Color32::from_rgb(0xB0, 0x7A, 0xA1),
    Color32::from_rgb(0x76, 0xB7, 0xB2),
    Color32::from_rgb(0xED, 0xC9, 0x48),
    Color32::from_rgb(0xFF, 0x9D, 0xA7),
];

/// Doorbell, floodlight, spotlight, bullet, turret, dome, indoor. Kept clear of
/// the yellow-to-purple coverage ramp so cones stand out from the overlay.
const KIND_COLORS: [Color32; 7] = [
    Color32::from_rgb(0x4E, 0x79, 0xA7),
    Color32::from_rgb(0x59, 0xA1, 0x4F),
    Color32::from_rgb(0xF2, 0x8E, 0x2B),
    Color32::from_rgb(0xE1, 0x57, 0x59),
    Color32::from_rgb(0x76, 0xB7, 0xB2),
    Color32::from_rgb(0xB0, 0x7A, 0xA1),
    Color32::from_rgb(0xFF, 0x9D, 0xA7),
];

const TOOL_COLORS: ToolColors = ToolColors {
    select: Color32::from_rgb(0x3B, 0x5B, 0xDB),
    place: Color32::from_rgb(0x2B, 0x8A, 0x3E),
    aim: Color32::from_rgb(0xD9, 0x48, 0x0F),
    move_: Color32::from_rgb(0x84, 0x3D, 0xC9),
    region: Color32::from_rgb(0x0C, 0x85, 0x99),
    on_tool: Color32::from_rgb(0xFF, 0xFF, 0xFF),
};

const COVERAGE_RAMP: [Color32; 4] = [
    Color32::from_rgba_unmultiplied_const(0xF5, 0xC5, 0x42, 140),
    Color32::from_rgba_unmultiplied_const(0xF2, 0x8C, 0x28, 165),
    Color32::from_rgba_unmultiplied_const(0xE0, 0x45, 0x3A, 185),
    Color32::from_rgba_unmultiplied_const(0x9B, 0x2C, 0x8C, 205),
];

impl Tokens {
    pub fn light() -> Self {
        Self {
            dark: false,
            bg: Color32::from_rgb(0xF4, 0xF5, 0xF7),
            surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            surface_raised: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            surface_sunken: Color32::from_rgb(0xEC, 0xEE, 0xF1),
            border: Color32::from_rgb(0xD9, 0xDC, 0xE1),
            text: Color32::from_rgb(0x1B, 0x1F, 0x24),
            text_muted: Color32::from_rgb(0x5F, 0x67, 0x72),
            accent: Color32::from_rgb(0x3B, 0x5B, 0xDB),
            on_accent: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            danger: Color32::from_rgb(0xC9, 0x2A, 0x2A),
            warning: Color32::from_rgb(0xB2, 0x6B, 0x00),
            scrim: Color32::from_rgba_unmultiplied_const(0x10, 0x14, 0x1A, 150),
            camera_palette: CAMERA_PALETTE,
            kind_colors: KIND_COLORS,
            tools: TOOL_COLORS,
            coverage_ramp: COVERAGE_RAMP,
            selection_box: Color32::from_rgb(0x3B, 0x5B, 0xDB),
            world: WorldColors {
                sky: Color32::from_rgb(0xE9, 0xED, 0xF2),
                ground: Color32::from_rgb(0xD8, 0xD6, 0xCF),
                building: Color32::from_rgb(0xF0, 0xEE, 0xE9),
                selection: Color32::from_rgb(0x3B, 0x5B, 0xDB),
                aim_target: Color32::from_rgb(0xE0, 0x45, 0x3A),
            },
            space: SPACE,
            radius: RADIUS,
            font: FONT,
            layout: LAYOUT,
            stroke_width: 1.0,
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: Color32::from_rgb(0x12, 0x14, 0x17),
            surface: Color32::from_rgb(0x1B, 0x1E, 0x23),
            surface_raised: Color32::from_rgb(0x24, 0x28, 0x2E),
            surface_sunken: Color32::from_rgb(0x15, 0x17, 0x1B),
            border: Color32::from_rgb(0x33, 0x38, 0x40),
            text: Color32::from_rgb(0xE8, 0xEA, 0xED),
            text_muted: Color32::from_rgb(0x9A, 0xA2, 0xAD),
            accent: Color32::from_rgb(0x7C, 0x95, 0xF2),
            on_accent: Color32::from_rgb(0x0E, 0x12, 0x1E),
            danger: Color32::from_rgb(0xF0, 0x6A, 0x6A),
            warning: Color32::from_rgb(0xF0, 0xB0, 0x4A),
            scrim: Color32::from_rgba_unmultiplied_const(0x00, 0x00, 0x00, 170),
            camera_palette: CAMERA_PALETTE,
            kind_colors: KIND_COLORS,
            tools: TOOL_COLORS,
            coverage_ramp: COVERAGE_RAMP,
            selection_box: Color32::from_rgb(0x7C, 0x95, 0xF2),
            world: WorldColors {
                sky: Color32::from_rgb(0x1A, 0x1D, 0x22),
                ground: Color32::from_rgb(0x2E, 0x31, 0x36),
                building: Color32::from_rgb(0xB8, 0xBC, 0xC2),
                selection: Color32::from_rgb(0x7C, 0x95, 0xF2),
                aim_target: Color32::from_rgb(0xF0, 0x6A, 0x6A),
            },
            space: SPACE,
            radius: RADIUS,
            font: FONT,
            layout: LAYOUT,
            stroke_width: 1.0,
        }
    }

    /// Picks light/dark from the browser's `prefers-color-scheme`.
    pub fn from_environment() -> Self {
        if prefers_dark() {
            Self::dark()
        } else {
            Self::light()
        }
    }

    /// Colour coding for an editing tool.
    pub fn tool_color(&self, tool: crate::state::Tool) -> Color32 {
        use crate::state::Tool;
        match tool {
            Tool::Select => self.tools.select,
            Tool::Place => self.tools.place,
            Tool::Aim => self.tools.aim,
            Tool::Move => self.tools.move_,
            Tool::Region => self.tools.region,
        }
    }

    /// Default camera colour for a camera type.
    pub fn kind_color(&self, kind: CameraKind) -> Color32 {
        let i = CameraKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
        self.kind_colors[i % self.kind_colors.len()]
    }

    /// Colour for a cell seen by `count` cameras (`count ≥ 1`).
    pub fn coverage_color(&self, count: u8) -> Color32 {
        self.coverage_ramp[(count.max(1) as usize - 1).min(self.coverage_ramp.len() - 1)]
    }

    /// Applies the tokens to an egui context.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut style = (*ctx.global_style()).clone();
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(self.font.caption)),
            (TextStyle::Body, FontId::proportional(self.font.body)),
            (TextStyle::Button, FontId::proportional(self.font.body)),
            (TextStyle::Heading, FontId::proportional(self.font.heading)),
            (
                TextStyle::Monospace,
                FontId::monospace(self.font.body - 1.0),
            ),
        ]
        .into();
        let sp = &mut style.spacing;
        sp.item_spacing = egui::vec2(self.space.sm, self.space.sm);
        sp.button_padding = egui::vec2(self.space.md, self.space.xs + 2.0);
        sp.interact_size = egui::vec2(self.layout.hit_target, self.layout.hit_target - 4.0);
        sp.window_margin = Margin::same(self.space.lg as i8);
        sp.menu_margin = Margin::same(self.space.sm as i8);
        sp.slider_width = 160.0;
        sp.combo_width = 180.0;
        sp.icon_width = self.font.body + 2.0;

        let v = &mut style.visuals;
        *v = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.override_text_color = Some(self.text);
        v.panel_fill = self.surface;
        v.window_fill = self.surface_raised;
        v.extreme_bg_color = self.surface_sunken;
        v.faint_bg_color = self.surface_sunken;
        v.code_bg_color = self.surface_sunken;
        v.window_stroke = Stroke::new(self.stroke_width, self.border);
        v.window_corner_radius = CornerRadius::same(self.radius.lg);
        v.menu_corner_radius = CornerRadius::same(self.radius.md);
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.accent;
        v.selection.stroke = Stroke::new(self.stroke_width, self.on_accent);
        v.warn_fg_color = self.warning;
        v.error_fg_color = self.danger;
        let r = CornerRadius::same(self.radius.md);
        for (w, fill, stroke) in [
            (&mut v.widgets.noninteractive, self.surface, self.border),
            (&mut v.widgets.inactive, self.surface_sunken, self.border),
            (&mut v.widgets.hovered, self.surface_raised, self.accent),
            (&mut v.widgets.active, self.surface_raised, self.accent),
            (&mut v.widgets.open, self.surface_raised, self.border),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = Stroke::new(self.stroke_width, stroke);
            w.corner_radius = r;
            w.fg_stroke = Stroke::new(self.stroke_width, self.text);
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(self.stroke_width, self.border);
        v.widgets.noninteractive.fg_stroke = Stroke::new(self.stroke_width, self.text_muted);
        ctx.set_global_style(style);
    }

    /// A frame for floating cards/panels.
    pub fn card(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.surface_raised)
            .stroke(Stroke::new(self.stroke_width, self.border))
            .corner_radius(CornerRadius::same(self.radius.lg))
            .inner_margin(Margin::same(self.space.md as i8))
    }

    /// A frame for docked panels.
    pub fn panel(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.surface)
            .stroke(Stroke::new(self.stroke_width, self.border))
            .inner_margin(Margin::same(self.space.md as i8))
    }
}

/// Converts a token colour to a Bevy colour.
pub fn to_bevy(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::srgba_u8(r, g, b, a)
}

/// Untinted image drawing (multiplies texels by 1).
pub const IMAGE_TINT: Color32 = Color32::WHITE;

/// Texel bytes (sRGB, straight alpha) → linear RGBA floats for vertex colours.
pub fn texel_to_linear(c: [u8; 4]) -> [f32; 4] {
    Color::srgba_u8(c[0], c[1], c[2], c[3])
        .to_linear()
        .to_f32_array()
}

/// A user-chosen camera colour (opaque sRGB bytes) as a token colour.
pub fn from_rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Opaque sRGB bytes of a token colour, for storing a user's choice.
pub fn to_rgb(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

/// Straight-alpha RGBA bytes for writing into textures.
pub fn rgba(c: Color32) -> [u8; 4] {
    c.to_srgba_unmultiplied()
}

#[cfg(target_arch = "wasm32")]
fn prefers_dark() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
        .is_some_and(|m| m.matches())
}

#[cfg(not(target_arch = "wasm32"))]
fn prefers_dark() -> bool {
    true
}
