//! The character map: a north-up grid with a coordinate ruler, and its
//! legend, worded for small models (every glyph spelt out, no addresses).

use super::{
    BUILDING, CameraKeyOut, GROUND_SEEN, Layer, LegendOut, MapModel, PINNED, RenderOut, STREET,
    STREET_SEEN, TARGET, compass_word, label_word, metres, short_metres,
};
use observation_core::camera::Lighting;
use observation_core::footage::AreaLabel;
use std::fmt::Write as _;

/// The glyph of open ground no camera reaches (or with coverage off).
const OPEN: char = ' ';

/// The grid's cells, row by row from the north, and whether each camera's
/// glyph made it onto the map.
#[derive(Debug, Clone)]
pub struct Grid {
    pub cells: Vec<Vec<char>>,
    pub camera_on_map: Vec<bool>,
}

impl Grid {
    fn has(&self, c: char) -> bool {
        self.cells.iter().flatten().any(|&g| g == c)
    }
}

/// The ground cells of the area's raster that fall in one map cell.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    n: u32,
    building: u32,
    street: u32,
    street_seen: u32,
    /// Seen cells that are not building.
    seen: u32,
}

fn tallies(m: &MapModel) -> Vec<Tally> {
    let v = m.view;
    let g = m.layers.grid();
    let mut out = vec![Tally::default(); v.columns * v.rows];
    let span = |lo: f32, hi: f32, gmin: f32, n: u32| {
        let a = ((lo - gmin) / g.cell_m).floor().max(0.0) as usize;
        let b = (((hi - gmin) / g.cell_m).ceil().max(0.0) as usize).min(n as usize);
        a..b
    };
    let vmax = v.max();
    let (building, street) = (m.layers.building_mask(), m.layers.street_mask());
    for gz in span(v.min[1], vmax[1], g.min.y, g.h) {
        for gx in span(v.min[0], vmax[0], g.min.x, g.w) {
            let i = gz * g.w as usize + gx;
            let c = g.center(i);
            let Some((col, row)) = v.cell_of([c.x, c.y]) else {
                continue;
            };
            let t = &mut out[row * v.columns + col];
            let seen = m.seen.as_ref().is_some_and(|s| s[i] > 0);
            t.n += 1;
            if building[i] {
                t.building += 1;
                continue;
            }
            if seen {
                t.seen += 1;
            }
            if street[i] {
                t.street += 1;
                if seen {
                    t.street_seen += 1;
                }
            }
        }
    }
    out
}

/// A cell's glyph from its ground: building where half of it is, street
/// where a fifth of it is (streets are narrow), and the seen glyphs where
/// at least half of that street, or of the open ground, is not ruled out.
fn ground_glyph(m: &MapModel, t: &Tally) -> char {
    if t.n == 0 {
        return OPEN;
    }
    if m.shows(Layer::Buildings) && t.building * 2 >= t.n {
        return BUILDING;
    }
    if m.shows(Layer::Streets) && t.street > 0 && t.street * 5 >= t.n {
        return if t.street_seen * 2 >= t.street && m.seen.is_some() {
            STREET_SEEN
        } else {
            STREET
        };
    }
    let open = t.n - t.building;
    if open > 0 && t.seen > 0 && t.seen * 2 >= open {
        return GROUND_SEEN;
    }
    OPEN
}

/// The map's cells: ground first, then area outlines, targets, the shared
/// point and the cameras on top.
pub fn grid(m: &MapModel) -> Grid {
    let v = m.view;
    let tallies = tallies(m);
    let mut cells: Vec<Vec<char>> = (0..v.rows)
        .map(|r| {
            (0..v.columns)
                .map(|c| ground_glyph(m, &tallies[r * v.columns + c]))
                .collect()
        })
        .collect();
    let mut put = |p: [f32; 2], g: char| {
        if let Some((c, r)) = v.cell_of(p) {
            cells[r][c] = g;
        }
    };
    if m.shows(Layer::Areas) {
        for a in &m.areas {
            let n = a.points.len();
            let edges = if n >= 3 { n } else { n - 1 };
            for k in 0..edges {
                let (p, q) = (a.points[k], a.points[(k + 1) % n]);
                let len = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
                let steps = ((len / (v.cell_m / 3.0)).ceil() as usize).max(1);
                for s in 0..=steps {
                    let t = s as f32 / steps as f32;
                    put(
                        [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t],
                        a.glyph,
                    );
                }
            }
        }
    }
    if m.shows(Layer::Targets) {
        for t in &m.targets {
            put(t.at, TARGET);
        }
    }
    if m.shows(Layer::SharedPoint)
        && let Some(p) = m.pinned
    {
        put(p, PINNED);
    }
    let mut camera_on_map = vec![false; m.cameras.len()];
    if m.shows(Layer::Cameras) {
        let mut taken: Vec<(usize, usize)> = Vec::new();
        for (k, c) in m.cameras.iter().enumerate() {
            let Some(cell) = v.cell_of(c.at) else {
                continue;
            };
            if taken.contains(&cell) {
                continue;
            }
            taken.push(cell);
            cells[cell.1][cell.0] = c.glyph;
            camera_on_map[k] = true;
        }
    }
    Grid {
        cells,
        camera_on_map,
    }
}

fn at(p: [f32; 2]) -> String {
    format!("[{}, {}]", metres(p[0]), metres(p[1]))
}

fn lighting_words(l: Lighting) -> &'static str {
    match l {
        Lighting::Day => "by day",
        Lighting::Night => "at night",
    }
}

/// "at Detect by day".
fn settings_words(m: &MapModel) -> String {
    format!(
        "at {} {}",
        m.params.dori.label(),
        lighting_words(m.params.lighting)
    )
}

fn quoted(name: &str) -> String {
    if name.is_empty() {
        String::new()
    } else {
        format!(" \"{name}\"")
    }
}

/// Why a mark at `p` drawn with `glyph` does not show, if it does not:
/// " (outside the map)" or " (under the glyph 1)".
fn hidden(m: &MapModel, g: &Grid, p: [f32; 2], glyph: char) -> String {
    match m.view.cell_of(p) {
        None => " (outside the map)".to_owned(),
        Some((c, r)) if g.cells[r][c] != glyph => format!(" (under the glyph {})", g.cells[r][c]),
        Some(_) => String::new(),
    }
}

/// What each camera's glyph is, and where.
pub fn camera_keys(m: &MapModel, g: &Grid) -> Vec<CameraKeyOut> {
    m.cameras
        .iter()
        .zip(&g.camera_on_map)
        .map(|(c, &on_map)| CameraKeyOut {
            glyph: c.glyph.to_string(),
            camera_id: c.id,
            camera_label: c.label.clone(),
            at: c.at,
            bearing_deg: c.yaw_deg.rem_euclid(360.0),
            faces: compass_word(c.yaw_deg).to_owned(),
            on_map,
            not_counted: c.not_counted.map(str::to_owned),
        })
        .collect()
}

fn camera_meaning(m: &MapModel, g: &Grid, k: usize) -> String {
    let c = &m.cameras[k];
    let mut s = format!(
        "camera {}{} at {}, faces {} ({:.0}°)",
        c.id,
        quoted(&c.label),
        at(c.at),
        compass_word(c.yaw_deg),
        c.yaw_deg.rem_euclid(360.0)
    );
    if let Some(why) = c.not_counted {
        let _ = write!(s, "; {why}, so not counted");
    }
    if m.view.cell_of(c.at).is_none() {
        s.push_str("; outside the map");
    } else if !g.camera_on_map[k] {
        let first = m
            .cameras
            .iter()
            .zip(&g.camera_on_map)
            .find(|(o, on)| **on && m.view.cell_of(o.at) == m.view.cell_of(c.at))
            .map(|(o, _)| o.glyph);
        if let Some(gl) = first {
            let _ = write!(s, "; in the same cell as {gl}");
        }
    }
    s
}

fn area_meaning(m: &MapModel, label: Option<AreaLabel>) -> String {
    let list: Vec<String> = m
        .areas
        .iter()
        .filter(|a| a.label == label)
        .map(|a| {
            let mut s = format!("area {}", a.id);
            if let Some(n) = &a.name {
                s.push_str(&quoted(n));
            }
            if !a.enabled {
                s.push_str(" (disabled)");
            }
            if a.limits_figures {
                s.push_str(" (limits the figures)");
            }
            s
        })
        .collect();
    let what = match label {
        Some(_) => format!("outline of an area labelled {}", label_word(label)),
        None => "outline of an unlabelled area".to_owned(),
    };
    format!("{what}: {}", list.join(", "))
}

/// Every glyph on the map and what it means, ground glyphs first, then
/// areas, targets, the shared point and each camera.
pub fn legend(m: &MapModel, g: &Grid) -> Vec<LegendOut> {
    let mut out = Vec::new();
    let mut add = |glyph: String, meaning: String| out.push(LegendOut { glyph, meaning });
    let at_settings = settings_words(m);
    let covered = m.seen.is_some();
    if g.has(BUILDING) {
        add(BUILDING.into(), "building".into());
    }
    if g.has(STREET) {
        let what = "street, driveway or footpath as mapped";
        add(
            STREET.into(),
            if covered {
                format!("{what}, that no enabled camera sees {at_settings}")
            } else {
                what.to_owned()
            },
        );
    }
    if g.has(STREET_SEEN) {
        add(
            STREET_SEEN.into(),
            format!(
                "street, driveway or footpath that some enabled camera is not ruled out from seeing {at_settings}"
            ),
        );
    }
    if g.has(GROUND_SEEN) {
        add(
            GROUND_SEEN.into(),
            format!("other ground some enabled camera is not ruled out from seeing {at_settings}"),
        );
    }
    if g.has(OPEN) {
        add(
            "(space)".into(),
            if covered {
                "other ground, no camera reaches it".into()
            } else {
                "other ground".into()
            },
        );
    }
    for label in [
        Some(AreaLabel::Own),
        Some(AreaLabel::Neighbour),
        Some(AreaLabel::Public),
        Some(AreaLabel::Other),
        None,
    ] {
        let glyph = super::area_glyph(label);
        if g.has(glyph) {
            add(glyph.into(), area_meaning(m, label));
        }
    }
    if m.shows(Layer::Targets) && !m.targets.is_empty() {
        let list: Vec<String> = m
            .targets
            .iter()
            .map(|t| {
                format!(
                    "{}{} at {}{}",
                    t.id,
                    quoted(&t.name),
                    at(t.at),
                    hidden(m, g, t.at, TARGET)
                )
            })
            .collect();
        add(TARGET.into(), format!("target: {}", list.join("; ")));
    }
    if m.shows(Layer::SharedPoint)
        && let Some(p) = m.pinned
    {
        add(
            PINNED.into(),
            format!(
                "the point the user shared, at {}{}",
                at(p),
                hidden(m, g, p, PINNED)
            ),
        );
    }
    if m.shows(Layer::Cameras) {
        for (k, c) in m.cameras.iter().enumerate() {
            add(c.glyph.into(), camera_meaning(m, g, k));
        }
    }
    out
}

/// Whether a legend entry is about the ground (the grid's own glyphs), not
/// a mark the picture shares.
fn ground_entry(e: &LegendOut) -> bool {
    [BUILDING, STREET, STREET_SEEN, GROUND_SEEN]
        .iter()
        .any(|c| e.glyph == c.to_string())
        || e.glyph == "(space)"
}

/// The rows of the grid with the ruler: x labels on top, z labels on the
/// left, each at the column or row whose cells start at that value.
fn ruled(m: &MapModel, g: &Grid) -> String {
    let v = m.view;
    let xt = v.ticks(0);
    let zt = v.ticks(1);
    let w = zt
        .iter()
        .map(|(_, z)| metres(*z).len())
        .max()
        .unwrap_or(0)
        .max(1);
    let pad = w + 2;
    let mut out = String::new();
    // x labels, left-aligned on their column; one that would run into the
    // last is dropped.
    let mut top: Vec<char> = format!("{:>w$}  ", "x").chars().collect();
    let mut marks: Vec<char> = " ".repeat(pad).chars().collect();
    for &(col, x) in &xt {
        let at = pad + col;
        let label = metres(x);
        if top.len() > at {
            continue;
        }
        top.resize(at, ' ');
        top.extend(label.chars());
        top.push(' ');
        marks.resize(at, ' ');
        marks.push('v');
    }
    let _ = writeln!(out, "{}", top.iter().collect::<String>().trim_end());
    let _ = writeln!(out, "{}", marks.iter().collect::<String>().trim_end());
    for (r, row) in g.cells.iter().enumerate() {
        let lead = match zt.iter().find(|(i, _)| *i == r) {
            Some((_, z)) => format!("{:>w$} >", metres(*z)),
            None => " ".repeat(pad),
        };
        let line: String = row.iter().collect();
        let _ = writeln!(out, "{}", format!("{lead}{line}").trim_end());
    }
    out
}

/// What the picture's colours mean.
const PICTURE_KEY: &str = "Picture: north up, same area as the grid. Dark grey: buildings. \
    Light grey: street, driveways and footpaths as mapped. Yellow, orange, red and purple \
    shading: ground 1, 2, 3, and 4 or more enabled cameras are not ruled out from seeing. \
    Blue dots: cameras, labelled with their glyphs, with a line the way each faces (grey: \
    not counted). White \
    diamonds: targets (T and the target id). Dashed outlines: areas as drawn (green own, \
    blue neighbour, light blue public, violet other, black unlabelled). Red ring: the point \
    the user shared. Ruler labels along the top (x) and left (z) edges, in metres.";

/// The text block: the header, the grid (when `with_grid`), the legend and
/// the sources.
pub fn document(m: &MapModel, g: &Grid, out: &RenderOut, with_grid: bool) -> String {
    let v = m.view;
    let max = v.max();
    let cell = short_metres(v.cell_m);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "Map from above, north up: x grows east (right), z grows south (down), in metres."
    );
    let _ = writeln!(
        s,
        "One character is one {cell} m x {cell} m cell. Shown: x {} to {}, z {} to {} ({} columns x {} rows).",
        metres(v.min[0]),
        metres(max[0]),
        metres(v.min[1]),
        metres(max[1]),
        v.columns,
        v.rows
    );
    let _ = writeln!(
        s,
        "Scale: {} characters = {} m. The ruler labels the column (top, v) or row (left, >) whose cells start at that value.",
        super::TICK_CELLS,
        short_metres(v.tick_m())
    );
    if let Some(c) = &out.coverage {
        let _ = writeln!(
            s,
            "Coverage {} ({} px/m), for people tested {} m above the ground.",
            settings_words(m),
            short_metres(c.ppm.round()),
            short_metres(c.subject_height_m)
        );
    }
    if with_grid {
        s.push('\n');
        s.push_str(&ruled(m, g));
    }
    if out.image.is_some() {
        s.push('\n');
        let _ = writeln!(s, "{PICTURE_KEY}");
    }
    s.push('\n');
    let _ = writeln!(s, "Legend:");
    for e in out.legend.iter().filter(|e| with_grid || !ground_entry(e)) {
        let _ = writeln!(s, "  {:<7} {}", e.glyph, e.meaning);
    }
    s.push('\n');
    match &out.osm_data_as_of {
        Some(d) => {
            let _ = writeln!(s, "OpenStreetMap data as of {d}.");
        }
        None => {
            let _ = writeln!(s, "OpenStreetMap data date unknown.");
        }
    }
    if let Some(c) = &out.caveats_summary {
        let _ = writeln!(s, "Estimate. {c}");
    }
    s
}
