//! SVG primitives for the sheet: escaping, number formats, text wrapping and
//! the palette.

use std::fmt::Write as _;

/// The sheet's colours, in one place.
///
/// The sheet must look the same whatever theme the app is in, and print on
/// white paper, so it does not follow the app's light or dark scheme: it
/// always takes the light scheme's roles from `observation-tokens`, and the
/// coverage ramp and area colours the app's overlay uses from the same
/// crate. Nothing else in this module names a colour.
pub(super) mod palette {
    use observation_tokens::{AREA_COLORS_LIGHT, COVERAGE_RAMP, LIGHT, Rgba};

    pub const PAPER: Rgba = LIGHT.surface;
    pub const INK: Rgba = LIGHT.text;
    pub const MUTED: Rgba = LIGHT.text_muted;
    pub const RULE: Rgba = LIGHT.border;
    pub const FRAME: Rgba = LIGHT.border_strong;
    pub const BAND: Rgba = LIGHT.surface_sunken;
    pub const ACCENT: Rgba = LIGHT.accent;
    pub const STAMP: Rgba = LIGHT.danger;
    /// The map's ground and roads.
    pub const GROUND: Rgba = LIGHT.bg;
    pub const ROAD: Rgba = LIGHT.border;
    /// Overlap ramp: seen by 1, 2, 3, 4+ cameras.
    pub const RAMP: [Rgba; 4] = COVERAGE_RAMP;
    /// Area outlines by label (Own, Neighbour, Public, Other); unlabelled
    /// areas are drawn in [`INK`].
    pub const AREAS: [Rgba; 4] = AREA_COLORS_LIGHT;
}

/// `#rrggbb` (the alpha goes in an opacity attribute, which every SVG
/// reader understands).
pub(super) fn hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// A colour's alpha as an opacity, `0` to `1` with two decimals.
pub(super) fn opacity(c: [u8; 4]) -> String {
    num(f32::from(c[3]) / 255.0, 2)
}

/// Text made safe for SVG content and attribute values.
pub(super) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            // Tab and line breaks are allowed, but read as a space anyway.
            c if c < ' ' || !xml_char(c) => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Whether XML 1.0 allows `c` in a document (its `Char` production): tab,
/// line feed, carriage return, and everything from U+0020 on but the
/// surrogates (which a Rust `char` never is) and U+FFFE and U+FFFF. No
/// character reference can stand for the others either, so they are
/// replaced.
pub(super) fn xml_char(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..
    )
}

/// `v` with at most `decimals` decimals and no trailing zeros (`-0` as `0`).
pub(super) fn num(v: f32, decimals: usize) -> String {
    if !v.is_finite() {
        return "0".to_owned();
    }
    let mut s = format!("{v:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".to_owned() } else { s }
}

/// A whole number with thousands separators: `12,345`.
pub(super) fn thousands(v: f64) -> String {
    let n = if v.is_finite() { v.round().abs() } else { 0.0 } as u64;
    let digits = n.to_string();
    let mut out = String::new();
    if v.round() < 0.0 {
        out.push('-');
    }
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}

/// `12,345 m²`.
pub(super) fn m2(v: f32) -> String {
    format!("{} m²", thousands(f64::from(v)))
}

/// `part` as a whole percentage of `whole`: `7%`, `<1%` for a share that is
/// not zero but rounds to it, and `–` for nothing to share.
pub(super) fn percent(part: f32, whole: f32) -> String {
    if whole <= 0.0 {
        return "–".to_owned();
    }
    let p = part / whole * 100.0;
    if part > 0.0 && p < 0.5 {
        "<1%".to_owned()
    } else {
        format!("{p:.0}%")
    }
}

/// Average advance of a character in the sheet's sans-serif face, in ems:
/// a little wide on purpose, so a line wrapped by it never overruns.
const CHAR_EM: f32 = 0.52;

/// Roughly how wide `s` is at `size` px.
pub(super) fn text_width(s: &str, size: f32) -> f32 {
    s.chars().count() as f32 * size * CHAR_EM
}

/// Splits `text` into lines no wider than `width` px at `size` px, at
/// spaces, and at the line breaks in it; a word longer than a line is cut.
pub(super) fn wrap(text: &str, width: f32, size: f32) -> Vec<String> {
    let max = ((width / (size * CHAR_EM)).floor() as usize).max(8);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ').filter(|w| !w.is_empty()) {
            let mut word: Vec<char> = word.chars().collect();
            while word.len() > max {
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                lines.push(word.drain(..max).collect());
            }
            let word: String = word.into_iter().collect();
            let len = line.chars().count();
            if len > 0 && len + 1 + word.chars().count() > max {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
        lines.push(line);
    }
    lines
}

/// How text is set.
#[derive(Debug, Clone, Copy)]
pub(super) struct Font {
    pub size: f32,
    pub bold: bool,
    pub color: [u8; 4],
}

impl Font {
    pub const fn new(size: f32) -> Self {
        Self {
            size,
            bold: false,
            color: palette::INK,
        }
    }

    pub const fn bold(self) -> Self {
        Self { bold: true, ..self }
    }

    pub const fn color(self, color: [u8; 4]) -> Self {
        Self { color, ..self }
    }

    /// Distance from one baseline to the next, px.
    pub fn line_height(self) -> f32 {
        (self.size * 1.35).round()
    }
}

/// Where text is anchored horizontally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Anchor {
    Start,
    Middle,
    End,
}

/// Writes a `<text>` element with its baseline at `y`.
pub(super) fn text(out: &mut String, x: f32, y: f32, font: Font, anchor: Anchor, s: &str) {
    let _ = write!(
        out,
        "<text x=\"{}\" y=\"{}\" font-size=\"{}\"",
        num(x, 1),
        num(y, 1),
        num(font.size, 1)
    );
    if font.bold {
        out.push_str(" font-weight=\"bold\"");
    }
    if font.color != palette::INK {
        let _ = write!(out, " fill=\"{}\"", hex(font.color));
    }
    match anchor {
        Anchor::Start => {}
        Anchor::Middle => out.push_str(" text-anchor=\"middle\""),
        Anchor::End => out.push_str(" text-anchor=\"end\""),
    }
    let _ = writeln!(out, ">{}</text>", esc(s));
}

/// Writes a `<rect>`; `fill` and `stroke` are optional.
pub(super) fn rect(
    out: &mut String,
    [x, y, w, h]: [f32; 4],
    fill: Option<[u8; 4]>,
    stroke: Option<[u8; 4]>,
) {
    let _ = write!(
        out,
        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
        num(x, 1),
        num(y, 1),
        num(w, 1),
        num(h, 1)
    );
    match fill {
        Some(c) => {
            let _ = write!(out, " fill=\"{}\"", hex(c));
            if c[3] < 255 {
                let _ = write!(out, " fill-opacity=\"{}\"", opacity(c));
            }
        }
        None => out.push_str(" fill=\"none\""),
    }
    if let Some(c) = stroke {
        let _ = write!(out, " stroke=\"{}\" stroke-width=\"0.75\"", hex(c));
    }
    out.push_str("/>\n");
}

/// Writes a horizontal rule from `x0` to `x1` at `y`.
pub(super) fn hline(out: &mut String, x0: f32, x1: f32, y: f32, color: [u8; 4]) {
    let _ = writeln!(
        out,
        "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"0.75\"/>",
        num(x0, 1),
        num(y, 1),
        num(x1, 1),
        num(y, 1),
        hex(color)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_as_people_write_them() {
        assert_eq!(num(14.0, 1), "14");
        assert_eq!(num(3.25, 2), "3.25");
        assert_eq!(num(-0.01, 1), "0");
        assert_eq!(num(f32::NAN, 1), "0");
        assert_eq!(thousands(0.4), "0");
        assert_eq!(thousands(1234567.0), "1,234,567");
        assert_eq!(thousands(-1234.0), "-1,234");
        assert_eq!(m2(4410.2), "4,410 m²");
        assert_eq!(percent(1.0, 400.0), "<1%");
        assert_eq!(percent(0.0, 400.0), "0%");
        assert_eq!(percent(100.0, 400.0), "25%");
        assert_eq!(percent(1.0, 0.0), "–");
    }

    #[test]
    fn text_is_escaped_and_wrapped() {
        assert_eq!(
            esc("<a href=\"x\">&'\u{7}"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39; "
        );
        // Every character XML 1.0 does not allow, and the controls it does.
        assert_eq!(esc("a\u{FFFE}b\u{FFFF}c\u{0}\t\n\r"), "a b c    ");
        assert_eq!(esc("é\u{FFFD}\u{10FFFF}"), "é\u{FFFD}\u{10FFFF}");
        assert!(!xml_char('\u{FFFE}') && !xml_char('\u{1F}') && xml_char('\u{E000}'));
        let lines = wrap("one two three four five six seven", 60.0, 10.0);
        assert!(lines.len() > 1);
        assert!(
            lines
                .iter()
                .all(|l| text_width(l, 10.0) <= 60.0 || !l.contains(' '))
        );
        assert_eq!(lines.join(" "), "one two three four five six seven");
        assert_eq!(wrap("a\nb", 500.0, 10.0), ["a", "b"]);
        let long = wrap(&"x".repeat(30), 50.0, 10.0);
        assert!(long.iter().all(|l| l.chars().count() <= 9), "{long:?}");
    }
}
