//! Lays the sheet out as A4 pages stacked in one SVG.

use super::figures::{CameraLine, Figures, GridCell, MapPanel};
use super::svg::{
    Anchor, Font, esc, hex, hline, m2, num, opacity, palette, percent, rect, text, text_width,
    thousands, wrap,
};
use super::{PAGE_HEIGHT, PAGE_WIDTH, SheetSvg};
use crate::camera::{DoriLevel, Lighting};
use crate::footage::{AreaLabel, Reason, SeenRange, Verdict};
use crate::provenance::{CAVEATS_SUMMARY, ENGINE_VERSION, format_utc_minute};
use std::fmt::Write as _;

const MARGIN: f32 = 40.0;
const CONTENT_W: f32 = PAGE_WIDTH - 2.0 * MARGIN;
/// Where content starts on a page, below the running header.
const TOP: f32 = 62.0;
/// Where content must end on a page, above the footer.
const BOTTOM: f32 = PAGE_HEIGHT - 58.0;
/// Tallest the map may be, px.
const MAP_MAX_H: f32 = 560.0;

const TITLE: Font = Font::new(20.0).bold();
const HEADING: Font = Font::new(13.0).bold();
const SUBHEADING: Font = Font::new(10.0).bold();
const BODY: Font = Font::new(9.0);
const SMALL: Font = Font::new(8.0);
const TINY: Font = Font::new(7.0).color(palette::MUTED);

/// Subject heights and the like: `1.0`, `1.65`.
pub(super) fn metres(v: f32) -> String {
    let s = num(v, 2);
    if s.contains('.') { s } else { format!("{s}.0") }
}

fn lighting_text(l: Lighting) -> &'static str {
    match l {
        Lighting::Day => "by day",
        Lighting::Night => "at night",
    }
}

fn level_text(level: DoriLevel, ppm: f32) -> String {
    format!("{} ({} px/m)", level.label(), num(ppm, 1))
}

fn label_text(l: Option<AreaLabel>) -> &'static str {
    l.map_or("Unlabelled", AreaLabel::label)
}

/// The content being laid out, page by page.
struct Pages {
    out: String,
    /// The page being filled, from 0.
    page: u32,
    /// The top of the next line on that page.
    y: f32,
}

impl Pages {
    fn abs(&self, local: f32) -> f32 {
        self.page as f32 * PAGE_HEIGHT + local
    }

    /// Starts a new page unless `h` px fit on this one.
    fn need(&mut self, h: f32) {
        if self.y + h > BOTTOM && self.y > TOP {
            self.page += 1;
            self.y = TOP;
        }
    }

    fn gap(&mut self, h: f32) {
        self.y += h;
    }

    /// One line of text at `x`.
    fn line(&mut self, x: f32, font: Font, anchor: Anchor, s: &str) {
        let lh = font.line_height();
        self.need(lh);
        let y = self.abs(self.y + font.size);
        text(&mut self.out, x, y, font, anchor, s);
        self.y += lh;
    }

    /// `s` wrapped to `width` from `x`.
    fn para(&mut self, x: f32, width: f32, font: Font, s: &str) {
        for l in wrap(s, width, font.size) {
            self.line(x, font, Anchor::Start, &l);
        }
    }

    /// A paragraph, or a list item when it starts with "• ".
    fn item(&mut self, font: Font, s: &str) {
        match s.strip_prefix("• ") {
            Some(rest) => {
                let lh = font.line_height();
                self.need(lh);
                let y = self.abs(self.y + font.size);
                text(&mut self.out, MARGIN + 4.0, y, font, Anchor::Start, "•");
                self.para(MARGIN + 14.0, CONTENT_W - 14.0, font, rest);
            }
            None => self.para(MARGIN, CONTENT_W, font, s),
        }
        self.gap(3.0);
    }

    /// A section heading, kept with the start of what follows it.
    fn heading(&mut self, s: &str) {
        self.need(HEADING.line_height() + 4.0 * BODY.line_height());
        self.gap(10.0);
        self.line(MARGIN, HEADING, Anchor::Start, s);
        let y = self.abs(self.y);
        hline(&mut self.out, MARGIN, MARGIN + CONTENT_W, y, palette::RULE);
        self.gap(6.0);
    }

    fn subheading(&mut self, s: &str) {
        self.need(SUBHEADING.line_height() + 2.0 * BODY.line_height());
        self.gap(4.0);
        self.line(MARGIN, SUBHEADING, Anchor::Start, s);
        self.gap(2.0);
    }

    /// Key and value lines: the key in the margin column, the value wrapped
    /// beside it.
    fn facts(&mut self, rows: &[(&str, String)]) {
        const KEY_W: f32 = 150.0;
        for (key, value) in rows {
            let lines = wrap(value, CONTENT_W - KEY_W, BODY.size);
            self.need(BODY.line_height() * lines.len() as f32);
            let y = self.abs(self.y + BODY.size);
            text(
                &mut self.out,
                MARGIN,
                y,
                BODY.color(palette::MUTED),
                Anchor::Start,
                key,
            );
            for l in lines {
                self.line(MARGIN + KEY_W, BODY, Anchor::Start, &l);
            }
            self.gap(1.0);
        }
    }

    /// A table whose header band carries `title` and, when `stamp` is set,
    /// the estimate stamp, so the stamp sits inside the figure. Columns are
    /// `(width, anchor)`; cells wrap within their column, and the header is
    /// drawn again on every page the table runs onto.
    fn table(
        &mut self,
        title: &str,
        stamp: bool,
        cols: &[(f32, Anchor)],
        header: &[&str],
        rows: &[Vec<String>],
    ) {
        const PAD: f32 = 4.0;
        let font = SMALL;
        let lh = font.line_height();
        let cells = |row: &[String], font: Font| -> Vec<Vec<String>> {
            row.iter()
                .zip(cols)
                .map(|(c, (w, _))| wrap(c, w - 2.0 * PAD, font.size))
                .collect()
        };
        let header_cells = cells(
            &header.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            font.bold(),
        );
        let header_lines = header_cells.iter().map(Vec::len).max().unwrap_or(1) as f32;
        let header_h = lh * (header_lines + 1.0) + 2.0 * PAD;
        let first_row_h = rows
            .first()
            .map_or(0.0, |r| row_height(&cells(r, font), lh, PAD));
        self.need(header_h + first_row_h);
        let draw_header = |p: &mut Self| {
            let top = p.abs(p.y);
            rect(
                &mut p.out,
                [MARGIN, top, CONTENT_W, header_h],
                Some(palette::BAND),
                None,
            );
            text(
                &mut p.out,
                MARGIN + PAD,
                top + PAD + font.size,
                font.bold(),
                Anchor::Start,
                title,
            );
            if stamp {
                text(
                    &mut p.out,
                    MARGIN + CONTENT_W - PAD,
                    top + PAD + font.size,
                    font.bold().color(palette::STAMP),
                    Anchor::End,
                    "ESTIMATE · modelled, not recorded footage",
                );
            }
            draw_row(
                &mut p.out,
                cols,
                &header_cells,
                top + PAD + lh,
                font.bold(),
                PAD,
            );
            p.y += header_h;
            let y = p.abs(p.y);
            hline(&mut p.out, MARGIN, MARGIN + CONTENT_W, y, palette::FRAME);
        };
        draw_header(self);
        for row in rows {
            let c = cells(row, font);
            let h = row_height(&c, lh, PAD);
            let page = self.page;
            self.need(h);
            if self.page != page {
                draw_header(self);
            }
            let top = self.abs(self.y);
            draw_row(&mut self.out, cols, &c, top + PAD, font, PAD);
            self.y += h;
            let y = self.abs(self.y);
            hline(&mut self.out, MARGIN, MARGIN + CONTENT_W, y, palette::RULE);
        }
        self.gap(8.0);
    }
}

fn row_height(cells: &[Vec<String>], lh: f32, pad: f32) -> f32 {
    cells.iter().map(Vec::len).max().unwrap_or(1).max(1) as f32 * lh + 2.0 * pad
}

/// Draws one row's cells, the first line's top at `top`.
fn draw_row(
    out: &mut String,
    cols: &[(f32, Anchor)],
    cells: &[Vec<String>],
    top: f32,
    font: Font,
    pad: f32,
) {
    let lh = font.line_height();
    let mut x = MARGIN;
    for ((w, anchor), lines) in cols.iter().zip(cells) {
        let tx = match anchor {
            Anchor::Start => x + pad,
            Anchor::Middle => x + w / 2.0,
            Anchor::End => x + w - pad,
        };
        for (k, l) in lines.iter().enumerate() {
            text(out, tx, top + k as f32 * lh + font.size, font, *anchor, l);
        }
        x += w;
    }
}

/// Columns that fill the content width: `first` px for the first column,
/// the rest shared equally, numbers aligned right.
fn columns(first: f32, rest: usize) -> Vec<(f32, Anchor)> {
    let w = (CONTENT_W - first) / rest.max(1) as f32;
    std::iter::once((first, Anchor::Start))
        .chain(std::iter::repeat_n((w, Anchor::End), rest))
        .collect()
}

/// The sheet as SVG.
pub(super) fn render(f: &Figures) -> SheetSvg {
    let mut p = Pages {
        out: String::new(),
        page: 0,
        y: TOP,
    };
    title_block(&mut p, f);
    facts(&mut p, f);
    if let Some(map) = &f.map {
        map_panel(&mut p, f, map);
    }
    cameras(&mut p, f);
    grid(&mut p, f);
    footage(&mut p, f);
    targets(&mut p, f);
    p.heading("Methodology");
    for s in &f.methodology {
        p.subheading(&s.heading);
        for para in &s.paragraphs {
            p.item(BODY, para);
        }
    }
    let pages = p.page + 1;
    let height = pages as f32 * PAGE_HEIGHT;
    let mut svg = String::with_capacity(p.out.len() + 4096);
    let _ = writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" \
         viewBox=\"0 0 {w} {h}\" font-family=\"Helvetica, Arial, sans-serif\" fill=\"{ink}\">",
        w = num(PAGE_WIDTH, 0),
        h = num(height, 0),
        ink = hex(palette::INK),
    );
    let _ = writeln!(
        svg,
        "<title>{}</title>",
        esc(&format!(
            "Camera coverage evidence sheet ({}): {}",
            f.tier.label(),
            f.title
        ))
    );
    let _ = writeln!(
        svg,
        "<desc>An estimate from a model, not recorded footage. Generated {} by Observation {}.</desc>",
        format_utc_minute(f.generated_unix),
        ENGINE_VERSION
    );
    rect(
        &mut svg,
        [0.0, 0.0, PAGE_WIDTH, height],
        Some(palette::PAPER),
        None,
    );
    for page in 0..pages {
        page_furniture(&mut svg, f, page, pages);
    }
    svg.push_str(&p.out);
    svg.push_str("</svg>\n");
    SheetSvg { svg, pages }
}

/// The watermark, running header and footer of one page.
fn page_furniture(out: &mut String, f: &Figures, page: u32, pages: u32) {
    let top = page as f32 * PAGE_HEIGHT;
    // The watermark, behind everything, so that no crop of a page loses it.
    let (cx, cy) = (PAGE_WIDTH / 2.0, top + PAGE_HEIGHT / 2.0);
    let mut marks = vec!["ESTIMATE · MODEL, NOT FOOTAGE"];
    if f.tier.shows_positions() {
        marks.push("SHOWS CAMERA POSITIONS");
    }
    for (k, mark) in marks.iter().enumerate() {
        let _ = writeln!(
            out,
            "<text x=\"{x}\" y=\"{y}\" font-size=\"40\" font-weight=\"bold\" text-anchor=\"middle\" \
             fill=\"{c}\" fill-opacity=\"0.08\" transform=\"rotate(-35 {x} {cy})\">{t}</text>",
            x = num(cx, 1),
            y = num(cy + k as f32 * 56.0, 1),
            cy = num(cy, 1),
            c = hex(palette::STAMP),
            t = esc(mark),
        );
    }
    // Running header.
    text(
        out,
        MARGIN,
        top + 30.0,
        SMALL.color(palette::MUTED),
        Anchor::Start,
        "Observation · camera coverage evidence sheet",
    );
    let badge = format!("{} sheet · {}", f.tier.label(), f.tier.badge());
    let badge_color = if f.tier.shows_positions() {
        palette::STAMP
    } else {
        palette::ACCENT
    };
    text(
        out,
        MARGIN + CONTENT_W,
        top + 30.0,
        SMALL.bold().color(badge_color),
        Anchor::End,
        &badge,
    );
    hline(out, MARGIN, MARGIN + CONTENT_W, top + 38.0, palette::RULE);
    // Footer.
    let fy = top + PAGE_HEIGHT - 44.0;
    hline(out, MARGIN, MARGIN + CONTENT_W, fy, palette::RULE);
    let data = f
        .provenance
        .osm_data_unix
        .map_or_else(|| "unknown".to_owned(), format_utc_minute);
    let line1 = format!(
        "{} · generated {} · Observation {} · OSM data as of {} · fingerprint {}",
        f.title,
        format_utc_minute(f.generated_unix),
        ENGINE_VERSION,
        data,
        f.provenance.osm_fingerprint.as_deref().unwrap_or("none")
    );
    let mut line2 = format!("Map data {} (ODbL).", f.osm_attribution);
    if !f.elevation_attribution.is_empty() {
        let _ = write!(line2, " {}.", f.elevation_attribution.trim_end_matches('.'));
    }
    line2.push_str(" An estimate from a model, not recorded footage.");
    let page_no = format!("Page {} of {pages}", page + 1);
    let room = CONTENT_W - text_width(&page_no, TINY.size) - 12.0;
    for (k, l) in [line1, line2].iter().enumerate() {
        let l = wrap(l, room, TINY.size)
            .into_iter()
            .next()
            .unwrap_or_default();
        text(
            out,
            MARGIN,
            fy + 12.0 + k as f32 * 10.0,
            TINY,
            Anchor::Start,
            &l,
        );
    }
    text(
        out,
        MARGIN + CONTENT_W,
        fy + 12.0,
        TINY,
        Anchor::End,
        &page_no,
    );
}

fn title_block(p: &mut Pages, f: &Figures) {
    p.line(
        MARGIN,
        TITLE,
        Anchor::Start,
        "Camera coverage evidence sheet",
    );
    p.gap(2.0);
    let scene = if f.title.is_empty() {
        "Scene: (no name)".to_owned()
    } else {
        format!("Scene: {} (the name as typed)", f.title)
    };
    p.para(MARGIN, CONTENT_W, SUBHEADING, &scene);
    p.gap(4.0);
    p.para(
        MARGIN,
        CONTENT_W,
        BODY.color(palette::MUTED),
        &format!("{} sheet. {}", f.tier.label(), f.tier.summary()),
    );
    p.gap(2.0);
    p.para(
        MARGIN,
        CONTENT_W,
        BODY.color(palette::STAMP),
        "Every figure here is an estimate from a model of the street, not a record of what \
         any camera films. Read them as \"up to\": see what the figures leave out, below.",
    );
}

fn facts(p: &mut Pages, f: &Figures) {
    p.heading("Provenance and settings");
    let pv = &f.provenance;
    let unknown = || "unknown".to_owned();
    let params = &f.params;
    let limit = match &f.limit {
        None => "the whole loaded area".to_owned(),
        Some(a) => format!(
            "{} ({}, outline {} as drawn)",
            a.title,
            label_text(a.label),
            m2(a.polygon_m2)
        ),
    };
    let level = level_text(params.dori, params.pixels_per_metre());
    p.facts(&[
        ("Generated", format_utc_minute(f.generated_unix)),
        ("App and engine", format!("Observation {ENGINE_VERSION}")),
        (
            "OSM data as of",
            pv.osm_data_unix.map_or_else(unknown, format_utc_minute),
        ),
        (
            "Area built",
            pv.bundle_built_unix.map_or_else(unknown, format_utc_minute),
        ),
        (
            "OSM fingerprint",
            pv.osm_fingerprint.clone().unwrap_or_else(unknown),
        ),
        (
            "Camera catalogue",
            if pv.catalog_version.is_empty() {
                unknown()
            } else {
                pv.catalog_version.clone()
            },
        ),
        ("Caveats", format!("version {}", pv.caveats_version)),
        ("Detail level shown", level),
        ("Lighting shown", lighting_text(params.lighting).to_owned()),
        (
            "Subject height",
            format!("{} m above the ground", metres(params.subject_height_m)),
        ),
        ("Figures limited to", limit),
    ]);
}

fn map_panel(p: &mut Pages, f: &Figures, map: &MapPanel) {
    p.heading("Map: cameras and coverage");
    let size = map.max - map.min;
    if size.x <= 0.0 || size.y <= 0.0 {
        return;
    }
    let mut w = CONTENT_W;
    let mut h = w * size.y / size.x;
    if h > MAP_MAX_H {
        h = MAP_MAX_H;
        w = h * size.x / size.y;
    }
    let s = w / size.x;
    p.need(h + 60.0);
    let left = MARGIN + (CONTENT_W - w) / 2.0;
    let top = p.abs(p.y);
    let out = &mut p.out;
    let _ = writeln!(
        out,
        "<defs><clipPath id=\"map-clip\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath></defs>",
        num(left, 1),
        num(top, 1),
        num(w, 1),
        num(h, 1)
    );
    out.push_str("<g clip-path=\"url(#map-clip)\">\n");
    rect(out, [left, top, w, h], Some(palette::GROUND), None);
    let _ = writeln!(
        out,
        "<g transform=\"translate({} {}) scale({}) translate({} {})\">",
        num(left, 2),
        num(top, 2),
        num(s, 4),
        num(-map.min.x, 2),
        num(-map.min.y, 2)
    );
    // Roads, as bands of their width (in world metres, as the street is
    // counted).
    for (width, pts) in &map.roads {
        let _ = writeln!(
            out,
            "<polyline points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{}\" \
             stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",
            points(pts),
            hex(palette::ROAD),
            num(*width, 2)
        );
    }
    // Coverage, one path per overlap class, in cell units.
    let _ = writeln!(
        out,
        "<g transform=\"translate({} {}) scale({})\">",
        num(map.grid_min.x, 2),
        num(map.grid_min.y, 2),
        num(map.cell_m, 3)
    );
    for (runs, color) in map.coverage.iter().zip(palette::RAMP) {
        if runs.is_empty() {
            continue;
        }
        let _ = write!(
            out,
            "<path fill=\"{}\" fill-opacity=\"{}\" d=\"",
            hex(color),
            opacity(color)
        );
        for (row, col, n) in runs {
            let _ = write!(out, "M{col} {row}h{n}v1h-{n}z");
        }
        out.push_str("\"/>\n");
    }
    out.push_str("</g>\n");
    // Buildings over the coverage (walls block the ground behind them).
    for rings in &map.footprints {
        let _ = write!(out, "<path d=\"");
        for ring in rings {
            for (k, q) in ring.iter().enumerate() {
                let _ = write!(
                    out,
                    "{}{} {}",
                    if k == 0 { "M" } else { "L" },
                    num(q[0], 1),
                    num(q[1], 1)
                );
            }
            out.push('Z');
        }
        let _ = writeln!(
            out,
            "\" fill=\"{}\" fill-rule=\"evenodd\" stroke=\"{}\" stroke-width=\"0.75\" \
             vector-effect=\"non-scaling-stroke\"/>",
            hex(palette::PAPER),
            hex(palette::FRAME)
        );
    }
    for (label, pts) in &map.areas {
        let color = label.map_or(palette::INK, |l| palette::AREAS[l as usize]);
        let _ = writeln!(
            out,
            "<polygon points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"1.5\" \
             stroke-dasharray=\"5 3\" vector-effect=\"non-scaling-stroke\"/>",
            points(pts),
            hex(color)
        );
    }
    out.push_str("</g>\n");
    let to_svg = |q: [f32; 2]| (left + (q[0] - map.min.x) * s, top + (q[1] - map.min.y) * s);
    let halo = format!(
        " stroke=\"{}\" stroke-width=\"3\" paint-order=\"stroke\"",
        hex(palette::PAPER)
    );
    for (n, at) in &map.targets {
        let (x, y) = to_svg(*at);
        let _ = writeln!(
            out,
            "<rect x=\"{}\" y=\"{}\" width=\"7\" height=\"7\" fill=\"{}\" stroke=\"{}\" \
             transform=\"rotate(45 {} {})\"/>",
            num(x - 3.5, 1),
            num(y - 3.5, 1),
            hex(palette::PAPER),
            hex(palette::INK),
            num(x, 1),
            num(y, 1)
        );
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"{}\" font-size=\"8\"{halo}>T{n}</text>",
            num(x + 6.0, 1),
            num(y + 3.0, 1)
        );
    }
    for (n, at, yaw) in &map.cameras {
        let (x, y) = to_svg(*at);
        let (sin, cos) = yaw.to_radians().sin_cos();
        let _ = writeln!(
            out,
            "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"2\"/>",
            num(x, 1),
            num(y, 1),
            num(x + sin * 13.0, 1),
            num(y - cos * 13.0, 1),
            hex(palette::INK)
        );
        let _ = writeln!(
            out,
            "<circle cx=\"{}\" cy=\"{}\" r=\"4.5\" fill=\"{}\" stroke=\"{}\" stroke-width=\"1.5\"/>",
            num(x, 1),
            num(y, 1),
            hex(palette::INK),
            hex(palette::PAPER)
        );
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"{}\" font-size=\"9\" font-weight=\"bold\"{halo}>{n}</text>",
            num(x + 6.0, 1),
            num(y - 5.0, 1)
        );
    }
    // The stamps, inside the image so that cropping cannot remove them.
    let stamp = |out: &mut String, y: f32, s: &str| {
        let font = SMALL.bold().color(palette::STAMP);
        let tw = text_width(s, font.size) + 10.0;
        rect(
            out,
            [left + 6.0, y, tw, 15.0],
            Some(palette::PAPER),
            Some(palette::STAMP),
        );
        text(out, left + 11.0, y + 11.0, font, Anchor::Start, s);
    };
    stamp(
        out,
        top + 6.0,
        "ESTIMATE · modelled from map data, not recorded footage",
    );
    stamp(
        out,
        top + h - 21.0,
        "SHOWS CAMERA POSITIONS · for the owner's own use",
    );
    // North arrow.
    let (nx, ny) = (left + w - 18.0, top + 14.0);
    let _ = writeln!(
        out,
        "<path d=\"M{x} {y0}L{xr} {y1}L{x} {ym}L{xl} {y1}Z\" fill=\"{c}\"/>",
        x = num(nx, 1),
        y0 = num(ny, 1),
        xr = num(nx + 6.0, 1),
        xl = num(nx - 6.0, 1),
        y1 = num(ny + 18.0, 1),
        ym = num(ny + 13.0, 1),
        c = hex(palette::INK)
    );
    text(out, nx, ny + 30.0, SMALL.bold(), Anchor::Middle, "N");
    // Scale bar: the longest round length up to a quarter of the width.
    let len = [5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0]
        .into_iter()
        .rfind(|&l| l <= size.x / 4.0)
        .unwrap_or(5.0);
    let (bx, by) = (left + w - 12.0 - len * s, top + h - 14.0);
    rect(
        out,
        [bx, by, len * s, 4.0],
        Some(palette::INK),
        Some(palette::PAPER),
    );
    text(
        out,
        bx + len * s / 2.0,
        by - 3.0,
        SMALL.bold(),
        Anchor::Middle,
        &format!("{} m", num(len, 0)),
    );
    out.push_str("</g>\n");
    rect(out, [left, top, w, h], None, Some(palette::FRAME));
    p.y += h + 8.0;

    // Legend.
    let mut x = MARGIN;
    let y = p.abs(p.y);
    let item = |out: &mut String, x: &mut f32, color: [u8; 4], label: &str| {
        rect(out, [*x, y, 12.0, 9.0], Some(color), Some(palette::FRAME));
        text(out, *x + 16.0, y + 8.0, SMALL, Anchor::Start, label);
        *x += 22.0 + text_width(label, SMALL.size);
    };
    let level = level_text(f.params.dori, f.params.pixels_per_metre());
    let lead = format!("Seen at {level} {}:", lighting_text(f.params.lighting));
    text(&mut p.out, x, y + 8.0, SMALL.bold(), Anchor::Start, &lead);
    x += text_width(&lead, SMALL.size) + 8.0;
    for (k, color) in palette::RAMP.into_iter().enumerate() {
        let label = match k {
            0 => "1 camera".to_owned(),
            3 => "4 or more".to_owned(),
            n => format!("{}", n + 1),
        };
        item(&mut p.out, &mut x, color, &label);
    }
    item(&mut p.out, &mut x, palette::ROAD, "street");
    item(&mut p.out, &mut x, palette::PAPER, "building");
    p.gap(14.0);
    p.para(
        MARGIN,
        CONTENT_W,
        SMALL.color(palette::MUTED),
        &format!(
            "Numbered dots are cameras, with a line in the direction each faces; diamonds \
             are targets (T1, T2, …); dashed outlines are the areas as drawn, coloured by \
             label. North is up. Map data {} (ODbL). {}",
            f.osm_attribution, f.elevation_attribution
        ),
    );
}

fn points(pts: &[[f32; 2]]) -> String {
    let mut s = String::new();
    for (k, q) in pts.iter().enumerate() {
        if k > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{},{}", num(q[0], 1), num(q[1], 1));
    }
    s
}

fn camera_name(c: &CameraLine) -> String {
    match &c.label {
        Some(l) => format!("Camera {} · {l}", c.number),
        None => format!("Camera {}", c.number),
    }
}

fn cameras(p: &mut Pages, f: &Figures) {
    p.heading("Cameras");
    if f.cameras.is_empty() {
        p.para(
            MARGIN,
            CONTENT_W,
            BODY,
            "No camera is enabled in this scene.",
        );
        return;
    }
    let level = level_text(f.params.dori, f.params.pixels_per_metre());
    let mut note = format!(
        "{} enabled. Ranges are at {level}, from each model's published resolution and field \
         of view.",
        if f.cameras.len() == 1 {
            "One camera is".to_owned()
        } else {
            format!("{} cameras are", f.cameras.len())
        }
    );
    if f.tier.shows_positions() {
        note.push_str(" Labels are as the user typed them.");
    } else {
        note.push_str(" Their labels and positions are left out of this sheet.");
    }
    p.para(MARGIN, CONTENT_W, BODY, &note);
    p.gap(4.0);
    let rows: Vec<Vec<String>> = f
        .cameras
        .iter()
        .map(|c| {
            let night = if c.ir_published {
                format!("{} m", num(c.range_night_m, 1))
            } else {
                format!("{} m (infrared range assumed)", num(c.range_night_m, 1))
            };
            vec![
                camera_name(c),
                c.model.clone(),
                format!("{} m", num(c.range_day_m, 1)),
                night,
            ]
        })
        .collect();
    p.table(
        "Cameras",
        false,
        &[
            (200.0, Anchor::Start),
            (254.0, Anchor::Start),
            (110.0, Anchor::End),
            (150.0, Anchor::End),
        ],
        &["Camera", "Model", "Range by day", "Range at night"],
        &rows,
    );
}

fn seen_of(seen: f32, total: f32) -> String {
    format!(
        "{} of {}\n{}",
        thousands(f64::from(seen)),
        m2(total),
        percent(seen, total)
    )
}

fn grid(p: &mut Pages, f: &Figures) {
    p.heading("Detail × lighting");
    p.para(
        MARGIN,
        CONTENT_W,
        BODY,
        "What the enabled cameras together can see, at every detail level by day and by \
         night, with the area each figure is out of. No single figure stands for the whole: \
         the higher the detail asked for, the less ground any camera covers.",
    );
    p.gap(4.0);
    let cols = [
        (150.0, Anchor::Start),
        (204.0, Anchor::Start),
        (180.0, Anchor::End),
        (180.0, Anchor::End),
    ];
    let reading = |level: DoriLevel| {
        level
            .reading()
            .unwrap_or("the pixel density set by the user")
            .to_owned()
    };
    let level_cell = |level: DoriLevel, ppm: f32| {
        let mut s = level_text(level, ppm);
        if level == f.params.dori {
            s.push_str("\nshown in the app");
        }
        s
    };
    let rows = |pick: &dyn Fn(&GridCell) -> (f32, f32)| -> Vec<Vec<String>> {
        f.grid
            .rows
            .iter()
            .map(|r| {
                let (dseen, dtotal) = pick(&r.day);
                let (nseen, ntotal) = pick(&r.night);
                vec![
                    level_cell(r.level, r.ppm),
                    reading(r.level),
                    seen_of(dseen, dtotal),
                    seen_of(nseen, ntotal),
                ]
            })
            .collect()
    };
    let header = ["Detail level", "Footage there", "By day", "At night"];
    p.table(
        "Street seen by at least one camera (m² seen of the street in the figures)",
        true,
        &cols,
        &header,
        &rows(&|c| (c.stats.street_seen_m2, c.stats.street_m2)),
    );
    p.table(
        "Street seen by two or more cameras",
        true,
        &cols,
        &header,
        &rows(&|c| (c.stats.street_seen_multi_m2, c.stats.street_m2)),
    );
    p.table(
        "Building walls seen (m² seen of the walls in the figures)",
        true,
        &cols,
        &header,
        &rows(&|c| (c.stats.facade_seen_m2, c.stats.facade_m2)),
    );
    // The street by kind, at the level shown.
    if let Some(r) = f.grid.rows.iter().find(|r| r.level == f.params.dori) {
        let total = f.grid.street;
        let kind_row = |name: &str, pick: &dyn Fn(&super::StreetKinds) -> f32| {
            vec![
                name.to_owned(),
                m2(pick(&total)),
                seen_of(pick(&r.day.street_seen), pick(&total)),
                seen_of(pick(&r.night.street_seen), pick(&total)),
            ]
        };
        p.table(
            &format!(
                "The street by kind, at {} (see what “street” means)",
                level_text(r.level, r.ppm)
            ),
            true,
            &cols,
            &["Kind", "In the figures", "Seen by day", "Seen at night"],
            &[
                kind_row("Carriageway", &|k| k.carriageway_m2),
                kind_row("Footpath and service", &|k| k.footpath_service_m2),
            ],
        );
    }
    p.para(
        MARGIN,
        CONTENT_W,
        SMALL.color(palette::MUTED),
        CAVEATS_SUMMARY,
    );
}

/// `36` when moving the outlines changes nothing at this rounding, else
/// `36\n30–41`.
fn range_text(r: &SeenRange) -> String {
    let (low, high) = (r.low_m2.round(), r.high_m2.round());
    let drawn = thousands(f64::from(r.as_drawn_m2));
    if low == high {
        drawn
    } else {
        format!(
            "{drawn}\n{}–{}",
            thousands(f64::from(low)),
            thousands(f64::from(high))
        )
    }
}

fn footage(p: &mut Pages, f: &Figures) {
    p.heading("Where footage lands");
    let level = level_text(f.params.dori, f.params.pixels_per_metre());
    p.para(
        MARGIN,
        CONTENT_W,
        BODY,
        &format!(
            "Each camera's visible ground at {level} {}, split by the areas drawn and \
             labelled in the scene. Areas and labels are as drawn by the user, not property \
             boundaries. A label figure is given as drawn, with below it the range it takes \
             when every outline is moved 1 m in and 1 m out. Areas may overlap, so the \
             columns need not add up.",
            lighting_text(f.params.lighting)
        ),
    );
    p.gap(4.0);
    if f.splits.is_empty() {
        p.para(MARGIN, CONTENT_W, BODY, "No camera is enabled.");
        return;
    }
    let labels: Vec<AreaLabel> = AreaLabel::ALL
        .into_iter()
        .filter(|l| {
            f.splits
                .iter()
                .any(|(_, s)| s.labels.iter().any(|x| x.label == *l))
        })
        .collect();
    let mut header = vec!["Camera", "Ground seen", "On the street"];
    header.extend(labels.iter().map(|l| l.label()));
    header.extend(["Unlabelled ground", "Walls seen"]);
    let cols = columns(110.0, header.len() - 1);
    let names: Vec<String> = f.cameras.iter().map(camera_name).collect();
    let rows: Vec<Vec<String>> = f
        .splits
        .iter()
        .map(|(n, s)| {
            let mut row = vec![
                names.get(n - 1).cloned().unwrap_or_default(),
                thousands(f64::from(s.ground_m2)),
                thousands(f64::from(s.street_m2)),
            ];
            row.extend(labels.iter().map(|l| {
                s.labels
                    .iter()
                    .find(|x| x.label == *l)
                    .map_or_else(|| "–".to_owned(), |x| range_text(&x.seen))
            }));
            row.push(range_text(&s.unlabelled));
            row.push(thousands(f64::from(s.walls_m2)));
            row
        })
        .collect();
    p.table(
        "Ground and walls each camera sees, m² (labels as drawn)",
        true,
        &cols,
        &header,
        &rows,
    );
    if f.areas.is_empty() {
        p.para(
            MARGIN,
            CONTENT_W,
            BODY,
            "No area is drawn, so each camera's ground is split only into the street and the rest.",
        );
    } else {
        p.subheading("Areas as drawn (names as the user typed them)");
        for a in &f.areas {
            let seen: Vec<String> = f
                .splits
                .iter()
                .filter_map(|(n, s)| {
                    let x = s.areas.iter().find(|x| x.area_id == a.id)?;
                    (x.seen.high_m2 > 0.0).then(|| {
                        format!(
                            "{}: {} m² ({}–{})",
                            names.get(n - 1).cloned().unwrap_or_default(),
                            thousands(f64::from(x.seen.as_drawn_m2)),
                            thousands(f64::from(x.seen.low_m2)),
                            thousands(f64::from(x.seen.high_m2))
                        )
                    })
                })
                .collect();
            let seen = if seen.is_empty() {
                "no camera sees ground in it, even with the outline moved out.".to_owned()
            } else {
                format!("ground seen by {}.", seen.join("; "))
            };
            p.item(
                BODY,
                &format!(
                    "• {}: {}, outline {} as drawn; {seen}",
                    a.title,
                    label_text(a.label),
                    m2(a.polygon_m2)
                ),
            );
        }
    }
    if f.tier.shows_positions() {
        p.subheading("Walls seen per building (buildings by their number in the loaded map)");
        for (n, s) in &f.splits {
            if s.walls.is_empty() {
                continue;
            }
            let mut walls = s.walls.clone();
            walls.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
            let shown: Vec<String> = walls
                .iter()
                .take(6)
                .map(|w| {
                    format!(
                        "building {} at {} m: {}",
                        w.building,
                        num(w.distance_m, 0),
                        m2(w.seen_m2)
                    )
                })
                .collect();
            let more = walls.len().saturating_sub(6);
            let tail = if more > 0 {
                format!("; and {more} more")
            } else {
                String::new()
            };
            p.item(
                BODY,
                &format!(
                    "• {}: {}{tail}.",
                    names.get(n - 1).cloned().unwrap_or_default(),
                    shown.join("; ")
                ),
            );
        }
    }
}

fn mark(v: Verdict) -> String {
    match v {
        Verdict::NotRuledOut => "✓".to_owned(),
        Verdict::RuledOut(r) => format!("✗ ({})", r.text()),
    }
}

fn targets(p: &mut Pages, f: &Figures) {
    p.heading("Targets");
    if f.targets.is_empty() {
        p.para(
            MARGIN,
            CONTENT_W,
            BODY,
            "No target is marked in this scene.",
        );
        return;
    }
    let params = &f.params;
    let level = level_text(params.dori, params.pixels_per_metre());
    let when = lighting_text(params.lighting);
    p.para(
        MARGIN,
        CONTENT_W,
        BODY,
        "For each target, whether the model rules out that each camera sees it. ✓ not ruled \
         out: some of the target is visible in the model. ✗ ruled out, for the reason given. \
         The wording is one-sided because hedges, fences, trees and vehicles are not modelled \
         and can only block more. Target names are as the user typed them.",
    );
    let names: Vec<String> = f.cameras.iter().map(camera_name).collect();
    let name = |n: usize| names.get(n - 1).cloned().unwrap_or_default();
    for t in &f.targets {
        p.subheading(&format!(
            "{} ({})",
            t.title,
            if t.wall { "wall patch" } else { "ground" }
        ));
        if t.cameras.is_empty() {
            p.para(MARGIN, CONTENT_W, BODY, "No camera is enabled.");
            continue;
        }
        let open: Vec<String> = t
            .cameras
            .iter()
            .filter(|(_, v)| v.at(params.dori, params.lighting) == Some(Verdict::NotRuledOut))
            .map(|(n, _)| name(*n))
            .collect();
        let mut reasons: Vec<(Reason, usize)> = Vec::new();
        for (_, v) in &t.cameras {
            if let Some(Verdict::RuledOut(r)) = v.at(params.dori, params.lighting) {
                match reasons.iter_mut().find(|(x, _)| *x == r) {
                    Some((_, k)) => *k += 1,
                    None => reasons.push((r, 1)),
                }
            }
        }
        let mut s = format!("At {level} {when}: ");
        if open.is_empty() {
            s.push_str("ruled out for every camera");
        } else {
            let _ = write!(s, "not ruled out for {}", open.join(", "));
        }
        if !reasons.is_empty() && !open.is_empty() {
            let ruled: usize = reasons.iter().map(|(_, k)| k).sum();
            let _ = write!(s, "; ruled out for {ruled}");
        }
        if !reasons.is_empty() {
            let why: Vec<String> = reasons
                .iter()
                .map(|(r, k)| format!("{} ({k})", r.text()))
                .collect();
            let _ = write!(s, ": {}", why.join(", "));
        }
        s.push('.');
        p.item(BODY, &s);
        for (n, v) in &t.cameras {
            if !v.verdicts.iter().any(|x| x.verdict == Verdict::NotRuledOut) {
                continue;
            }
            let ladder = |lighting: Lighting| -> String {
                v.verdicts
                    .iter()
                    .filter(|x| x.lighting == lighting)
                    .map(|x| format!("{} {}", x.level.label(), mark(x.verdict)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            p.item(
                SMALL,
                &format!(
                    "• {}: by day {}; at night {}.",
                    name(*n),
                    ladder(Lighting::Day),
                    ladder(Lighting::Night)
                ),
            );
        }
    }
}
