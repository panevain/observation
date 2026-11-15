//! What the sheet prints, worked out from the scene and each camera's sight
//! lines, and already cut down to what the tier may show.

use super::method::{Section, methodology};
use super::{SheetError, SheetInputs, Tier};
use crate::camera::{CameraModel, DoriLevel, Lighting};
use crate::coverage::{
    CoverageParams, CoverageStats, RegionMask, Sight, StaticLayers, Tally, road_band_mask,
};
use crate::footage::{
    AreaLabel, AreaMasks, CameraVerdicts, DEFAULT_OFFSET_M, FootageCamera, Split, TargetPlace,
    area_by_id, split_seen, target_verdicts,
};
use crate::osm::tags::is_carriageway;
use crate::provenance::Provenance;
use crate::road::Road;
use glam::Vec2;

/// Street area by kind (see the methodology): carriageway, and footpath and
/// service ways, m².
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StreetKinds {
    pub carriageway_m2: f32,
    pub footpath_service_m2: f32,
}

/// The figures at one detail level and lighting.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GridCell {
    /// As the app's Coverage card computes them, limited to the same area.
    pub stats: CoverageStats,
    /// The street seen by at least one camera, by kind.
    pub street_seen: StreetKinds,
}

/// One detail level by day and by night.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridRow {
    pub level: DoriLevel,
    /// Its pixel density, px/m.
    pub ppm: f32,
    pub day: GridCell,
    pub night: GridCell,
}

impl GridRow {
    pub fn at(&self, lighting: Lighting) -> &GridCell {
        match lighting {
            Lighting::Day => &self.day,
            Lighting::Night => &self.night,
        }
    }
}

/// Every level of [`CoverageParams::ladder`] by day and by night, and the
/// street the figures are counted over.
#[derive(Debug, Clone, PartialEq)]
pub struct DetailGrid {
    pub rows: Vec<GridRow>,
    /// The street inside the area the figures are limited to, by kind.
    pub street: StreetKinds,
}

/// Ground cell counts at one level and lighting, kept for the map.
type Counts = Vec<u8>;

/// The detail × lighting grid from each camera's sight lines: one
/// [`Tally`] per level and lighting, each counting every camera's cells
/// within its range there. No sight line is cast: range only cuts the cells
/// each camera's [`Sight`] already holds (they must reach
/// [`CoverageParams::sight_range_m`]). `roads` are the ones the layers were
/// built from. Also returns the ground counts at `params`' own level and
/// lighting.
pub fn detail_grid(
    layers: &StaticLayers,
    roads: &[Road],
    region: Option<&RegionMask>,
    params: &CoverageParams,
    cameras: &[(&CameraModel, &Sight)],
) -> DetailGrid {
    detail_grid_and_counts(layers, roads, region, params, cameras).0
}

fn detail_grid_and_counts(
    layers: &StaticLayers,
    roads: &[Road],
    region: Option<&RegionMask>,
    params: &CoverageParams,
    cameras: &[(&CameraModel, &Sight)],
) -> (DetailGrid, Counts) {
    let carriageway = road_band_mask(
        layers.grid(),
        layers.building_mask(),
        roads.iter().filter(|r| is_carriageway(&r.highway)),
    );
    let cell_m2 = layers.grid().cell_area_m2();
    let in_scope = |i: usize| region.is_none_or(|r| r.ground[i]);
    let kinds = |seen: &dyn Fn(usize) -> bool| {
        let (mut major, mut minor) = (0u64, 0u64);
        for (i, &street) in layers.street_mask().iter().enumerate() {
            if street && in_scope(i) && seen(i) {
                if carriageway[i] {
                    major += 1;
                } else {
                    minor += 1;
                }
            }
        }
        StreetKinds {
            carriageway_m2: major as f32 * cell_m2,
            footpath_service_m2: minor as f32 * cell_m2,
        }
    };
    let mut selected = Vec::new();
    let mut rows = Vec::new();
    for (level, ppm) in params.ladder() {
        let mut cell = |lighting: Lighting| {
            let mut tally = Tally::new(layers, region);
            for (model, sight) in cameras {
                tally.add_within(
                    layers,
                    region,
                    sight,
                    model.effective_range_m(ppm, lighting),
                );
            }
            let counts = &tally.counts().ground;
            let cell = GridCell {
                stats: tally.stats(layers),
                street_seen: kinds(&|i| counts[i] >= 1),
            };
            if level == params.dori && lighting == params.lighting {
                selected = tally.counts.ground;
            }
            cell
        };
        let (day, night) = (cell(Lighting::Day), cell(Lighting::Night));
        rows.push(GridRow {
            level,
            ppm,
            day,
            night,
        });
    }
    let grid = DetailGrid {
        rows,
        street: kinds(&|_| true),
    };
    (grid, selected)
}

/// A camera as the sheet names it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct CameraLine {
    /// From 1, in the scene's order of enabled cameras.
    pub number: usize,
    pub model: String,
    /// The user's label: only on the Exact sheet (labels often name a house).
    pub label: Option<String>,
    /// Range at the selected level by day and by night, metres.
    pub range_day_m: f32,
    pub range_night_m: f32,
    /// Whether the model publishes an infrared range.
    pub ir_published: bool,
}

/// An area as the sheet lists it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct AreaLine {
    pub id: u32,
    /// Its name as typed, or "Area 3".
    pub title: String,
    pub label: Option<AreaLabel>,
    pub polygon_m2: f32,
}

/// One target with every camera's verdicts.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TargetLine {
    pub title: String,
    pub wall: bool,
    /// By camera number.
    pub cameras: Vec<(usize, CameraVerdicts)>,
}

/// The map panel: only on the Exact sheet.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct MapPanel {
    /// The world rectangle drawn, `[x, z]` metres.
    pub min: Vec2,
    pub max: Vec2,
    /// Building rings, outer first then holes.
    pub footprints: Vec<Vec<Vec<[f32; 2]>>>,
    /// Each road's width and centre line.
    pub roads: Vec<(f32, Vec<[f32; 2]>)>,
    /// Runs of ground cells seen by 1, 2, 3 and 4+ cameras at the selected
    /// level and lighting: `(row, first column, count)`.
    pub coverage: [Vec<(u32, u32, u32)>; 4],
    /// The ground grid's north-west corner and cell size.
    pub grid_min: Vec2,
    pub cell_m: f32,
    /// Active areas: label and outline.
    pub areas: Vec<(Option<AreaLabel>, Vec<[f32; 2]>)>,
    /// Targets by number: where to put the marker.
    pub targets: Vec<(usize, [f32; 2])>,
    /// Cameras by number: plan position and compass bearing.
    pub cameras: Vec<(usize, [f32; 2], f32)>,
}

/// Everything the sheet prints.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Figures {
    pub tier: Tier,
    pub title: String,
    pub generated_unix: u64,
    pub provenance: Provenance,
    pub params: CoverageParams,
    /// The area the figures are limited to, if any.
    pub limit: Option<AreaLine>,
    pub grid: DetailGrid,
    pub cameras: Vec<CameraLine>,
    pub areas: Vec<AreaLine>,
    /// Each camera's split at the selected level and lighting, by number.
    /// On the Aggregate sheet the walls per building are left out.
    pub splits: Vec<(usize, Split)>,
    pub targets: Vec<TargetLine>,
    pub methodology: Vec<Section>,
    pub osm_attribution: String,
    pub elevation_attribution: String,
    pub map: Option<MapPanel>,
}

/// An area's or target's title: its name as typed, or `{fallback} {id}`.
fn title(name: Option<&str>, fallback: &str, id: u32) -> String {
    name.map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| format!("{fallback} {id}"), str::to_owned)
}

impl Figures {
    pub(super) fn compute(inputs: &SheetInputs, tier: Tier) -> Result<Self, SheetError> {
        let layers = inputs.layers;
        let params = inputs.params;
        let cams: Vec<_> = inputs
            .cameras
            .iter()
            .filter(|c| c.placement.enabled)
            .collect();
        for c in &cams {
            if !c.sight.covers(params.sight_range_m(c.model)) {
                return Err(SheetError::NotComputed {
                    camera_id: c.placement.id,
                });
            }
        }

        let limit_area =
            area_by_id(inputs.areas, inputs.limit_figures_to).filter(|a| a.is_active());
        let region = limit_area.map(|a| RegionMask::new(layers, &a.points));
        let sights: Vec<(&CameraModel, &Sight)> = cams.iter().map(|c| (c.model, c.sight)).collect();
        let (grid, counts) = detail_grid_and_counts(
            layers,
            &inputs.bundle.roads,
            region.as_ref(),
            &params,
            &sights,
        );

        let area_line = |a: &crate::footage::LabelledArea| AreaLine {
            id: a.id,
            title: title(a.name.as_deref(), "Area", a.id),
            label: a.label,
            polygon_m2: a.area_m2(),
        };
        let ppm = params.pixels_per_metre();
        let cameras: Vec<CameraLine> = cams
            .iter()
            .enumerate()
            .map(|(i, c)| CameraLine {
                number: i + 1,
                model: c.model.display_name(),
                label: (tier.shows_positions() && !c.placement.label.trim().is_empty())
                    .then(|| c.placement.label.trim().to_owned()),
                range_day_m: c.model.effective_range_m(ppm, Lighting::Day),
                range_night_m: c.model.effective_range_m(ppm, Lighting::Night),
                ir_published: c.model.ir_range_m.is_some(),
            })
            .collect();

        let masks = AreaMasks::new(layers, inputs.areas, DEFAULT_OFFSET_M);
        let splits = cams
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let range = c.model.effective_range_m(ppm, params.lighting);
                let mut split =
                    split_seen(layers, &masks, c.placement.eye(), &c.sight.within(range));
                if !tier.shows_positions() {
                    // A distance from a camera to a numbered building gives
                    // the camera's position away.
                    split.walls.clear();
                }
                (i + 1, split)
            })
            .collect();

        let footage: Vec<FootageCamera> = cams
            .iter()
            .map(|c| FootageCamera {
                placement: c.placement,
                model: c.model,
            })
            .collect();
        let verdicts = target_verdicts(layers, &params, &footage, inputs.targets);
        let targets = inputs
            .targets
            .iter()
            .zip(verdicts)
            .map(|(t, v)| TargetLine {
                title: title(Some(&t.name), "Target", t.id),
                wall: matches!(t.at, TargetPlace::Wall { .. }),
                cameras: v
                    .cameras
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut cv)| {
                        if !tier.shows_positions() {
                            cv.nearest_m = 0.0;
                        }
                        (i + 1, cv)
                    })
                    .collect(),
            })
            .collect();

        let map = tier.shows_positions().then(|| MapPanel {
            min: layers.grid().min,
            max: layers.grid().max(),
            footprints: inputs
                .bundle
                .footprints()
                .map(|f| {
                    std::iter::once(f.outer.clone())
                        .chain(f.holes.iter().cloned())
                        .collect()
                })
                .collect(),
            roads: inputs
                .bundle
                .roads
                .iter()
                .map(|r| (r.width_m, r.points.clone()))
                .collect(),
            coverage: runs(&counts, layers.grid().w),
            grid_min: layers.grid().min,
            cell_m: layers.grid().cell_m,
            areas: inputs
                .areas
                .iter()
                .filter(|a| a.is_active())
                .map(|a| (a.label, a.points.clone()))
                .collect(),
            targets: inputs
                .targets
                .iter()
                .enumerate()
                .map(|(i, t)| (i + 1, target_marker(&t.at)))
                .collect(),
            cameras: cams
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let p = c.placement.position;
                    (i + 1, [p[0], p[2]], c.placement.yaw_deg)
                })
                .collect(),
        });

        let osm = inputs.bundle.attribution.trim();
        Ok(Self {
            tier,
            title: inputs.title.trim().to_owned(),
            generated_unix: inputs.generated_unix,
            provenance: inputs.provenance.clone(),
            params,
            limit: limit_area.map(area_line),
            grid,
            cameras,
            areas: inputs
                .areas
                .iter()
                .filter(|a| a.is_active())
                .map(area_line)
                .collect(),
            splits,
            targets,
            methodology: methodology(
                &params,
                layers.grid().cell_m,
                inputs.facade_cell_m,
                inputs.agent_log,
                tier,
            ),
            osm_attribution: if osm.is_empty() {
                "© OpenStreetMap contributors".to_owned()
            } else {
                osm.to_owned()
            },
            elevation_attribution: inputs.bundle.terrain.attribution.trim().to_owned(),
            map,
        })
    }
}

/// Where a target's marker goes on the map.
fn target_marker(at: &TargetPlace) -> [f32; 2] {
    match at {
        TargetPlace::Ground { points } => {
            let n = points.len().max(1) as f32;
            let s = points
                .iter()
                .fold(Vec2::ZERO, |s, p| s + Vec2::from_array(*p));
            (s / n).to_array()
        }
        TargetPlace::Wall { point, .. } => [point[0], point[2]],
    }
}

/// Runs of cells with the same overlap class (1, 2, 3, 4+), row by row.
fn runs(counts: &[u8], w: u32) -> [Vec<(u32, u32, u32)>; 4] {
    let mut out: [Vec<(u32, u32, u32)>; 4] = Default::default();
    if w == 0 {
        return out;
    }
    for (row, cells) in counts.chunks(w as usize).enumerate() {
        let mut col = 0;
        while col < cells.len() {
            let class = cells[col].min(4);
            let start = col;
            while col < cells.len() && cells[col].min(4) == class {
                col += 1;
            }
            if class > 0 {
                out[class as usize - 1].push((row as u32, start as u32, (col - start) as u32));
            }
        }
    }
    out
}
