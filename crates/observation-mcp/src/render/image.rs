//! The map's picture: the same view and layers as the character grid, in
//! colour, at most [`IMAGE_LONG_SIDE_PX`] pixels on its longer side.

use super::raster::{Canvas, text_height, text_width};
use super::{IMAGE_LONG_SIDE_PX, Layer, MapModel, metres, short_metres};
use observation_core::glam::Vec2;

/// Every colour of the picture, from the design tokens: the coverage ramp
/// and the area outlines are the app's and the evidence sheet's, so the
/// picture reads like the screen. Nothing else in this module names a
/// colour.
mod palette {
    use observation_tokens::{AREA_COLORS_LIGHT, COVERAGE_RAMP, LIGHT, Rgba};

    pub const GROUND: Rgba = LIGHT.surface;
    /// Beyond the area's data.
    pub const OUTSIDE: Rgba = LIGHT.surface_sunken;
    pub const ROAD: Rgba = LIGHT.border;
    pub const BUILDING: Rgba = LIGHT.border_strong;
    pub const BUILDING_EDGE: Rgba = LIGHT.text_muted;
    pub const INK: Rgba = LIGHT.text;
    pub const MUTED: Rgba = LIGHT.text_muted;
    pub const HALO: Rgba = LIGHT.surface;
    pub const CAMERA: Rgba = LIGHT.accent;
    pub const PINNED: Rgba = LIGHT.danger;
    /// Ground seen by 1, 2, 3 and 4 or more cameras.
    pub const RAMP: [Rgba; 4] = COVERAGE_RAMP;
    /// Area outlines by label (Own, Neighbour, Public, Other); unlabelled
    /// areas are drawn in [`INK`].
    pub const AREAS: [Rgba; 4] = AREA_COLORS_LIGHT;
}

/// Font scale of every label: 10 × 14 pixel characters.
const SCALE: i64 = 2;

/// Draws the picture; returns it with its metres per pixel.
pub fn draw(m: &MapModel) -> (Canvas, f32) {
    let v = m.view;
    let (w_m, h_m) = (v.columns as f32 * v.cell_m, v.rows as f32 * v.cell_m);
    let px_per_m = IMAGE_LONG_SIDE_PX as f32 / w_m.max(h_m);
    let width = ((w_m * px_per_m).round() as u32).clamp(1, IMAGE_LONG_SIDE_PX);
    let height = ((h_m * px_per_m).round() as u32).clamp(1, IMAGE_LONG_SIDE_PX);
    let to_px = |p: [f32; 2]| [(p[0] - v.min[0]) * px_per_m, (p[1] - v.min[1]) * px_per_m];
    let mut c = Canvas::new(width, height, palette::GROUND);

    // Ground: the street and the coverage, from the area's raster.
    let grid = m.layers.grid();
    let street = m.layers.street_mask();
    for y in 0..height {
        for x in 0..width {
            let world = Vec2::new(
                v.min[0] + (x as f32 + 0.5) / px_per_m,
                v.min[1] + (y as f32 + 0.5) / px_per_m,
            );
            let (xi, yi) = (i64::from(x), i64::from(y));
            let Some(i) = grid.cell_at(world) else {
                c.blend(xi, yi, palette::OUTSIDE);
                continue;
            };
            if m.shows(Layer::Streets) && street[i] {
                c.blend(xi, yi, palette::ROAD);
            }
            if let Some(n) = m.seen.as_ref().map(|s| s[i]).filter(|&n| n > 0) {
                c.blend(xi, yi, palette::RAMP[usize::from(n.min(4)) - 1]);
            }
        }
    }

    if m.shows(Layer::Buildings) {
        let (vmin, vmax) = (v.min, v.max());
        for b in &m.bundle.buildings {
            let fp = &b.footprint;
            let (lo, hi) = bounds(&fp.outer);
            if hi[0] < vmin[0] || hi[1] < vmin[1] || lo[0] > vmax[0] || lo[1] > vmax[1] {
                continue;
            }
            let rings: Vec<Vec<[f32; 2]>> = std::iter::once(&fp.outer)
                .chain(&fp.holes)
                .map(|r| r.iter().map(|&p| to_px(p)).collect())
                .collect();
            c.fill_polygon(&rings, palette::BUILDING);
            for ring in &rings {
                for k in 0..ring.len() {
                    c.line(
                        ring[k],
                        ring[(k + 1) % ring.len()],
                        1.0,
                        palette::BUILDING_EDGE,
                    );
                }
            }
        }
    }

    if m.shows(Layer::Areas) {
        for a in &m.areas {
            let color = a.label.map_or(palette::INK, |l| palette::AREAS[l as usize]);
            let pts: Vec<[f32; 2]> = a.points.iter().map(|&p| to_px(p)).collect();
            let n = pts.len();
            let edges = if n >= 3 { n } else { n - 1 };
            for k in 0..edges {
                c.dashed_line(pts[k], pts[(k + 1) % n], 3.0, 9.0, color);
            }
        }
    }

    if m.shows(Layer::Targets) {
        for t in &m.targets {
            let p = to_px(t.at);
            let r = 8.0;
            let diamond = vec![
                [p[0], p[1] - r],
                [p[0] + r, p[1]],
                [p[0], p[1] + r],
                [p[0] - r, p[1]],
            ];
            c.fill_polygon(std::slice::from_ref(&diamond), palette::HALO);
            for k in 0..4 {
                c.line(diamond[k], diamond[(k + 1) % 4], 2.0, palette::INK);
            }
            label(&mut c, p, &format!("T{}", t.id), palette::INK);
        }
    }

    if m.shows(Layer::SharedPoint)
        && let Some(at) = m.pinned
    {
        let p = to_px(at);
        c.disc(p, 10.0, palette::PINNED);
        c.disc(p, 6.5, palette::HALO);
        c.disc(p, 3.0, palette::PINNED);
    }

    if m.shows(Layer::Cameras) {
        for cam in &m.cameras {
            let p = to_px(cam.at);
            let (sin, cos) = cam.yaw_deg.to_radians().sin_cos();
            // North is up, so a bearing turns clockwise from straight up.
            let tip = [p[0] + sin * 28.0, p[1] - cos * 28.0];
            // Cameras the coverage leaves out are grey.
            let color = if cam.not_counted.is_some() {
                palette::MUTED
            } else {
                palette::CAMERA
            };
            c.line(p, tip, 5.0, palette::HALO);
            c.line(p, tip, 3.0, color);
            c.disc(p, 7.0, palette::HALO);
            c.disc(p, 5.0, color);
            label(&mut c, p, &cam.glyph.to_string(), palette::INK);
        }
    }

    ruler(&mut c, m, px_per_m);
    north_and_scale(&mut c, m, px_per_m);
    (c, 1.0 / px_per_m)
}

fn bounds(ring: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    ring.iter().fold(
        ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]),
        |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1])],
                [hi[0].max(p[0]), hi[1].max(p[1])],
            )
        },
    )
}

/// A mark's label, above and to the right of it.
fn label(c: &mut Canvas, p: [f32; 2], s: &str, color: observation_tokens::Rgba) {
    let (x, y) = (
        p[0].round() as i64 + 9,
        p[1].round() as i64 - 9 - text_height(SCALE),
    );
    c.text(x, y, s, SCALE, color, palette::HALO);
}

/// Tick marks and labels along the top (x) and left (z) edges, at the text
/// map's ticks.
fn ruler(c: &mut Canvas, m: &MapModel, px_per_m: f32) {
    let v = m.view;
    let tick_px = |i: usize| i as f32 * v.cell_m * px_per_m;
    let edge = text_height(SCALE) + 8;
    for (col, x) in v.ticks(0) {
        let px = tick_px(col);
        c.line([px, 0.0], [px, 8.0], 2.0, palette::MUTED);
        c.text(
            px.round() as i64 + 4,
            4,
            &metres(x),
            SCALE,
            palette::MUTED,
            palette::HALO,
        );
    }
    for (row, z) in v.ticks(1) {
        let py = tick_px(row);
        if (py as i64) < edge {
            continue;
        }
        c.line([0.0, py], [8.0, py], 2.0, palette::MUTED);
        c.text(
            4,
            py.round() as i64 + 4,
            &metres(z),
            SCALE,
            palette::MUTED,
            palette::HALO,
        );
    }
}

/// A north arrow in the top right corner and a scale bar of one tick step
/// in the bottom right.
fn north_and_scale(c: &mut Canvas, m: &MapModel, px_per_m: f32) {
    let (w, h) = (c.width as f32, c.height as f32);
    let (nx, ny) = (w - 24.0, 34.0);
    let arrow = vec![
        [nx, ny],
        [nx + 9.0, ny + 26.0],
        [nx, ny + 19.0],
        [nx - 9.0, ny + 26.0],
    ];
    c.fill_polygon(std::slice::from_ref(&arrow), palette::INK);
    let n_w = text_width("N", SCALE);
    c.text(
        nx as i64 - n_w / 2,
        ny as i64 - text_height(SCALE) - 6,
        "N",
        SCALE,
        palette::INK,
        palette::HALO,
    );
    let len_m = m.view.tick_m();
    let len_px = len_m * px_per_m;
    if len_px + 24.0 > w {
        return;
    }
    let (x1, y) = (w - 12.0, h - 14.0);
    let x0 = x1 - len_px;
    c.line([x0, y], [x1, y], 7.0, palette::HALO);
    c.line([x0, y], [x1, y], 4.0, palette::INK);
    let text = format!("{} m", short_metres(len_m));
    let tw = text_width(&text, SCALE);
    c.text(
        ((x0 + x1) / 2.0) as i64 - tw / 2,
        y as i64 - 8 - text_height(SCALE),
        &text,
        SCALE,
        palette::INK,
        palette::HALO,
    );
}
