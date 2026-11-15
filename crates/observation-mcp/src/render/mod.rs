//! A top-down picture of a scene for agents (`render_map`): a north-up
//! character grid that any model can read, and on request a PNG of the same
//! layers for models that take images.
//!
//! Both are drawn from one [`MapModel`]: the view (a grid of square cells
//! of a round size, at most [`MAX_COLUMNS`] × [`MAX_ROWS`]), the area's
//! ground raster (buildings and the mapped street, as the coverage figures
//! count them), how many enabled cameras do not rule out each ground cell
//! (the line-of-sight pass of `footage::seen_ground`, which `find_gaps`
//! uses, counted per camera), and the
//! scene's cameras, labelled areas, targets and the point the user shared.
//! Buildings are drawn without a name or number: they are never named by
//! address.

mod geom;
mod image;
mod raster;
mod text;

#[cfg(test)]
mod tests;

use crate::ToolError;
use crate::area::{self, bundle_for, check_inside, layers_for};
use crate::catalog::catalog;
use crate::edit::{self, Dori, Light};
use crate::queries::{Settings, off_runtime, skipped, with_overrides};
use crate::schema::JsonObject;
use crate::server::{ObservationMcp, SkippedOut};
use base64::Engine as _;
use observation_core::bundle::AreaBundle;
use observation_core::coverage::{CoverageParams, StaticLayers};
use observation_core::footage::{AreaLabel, FootageCamera, TargetPlace};
use observation_core::frustum::ViewCone;
use observation_core::provenance::{CAVEATS_SUMMARY, format_date};
use observation_core::scene::{AreaRef, SceneFile};
use rmcp::handler::server::common::schema_for_output;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Most columns the text map has.
pub const MAX_COLUMNS: usize = 80;
/// Most rows the text map has.
pub const MAX_ROWS: usize = 40;
/// Half the side of the square shown around a `center` given without a
/// radius, metres.
pub const DEFAULT_RADIUS_M: f32 = 60.0;
/// The smallest and largest radius accepted, metres.
pub const MIN_RADIUS_M: f32 = 10.0;
pub const MAX_RADIUS_M: f32 = 2000.0;
/// The cell sizes a map may use, metres: the smallest that fits is chosen.
/// Each is a round number, and so is ten of it (the ruler's step). The
/// ground raster's own cells are 0.5 m.
const CELL_SIZES_M: [f32; 25] = [
    0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 7.5, 8.0, 10.0, 12.5, 15.0, 20.0, 25.0, 30.0,
    40.0, 50.0, 75.0, 100.0, 150.0, 200.0, 250.0, 500.0,
];
/// Cells between ruler ticks (so a tick every 10 cells, a round number of
/// metres for every cell size).
pub const TICK_CELLS: usize = 10;
/// The picture's longer side, pixels.
pub const IMAGE_LONG_SIDE_PX: u32 = 1024;
/// Camera glyphs: camera id 1 is `1`, id 10 is `A`, and so on to id 34,
/// `Z`. `T` is left out: it marks targets.
const CAMERA_GLYPHS: &[u8] = b"123456789ABCDEFGHIJKLMNOPQRSUVWXYZ";
/// The glyph of every camera once the glyphs run out.
const MORE_CAMERAS_GLYPH: char = '+';

pub const BUILDING: char = '#';
pub const STREET: char = '=';
pub const STREET_SEEN: char = '.';
pub const GROUND_SEEN: char = ',';
pub const TARGET: char = 'T';
pub const PINNED: char = '@';
/// A cell with no map data: beyond the area's ground raster.
pub const NO_DATA: char = '~';

/// The grid a map is drawn on: `columns` × `rows` square cells of `cell_m`
/// metres, the first with its north-west corner at `min`. Columns run east
/// (x grows), rows run south (z grows), so north is up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub min: [f32; 2],
    pub cell_m: f32,
    pub columns: usize,
    pub rows: usize,
}

impl View {
    /// The south-east corner.
    pub fn max(&self) -> [f32; 2] {
        [
            self.min[0] + self.columns as f32 * self.cell_m,
            self.min[1] + self.rows as f32 * self.cell_m,
        ]
    }

    /// Metres between ruler ticks.
    pub fn tick_m(&self) -> f32 {
        self.cell_m * TICK_CELLS as f32
    }

    /// The `(column, row)` of the cell holding world `[x, z]`, if on the map.
    pub fn cell_of(&self, p: [f32; 2]) -> Option<(usize, usize)> {
        let c = ((p[0] - self.min[0]) / self.cell_m).floor();
        let r = ((p[1] - self.min[1]) / self.cell_m).floor();
        let inside = c >= 0.0 && r >= 0.0 && c < self.columns as f32 && r < self.rows as f32;
        inside.then_some((c as usize, r as usize))
    }

    /// The ruler along x (`axis` 0) or z (1): each tick's column or row and
    /// the coordinate where that column's or row's cells start, every
    /// [`Self::tick_m`] metres on round values.
    pub fn ticks(&self, axis: usize) -> Vec<(usize, f32)> {
        let step = self.tick_m();
        let n = if axis == 0 { self.columns } else { self.rows };
        let lo = self.min[axis];
        let first = (lo / step).ceil() as i64;
        (first..)
            .map(|k| k as f32 * step)
            .map(|t| (((t - lo) / self.cell_m).round() as usize, t))
            .take_while(|&(i, _)| i < n)
            .collect()
    }
}

/// The view for the area spanning `area` (`(min, max)`, world `[x, z]`):
/// the square of half-side `radius_m` around `center`, cut to the area,
/// with the smallest round cell size that keeps the grid within
/// [`MAX_COLUMNS`] × [`MAX_ROWS`]. Without a centre the map is centred on
/// the area; without a radius it shows [`DEFAULT_RADIUS_M`] around a centre
/// given, or else the whole area. The grid's edges fall on multiples of the
/// cell size, so it may reach a little past the area.
pub fn choose_view(
    area: ([f32; 2], [f32; 2]),
    center: Option<[f32; 2]>,
    radius_m: Option<f32>,
) -> Result<View, ToolError> {
    let (amin, amax) = area;
    if let Some(r) = radius_m
        && !(r.is_finite() && (MIN_RADIUS_M..=MAX_RADIUS_M).contains(&r))
    {
        return Err(ToolError::invalid(format!(
            "radius_m {r} is out of range: use {MIN_RADIUS_M:.0} to {MAX_RADIUS_M:.0} m"
        )));
    }
    let around = |c: [f32; 2], r: f32| {
        (
            [(c[0] - r).max(amin[0]), (c[1] - r).max(amin[1])],
            [(c[0] + r).min(amax[0]), (c[1] + r).min(amax[1])],
        )
    };
    let middle = [(amin[0] + amax[0]) / 2.0, (amin[1] + amax[1]) / 2.0];
    let (lo, hi) = match (center, radius_m) {
        (Some(c), r) => around(c, r.unwrap_or(DEFAULT_RADIUS_M)),
        (None, Some(r)) => around(middle, r),
        (None, None) => (amin, amax),
    };
    let (w, h) = ((hi[0] - lo[0]).max(0.0), (hi[1] - lo[1]).max(0.0));
    let needed = (w / MAX_COLUMNS as f32).max(h / MAX_ROWS as f32);
    for cell_m in CELL_SIZES_M.into_iter().filter(|&c| c >= needed) {
        let min = [
            (lo[0] / cell_m).floor() * cell_m,
            (lo[1] / cell_m).floor() * cell_m,
        ];
        let columns = (((hi[0] - min[0]) / cell_m).ceil() as usize).max(1);
        let rows = (((hi[1] - min[1]) / cell_m).ceil() as usize).max(1);
        if columns <= MAX_COLUMNS && rows <= MAX_ROWS {
            return Ok(View {
                min,
                cell_m,
                columns,
                rows,
            });
        }
    }
    Err(ToolError::invalid(
        "the area is too large to draw: give a center and a smaller radius_m",
    ))
}

/// The layers a map can show besides coverage (which `coverage` switches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Buildings,
    Streets,
    Cameras,
    Targets,
    /// Outlines of the areas the user drew.
    Areas,
    /// The point the user shared in the connected app.
    SharedPoint,
}

impl Layer {
    pub const ALL: [Self; 6] = [
        Self::Buildings,
        Self::Streets,
        Self::Cameras,
        Self::Targets,
        Self::Areas,
        Self::SharedPoint,
    ];
}

/// What the tool returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// The character grid with its legend.
    #[default]
    Text,
    /// A PNG picture, with the legend as text.
    Image,
    /// The picture and the character grid.
    Both,
}

/// A camera as the map marks it.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraMark {
    pub glyph: char,
    pub id: u32,
    pub label: String,
    pub at: [f32; 2],
    pub yaw_deg: f32,
    /// Why coverage leaves it out, if it does.
    pub not_counted: Option<String>,
}

/// A labelled area as the map outlines it.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaMark {
    pub glyph: char,
    pub id: u32,
    pub name: Option<String>,
    pub label: Option<AreaLabel>,
    pub points: Vec<[f32; 2]>,
    pub enabled: bool,
    pub limits_figures: bool,
}

/// A target as the map marks it: the centre of a ground target, or a wall
/// target's point.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetMark {
    pub id: u32,
    pub name: String,
    pub at: [f32; 2],
}

/// Everything a map is drawn from.
#[derive(Debug)]
pub struct MapModel<'a> {
    pub view: View,
    pub bundle: &'a AreaBundle,
    pub layers: &'a StaticLayers,
    pub show: Vec<Layer>,
    /// Per ground cell, how many enabled cameras do not rule it out
    /// (saturating at 255); `None` when coverage is not shown.
    pub seen: Option<Vec<u8>>,
    /// The detail level and lighting of `seen`.
    pub params: CoverageParams,
    pub cameras: Vec<CameraMark>,
    pub areas: Vec<AreaMark>,
    pub targets: Vec<TargetMark>,
    pub pinned: Option<[f32; 2]>,
}

impl MapModel<'_> {
    pub fn shows(&self, l: Layer) -> bool {
        self.show.contains(&l)
    }
}

/// The glyph that is camera id `id` itself: `1` to `9`, then `A` (10) to
/// `Z` (34) without `T`.
pub fn id_glyph(id: u32) -> Option<char> {
    let k = usize::try_from(id.checked_sub(1)?).ok()?;
    CAMERA_GLYPHS.get(k).map(|&b| char::from(b))
}

/// The glyphs of cameras with ids `ids`, in order. A camera whose id has a
/// glyph of its own ([`id_glyph`]) and is not repeated gets that glyph, so
/// after a deletion (ids 1, 3, 4) camera 3 is still `3`; any other camera
/// takes the first glyph no camera's id names, and `+` once they run out.
pub fn camera_glyphs(ids: &[u32]) -> Vec<char> {
    let unique = |id: u32| ids.iter().filter(|&&o| o == id).count() == 1;
    let own: Vec<Option<char>> = ids
        .iter()
        .map(|&id| id_glyph(id).filter(|_| unique(id)))
        .collect();
    let mut free = CAMERA_GLYPHS
        .iter()
        .map(|&b| char::from(b))
        .filter(|g| !ids.iter().any(|&id| id_glyph(id) == Some(*g)));
    own.into_iter()
        .map(|g| g.unwrap_or_else(|| free.next().unwrap_or(MORE_CAMERAS_GLYPH)))
        .collect()
}

/// The outline glyph of an area with `label`: lower case, apart from the
/// camera glyphs.
pub fn area_glyph(label: Option<AreaLabel>) -> char {
    match label {
        Some(AreaLabel::Own) => 'o',
        Some(AreaLabel::Neighbour) => 'n',
        Some(AreaLabel::Public) => 'p',
        Some(AreaLabel::Other) => 'x',
        None => 'u',
    }
}

/// The scene file's name for a label.
fn label_word(l: Option<AreaLabel>) -> &'static str {
    match l {
        Some(AreaLabel::Own) => "own",
        Some(AreaLabel::Neighbour) => "neighbour",
        Some(AreaLabel::Public) => "public",
        Some(AreaLabel::Other) => "other",
        None => "unlabelled",
    }
}

/// The compass point nearest a bearing: "north", "north-east", and so on.
pub fn compass_word(yaw_deg: f32) -> &'static str {
    const POINTS: [&str; 8] = [
        "north",
        "north-east",
        "east",
        "south-east",
        "south",
        "south-west",
        "west",
        "north-west",
    ];
    let k = (yaw_deg.rem_euclid(360.0) / 45.0).round() as usize % 8;
    POINTS[k]
}

/// Whole metres, without a "-0".
pub fn metres(v: f32) -> String {
    let r = v.round();
    if r == 0.0 {
        "0".to_owned()
    } else {
        format!("{r:.0}")
    }
}

/// A length or coordinate as short as it is exact, as "4" or "-2.5",
/// without a "-0".
pub fn short_metres(v: f32) -> String {
    if v == 0.0 {
        "0".to_owned()
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

/// How many cameras in `cams` do not rule out each ground cell, at the level
/// and lighting `params` select: the visibility pass of
/// `footage::seen_ground` (so the map agrees with `find_gaps`), counted
/// straight from each camera's visible cells. A camera whose range cannot
/// reach `view` is skipped, since none of what it sees would be drawn.
fn seen_counts(
    layers: &StaticLayers,
    params: &CoverageParams,
    cams: &[FootageCamera],
    view: &View,
) -> Vec<u8> {
    let mut counts = vec![0u8; layers.grid().len()];
    let (lo, hi) = (view.min, view.max());
    for c in cams.iter().filter(|c| c.placement.enabled) {
        let range = c
            .model
            .effective_range_m(params.pixels_per_metre(), params.lighting);
        let p = c.placement.position;
        let dx = (lo[0] - p[0]).max(p[0] - hi[0]).max(0.0);
        let dz = (lo[1] - p[2]).max(p[2] - hi[1]).max(0.0);
        if dx.hypot(dz) > range {
            continue;
        }
        let cone = ViewCone::new(c.placement, c.model, range);
        for &i in &layers.visibility(&cone, params.subject_height_m).ground {
            if let Some(n) = counts.get_mut(i as usize) {
                *n = n.saturating_add(1);
            }
        }
    }
    counts
}

/// The scene's cameras, areas and targets as the map marks them. `skipped`
/// says which cameras coverage leaves out, and why.
fn marks(
    scene: &SceneFile,
    skipped: &[SkippedOut],
) -> (Vec<CameraMark>, Vec<AreaMark>, Vec<TargetMark>) {
    let ids: Vec<u32> = scene.cameras.iter().map(|c| c.id).collect();
    let cameras = scene
        .cameras
        .iter()
        .zip(camera_glyphs(&ids))
        .map(|(c, glyph)| CameraMark {
            glyph,
            id: c.id,
            label: c.label.clone(),
            at: [c.position[0], c.position[2]],
            yaw_deg: c.yaw_deg,
            not_counted: skipped
                .iter()
                .find(|s| s.id == c.id)
                .map(|s| s.reason.clone()),
        })
        .collect();
    let areas = scene
        .areas
        .iter()
        .filter(|a| a.points.len() >= 2)
        .map(|a| AreaMark {
            glyph: area_glyph(a.label),
            id: a.id,
            name: a.name.clone(),
            label: a.label,
            points: a.points.clone(),
            enabled: a.enabled,
            limits_figures: scene.limit_figures_to == Some(a.id),
        })
        .collect();
    let targets = scene
        .targets
        .iter()
        .filter_map(|t| {
            let at = match &t.at {
                TargetPlace::Ground { points } => geom::inside_point(points)?,
                TargetPlace::Wall { point, .. } => [point[0], point[2]],
            };
            Some(TargetMark {
                id: t.id,
                name: t.name.clone(),
                at,
            })
        })
        .collect();
    (cameras, areas, targets)
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// One glyph of the map and what it stands for.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LegendOut {
    pub glyph: String,
    pub meaning: String,
}

/// A camera's glyph on the map.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CameraKeyOut {
    pub glyph: String,
    pub camera_id: u32,
    pub camera_label: String,
    /// `[x, z]`, metres.
    pub at: [f32; 2],
    /// Compass bearing it faces, degrees clockwise from north.
    pub bearing_deg: f32,
    /// The bearing as a compass point, such as "north-east".
    pub faces: String,
    /// Whether its glyph is on the map (false when it is outside the map,
    /// or under another camera's glyph).
    pub on_map: bool,
    /// Why coverage leaves it out, if it does: "disabled" or "unknown model".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_counted: Option<String>,
}

/// The picture's size.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImageOut {
    pub width_px: u32,
    pub height_px: u32,
    pub metres_per_px: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RenderOut {
    /// The side of one square cell (one character), metres.
    pub cell_m: f32,
    /// The map's north-west corner `[x, z]`, metres.
    pub min: [f32; 2],
    /// Its south-east corner `[x, z]`, metres.
    pub max: [f32; 2],
    pub columns: usize,
    pub rows: usize,
    /// Metres between ruler ticks.
    pub tick_m: f32,
    /// Every glyph on the map and what it stands for.
    pub legend: Vec<LegendOut>,
    /// Each camera's glyph, in the scene's order.
    pub cameras: Vec<CameraKeyOut>,
    /// The detail level and lighting the coverage is shaded at, when shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Settings>,
    /// Cameras the coverage leaves out, and why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cameras_skipped: Vec<SkippedOut>,
    /// When the OpenStreetMap data was current, if known.
    pub osm_data_as_of: Option<String>,
    /// One line to pass on with the coverage, when shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caveats_summary: Option<String>,
    /// The picture's size, when one was drawn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageOut>,
}

/// A drawn map: the text block, the PNG if asked for, and the structured
/// result.
#[derive(Debug)]
pub struct Rendered {
    pub text: String,
    pub png: Option<Vec<u8>>,
    pub out: RenderOut,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenderMapArgs {
    #[schemars(with = "JsonObject")]
    pub scene: Value,
    /// The map's centre `[x, z]`, metres; omit for the area's centre.
    pub center: Option<[f32; 2]>,
    /// Half the side of the square shown, metres (10 to 2000); omit for
    /// 60 m around `center`, or the whole area when `center` is omitted too.
    pub radius_m: Option<f32>,
    /// What to draw; omit for everything: buildings, streets, cameras,
    /// targets, areas and shared_point.
    pub layers: Option<Vec<Layer>>,
    /// Shade the ground some enabled camera is not ruled out from seeing
    /// (default true).
    pub coverage: Option<bool>,
    /// Detail level for the coverage (detect, observe, recognise or
    /// identify); omit for the scene's.
    #[serde(alias = "dori")]
    pub level: Option<Dori>,
    /// Day or night for the coverage; omit for the scene's.
    pub lighting: Option<Light>,
    /// "text" (default): a character grid; "image": a PNG and the legend;
    /// "both": the PNG and the grid.
    pub format: Option<Format>,
}

/// Draws the map `a` asks for. `pinned` is the point the user shared in
/// the connected app, with the area it is on; it is shown only on a scene on
/// that area.
pub fn render(
    a: &RenderMapArgs,
    pinned: Option<(AreaRef, [f32; 2])>,
) -> Result<Rendered, ToolError> {
    let cat = catalog()?;
    let s = edit::parse(&a.scene)?;
    let bundle = bundle_for(&s)?;
    if let Some(c) = a.center {
        check_inside(&bundle, c[0], c[1])?;
    }
    let view = choose_view(area::extent(&bundle), a.center, a.radius_m)?;
    let layers = layers_for(&s)?;
    let params = with_overrides(s.params, a.level, a.lighting);
    let coverage = a.coverage.unwrap_or(true);
    let seen = coverage.then(|| seen_counts(&layers, &params, &s.footage_cameras(cat), &view));
    let show = a.layers.clone().unwrap_or_else(|| Layer::ALL.to_vec());
    let not_counted = skipped(&s, cat);
    let (cameras, areas, targets) = marks(&s, &not_counted);
    let model = MapModel {
        view,
        bundle: &bundle,
        layers: &layers,
        show,
        seen,
        params,
        cameras,
        areas,
        targets,
        pinned: pinned.filter(|(at, _)| *at == s.area).map(|(_, p)| p),
    };
    let format = a.format.unwrap_or_default();
    let grid = text::grid(&model);
    let mut out = RenderOut {
        cell_m: view.cell_m,
        min: view.min,
        max: view.max(),
        columns: view.columns,
        rows: view.rows,
        tick_m: view.tick_m(),
        legend: text::legend(&model, &grid),
        cameras: text::camera_keys(&model, &grid),
        coverage: coverage.then(|| Settings::of(&params)),
        cameras_skipped: if coverage { not_counted } else { Vec::new() },
        osm_data_as_of: bundle.osm_base_unix.map(format_date),
        caveats_summary: coverage.then(|| CAVEATS_SUMMARY.to_owned()),
        image: None,
    };
    let png = match format {
        Format::Text => None,
        Format::Image | Format::Both => {
            let (canvas, metres_per_px) = image::draw(&model);
            out.image = Some(ImageOut {
                width_px: canvas.width,
                height_px: canvas.height,
                metres_per_px,
            });
            Some(
                canvas
                    .png()
                    .map_err(|e| ToolError::invalid(format!("the picture failed: {e}")))?,
            )
        }
    };
    let text = text::document(&model, &grid, &out, format != Format::Image);
    Ok(Rendered { text, png, out })
}

#[tool_router(router = render_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Draw the scene from above, north up, as a character grid with a legend: # buildings, = street, . and , ground not ruled out for some camera, camera glyphs 1-9 then A-Z, T targets. Zoom with center [x, z] and radius_m. format \"image\" adds a PNG.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<RenderOut>()
    )]
    pub(crate) async fn render_map(
        &self,
        Parameters(a): Parameters<RenderMapArgs>,
    ) -> Result<CallToolResult, String> {
        let pinned = self
            .link
            .snapshot()
            .view
            .and_then(|v| v.state)
            .and_then(|st| Some((st.area?, st.pinned?)))
            .map(|(area, p)| (area, [p.x, p.z]));
        let r = off_runtime(move || render(&a, pinned)).await?;
        let structured = serde_json::to_value(&r.out).map_err(|e| e.to_string())?;
        let mut content = vec![ContentBlock::text(r.text)];
        if let Some(png) = r.png {
            content.push(ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(png),
                "image/png",
            ));
        }
        let mut result = CallToolResult::success(content);
        result.structured_content = Some(structured);
        Ok(result)
    }
}
