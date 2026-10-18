//! Slippy-map (Web Mercator) tile maths.

use crate::geo::{BBox, LatLon};
use serde::{Deserialize, Serialize};

pub const MAX_ZOOM: u8 = 19;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub fn new(z: u8, x: u32, y: u32) -> Option<Self> {
        let n = 1u32.checked_shl(u32::from(z))?;
        (z <= MAX_ZOOM && x < n && y < n).then_some(Self { z, x, y })
    }

    /// North-west corner of the tile.
    pub fn nw(&self) -> LatLon {
        LatLon::new(
            y_to_lat(f64::from(self.y), self.z),
            x_to_lon(f64::from(self.x), self.z),
        )
    }

    /// Geographic bounds of the tile.
    pub fn bounds(&self) -> BBox {
        let nw = self.nw();
        let se = LatLon::new(
            y_to_lat(f64::from(self.y + 1), self.z),
            x_to_lon(f64::from(self.x + 1), self.z),
        );
        BBox {
            south: se.lat,
            west: nw.lon,
            north: nw.lat,
            east: se.lon,
        }
    }
}

/// Fractional tile x for a longitude.
pub fn lon_to_x(lon: f64, z: u8) -> f64 {
    (lon + 180.0) / 360.0 * f64::from(1u32 << z)
}

/// Fractional tile y for a latitude.
pub fn lat_to_y(lat: f64, z: u8) -> f64 {
    let r = lat.to_radians();
    (1.0 - r.tan().asinh() / std::f64::consts::PI) / 2.0 * f64::from(1u32 << z)
}

pub fn x_to_lon(x: f64, z: u8) -> f64 {
    x / f64::from(1u32 << z) * 360.0 - 180.0
}

pub fn y_to_lat(y: f64, z: u8) -> f64 {
    let n = std::f64::consts::PI - 2.0 * std::f64::consts::PI * y / f64::from(1u32 << z);
    n.sinh().atan().to_degrees()
}

/// Inclusive tile index ranges `(x0..=x1, y0..=y1)` covering `bbox` at zoom `z`.
pub fn tile_range(bbox: &BBox, z: u8) -> (u32, u32, u32, u32) {
    let max = (1u32 << z) - 1;
    let clamp = |v: f64| (v.floor().max(0.0) as u32).min(max);
    let x0 = clamp(lon_to_x(bbox.west, z));
    let x1 = clamp(lon_to_x(bbox.east, z));
    let y0 = clamp(lat_to_y(bbox.north, z));
    let y1 = clamp(lat_to_y(bbox.south, z));
    (x0, x1, y0, y1)
}

/// All tiles covering `bbox` at zoom `z`, row-major from the north-west.
pub fn tiles_covering(bbox: &BBox, z: u8) -> Vec<TileId> {
    let (x0, x1, y0, y1) = tile_range(bbox, z);
    (y0..=y1)
        .flat_map(|y| (x0..=x1).map(move |x| TileId { z, x, y }))
        .collect()
}

/// Highest zoom (≤ `max_zoom`) at which `bbox` is covered by at most `max_tiles` tiles.
pub fn zoom_for_bbox(bbox: &BBox, max_tiles: usize, max_zoom: u8) -> u8 {
    (0..=max_zoom.min(MAX_ZOOM))
        .rev()
        .find(|&z| {
            let (x0, x1, y0, y1) = tile_range(bbox, z);
            ((x1 - x0 + 1) as usize) * ((y1 - y0 + 1) as usize) <= max_tiles
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tile() {
        // London, z=10 → tile 511/340 (well-known example).
        let x = lon_to_x(-0.1276, 10).floor() as u32;
        let y = lat_to_y(51.5072, 10).floor() as u32;
        assert_eq!((x, y), (511, 340));
    }

    #[test]
    fn round_trip_and_bounds() {
        let t = TileId::new(18, 130_000, 87_000).unwrap();
        let b = t.bounds();
        assert!(b.north > b.south && b.east > b.west);
        assert!((lon_to_x(b.west, 18) - 130_000.0).abs() < 1e-6);
        assert!((lat_to_y(b.north, 18) - 87_000.0).abs() < 1e-6);
        assert!(TileId::new(2, 4, 0).is_none());
    }

    #[test]
    fn covering_and_zoom() {
        let bbox = crate::geo::BBox::around(LatLon::new(51.5, -0.12), 400.0);
        let z = zoom_for_bbox(&bbox, 36, 19);
        let n = tiles_covering(&bbox, z).len();
        assert!((1..=36).contains(&n));
        assert!(z >= 17, "400 m at ≤36 tiles should allow z17+ (got {z})");
        assert!(tiles_covering(&bbox, z + 1).len() > 36 || z == 19);
    }
}
