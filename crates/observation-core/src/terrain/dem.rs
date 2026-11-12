//! Sampling the grid from Terrarium DEM tiles.

use super::{TERRAIN_MARGIN_M, Terrain};
use crate::geo::{BBox, LatLon, Projection};
use crate::tiles::{TileId, lat_to_y, lon_to_x, tiles_covering};
use glam::Vec2;

/// Credit for the default elevation source, the AWS Open Data Terrain Tiles.
pub const TERRAIN_TILES_ATTRIBUTION: &str = "Elevation: Mapzen Terrain Tiles, USGS and others";
/// Where the Terrain Tiles list every source's required credit.
pub const TERRAIN_TILES_CREDITS_URL: &str =
    "https://github.com/tilezen/joerd/blob/master/docs/attribution.md";

/// Zoom of the DEM tiles: Terrarium's finest, about 3-5 m per pixel at
/// mid latitudes, finer than the source data almost everywhere.
pub const DEM_ZOOM: u8 = 15;
/// Pixels along each side of a DEM tile.
pub const DEM_TILE_PX: u32 = 256;

/// World `[x, z]` rectangle a bbox's terrain covers: the bbox plus
/// [`TERRAIN_MARGIN_M`] on every side.
pub fn terrain_extent(bbox: &BBox) -> (Vec2, Vec2) {
    let (min, max) = Projection::new(bbox.center()).world_extent(bbox);
    (min - TERRAIN_MARGIN_M, max + TERRAIN_MARGIN_M)
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

/// The DEM tiles a bbox's terrain needs ([`dem_tiles_for`]), read as one
/// seamless grid of pixels: the tile lookup and bilinear sampling behind
/// [`terrain_from_dem`].
#[derive(Debug, Clone, Copy)]
pub struct DemMosaic<'a> {
    tiles: &'a [DemTile],
    /// Global pixel coordinates the tiles cover, pixel centres at +0.5.
    lo_x: f64,
    lo_y: f64,
    hi_x: f64,
    hi_y: f64,
}

impl<'a> DemMosaic<'a> {
    /// The mosaic of `tiles` over a bbox's terrain. `None` if a tile it
    /// needs is missing or has the wrong size.
    pub fn new(bbox: &BBox, tiles: &'a [DemTile]) -> Option<Self> {
        let need = dem_tiles_for(bbox);
        let (x0, y0) = (need.first()?.x, need.first()?.y);
        let (x1, y1) = (need.last()?.x, need.last()?.y);
        let mosaic = Self {
            tiles,
            lo_x: f64::from(x0) * PX,
            lo_y: f64::from(y0) * PX,
            hi_x: f64::from(x1 + 1) * PX - 1.0,
            hi_y: f64::from(y1 + 1) * PX - 1.0,
        };
        if need.iter().any(|id| mosaic.tile(*id).is_none()) {
            return None;
        }
        Some(mosaic)
    }

    /// The first tile with this id and the right size.
    fn tile(&self, id: TileId) -> Option<&'a DemTile> {
        let n = (DEM_TILE_PX * DEM_TILE_PX) as usize;
        self.tiles
            .iter()
            .find(|t| t.id == id && t.elevations.len() == n)
    }

    /// The elevation of the pixel at global pixel coordinates `(gx, gy)`,
    /// clamped to the mosaic.
    fn pixel(&self, gx: f64, gy: f64) -> Option<f32> {
        let (gx, gy) = (
            gx.clamp(self.lo_x, self.hi_x),
            gy.clamp(self.lo_y, self.hi_y),
        );
        let (tx, ty) = ((gx / PX) as u32, (gy / PX) as u32);
        let t = self.tile(TileId {
            z: DEM_ZOOM,
            x: tx,
            y: ty,
        })?;
        let (col, row) = (
            (gx - f64::from(tx) * PX) as usize,
            (gy - f64::from(ty) * PX) as usize,
        );
        t.elevations.get(row * DEM_TILE_PX as usize + col).copied()
    }

    /// Elevation in metres above sea level at `p`, interpolated bilinearly
    /// between the four nearest pixel centres; past the mosaic's edge, the
    /// edge pixels' value.
    pub fn elevation_at(&self, p: LatLon) -> Option<f32> {
        let gx = lon_to_x(p.lon, DEM_ZOOM) * PX - 0.5;
        let gy = lat_to_y(p.lat, DEM_ZOOM) * PX - 0.5;
        self.bilinear(gx, gy)
    }

    /// [`Self::elevation_at`] at global pixel coordinates measured from the
    /// pixel centres (pixel `(c, r)`'s centre is at `(c, r)`).
    fn bilinear(&self, gx: f64, gy: f64) -> Option<f32> {
        let (fx, fy) = (gx.floor(), gy.floor());
        let (tx, ty) = ((gx - fx) as f32, (gy - fy) as f32);
        let top = self.pixel(fx, fy)? * (1.0 - tx) + self.pixel(fx + 1.0, fy)? * tx;
        let bottom = self.pixel(fx, fy + 1.0)? * (1.0 - tx) + self.pixel(fx + 1.0, fy + 1.0)? * tx;
        Some(top * (1.0 - ty) + bottom * ty)
    }
}

/// [`DEM_TILE_PX`] as the `f64` the pixel maths uses.
const PX: f64 = DEM_TILE_PX as f64;

/// Samples a bbox's terrain from its DEM tiles ([`dem_tiles_for`]).
/// `None` if a tile is missing or has the wrong size.
pub fn terrain_from_dem(
    bbox: &BBox,
    tiles: &[DemTile],
    attribution: impl Into<String>,
) -> Option<Terrain> {
    let mosaic = DemMosaic::new(bbox, tiles)?;
    let proj = Projection::new(bbox.center());
    let elevation = |w: Vec2| mosaic.elevation_at(proj.to_latlon([f64::from(w.x), f64::from(w.y)]));
    let (min, max) = terrain_extent(bbox);
    Terrain::sample_from(min, max, elevation, attribution)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrarium_decoding() {
        assert_eq!(terrarium_m(128, 0, 0), 0.0);
        assert_eq!(terrarium_m(128, 100, 128), 100.5);
        assert_eq!(terrarium_m(127, 255, 0), -1.0);
    }

    #[test]
    fn terrain_extent_is_the_bbox_plus_the_margin() {
        let bbox = BBox::around(LatLon::new(47.7086, -122.206), 300.0);
        let (min, max) = terrain_extent(&bbox);
        let size = max - min;
        let want = 300.0 + 2.0 * TERRAIN_MARGIN_M;
        assert!(
            (size.x - want).abs() < 0.01 && (size.y - want).abs() < 0.01,
            "{size}"
        );
        assert!(
            ((min + max) / 2.0).length() < 0.01,
            "centred on the bbox centre"
        );
    }

    #[test]
    fn a_mosaic_of_a_plane_samples_the_plane() {
        // Each pixel centre holds a plane over global pixel coordinates,
        // continued across tile seams; bilinear sampling reproduces it.
        let bbox = BBox::around(LatLon::new(-33.8688, 151.2093), 450.0);
        let need = dem_tiles_for(&bbox);
        assert!(need.len() > 1, "the box must straddle a seam: {need:?}");
        let (x0, y0) = (f64::from(need[0].x) * PX, f64::from(need[0].y) * PX);
        let plane = |gx: f64, gy: f64| 100.0 + 0.25 * (gx - x0) + 0.5 * (gy - y0);
        let tiles: Vec<DemTile> = need
            .iter()
            .map(|&id| DemTile {
                id,
                elevations: (0..DEM_TILE_PX * DEM_TILE_PX)
                    .map(|i| {
                        let gx = f64::from(id.x * DEM_TILE_PX + i % DEM_TILE_PX);
                        let gy = f64::from(id.y * DEM_TILE_PX + i / DEM_TILE_PX);
                        plane(gx, gy) as f32
                    })
                    .collect(),
            })
            .collect();
        let mosaic = DemMosaic::new(&bbox, &tiles).unwrap();
        for i in 0..=20 {
            for j in 0..=20 {
                let p = LatLon::new(
                    bbox.south + (bbox.north - bbox.south) * f64::from(i) / 20.0,
                    bbox.west + (bbox.east - bbox.west) * f64::from(j) / 20.0,
                );
                let gx = lon_to_x(p.lon, DEM_ZOOM) * PX - 0.5;
                let gy = lat_to_y(p.lat, DEM_ZOOM) * PX - 0.5;
                let got = mosaic.elevation_at(p).unwrap();
                let want = plane(gx, gy) as f32;
                assert!((got - want).abs() < 1e-3, "{p:?}: {got} != {want}");
            }
        }
        // Past the mosaic's north-west corner: the corner pixel.
        let far = LatLon::new(bbox.north + 1.0, bbox.west - 1.0);
        assert_eq!(mosaic.elevation_at(far), Some(100.0));
        // Past the south-east corner: the last pixel.
        let last = need[need.len() - 1];
        let (x1, y1) = (
            f64::from(last.x + 1) * PX - 1.0,
            f64::from(last.y + 1) * PX - 1.0,
        );
        let far = LatLon::new(bbox.south - 1.0, bbox.east + 1.0);
        assert_eq!(mosaic.elevation_at(far), Some(plane(x1, y1) as f32));
        // A missing or truncated tile is refused.
        assert!(DemMosaic::new(&bbox, &tiles[1..]).is_none());
        let mut short = tiles.clone();
        short[0].elevations.pop();
        assert!(DemMosaic::new(&bbox, &short).is_none());
    }

    #[test]
    fn mosaic_samples_are_pinned_to_the_bit() {
        // A noisy, non-planar DEM over a box that straddles a tile seam. The
        // expected values are the exact f32 bits the sampling gives today,
        // which are the same bits the closures in `terrain_from_dem` gave
        // before they moved into DemMosaic. They pin the pixel and bilinear
        // arithmetic token for token, so a rewrite that moves results by a
        // few ULP fails here. They may only change with a deliberate,
        // documented change to how terrain is sampled.
        let bbox = BBox::around(LatLon::new(-33.8688, 151.2093), 450.0);
        let need = dem_tiles_for(&bbox);
        assert!(need.len() > 1, "the box must straddle a seam: {need:?}");
        let tiles: Vec<DemTile> = need
            .iter()
            .map(|&id| {
                let mut seed = u64::from(id.x) * 1_000_003 + u64::from(id.y);
                let elevations = (0..DEM_TILE_PX * DEM_TILE_PX)
                    .map(|_| {
                        seed = seed
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1_442_695_040_888_963_407);
                        ((seed >> 33) % 100_000) as f32 / 37.0 - 500.0
                    })
                    .collect();
                DemTile { id, elevations }
            })
            .collect();
        let mosaic = DemMosaic::new(&bbox, &tiles).unwrap();
        // Sampled at pixel coordinates rather than lat/lon, so that no
        // platform-dependent transcendental function feeds the pinned bits.
        let (x0, y0) = (f64::from(need[0].x) * PX, f64::from(need[0].y) * PX);
        let last = need[need.len() - 1];
        let (x1, y1) = (f64::from(last.x + 1) * PX, f64::from(last.y + 1) * PX);
        let seam_x = f64::from(need[0].x + 1) * PX;
        let points = [
            ("interior", (x0 + 100.3, y0 + 57.8)),
            ("x seam", (seam_x - 0.4, y0 + 20.6)),
            ("north-west, clamped", (x0 - 40.25, y0 - 7.5)),
            ("south-east, clamped", (x1 + 12.75, y1 + 30.125)),
            ("past the east edge", (x1 + 3.2, (y0 + y1) / 2.0 + 0.7)),
        ];
        let bits: Vec<(&str, Option<u32>)> = points
            .iter()
            .map(|&(name, (gx, gy))| (name, mosaic.bilinear(gx, gy).map(f32::to_bits)))
            .collect();
        assert_eq!(
            bits,
            [
                ("interior", Some(0x4414_8fea)),
                ("x seam", Some(0x441b_2a8d)),
                ("north-west, clamped", Some(0x431c_b3e4)),
                ("south-east, clamped", Some(0x41ab_3e40)),
                ("past the east edge", Some(0x44c5_d532)),
            ]
        );
        // And every point of a 61 × 61 lattice over the mosaic and past its
        // edges, folded into one FNV-1a hash of their bits (single points can
        // agree by chance).
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for i in 0..=60 {
            for j in 0..=60 {
                let gx = x0 - 5.0 + (x1 - x0 + 10.0) * f64::from(j) / 60.0 + 0.137;
                let gy = y0 - 5.0 + (y1 - y0 + 10.0) * f64::from(i) / 60.0 + 0.291;
                let v = mosaic.bilinear(gx, gy).map_or(u32::MAX, f32::to_bits);
                for b in v.to_le_bytes() {
                    hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        assert_eq!(hash, 0xf92b_aa9a_6760_427e);
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
