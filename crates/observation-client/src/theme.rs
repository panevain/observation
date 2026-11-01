//! Design tokens: the single source of colours, spacing, radii and type scale.
//!
//! No other module may contain colour literals (enforced in CI by
//! `scripts/check-design-tokens.sh`). The scales, the minimum viewport and
//! the UI colour roles come from `observation-tokens`, which also writes them
//! as the CSS of the loading screen and the small-viewport gate in
//! `web/index.html`.

use bevy::prelude::*;
use bevy_egui::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow,
    Stroke, TextStyle,
};
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_core::camera::CameraKind;

use observation_tokens::{FONT, LAYOUT, RADIUS, Rgba, SPACE};
pub use observation_tokens::{Layout, Radius, Space, TypeScale};

/// Smallest supported viewport (CSS px). Below this the app shows a gate.
pub const MIN_VIEWPORT: Vec2 = Vec2::from_array(observation_tokens::MIN_VIEWPORT);

/// Colour roles for the 3D scene (sRGB, straight alpha).
#[derive(Debug, Clone, Copy)]
pub struct WorldColors {
    pub sky: Color32,
    pub ground: Color32,
    pub building: Color32,
    /// Selection ring around the selected camera, drawn over [`Self::halo`].
    pub selection: Color32,
    /// Where the selected camera's axis meets the ground, drawn over [`Self::halo`].
    pub aim_target: Color32,
    /// Dark casing under neutral markers so they read on any map tile.
    pub halo: Color32,
    /// Outline of the coverage region.
    pub region: Color32,
}

#[derive(Debug, Clone, Copy, Resource)]
pub struct Tokens {
    pub dark: bool,
    // UI colour roles
    pub bg: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub surface_sunken: Color32,
    /// Dividers and card edges.
    pub border: Color32,
    /// Edges of inputs (text fields, number fields, combo boxes, checkboxes):
    /// 3:1 against the panel, as WCAG 1.4.11 asks of control boundaries.
    pub border_strong: Color32,
    /// Fill of a hovered button or row.
    pub hover: Color32,
    /// Drop shadow under floating cards and menus.
    pub shadow: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub scrim: Color32,
    /// Quick picks offered for camera colours.
    pub camera_palette: [Color32; 8],
    /// Default camera colour per type, in [`CameraKind::ALL`] order. Cool and
    /// neutral hues only: the warm band belongs to the coverage ramp.
    pub kind_colors: [Color32; 7],
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

/// Quick picks for camera colours: the type colours plus a bluish green, all
/// clear of the coverage ramp.
const CAMERA_PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x00, 0x72, 0xB2),
    Color32::from_rgb(0x1B, 0x7F, 0x3B),
    Color32::from_rgb(0x56, 0xB4, 0xE9),
    Color32::from_rgb(0x2B, 0x2D, 0x8E),
    Color32::from_rgb(0x14, 0xB8, 0xA6),
    Color32::from_rgb(0x8B, 0x7C, 0xF6),
    Color32::from_rgb(0x5B, 0x65, 0x73),
    Color32::from_rgb(0x00, 0x9E, 0x73),
];

/// As [`CAMERA_PALETTE`], with the same lifted indigo and slate as
/// [`KIND_COLORS_DARK`].
const CAMERA_PALETTE_DARK: [Color32; 8] = [
    Color32::from_rgb(0x00, 0x72, 0xB2),
    Color32::from_rgb(0x1B, 0x7F, 0x3B),
    Color32::from_rgb(0x56, 0xB4, 0xE9),
    Color32::from_rgb(0x55, 0x58, 0xD6),
    Color32::from_rgb(0x14, 0xB8, 0xA6),
    Color32::from_rgb(0x8B, 0x7C, 0xF6),
    Color32::from_rgb(0x7D, 0x87, 0x96),
    Color32::from_rgb(0x00, 0x9E, 0x73),
];

/// Doorbell blue, floodlight green, spotlight sky, bullet indigo, turret teal,
/// dome violet, indoor slate. Cool and neutral hues so they never read as a
/// step of the yellow-to-purple coverage ramp; each type also has an icon, so
/// colour is never the only cue.
const KIND_COLORS_LIGHT: [Color32; 7] = [
    Color32::from_rgb(0x00, 0x72, 0xB2),
    Color32::from_rgb(0x1B, 0x7F, 0x3B),
    Color32::from_rgb(0x56, 0xB4, 0xE9),
    Color32::from_rgb(0x2B, 0x2D, 0x8E),
    Color32::from_rgb(0x14, 0xB8, 0xA6),
    Color32::from_rgb(0x8B, 0x7C, 0xF6),
    Color32::from_rgb(0x5B, 0x65, 0x73),
];

/// As [`KIND_COLORS_LIGHT`], with indigo and slate lifted so their swatches
/// stay visible on the dark panels.
const KIND_COLORS_DARK: [Color32; 7] = [
    Color32::from_rgb(0x00, 0x72, 0xB2),
    Color32::from_rgb(0x1B, 0x7F, 0x3B),
    Color32::from_rgb(0x56, 0xB4, 0xE9),
    Color32::from_rgb(0x55, 0x58, 0xD6),
    Color32::from_rgb(0x14, 0xB8, 0xA6),
    Color32::from_rgb(0x8B, 0x7C, 0xF6),
    Color32::from_rgb(0x7D, 0x87, 0x96),
];

const COVERAGE_RAMP: [Color32; 4] = [
    Color32::from_rgba_unmultiplied_const(0xF5, 0xC5, 0x42, 140),
    Color32::from_rgba_unmultiplied_const(0xF2, 0x8C, 0x28, 165),
    Color32::from_rgba_unmultiplied_const(0xE0, 0x45, 0x3A, 185),
    Color32::from_rgba_unmultiplied_const(0x9B, 0x2C, 0x8C, 205),
];

impl Tokens {
    pub fn light() -> Self {
        let ui = &observation_tokens::LIGHT;
        Self {
            dark: false,
            bg: color(ui.bg),
            surface: color(ui.surface),
            surface_raised: color(ui.surface_raised),
            surface_sunken: color(ui.surface_sunken),
            border: color(ui.border),
            border_strong: color(ui.border_strong),
            hover: color(ui.hover),
            shadow: color(ui.shadow),
            text: color(ui.text),
            text_muted: color(ui.text_muted),
            accent: color(ui.accent),
            on_accent: color(ui.on_accent),
            danger: color(ui.danger),
            warning: color(ui.warning),
            scrim: color(ui.scrim),
            camera_palette: CAMERA_PALETTE,
            kind_colors: KIND_COLORS_LIGHT,
            coverage_ramp: COVERAGE_RAMP,
            selection_box: Color32::from_rgb(0x3B, 0x5B, 0xDB),
            world: WorldColors {
                sky: Color32::from_rgb(0xE9, 0xED, 0xF2),
                ground: Color32::from_rgb(0xD8, 0xD6, 0xCF),
                building: Color32::from_rgb(0xF0, 0xEE, 0xE9),
                selection: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                aim_target: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                halo: Color32::from_rgb(0x1B, 0x1F, 0x24),
                region: Color32::from_rgb(0x1B, 0x1F, 0x24),
            },
            space: SPACE,
            radius: RADIUS,
            font: FONT,
            layout: LAYOUT,
            stroke_width: 1.0,
        }
    }

    pub fn dark() -> Self {
        let ui = &observation_tokens::DARK;
        Self {
            dark: true,
            bg: color(ui.bg),
            surface: color(ui.surface),
            surface_raised: color(ui.surface_raised),
            surface_sunken: color(ui.surface_sunken),
            border: color(ui.border),
            border_strong: color(ui.border_strong),
            hover: color(ui.hover),
            shadow: color(ui.shadow),
            text: color(ui.text),
            text_muted: color(ui.text_muted),
            accent: color(ui.accent),
            on_accent: color(ui.on_accent),
            danger: color(ui.danger),
            warning: color(ui.warning),
            scrim: color(ui.scrim),
            camera_palette: CAMERA_PALETTE_DARK,
            kind_colors: KIND_COLORS_DARK,
            coverage_ramp: COVERAGE_RAMP,
            selection_box: Color32::from_rgb(0x7C, 0x95, 0xF2),
            world: WorldColors {
                sky: Color32::from_rgb(0x1A, 0x1D, 0x22),
                ground: Color32::from_rgb(0x2E, 0x31, 0x36),
                building: Color32::from_rgb(0xB8, 0xBC, 0xC2),
                selection: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                aim_target: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                halo: Color32::from_rgb(0x12, 0x14, 0x17),
                region: Color32::from_rgb(0x12, 0x14, 0x17),
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
        Self::for_scheme(prefers_dark())
    }

    fn for_scheme(dark: bool) -> Self {
        if dark { Self::dark() } else { Self::light() }
    }

    /// Default colour of the `nth` camera (from 0) of a type without a colour
    /// of its own: the type's colour, then a lighter and a darker shade of it.
    /// More shades than that stop being distinguishable, so they repeat; the
    /// camera's name tells repeats apart.
    pub fn kind_shade(&self, kind: CameraKind, nth: usize) -> Color32 {
        const SHADES: [f32; 3] = [0.0, 0.4, -0.35];
        let base = self.kind_color(kind);
        match SHADES[nth % SHADES.len()] {
            s if s >= 0.0 => base.lerp_to_gamma(Color32::WHITE, s),
            s => base.lerp_to_gamma(Color32::BLACK, -s),
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
        v.window_shadow = self.shadow();
        v.popup_shadow = self.shadow();
        v.menu_corner_radius = CornerRadius::same(self.radius.md);
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.accent;
        v.selection.stroke = Stroke::new(self.stroke_width, self.on_accent);
        v.warn_fg_color = self.warning;
        v.error_fg_color = self.danger;
        // States are told apart by fill; strokes are kept for input edges
        // (buttons drawn with the helpers in `ui::widgets` drop them).
        let r = CornerRadius::same(self.radius.md);
        for (w, fill, stroke) in [
            (&mut v.widgets.noninteractive, self.surface, self.border),
            (
                &mut v.widgets.inactive,
                self.surface_sunken,
                self.border_strong,
            ),
            (&mut v.widgets.hovered, self.hover, self.border_strong),
            (&mut v.widgets.active, self.hover, self.accent),
            (&mut v.widgets.open, self.hover, self.border_strong),
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

    /// Shadow under floating cards, windows and menus.
    pub fn shadow(&self) -> Shadow {
        Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: self.shadow,
        }
    }

    /// A frame for floating cards/panels.
    pub fn card(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.surface_raised)
            .stroke(Stroke::new(self.stroke_width, self.border))
            .corner_radius(CornerRadius::same(self.radius.lg))
            .shadow(self.shadow())
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

/// Typeface for the whole UI, chosen in Settings. Iosevka is always the
/// monospace face. Both are subset to Latin-1 (see `scripts/subset-fonts.sh`),
/// and Inter has tabular digits built in so live figures do not jitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Resource)]
pub enum UiFont {
    #[default]
    Inter,
    Iosevka,
}

impl UiFont {
    pub const ALL: [Self; 2] = [Self::Inter, Self::Iosevka];

    pub fn label(self) -> &'static str {
        match self {
            Self::Inter => "Inter",
            Self::Iosevka => "Iosevka",
        }
    }

    /// Stable name for storing the choice.
    pub fn key(self) -> &'static str {
        match self {
            Self::Inter => "inter",
            Self::Iosevka => "iosevka",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.key() == key)
    }

    fn files(self) -> (&'static [u8], &'static [u8]) {
        match self {
            Self::Inter => (INTER_REGULAR, INTER_SEMIBOLD),
            Self::Iosevka => (IOSEVKA_REGULAR, IOSEVKA_SEMIBOLD),
        }
    }
}

const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const IOSEVKA_REGULAR: &[u8] = include_bytes!("../assets/fonts/IosevkaTerm-Regular.ttf");
const IOSEVKA_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/IosevkaTerm-SemiBold.ttf");

/// Family for headings, figures and other emphasised text (the semibold cut of
/// the UI font). egui has no font weights, so weight is a family.
pub fn strong_family() -> FontFamily {
    FontFamily::Name("ui-strong".into())
}

/// Installs the UI font, its semibold cut, the monospace face and the icons.
/// egui's bundled fonts stay as fallbacks for anything outside the subsets.
pub fn install_fonts(ctx: &egui::Context, font: UiFont) {
    let mut fonts = FontDefinitions::default();
    let (regular, semibold) = font.files();
    for (name, bytes) in [
        ("ui", regular),
        ("ui-semibold", semibold),
        ("mono", IOSEVKA_REGULAR),
    ] {
        fonts
            .font_data
            .insert(name.into(), FontData::from_static(bytes).into());
    }
    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let with = |first: &str| {
        let mut keys = vec![first.to_owned()];
        keys.extend(defaults.iter().cloned());
        keys
    };
    fonts.families.insert(FontFamily::Proportional, with("ui"));
    fonts.families.insert(strong_family(), with("ui-semibold"));
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        mono.insert(0, "mono".into());
    }
    crate::icons::install(&mut fonts);
    ctx.set_fonts(fonts);
}

/// A colour from `observation-tokens` as a token colour.
const fn color([r, g, b, a]: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied_const(r, g, b, a)
}

/// Converts a token colour to a Bevy colour.
pub fn to_bevy(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::srgba_u8(r, g, b, a)
}

/// No fill: a button or row at rest.
pub const NO_FILL: Color32 = Color32::TRANSPARENT;

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

/// Light/dark changes of the system setting after load, reported by the
/// browser through a channel.
#[derive(Resource)]
pub struct SchemeChanges(Receiver<bool>);

impl SchemeChanges {
    /// Starts listening to the browser. Call once: each call adds a listener
    /// that lives as long as the page.
    pub fn watch() -> Self {
        let (tx, rx) = unbounded();
        watch_scheme(tx);
        Self(rx)
    }
}

/// Swaps [`Tokens`] for the other variant when the system setting flips.
/// `Tokens` is only replaced on an actual flip, so its change detection (which
/// re-themes egui and the 3D scene) does not fire every frame.
pub fn follow_scheme(changes: Res<SchemeChanges>, mut tokens: ResMut<Tokens>) {
    // Only the latest setting matters.
    if let Some(dark) = changes.0.try_iter().last()
        && dark != tokens.dark
    {
        *tokens = Tokens::for_scheme(dark);
    }
}

#[cfg(target_arch = "wasm32")]
fn dark_query() -> Option<web_sys::MediaQueryList> {
    web_sys::window().and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
}

#[cfg(target_arch = "wasm32")]
fn prefers_dark() -> bool {
    dark_query().is_some_and(|m| m.matches())
}

/// Sends the new setting whenever the media query's `change` event fires.
#[cfg(target_arch = "wasm32")]
fn watch_scheme(tx: Sender<bool>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(query) = dark_query() else { return };
    let on_change = {
        let query = query.clone();
        Closure::<dyn Fn(web_sys::Event)>::new(move |_: web_sys::Event| {
            // The receiver lives as long as the app; a send error means shutdown.
            let _ = tx.send(query.matches());
        })
    };
    if let Err(e) =
        query.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())
    {
        warn!(
            "cannot follow the system theme: {}",
            crate::net::js_cause(&e)
        );
        return;
    }
    // The handler (and the query it holds, which must stay alive to keep
    // firing) lives as long as the page.
    on_change.forget();
}

#[cfg(not(target_arch = "wasm32"))]
fn prefers_dark() -> bool {
    true
}

/// Native builds stay dark: there is no system setting to follow.
#[cfg(not(target_arch = "wasm32"))]
fn watch_scheme(_: Sender<bool>) {}
