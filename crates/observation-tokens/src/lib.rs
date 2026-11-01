//! Design token values shared by the client's `theme.rs` and the page CSS.
//!
//! The spacing, radius, type and layout scales, the minimum viewport and the
//! UI colour roles live here as plain data. `theme.rs` builds its `Tokens`
//! from them, and [`css`] writes them as CSS custom properties for the
//! loading screen and the small-viewport gate in `web/index.html` (trunk's
//! pre_build hook runs the `token-css` binary; see `Trunk.toml`). Colours
//! used only by the 3D scene and the camera palettes stay in `theme.rs`.

use std::fmt::Write as _;

/// An sRGB colour with straight alpha.
pub type Rgba = [u8; 4];

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [r, g, b, 0xFF]
}

/// Smallest supported viewport (CSS px): width, height. Below this the app
/// shows a gate.
pub const MIN_VIEWPORT: [f32; 2] = [768.0, 600.0];

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

pub const SPACE: Space = Space {
    xs: 4.0,
    sm: 8.0,
    md: 12.0,
    lg: 16.0,
    xl: 24.0,
};
pub const RADIUS: Radius = Radius {
    sm: 4,
    md: 6,
    lg: 8,
};
pub const FONT: TypeScale = TypeScale {
    caption: 12.0,
    body: 14.0,
    title: 16.0,
    heading: 20.0,
};
pub const LAYOUT: Layout = Layout {
    hit_target: 32.0,
    toolbar_width: 76.0,
    inspector_width: 320.0,
    topbar_height: 48.0,
    statusbar_height: 28.0,
};

/// UI colour roles of one colour scheme.
#[derive(Debug, Clone, Copy)]
pub struct UiColors {
    pub bg: Rgba,
    pub surface: Rgba,
    pub surface_raised: Rgba,
    pub surface_sunken: Rgba,
    /// Dividers and card edges.
    pub border: Rgba,
    /// Edges of inputs (text fields, number fields, combo boxes, checkboxes):
    /// 3:1 against the panel, as WCAG 1.4.11 asks of control boundaries.
    pub border_strong: Rgba,
    /// Fill of a hovered button or row.
    pub hover: Rgba,
    /// Drop shadow under floating cards and menus.
    pub shadow: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub accent: Rgba,
    pub on_accent: Rgba,
    pub danger: Rgba,
    pub warning: Rgba,
    pub scrim: Rgba,
}

impl UiColors {
    /// Every role with its CSS name, in declaration order.
    fn roles(&self) -> [(&'static str, Rgba); 15] {
        // No `..`: a new role fails to compile until it is listed here.
        let Self {
            bg,
            surface,
            surface_raised,
            surface_sunken,
            border,
            border_strong,
            hover,
            shadow,
            text,
            text_muted,
            accent,
            on_accent,
            danger,
            warning,
            scrim,
        } = *self;
        [
            ("bg", bg),
            ("surface", surface),
            ("surface-raised", surface_raised),
            ("surface-sunken", surface_sunken),
            ("border", border),
            ("border-strong", border_strong),
            ("hover", hover),
            ("shadow", shadow),
            ("text", text),
            ("text-muted", text_muted),
            ("accent", accent),
            ("on-accent", on_accent),
            ("danger", danger),
            ("warning", warning),
            ("scrim", scrim),
        ]
    }
}

pub const LIGHT: UiColors = UiColors {
    bg: rgb(0xF4, 0xF5, 0xF7),
    surface: rgb(0xFF, 0xFF, 0xFF),
    surface_raised: rgb(0xFF, 0xFF, 0xFF),
    surface_sunken: rgb(0xEC, 0xEE, 0xF1),
    border: rgb(0xD9, 0xDC, 0xE1),
    border_strong: rgb(0x8A, 0x91, 0x9C),
    hover: rgb(0xE3, 0xE6, 0xEA),
    shadow: [0x10, 0x14, 0x1A, 40],
    text: rgb(0x1B, 0x1F, 0x24),
    text_muted: rgb(0x5F, 0x67, 0x72),
    accent: rgb(0x3B, 0x5B, 0xDB),
    on_accent: rgb(0xFF, 0xFF, 0xFF),
    danger: rgb(0xC9, 0x2A, 0x2A),
    warning: rgb(0xA1, 0x5F, 0x00),
    scrim: [0x10, 0x14, 0x1A, 150],
};

pub const DARK: UiColors = UiColors {
    bg: rgb(0x12, 0x14, 0x17),
    surface: rgb(0x1B, 0x1E, 0x23),
    surface_raised: rgb(0x24, 0x28, 0x2E),
    surface_sunken: rgb(0x15, 0x17, 0x1B),
    border: rgb(0x33, 0x38, 0x40),
    border_strong: rgb(0x64, 0x6D, 0x79),
    hover: rgb(0x2C, 0x31, 0x38),
    shadow: [0x00, 0x00, 0x00, 90],
    text: rgb(0xE8, 0xEA, 0xED),
    text_muted: rgb(0x9A, 0xA2, 0xAD),
    accent: rgb(0x7C, 0x95, 0xF2),
    on_accent: rgb(0x0E, 0x12, 0x1E),
    danger: rgb(0xF0, 0x6A, 0x6A),
    warning: rgb(0xF0, 0xB0, 0x4A),
    scrim: [0x00, 0x00, 0x00, 170],
};

/// The tokens as a stylesheet: `--obs-*` custom properties on `:root` (the
/// colours swapped under `prefers-color-scheme: dark`), and the gate rules:
/// `#gate` hidden, then shown (with `#loader` and `#app` hidden) below
/// [`MIN_VIEWPORT`] by a media query, since custom properties cannot be used
/// in media queries. Both gate rules live here so their order is fixed.
pub fn css() -> String {
    let mut out = String::from(
        "/* Generated from crates/observation-tokens by trunk's pre_build hook (Trunk.toml). Do not edit. */\n:root {\n",
    );
    push_colors(&mut out, &LIGHT, "  ");
    let scales: &[(&str, f32)] = &[
        ("space-xs", SPACE.xs),
        ("space-sm", SPACE.sm),
        ("space-md", SPACE.md),
        ("space-lg", SPACE.lg),
        ("space-xl", SPACE.xl),
        ("radius-sm", f32::from(RADIUS.sm)),
        ("radius-md", f32::from(RADIUS.md)),
        ("radius-lg", f32::from(RADIUS.lg)),
        ("font-caption", FONT.caption),
        ("font-body", FONT.body),
        ("font-title", FONT.title),
        ("font-heading", FONT.heading),
        ("hit-target", LAYOUT.hit_target),
        ("toolbar-width", LAYOUT.toolbar_width),
        ("inspector-width", LAYOUT.inspector_width),
        ("topbar-height", LAYOUT.topbar_height),
        ("statusbar-height", LAYOUT.statusbar_height),
        ("min-width", MIN_VIEWPORT[0]),
        ("min-height", MIN_VIEWPORT[1]),
    ];
    for (name, px) in scales {
        writeln!(out, "  --obs-{name}: {px}px;").expect("write to String");
    }
    out.push_str("}\n@media (prefers-color-scheme: dark) {\n  :root {\n");
    push_colors(&mut out, &DARK, "    ");
    out.push_str("  }\n}\n");
    // Just below the minimum, as the browser may report fractional CSS px.
    let [w, h] = MIN_VIEWPORT.map(|px| f64::from(px) - 0.02);
    writeln!(
        out,
        "#gate {{ display: none; }}\n\
         /* Phones and very small windows: show the gate immediately and hide the app (it still downloads; the app shows the same gate). */\n\
         @media (max-width: {w:.2}px), (max-height: {h:.2}px) {{\n  #gate {{ display: block; }}\n  #loader, #app {{ display: none; }}\n}}",
    )
    .expect("write to String");
    out
}

fn push_colors(out: &mut String, colors: &UiColors, indent: &str) {
    for (name, [r, g, b, a]) in colors.roles() {
        write!(out, "{indent}--obs-{name}: #{r:02x}{g:02x}{b:02x}").expect("write to String");
        if a != 0xFF {
            write!(out, "{a:02x}").expect("write to String");
        }
        out.push_str(";\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_has_light_and_dark_colours() {
        let css = css();
        assert!(css.contains("  --obs-bg: #f4f5f7;\n"));
        assert!(css.contains("    --obs-bg: #121417;\n"));
        assert!(css.contains("  --obs-surface-raised: #24282e;\n"));
        assert!(css.contains("--obs-scrim: #10141a96;\n"));
    }

    #[test]
    fn css_has_scales_and_gate() {
        let css = css();
        assert!(css.contains("--obs-space-xl: 24px;\n"));
        assert!(css.contains("--obs-radius-lg: 8px;\n"));
        assert!(css.contains("--obs-min-width: 768px;\n"));
        assert!(css.contains("@media (max-width: 767.98px), (max-height: 599.98px) {"));
    }

    #[test]
    fn gate_is_hidden_before_the_media_query_shows_it() {
        let css = css();
        let hide = css
            .find("#gate { display: none; }")
            .expect("gate hidden by default");
        let show = css
            .find("#gate { display: block; }")
            .expect("gate shown when small");
        assert!(hide < show);
    }

    /// The gate's notice in `web/index.html` is hand-written; it must state
    /// the same minimum as the media query and the in-app gate.
    #[test]
    fn index_html_states_the_minimum_viewport() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/index.html");
        let html = std::fs::read_to_string(path).expect("read web/index.html");
        let [w, h] = MIN_VIEWPORT;
        assert!(html.contains(&format!("at least {w} × {h} pixels")));
    }
}
