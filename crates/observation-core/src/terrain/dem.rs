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
        let (col, row) = (
            (gx - f64::from(tx) * px) as usize,
            (gy - f64::from(ty) * px) as usize,
        );
        t.elevations.get(row * DEM_TILE_PX as usize + col).copied()
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
mod tests {
    use super::*;

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
