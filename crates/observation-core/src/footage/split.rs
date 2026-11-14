//! Where one camera's footage lands: its visible ground split by labelled
//! area, label, street and the rest, and the wall it sees per building.

use super::{AreaLabel, FootageCamera, LabelledArea};
use crate::coverage::{CoverageParams, StaticLayers, Visibility, scan_inside};
use crate::frustum::ViewCone;
use crate::util::planar::dist_to_segment;
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How far the drawn outlines are moved in and out, metres, to show what a
/// generous or a tight outline changes.
pub const DEFAULT_OFFSET_M: f32 = 1.0;

/// A ground figure under three readings of the drawn outlines, m².
///
/// For an area or a label, `low_m2` counts with every outline moved inward
/// by the offset, `as_drawn_m2` as drawn, and `high_m2` moved outward. For
/// the unlabelled ground it is the other way round (moving the outlines out
/// leaves less ground outside them), so `low_m2 <= as_drawn_m2 <= high_m2`
/// always holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SeenRange {
    pub low_m2: f32,
    pub as_drawn_m2: f32,
    pub high_m2: f32,
}

/// The ground one camera sees inside one enabled area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaSeen {
    pub area_id: u32,
    pub label: Option<AreaLabel>,
    /// The outline's own enclosed area as drawn, m², printed beside the
    /// figures so that an outline drawn too generously shows.
    pub polygon_m2: f32,
    pub seen: SeenRange,
}

/// The ground one camera sees inside any enabled area with one label (each
/// cell once, where that label's areas overlap).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelSeen {
    pub label: AreaLabel,
    pub seen: SeenRange,
}

/// The wall of one building one camera sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallSeen {
    /// The building's index in the bundle at hand (see
    /// [`FacadeCell::building`](crate::coverage::FacadeCell::building)).
    /// Buildings are named by index and distance only, never by address.
    pub building: u32,
    /// Horizontal distance from the camera to the nearest wall patch it sees
    /// on the building, metres.
    pub distance_m: f32,
    pub seen_m2: f32,
}

/// Where one camera's visible ground and walls lie.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Split {
    /// All the ground the camera sees, m².
    pub ground_m2: f32,
    /// The part of it on the street (the street mask: road surfaces as
    /// mapped, whatever areas are drawn over them), m².
    pub street_m2: f32,
    /// The part neither on the street nor inside a labelled area.
    pub unlabelled: SeenRange,
    /// Each enabled area with a usable outline, labelled or not, in the
    /// scene's order. Areas may overlap, so these need not add up.
    pub areas: Vec<AreaSeen>,
    /// Each label that some enabled area has, in [`AreaLabel::ALL`] order.
    pub labels: Vec<LabelSeen>,
    /// All the wall the camera sees, m².
    pub walls_m2: f32,
    /// The wall it sees on each building, by building index.
    pub walls: Vec<WallSeen>,
}

/// One camera's [`Split`] at the current detail level and lighting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraSplit {
    pub camera_id: u32,
    /// The range the split was counted out to, metres.
    pub range_m: f32,
    pub split: Split,
}

/// Which ground cells lie inside each enabled area with a usable outline,
/// as drawn and with the outline moved in and out: built once per area set
/// and reused for every camera.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaMasks {
    offset_m: f32,
    grid_w: usize,
    areas: Vec<AreaMask>,
}

/// One area's cells, in a window of the grid around it. Each cell holds how
/// deep inside it is: 3 inside the inward offset, 2 inside the outline as
/// drawn, 1 inside the outward offset only, 0 outside.
#[derive(Debug, Clone, PartialEq)]
struct AreaMask {
    id: u32,
    label: Option<AreaLabel>,
    polygon_m2: f32,
    col0: usize,
    row0: usize,
    w: usize,
    h: usize,
    depth: Vec<u8>,
}

impl AreaMask {
    fn new(layers: &StaticLayers, area: &LabelledArea, offset_m: f32) -> Self {
        let grid = layers.grid();
        let gw = grid.w as usize;
        let mut m = Self {
            id: area.id,
            label: area.label,
            polygon_m2: area.area_m2(),
            col0: 0,
            row0: 0,
            w: 0,
            h: 0,
            depth: Vec::new(),
        };
        let pts: Vec<Vec2> = area.points.iter().map(|p| Vec2::from_array(*p)).collect();
        let lo = pts.iter().copied().fold(Vec2::INFINITY, Vec2::min) - offset_m;
        let hi = pts.iter().copied().fold(Vec2::NEG_INFINITY, Vec2::max) + offset_m;
        let Some((x0, x1, z0, z1)) = grid.range(lo, hi) else {
            return m;
        };
        (m.col0, m.row0) = (x0 as usize, z0 as usize);
        (m.w, m.h) = ((x1 - x0) as usize + 1, (z1 - z0) as usize + 1);
        // Inside the outline as drawn, by cell centre, exactly as a region
        // mask has it.
        let mut inside = vec![false; m.w * m.h];
        scan_inside(grid, &area.points, |start, end| {
            let row = start / gw;
            if row < m.row0 || row >= m.row0 + m.h {
                return;
            }
            let (a, b) = (start % gw, (end - 1) % gw + 1);
            let (a, b) = (a.max(m.col0), b.min(m.col0 + m.w));
            for col in a..b {
                inside[(row - m.row0) * m.w + col - m.col0] = true;
            }
        });
        // Within the offset of the outline, measured to the nearest edge.
        let mut near = vec![false; m.w * m.h];
        if offset_m > 0.0 {
            for (i, &a) in pts.iter().enumerate() {
                let b = pts[(i + 1) % pts.len()];
                let Some((ex0, ex1, ez0, ez1)) =
                    grid.range(a.min(b) - offset_m, a.max(b) + offset_m)
                else {
                    continue;
                };
                for z in ez0..=ez1 {
                    for x in ex0..=ex1 {
                        let c = grid.center(z as usize * gw + x as usize);
                        if dist_to_segment(c, a, b) <= offset_m {
                            near[(z as usize - m.row0) * m.w + x as usize - m.col0] = true;
                        }
                    }
                }
            }
        }
        m.depth = inside
            .iter()
            .zip(&near)
            .map(|(&i, &n)| match (i, n) {
                (true, false) => 3,
                (true, true) => 2,
                (false, true) => 1,
                (false, false) => 0,
            })
            .collect();
        m
    }

    /// How deep inside ground cell `i` lies (see [`AreaMask`]).
    fn depth(&self, i: usize, grid_w: usize) -> u8 {
        let (col, row) = (i % grid_w, i / grid_w);
        if col < self.col0 || row < self.row0 {
            return 0;
        }
        let (c, r) = (col - self.col0, row - self.row0);
        if c >= self.w || r >= self.h {
            return 0;
        }
        self.depth[r * self.w + c]
    }
}

impl AreaMasks {
    /// The masks of `areas` that are enabled and have a usable outline, with
    /// the outlines moved in and out by `offset_m` (at least 0).
    pub fn new(layers: &StaticLayers, areas: &[LabelledArea], offset_m: f32) -> Self {
        let offset_m = if offset_m.is_finite() {
            offset_m.max(0.0)
        } else {
            0.0
        };
        Self {
            offset_m,
            grid_w: layers.grid().w as usize,
            areas: areas
                .iter()
                .filter(|a| a.is_active())
                .map(|a| AreaMask::new(layers, a, offset_m))
                .collect(),
        }
    }

    /// How far the outlines were moved in and out, metres.
    pub fn offset_m(&self) -> f32 {
        self.offset_m
    }

    /// The ids of the areas that take part, in order.
    pub fn area_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.areas.iter().map(|a| a.id)
    }
}

/// Counts of cells under the three readings: inward, as drawn, outward.
#[derive(Debug, Clone, Copy, Default)]
struct Three([u64; 3]);

impl Three {
    /// Counts a cell `depth` deep inside (see [`AreaMask`]).
    fn add_depth(&mut self, depth: u8) {
        for (k, n) in self.0.iter_mut().enumerate() {
            // Inward needs 3, as drawn 2, outward 1.
            if depth >= 3 - k as u8 {
                *n += 1;
            }
        }
    }

    fn range(self, cell_m2: f64) -> SeenRange {
        let [low, drawn, high] = self.0.map(|n| (n as f64 * cell_m2) as f32);
        SeenRange {
            low_m2: low,
            as_drawn_m2: drawn,
            high_m2: high,
        }
    }
}

/// Splits what one camera at `eye` sees (`seen`, its cells at the level and
/// lighting wanted) over the areas in `masks`.
pub fn split_seen(layers: &StaticLayers, masks: &AreaMasks, eye: Vec3, seen: &Visibility) -> Split {
    let cell_m2 = f64::from(layers.grid().cell_area_m2());
    let street_mask = layers.street_mask();
    let (mut ground, mut street) = (0u64, 0u64);
    let mut per_area = vec![Three::default(); masks.areas.len()];
    let mut per_label = [Three::default(); 4];
    let mut unlabelled = Three::default();
    for &i in &seen.ground {
        let i = i as usize;
        ground += 1;
        let on_street = street_mask.get(i).copied().unwrap_or(false);
        street += u64::from(on_street);
        let mut deepest = [0u8; 4];
        for (m, n) in masks.areas.iter().zip(&mut per_area) {
            let d = m.depth(i, masks.grid_w);
            if d == 0 {
                continue;
            }
            n.add_depth(d);
            if let Some(l) = m.label {
                let k = l as usize;
                deepest[k] = deepest[k].max(d);
            }
        }
        for (n, &d) in per_label.iter_mut().zip(&deepest) {
            n.add_depth(d);
        }
        if !on_street {
            // Outside every labelled outline: moved outward (low), as drawn,
            // moved inward (high).
            let d = deepest.iter().copied().max().unwrap_or(0);
            for (k, n) in unlabelled.0.iter_mut().enumerate() {
                if d < k as u8 + 1 {
                    *n += 1;
                }
            }
        }
    }
    let areas = masks
        .areas
        .iter()
        .zip(per_area)
        .map(|(m, n)| AreaSeen {
            area_id: m.id,
            label: m.label,
            polygon_m2: m.polygon_m2,
            seen: n.range(cell_m2),
        })
        .collect();
    let labels = AreaLabel::ALL
        .iter()
        .zip(per_label)
        .filter(|(l, _)| masks.areas.iter().any(|m| m.label == Some(**l)))
        .map(|(&label, n)| LabelSeen {
            label,
            seen: n.range(cell_m2),
        })
        .collect();
    // Wall per building: area summed in f64, nearest patch by plan distance.
    let mut walls: BTreeMap<u32, (f64, f32)> = BTreeMap::new();
    let mut walls_m2 = 0f64;
    let facades = layers.facades();
    for &i in &seen.facade {
        let Some(f) = facades.get(i as usize) else {
            continue;
        };
        let a = f64::from(f.area_m2());
        walls_m2 += a;
        let d2 = Vec2::new(f.center.x - eye.x, f.center.z - eye.z).length_squared();
        let e = walls.entry(f.building).or_insert((0.0, f32::INFINITY));
        e.0 += a;
        e.1 = e.1.min(d2);
    }
    Split {
        ground_m2: (ground as f64 * cell_m2) as f32,
        street_m2: (street as f64 * cell_m2) as f32,
        unlabelled: unlabelled.range(cell_m2),
        areas,
        labels,
        walls_m2: walls_m2 as f32,
        walls: walls
            .into_iter()
            .map(|(building, (a, d2))| WallSeen {
                building,
                distance_m: d2.sqrt(),
                seen_m2: a as f32,
            })
            .collect(),
    }
}

/// The [`Split`] of every enabled camera in `cameras`, at the level and
/// lighting `params` select, in the order given.
pub fn camera_splits(
    layers: &StaticLayers,
    params: &CoverageParams,
    cameras: &[FootageCamera],
    masks: &AreaMasks,
) -> Vec<CameraSplit> {
    cameras
        .iter()
        .filter(|c| c.placement.enabled)
        .map(|c| {
            let range = c
                .model
                .effective_range_m(params.pixels_per_metre(), params.lighting);
            let cone = ViewCone::new(c.placement, c.model, range);
            let seen = layers.visibility(&cone, params.subject_height_m);
            CameraSplit {
                camera_id: c.placement.id,
                range_m: range,
                split: split_seen(layers, masks, cone.eye, &seen),
            }
        })
        .collect()
}
