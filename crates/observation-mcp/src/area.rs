//! The demo street: the one area available offline.

use crate::ToolError;
use observation_core::bundle::AreaBundle;
use observation_core::coverage::StaticLayers;
use observation_core::glam::Vec2;
use observation_core::scene::{AreaRef, SceneFile};
use observation_server::config::{DEFAULT_ATTRIBUTION, DEFAULT_DEM_ATTRIBUTION};
use std::sync::LazyLock;

/// Ground raster resolution, metres (the client's `GROUND_CELL_M`).
pub const GROUND_CELL_M: f32 = 0.5;
/// Façade patch size, metres (the client's `FACADE_CELL_M`).
pub const FACADE_CELL_M: f32 = 1.0;

static DEMO: LazyLock<Result<AreaBundle, String>> = LazyLock::new(|| {
    // Built from the server's committed fixture, so it needs no network. The build time is
    // left at 0 ("unknown"): the data date that matters is the fixture's own.
    let bytes =
        observation_server::demo::build_demo(DEFAULT_ATTRIBUTION, DEFAULT_DEM_ATTRIBUTION, 0)
            .map_err(|e| e.to_string())?;
    AreaBundle::decode(&bytes).map_err(|e| e.to_string())
});

static DEMO_LAYERS: LazyLock<Result<StaticLayers, String>> = LazyLock::new(|| {
    let bundle = DEMO.as_ref().map_err(Clone::clone)?;
    Ok(StaticLayers::new(bundle, GROUND_CELL_M, FACADE_CELL_M))
});

/// The demo street's bundle, built on first use.
pub fn demo() -> Result<&'static AreaBundle, ToolError> {
    DEMO.as_ref().map_err(|e| ToolError::Demo(e.clone()))
}

/// The demo street's coverage layers (ground raster, façades, occluders),
/// built on first use. Slower than [`demo`]: call it off the async runtime.
pub fn demo_layers() -> Result<&'static StaticLayers, ToolError> {
    DEMO_LAYERS.as_ref().map_err(|e| ToolError::Demo(e.clone()))
}

/// The bundle of the area `scene` is on, if it is available offline.
pub fn bundle_for(scene: &SceneFile) -> Result<&'static AreaBundle, ToolError> {
    match scene.area {
        AreaRef::Demo => demo(),
        AreaRef::Osm { .. } => Err(ToolError::AreaUnavailable),
    }
}

/// The coverage layers of the area `scene` is on (see [`demo_layers`]).
pub fn layers_for(scene: &SceneFile) -> Result<&'static StaticLayers, ToolError> {
    match scene.area {
        AreaRef::Demo => demo_layers(),
        AreaRef::Osm { .. } => Err(ToolError::AreaUnavailable),
    }
}

/// World `y` of the ground at world `[x, z]`.
pub fn ground_y(bundle: &AreaBundle, x: f32, z: f32) -> f32 {
    bundle.terrain.height_at(Vec2::new(x, z))
}

/// The area's extent in world `[x, z]`, metres: `(min, max)`.
pub fn extent(bundle: &AreaBundle) -> ([f32; 2], [f32; 2]) {
    let (min, max) = bundle.projection().world_extent(&bundle.bbox);
    (min.to_array(), max.to_array())
}

/// Refuses a point outside the area: nothing is analysed there.
pub fn check_inside(bundle: &AreaBundle, x: f32, z: f32) -> Result<(), ToolError> {
    let (min, max) = extent(bundle);
    let inside = x.is_finite()
        && z.is_finite()
        && (min[0]..=max[0]).contains(&x)
        && (min[1]..=max[1]).contains(&z);
    if inside {
        Ok(())
    } else {
        Err(ToolError::invalid(format!(
            "[{x}, {z}] is outside the area, which spans x {:.0} to {:.0} and z {:.0} to {:.0} m",
            min[0], max[0], min[1], max[1]
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_street_builds_offline() {
        let b = demo().unwrap();
        assert!(b.buildings.len() > 80);
        let (min, max) = extent(b);
        assert!(min[0] < 0.0 && max[0] > 0.0 && min[1] < 0.0 && max[1] > 0.0);
        assert!(check_inside(b, 0.0, 0.0).is_ok());
        assert!(check_inside(b, max[0] + 1.0, 0.0).is_err());
        assert!(check_inside(b, f32::NAN, 0.0).is_err());
    }
}
