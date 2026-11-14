//! Roads as the bundle carries them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Road {
    pub osm_id: i64,
    /// OSM `highway=*` value.
    pub highway: String,
    pub name: Option<String>,
    /// Carriageway width used for street-coverage statistics, metres.
    pub width_m: f32,
    /// Polyline in world `[x, z]`.
    pub points: Vec<[f32; 2]>,
}
