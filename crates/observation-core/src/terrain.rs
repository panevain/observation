//! Ground elevation: a height grid over an area, sampled from Terrarium DEM
//! tiles (<https://github.com/tilezen/joerd/blob/master/docs/formats.md>).
//!
//! Heights are world `y` values: metres above the ground at the area origin
//! (the bbox centre), which is `y = 0`. [`Terrain::datum_m`] gives that
//! point's elevation above sea level. A [`Terrain::default`] grid is flat:
//! every height is 0.

use crate::geo::{BBox, LatLon, Projection};
use crate::occlusion::point_in_ring;
use crate::tiles::{TileId, lat_to_y, lon_to_x, tiles_covering};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Credit for the default elevation source, the AWS Open Data Terrain Tiles.
pub const TERRAIN_TILES_ATTRIBUTION: &str = "Elevation: Mapzen Terrain Tiles, USGS and others";
/// Where the Terrain Tiles list every source's required credit.
pub const TERRAIN_TILES_CREDITS_URL: &str =
    "https://github.com/tilezen/joerd/blob/master/docs/attribution.md";

/// Ground covered beyond the area on every side, metres. The client draws
/// map tiles and the pick surface this far out.
pub const TERRAIN_MARGIN_M: f32 = 150.0;
/// Preferred spacing of the height samples, metres.
pub const TERRAIN_CELL_M: f32 = 5.0;
/// Most samples along either axis (a larger extent gets coarser cells).
pub const MAX_TERRAIN_SAMPLES: u32 = 256;
/// Zoom of the DEM tiles: Terrarium's finest, about 3-5 m per pixel at
/// mid latitudes, finer than the source data almost everywhere.
pub const DEM_ZOOM: u8 = 15;
/// Pixels along each side of a DEM tile.
pub const DEM_TILE_PX: u32 = 256;

/// How far below a sight line the ground must reach before it blocks it,
/// metres. Bilinear heights between coarse samples are only approximate.
const BLOCK_TOLERANCE_M: f32 = 0.1;
/// Sight lines are not tested this close (horizontally) to either end,
/// metres: a subject at ground level touches the ground there.
const END_SKIP_M: f32 = 0.75;

/// Height samples on a regular grid in the world `[x, z]` plane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Terrain {
    /// World `[x, z]` of sample `(0, 0)`, the north-west corner.
    pub min: [f32; 2],
    /// Spacing of the samples, metres.
    pub cell_m: f32,
    pub cols: u32,
    pub rows: u32,
    /// World `y` of each sample in decimetres, row-major with rows along
    /// `+Z` (south).
    pub heights_dm: Vec<i16>,
    /// Elevation of world `y = 0` above sea level, metres.
    pub datum_m: f32,
    /// Credit for the elevation data; empty for a flat grid.
    pub attribution: String,
}

/// Flat ground, as a constant (the same as [`Terrain::default`]).
pub const FLAT: Terrain = Terrain {
    min: [0.0, 0.0],
    cell_m: 0.0,
    cols: 0,
    rows: 0,
    heights_dm: Vec::new(),
    datum_m: 0.0,
    attribution: String::new(),
};

/// Smallest and largest sample spacing a grid from the wire may have, metres.
const CELL_RANGE_M: std::ops::RangeInclusive<f32> = 1.0..=1000.0;

/// Why a [`Terrain`] read from the wire cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTerrain;

impl Terrain {
    /// Whether every height is 0 (no elevation data).
    pub fn is_flat(&self) -> bool {
        self.heights_dm.iter().all(|&h| h == 0)
    }

    /// Checks a grid from an untrusted source: the sample count matches the
    /// size, which is bounded, and the spacing and position are usable.
    pub fn validate(&self) -> Result<(), InvalidTerrain> {
        let max = MAX_TERRAIN_SAMPLES * 4;
        let flat_empty = self.cols == 0 && self.rows == 0 && self.heights_dm.is_empty();
        let sized = (1..=max).contains(&self.cols)
            && (1..=max).contains(&self.rows)
            && self.heights_dm.len() == self.cols as usize * self.rows as usize;
        let numbers = CELL_RANGE_M.contains(&self.cell_m)
            && self.min.iter().all(|v| v.is_finite() && v.abs() < 1.0e5)
            && self.datum_m.is_finite();
        if flat_empty || (sized && numbers) {
            Ok(())
        } else {
            Err(InvalidTerrain)
        }
    }

    fn usable(&self) -> bool {
        self.cols > 0
            && self.rows > 0
            && self.cell_m > 0.0
            && self.heights_dm.len() == self.cols as usize * self.rows as usize
    }

    fn sample(&self, col: u32, row: u32) -> f32 {
        f32::from(self.heights_dm[(row * self.cols + col) as usize]) * 0.1
    }

    /// Ground height (world `y`) at world `[x, z]`, interpolated between
    /// samples and held at the edge value beyond the grid. 0 for a flat or
    /// unusable grid, and for non-finite input.
    pub fn height_at(&self, p: Vec2) -> f32 {
        if !self.usable() || !p.is_finite() {
            return 0.0;
        }
        let max = Vec2::new((self.cols - 1) as f32, (self.rows - 1) as f32);
        let f = ((p - Vec2::from_array(self.min)) / self.cell_m).clamp(Vec2::ZERO, max);
        let (c0, r0) = (f.x.floor() as u32, f.y.floor() as u32);
        let (c1, r1) = ((c0 + 1).min(self.cols - 1), (r0 + 1).min(self.rows - 1));
        let (tx, tz) = (f.x - c0 as f32, f.y - r0 as f32);
        let top = self.sample(c0, r0) * (1.0 - tx) + self.sample(c1, r0) * tx;
        let bottom = self.sample(c0, r1) * (1.0 - tx) + self.sample(c1, r1) * tx;
        top * (1.0 - tz) + bottom * tz
    }

    /// Lowest ground under a footprint ring (world `[x, z]` points): at its
    /// corners and at the samples inside it. 0 for an empty ring.
    pub fn min_under(&self, ring: &[[f32; 2]]) -> f32 {
        let corners = ring.iter().map(|p| self.height_at(Vec2::from_array(*p)));
        let mut low = corners.fold(f32::INFINITY, f32::min);
        if self.usable() && !ring.is_empty() {
            let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
            for p in ring {
                lo = lo.min(Vec2::from_array(*p));
                hi = hi.max(Vec2::from_array(*p));
            }
            let origin = Vec2::from_array(self.min);
            let index = |v: f32, n: u32| (v.max(0.0) as u32).min(n - 1);
            let a = ((lo - origin) / self.cell_m).ceil();
            let b = ((hi - origin) / self.cell_m).floor();
            if a.is_finite() && b.is_finite() && b.x >= 0.0 && b.y >= 0.0 {
                for row in index(a.y, self.rows)..=index(b.y, self.rows) {
                    for col in index(a.x, self.cols)..=index(b.x, self.cols) {
                        let at = origin + Vec2::new(col as f32, row as f32) * self.cell_m;
                        if point_in_ring(at, ring) {
                            low = low.min(self.sample(col, row));
                        }
                    }
                }
            }
        }
        if low.is_finite() { low } else { 0.0 }
    }

    /// Height of world point `p` above the ground under it.
    pub fn height_above(&self, p: [f32; 3]) -> f32 {
        p[1] - self.height_at(Vec2::new(p[0], p[2]))
    }

    /// `p` moved to world `[x, z]`, keeping its height above the ground.
    pub fn moved_to(&self, p: [f32; 3], x: f32, z: f32) -> [f32; 3] {
        [x, self.height_at(Vec2::new(x, z)) + self.height_above(p), z]
    }

    /// Lowest sample, metres (0 for a flat grid).
    pub fn bottom(&self) -> f32 {
        self.heights_dm
            .iter()
            .min()
            .map_or(0.0, |&h| f32::from(h) * 0.1)
            .min(0.0)
    }

    /// Highest sample, metres (0 for a flat grid).
    pub fn top(&self) -> f32 {
        self.heights_dm
            .iter()
            .max()
            .map_or(0.0, |&h| f32::from(h) * 0.1)
            .max(0.0)
    }

    /// Whether the ground rises above the straight line from `a` to `b`
    /// somewhere between them. `top` is [`Self::top`], passed in so callers
    /// testing many lines compute it once: a line above it is never blocked.
    pub fn segment_blocked(&self, a: Vec3, b: Vec3, top: f32) -> bool {
        if !self.usable() || a.y.min(b.y) > top + BLOCK_TOLERANCE_M {
            return false;
        }
        let (a2, b2) = (Vec2::new(a.x, a.z), Vec2::new(b.x, b.z));
        let len = a2.distance(b2);
        if !len.is_finite() || len <= 2.0 * END_SKIP_M {
            return false;
        }
        // Half a cell apart, which no ridge between two samples can hide in.
        let steps = (len / (self.cell_m * 0.5)).ceil().min(4096.0) as u32;
        (1..steps).any(|i| {
            let t = i as f32 / steps as f32;
            let along = t * len;
            if along < END_SKIP_M || len - along < END_SKIP_M {
                return false;
            }
            let p = a.lerp(b, t);
            self.height_at(Vec2::new(p.x, p.z)) > p.y + BLOCK_TOLERANCE_M
        })
    }

    /// Where a ray from `origin` along `dir` first meets the ground, within
    /// `max_dist` metres. Marches in steps of half a cell, then bisects.
    pub fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Vec3> {
        let dir = dir.normalize_or_zero();
        if dir == Vec3::ZERO || !origin.is_finite() || !max_dist.is_finite() || max_dist <= 0.0 {
            return None;
        }
        let above = |p: Vec3| p.y - self.height_at(Vec2::new(p.x, p.z));
        if !self.usable() {
            // Flat ground: the plane y = 0.
            if dir.y.abs() < 1e-6 {
                return None;
            }
            let t = -origin.y / dir.y;
            return (t >= 0.0 && t <= max_dist).then(|| origin + dir * t);
        }
        if above(origin) < 0.0 {
            return None;
        }
        // Past the lowest sample, or rising above the highest, a ray meets
        // nothing more (the ground beyond the grid holds its edge heights).
        let max_dist = if dir.y < -1e-6 {
            max_dist.min((origin.y - self.bottom()) / -dir.y + 1.0)
        } else if origin.y > self.top() {
            return None;
        } else {
            max_dist
        };
        let step = (self.cell_m * 0.5).max(0.1);
        let steps = (max_dist / step).ceil().min(100_000.0) as u32;
        let mut t0 = 0.0;
        for i in 1..=steps {
            let t1 = (i as f32 * step).min(max_dist);
            if above(origin + dir * t1) <= 0.0 {
                let (mut lo, mut hi) = (t0, t1);
                for _ in 0..24 {
                    let mid = (lo + hi) * 0.5;
                    if above(origin + dir * mid) > 0.0 {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let p = origin + dir * hi;
                return Some(Vec3::new(p.x, self.height_at(Vec2::new(p.x, p.z)), p.z));
            }
            t0 = t1;
        }
        None
    }

    /// Coordinates along one axis from `lo` to `hi` (both included) with the
    /// grid's sample lines in between, so a surface built on them meets the
    /// samples exactly. `axis` is 0 for `x`, 1 for `z`. Just the two ends
    /// for a flat grid.
    pub fn breaks(&self, lo: f32, hi: f32, axis: usize) -> Vec<f32> {
        let mut out = vec![lo];
        if self.usable() && lo.is_finite() && hi.is_finite() && hi > lo {
            let origin = self.min[axis];
            let first = ((lo - origin) / self.cell_m).floor() + 1.0;
            let last = ((hi - origin) / self.cell_m).ceil() - 1.0;
            if last >= first && last - first < 4096.0 {
                for k in first as i64..=last as i64 {
                    let v = origin + k as f32 * self.cell_m;
                    if v > lo + 1e-3 && v < hi - 1e-3 {
                        out.push(v);
                    }
                }
            }
        }
        out.push(hi);
        out
    }

    /// Unit surface normal at world `[x, z]`.
    pub fn normal_at(&self, p: Vec2) -> Vec3 {
        let e = (self.cell_m * 0.5).max(0.1);
        let dx = self.height_at(p + Vec2::X * e) - self.height_at(p - Vec2::X * e);
        let dz = self.height_at(p + Vec2::Y * e) - self.height_at(p - Vec2::Y * e);
        Vec3::new(-dx, 2.0 * e, -dz).normalize_or(Vec3::Y)
    }

    /// The line from `a` to `b` (world `[x, z]`) laid on the ground, `lift`
    /// metres above it: points at least every half cell (just the ends on
    /// flat ground), `a` first, `b` excluded so polylines can be chained.
    pub fn drape(&self, a: Vec2, b: Vec2, lift: f32) -> Vec<Vec3> {
        let len = a.distance(b);
        let steps = if self.usable() && len.is_finite() {
            (len / (self.cell_m * 0.5)).ceil().clamp(1.0, 1024.0) as u32
        } else {
            1
        };
        (0..steps)
            .map(|i| {
                let p = a.lerp(b, i as f32 / steps as f32);
                Vec3::new(p.x, self.height_at(p) + lift, p.y)
            })
            .collect()
    }

    /// Samples a grid over `[min, max]` (world `[x, z]`) from elevations
    /// above sea level, with `y = 0` at the elevation of the world origin.
    /// `None` if `elevation` has no value somewhere.
    pub fn sample_from(
        min: Vec2,
        max: Vec2,
        elevation: impl Fn(Vec2) -> Option<f32>,
        attribution: impl Into<String>,
    ) -> Option<Self> {
        let size = (max - min).max(Vec2::splat(1.0));
        let cell_m = TERRAIN_CELL_M.max(size.max_element() / (MAX_TERRAIN_SAMPLES - 1) as f32);
        let count = |v: f32| ((v / cell_m).ceil() as u32 + 1).min(MAX_TERRAIN_SAMPLES);
        let (cols, rows) = (count(size.x), count(size.y));
        let datum_m = elevation(Vec2::ZERO)?;
        let mut heights_dm = Vec::with_capacity(cols as usize * rows as usize);
        for row in 0..rows {
            for col in 0..cols {
                let p = min + Vec2::new(col as f32, row as f32) * cell_m;
                let dm = ((elevation(p)? - datum_m) * 10.0).round();
                heights_dm.push(dm.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16);
            }
        }
        Some(Self {
            min: min.to_array(),
            cell_m,
            cols,
            rows,
            heights_dm,
            datum_m,
            attribution: attribution.into(),
        })
    }
}

/// World `[x, z]` rectangle a bbox's terrain covers: the bbox plus
/// [`TERRAIN_MARGIN_M`] on every side.
pub fn terrain_extent(bbox: &BBox) -> (Vec2, Vec2) {
    let proj = Projection::new(bbox.center());
    let corner =
        |lat, lon| Vec2::from_array(proj.to_world(LatLon::new(lat, lon)).map(|v| v as f32));
    let nw = corner(bbox.north, bbox.west);
    let se = corner(bbox.south, bbox.east);
    (nw.min(se) - TERRAIN_MARGIN_M, nw.max(se) + TERRAIN_MARGIN_M)
}

/// The lat/lon box of [`terrain_extent`].
fn extent_bbox(bbox: &BBox) -> BBox {
    let proj = Projection::new(bbox.center());
    let (min, max) = terrain_extent(bbox);
    let nw = proj.to_latlon([f64::from(min.x), f64::from(min.y)]);
    let se = proj.to_latlon([f64::from(max.x), f64::from(max.y)]);
    BBox {
        south: se.lat,
        west: nw.lon,
        north: nw.lat,
        east: se.lon,
    }
}

/// DEM tiles needed for a bbox's terrain, row-major from the north-west.
pub fn dem_tiles_for(bbox: &BBox) -> Vec<TileId> {
    tiles_covering(&extent_bbox(bbox), DEM_ZOOM)
}

/// Elevation in metres from a Terrarium pixel.
pub fn terrarium_m(r: u8, g: u8, b: u8) -> f32 {
    f32::from(r) * 256.0 + f32::from(g) + f32::from(b) / 256.0 - 32768.0
}

/// One decoded DEM tile: [`DEM_TILE_PX`]² elevations, row-major from the
/// north-west, metres above sea level.
#[derive(Debug, Clone, PartialEq)]
pub struct DemTile {
    pub id: TileId,
    pub elevations: Vec<f32>,
}

impl DemTile {
    /// From 8-bit RGB pixels (3 bytes each), `None` if the size is wrong.
    pub fn from_terrarium_rgb(id: TileId, rgb: &[u8]) -> Option<Self> {
        let n = (DEM_TILE_PX * DEM_TILE_PX) as usize;
        (rgb.len() == n * 3).then(|| Self {
            id,
            elevations: rgb
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| terrarium_m(p[0], p[1], p[2]))
                .collect(),
        })
    }
}

/// Samples a bbox's terrain from its DEM tiles ([`dem_tiles_for`]).
/// `None` if a tile is missing or has the wrong size.
pub fn terrain_from_dem(
    bbox: &BBox,
    tiles: &[DemTile],
    attribution: impl Into<String>,
) -> Option<Terrain> {
    let need = dem_tiles_for(bbox);
    let (x0, y0) = (need.first()?.x, need.first()?.y);
    let (x1, y1) = (need.last()?.x, need.last()?.y);
    let n = (DEM_TILE_PX * DEM_TILE_PX) as usize;
    let lookup = |id: TileId| tiles.iter().find(|t| t.id == id && t.elevations.len() == n);
    if need.iter().any(|id| lookup(*id).is_none()) {
        return None;
    }
    let px = DEM_TILE_PX as f64;
    // Global pixel coordinates the tiles cover, pixel centres at +0.5.
    let (lo_x, lo_y) = (f64::from(x0) * px, f64::from(y0) * px);
    let (hi_x, hi_y) = (f64::from(x1 + 1) * px - 1.0, f64::from(y1 + 1) * px - 1.0);
    let pixel = |gx: f64, gy: f64| -> Option<f32> {
        let (gx, gy) = (gx.clamp(lo_x, hi_x), gy.clamp(lo_y, hi_y));
        let (tx, ty) = ((gx / px) as u32, (gy / px) as u32);
        let t = lookup(TileId {
            z: DEM_ZOOM,
            x: tx,
            y: ty,
        })?;
        let (ix, iy) = (
            (gx - f64::from(tx) * px) as usize,
            (gy - f64::from(ty) * px) as usize,
        );
        t.elevations.get(iy * DEM_TILE_PX as usize + ix).copied()
    };
    let proj = Projection::new(bbox.center());
    let elevation = |w: Vec2| -> Option<f32> {
        let p = proj.to_latlon([f64::from(w.x), f64::from(w.y)]);
        let gx = lon_to_x(p.lon, DEM_ZOOM) * px - 0.5;
        let gy = lat_to_y(p.lat, DEM_ZOOM) * px - 0.5;
        let (fx, fy) = (gx.floor(), gy.floor());
        let (tx, ty) = ((gx - fx) as f32, (gy - fy) as f32);
        let top = pixel(fx, fy)? * (1.0 - tx) + pixel(fx + 1.0, fy)? * tx;
        let bottom = pixel(fx, fy + 1.0)? * (1.0 - tx) + pixel(fx + 1.0, fy + 1.0)? * tx;
        Some(top * (1.0 - ty) + bottom * ty)
    };
    let (min, max) = terrain_extent(bbox);
    Terrain::sample_from(min, max, elevation, attribution)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A plane rising 1 m per 10 m eastward, `y = x / 10`, over ±200 m.
    pub(crate) fn slope() -> Terrain {
        Terrain::sample_from(
            Vec2::splat(-200.0),
            Vec2::splat(200.0),
            |p| Some(100.0 + p.x / 10.0),
            "test",
        )
        .unwrap()
    }

    #[test]
    fn heights_interpolate_and_hold_at_the_edges() {
        let t = slope();
        assert_eq!(t.datum_m, 100.0);
        assert!(t.validate().is_ok());
        assert!(t.height_at(Vec2::ZERO).abs() < 0.05);
        assert!((t.height_at(Vec2::new(37.0, 12.0)) - 3.7).abs() < 0.06);
        assert!((t.height_at(Vec2::new(500.0, 0.0)) - 20.0).abs() < 0.06);
        assert!((t.top() - 20.0).abs() < 0.06);
        let flat = Terrain::default();
        assert!(flat.is_flat() && !t.is_flat());
        assert_eq!(flat.height_at(Vec2::new(5.0, 5.0)), 0.0);
        assert!(flat.validate().is_ok());
        assert_eq!(t.height_at(Vec2::NAN), 0.0);
    }

    #[test]
    fn rejects_inconsistent_grids() {
        let mut t = slope();
        t.heights_dm.pop();
        assert_eq!(t.validate(), Err(InvalidTerrain));
        // Still safe to query.
        assert_eq!(t.height_at(Vec2::ZERO), 0.0);
        let mut t = slope();
        t.cell_m = f32::NAN;
        assert_eq!(t.validate(), Err(InvalidTerrain));
        // Samples closer than a metre would make the client's meshes huge.
        let mut t = slope();
        t.cell_m = 0.1;
        assert_eq!(t.validate(), Err(InvalidTerrain));
        assert_eq!(FLAT, Terrain::default());
    }

    #[test]
    fn lowest_ground_under_a_footprint() {
        let t = slope();
        let ring = [[10.0, 0.0], [30.0, 0.0], [30.0, 20.0], [10.0, 20.0]];
        assert!((t.min_under(&ring) - 1.0).abs() < 0.06);
        // A pit inside the footprint, between its corners.
        let mut pit = Terrain::sample_from(
            Vec2::splat(-50.0),
            Vec2::splat(50.0),
            |p| {
                Some(if p.distance(Vec2::splat(20.0)) < 3.0 {
                    95.0
                } else {
                    100.0
                })
            },
            "",
        )
        .unwrap();
        let square = [[10.0, 10.0], [30.0, 10.0], [30.0, 30.0], [10.0, 30.0]];
        assert!(
            square
                .iter()
                .all(|p| pit.height_at(Vec2::from_array(*p)) == 0.0)
        );
        assert!((pit.min_under(&square) + 5.0).abs() < 0.06);
        pit.heights_dm.clear();
        assert_eq!(pit.min_under(&square), 0.0);
    }

    #[test]
    fn hills_block_sight_lines() {
        // A 10 m ridge along x = 30.
        let ridge = Terrain::sample_from(
            Vec2::splat(-100.0),
            Vec2::splat(100.0),
            |p| Some(if (p.x - 30.0).abs() < 6.0 { 10.0 } else { 0.0 }),
            "",
        )
        .unwrap();
        let top = ridge.top();
        let (a, b) = (Vec3::new(-20.0, 2.0, 0.0), Vec3::new(80.0, 1.0, 0.0));
        assert!(ridge.segment_blocked(a, b, top));
        assert!(!ridge.segment_blocked(a + Vec3::Y * 20.0, b + Vec3::Y * 20.0, top));
        // Both ends on the same side.
        assert!(!ridge.segment_blocked(a, Vec3::new(10.0, 1.0, 5.0), top));
        // On a slope, a subject standing on the ground uphill is seen.
        let t = slope();
        let eye = Vec3::new(-30.0, -3.0 + 2.5, 0.0);
        let subject = Vec3::new(10.0, 1.0, 4.0);
        assert!(!t.segment_blocked(eye, subject, t.top()));
        // Flat ground blocks nothing.
        assert!(!Terrain::default().segment_blocked(a, b, 0.0));
    }

    #[test]
    fn rays_meet_the_ground() {
        let t = slope();
        let hit = t
            .ray_hit(Vec3::new(0.0, 50.0, 0.0), Vec3::new(1.0, -1.0, 0.0), 500.0)
            .unwrap();
        // y = x / 10 and y = 50 - x meet at x = 500 / 11.
        assert!((hit.x - 45.45).abs() < 0.1, "{hit}");
        assert!((hit.y - t.height_at(Vec2::new(hit.x, hit.z))).abs() < 1e-3);
        assert!(
            t.ray_hit(Vec3::new(0.0, 50.0, 0.0), Vec3::Y, 500.0)
                .is_none()
        );
        // Upward from above the highest sample: nothing to meet.
        assert!(
            t.ray_hit(Vec3::new(0.0, 50.0, 0.0), Vec3::new(1.0, 0.1, 0.0), 500.0)
                .is_none()
        );
        // Moving keeps the height above the ground.
        let p = t.moved_to([0.0, 2.0, 0.0], 30.0, 5.0);
        assert!((p[1] - 5.0).abs() < 0.06 && (t.height_above(p) - 2.0).abs() < 1e-4);
        let flat = Terrain::default();
        let hit = flat
            .ray_hit(Vec3::new(0.0, 10.0, 0.0), Vec3::new(1.0, -1.0, 0.0), 100.0)
            .unwrap();
        assert!((hit - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn surfaces_and_lines_follow_the_samples() {
        let t = slope();
        // Samples every 5 m from -200.
        assert_eq!(t.breaks(-12.0, 3.0, 0), vec![-12.0, -10.0, -5.0, 0.0, 3.0]);
        assert_eq!(t.breaks(0.0, 5.0, 1), vec![0.0, 5.0]);
        assert_eq!(Terrain::default().breaks(-12.0, 3.0, 0), vec![-12.0, 3.0]);
        let n = t.normal_at(Vec2::new(10.0, 10.0));
        assert!(
            (n - Vec3::new(-0.1, 1.0, 0.0).normalize()).length() < 1e-3,
            "{n}"
        );
        assert_eq!(Terrain::default().normal_at(Vec2::ZERO), Vec3::Y);
        let line = t.drape(Vec2::ZERO, Vec2::new(10.0, 0.0), 0.5);
        assert_eq!(line.len(), 4);
        assert!(line.iter().all(|p| (p.y - p.x / 10.0 - 0.5).abs() < 0.06));
        assert_eq!(Terrain::default().drape(Vec2::ZERO, Vec2::X, 0.0).len(), 1);
    }

    #[test]
    fn terrarium_decoding() {
        assert_eq!(terrarium_m(128, 0, 0), 0.0);
        assert_eq!(terrarium_m(128, 100, 128), 100.5);
        assert_eq!(terrarium_m(127, 255, 0), -1.0);
    }

    #[test]
    fn samples_dem_tiles() {
        let bbox = BBox::around(LatLon::new(47.7086, -122.206), 300.0);
        let need = dem_tiles_for(&bbox);
        assert!(!need.is_empty() && need.len() <= 4, "{need:?}");
        // 1 m higher per pixel row southward, counted across tiles so the
        // mosaic is seamless.
        let first = need[0].y * DEM_TILE_PX;
        let tiles: Vec<DemTile> = need
            .iter()
            .map(|&id| {
                let mut rgb = Vec::new();
                for row in 0..DEM_TILE_PX {
                    let e = 32768 + id.y * DEM_TILE_PX + row - first;
                    for _ in 0..DEM_TILE_PX {
                        rgb.extend([(e >> 8) as u8, (e & 0xff) as u8, 0]);
                    }
                }
                DemTile::from_terrarium_rgb(id, &rgb).unwrap()
            })
            .collect();
        let t = terrain_from_dem(&bbox, &tiles, "dem").unwrap();
        assert_eq!(t.attribution, "dem");
        assert!(t.validate().is_ok());
        // About 3.2 m per pixel here: southward (+Z) is uphill by ~0.31 m/m.
        let (n, s) = (
            t.height_at(Vec2::new(0.0, -100.0)),
            t.height_at(Vec2::new(0.0, 100.0)),
        );
        assert!((s - n - 62.0).abs() < 3.0, "{n} {s}");
        assert!(terrain_from_dem(&bbox, &tiles[1..], "dem").is_none());
    }
}
