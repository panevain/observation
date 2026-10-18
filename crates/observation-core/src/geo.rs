//! WGS84 ⇄ local world-frame projection and bounding boxes.
//!
//! Areas are small (≤ [`MAX_BBOX_SIDE_M`]), so a local equirectangular
//! projection around the area centre is accurate to well under a metre.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Mean Earth radius (IUGG), metres.
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Largest allowed side length of a requested area, metres.
pub const MAX_BBOX_SIDE_M: f64 = 500.0;

/// Smallest allowed side length of a requested area, metres.
pub const MIN_BBOX_SIDE_M: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub const fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum BBoxError {
    #[error("expected four comma-separated numbers: south,west,north,east")]
    Format,
    #[error("coordinates out of range")]
    Range,
    #[error("south must be < north and west must be < east")]
    Order,
    #[error("area side lengths must be between {min} and {max} m (got {w:.0} × {h:.0} m)")]
    Size { w: f64, h: f64, min: f64, max: f64 },
}

/// Axis-aligned lat/lon box. Does not support crossing the antimeridian.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl BBox {
    /// Builds a box and checks ordering and coordinate ranges (not size).
    pub fn new(south: f64, west: f64, north: f64, east: f64) -> Result<Self, BBoxError> {
        let vals = [south, west, north, east];
        if vals.iter().any(|v| !v.is_finite()) {
            return Err(BBoxError::Format);
        }
        if !(-85.0..=85.0).contains(&south)
            || !(-85.0..=85.0).contains(&north)
            || !(-180.0..=180.0).contains(&west)
            || !(-180.0..=180.0).contains(&east)
        {
            return Err(BBoxError::Range);
        }
        if south >= north || west >= east {
            return Err(BBoxError::Order);
        }
        Ok(Self {
            south,
            west,
            north,
            east,
        })
    }

    /// Parses `"south,west,north,east"` (the Overpass ordering).
    pub fn parse(s: &str) -> Result<Self, BBoxError> {
        let parts: Vec<f64> = s
            .split(',')
            .map(|p| p.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|_| BBoxError::Format)?;
        match parts.as_slice() {
            [s, w, n, e] => Self::new(*s, *w, *n, *e),
            _ => Err(BBoxError::Format),
        }
    }

    /// Builds a square-ish box of `side_m` metres centred on `c`.
    pub fn around(c: LatLon, side_m: f64) -> Self {
        let half_lat = (side_m / 2.0 / EARTH_RADIUS_M).to_degrees();
        let half_lon = half_lat / c.lat.to_radians().cos().max(1e-6);
        Self {
            south: c.lat - half_lat,
            west: c.lon - half_lon,
            north: c.lat + half_lat,
            east: c.lon + half_lon,
        }
    }

    pub fn center(&self) -> LatLon {
        LatLon::new(
            (self.south + self.north) / 2.0,
            (self.west + self.east) / 2.0,
        )
    }

    /// East–west extent, metres (measured at the centre latitude).
    pub fn width_m(&self) -> f64 {
        (self.east - self.west).to_radians() * EARTH_RADIUS_M * self.center().lat.to_radians().cos()
    }

    /// North–south extent, metres.
    pub fn height_m(&self) -> f64 {
        (self.north - self.south).to_radians() * EARTH_RADIUS_M
    }

    /// Checks the box is within the allowed request size.
    pub fn validate_size(&self) -> Result<(), BBoxError> {
        let (w, h) = (self.width_m(), self.height_m());
        let ok = |v: f64| (MIN_BBOX_SIDE_M..=MAX_BBOX_SIDE_M + 1e-6).contains(&v);
        if ok(w) && ok(h) {
            Ok(())
        } else {
            Err(BBoxError::Size {
                w,
                h,
                min: MIN_BBOX_SIDE_M,
                max: MAX_BBOX_SIDE_M,
            })
        }
    }

    /// Expands the box outward so each edge lies on a grid of `step_deg`
    /// degrees. Used to build stable cache keys from user-drawn boxes.
    pub fn snapped(&self, step_deg: f64) -> Self {
        Self {
            south: (self.south / step_deg).floor() * step_deg,
            west: (self.west / step_deg).floor() * step_deg,
            north: (self.north / step_deg).ceil() * step_deg,
            east: (self.east / step_deg).ceil() * step_deg,
        }
    }

    /// Stable textual key, e.g. for cache file names.
    pub fn cache_key(&self) -> String {
        format!(
            "{:.5}_{:.5}_{:.5}_{:.5}",
            self.south, self.west, self.north, self.east
        )
    }

    pub fn contains(&self, p: LatLon) -> bool {
        (self.south..=self.north).contains(&p.lat) && (self.west..=self.east).contains(&p.lon)
    }

    /// Overpass `(s,w,n,e)` filter string.
    pub fn overpass(&self) -> String {
        format!(
            "{:.7},{:.7},{:.7},{:.7}",
            self.south, self.west, self.north, self.east
        )
    }
}

/// Local projection centred on `origin`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    origin: LatLon,
    cos_lat: f64,
}

impl Projection {
    pub fn new(origin: LatLon) -> Self {
        Self {
            origin,
            cos_lat: origin.lat.to_radians().cos(),
        }
    }

    pub fn origin(&self) -> LatLon {
        self.origin
    }

    /// Lat/lon → world `[x, z]` (east, south) in metres.
    pub fn to_world(&self, p: LatLon) -> [f64; 2] {
        let x = (p.lon - self.origin.lon).to_radians() * EARTH_RADIUS_M * self.cos_lat;
        let z = -(p.lat - self.origin.lat).to_radians() * EARTH_RADIUS_M;
        [x, z]
    }

    /// World `[x, z]` → lat/lon.
    pub fn to_latlon(&self, w: [f64; 2]) -> LatLon {
        LatLon {
            lat: self.origin.lat - (w[1] / EARTH_RADIUS_M).to_degrees(),
            lon: self.origin.lon + (w[0] / (EARTH_RADIUS_M * self.cos_lat)).to_degrees(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_validate() {
        let b = BBox::parse("51.5, -0.13, 51.503, -0.125").unwrap();
        assert!(b.width_m() > 300.0 && b.width_m() < 400.0);
        assert!(b.validate_size().is_ok());
        assert_eq!(BBox::parse("1,2,3"), Err(BBoxError::Format));
        assert_eq!(BBox::parse("2,0,1,1"), Err(BBoxError::Order));
        assert_eq!(BBox::parse("89,0,89.5,1"), Err(BBoxError::Range));
        let big = BBox::parse("51.0,0.0,51.1,0.1").unwrap();
        assert!(matches!(big.validate_size(), Err(BBoxError::Size { .. })));
    }

    #[test]
    fn around_has_requested_size() {
        let b = BBox::around(LatLon::new(40.0, -75.0), 300.0);
        assert!((b.width_m() - 300.0).abs() < 1.0);
        assert!((b.height_m() - 300.0).abs() < 1.0);
    }

    #[test]
    fn projection_round_trip_and_axes() {
        let o = LatLon::new(47.6, -122.3);
        let p = Projection::new(o);
        assert_eq!(p.to_world(o), [0.0, 0.0]);
        let north = p.to_world(LatLon::new(47.601, -122.3));
        assert!(north[1] < -100.0, "north must be -Z");
        let east = p.to_world(LatLon::new(47.6, -122.299));
        assert!(east[0] > 70.0, "east must be +X");
        let q = LatLon::new(47.6012, -122.2987);
        let back = p.to_latlon(p.to_world(q));
        assert!((back.lat - q.lat).abs() < 1e-9 && (back.lon - q.lon).abs() < 1e-9);
    }

    #[test]
    fn snapping_expands() {
        let b = BBox::parse("51.50012,-0.13007,51.50291,-0.12512").unwrap();
        let s = b.snapped(0.0001);
        assert!(s.south <= b.south && s.west <= b.west && s.north >= b.north && s.east >= b.east);
        assert_eq!(s.cache_key(), b.snapped(0.0001).cache_key());
    }
}
