//! The areas the tools work on: the demo street, which is always available
//! offline, and the user's real areas while the app is connected.
//!
//! A real area comes from the paired server's `/api/area` (see
//! [`crate::link`]). The server fetches it before a tool runs and puts it in
//! the connection's [`AreaCache`]; the tool reaches that cache through
//! [`with_areas`], so the edits stay plain functions of a scene. Without a
//! connection the cache is empty and a scene on an OpenStreetMap area is
//! refused, as before.

use crate::ToolError;
use observation_core::bundle::AreaBundle;
use observation_core::coverage::StaticLayers;
use observation_core::geo::BBox;
use observation_core::glam::Vec2;
use observation_core::scene::{AreaRef, SceneFile};
use observation_core::terrain::TERRAIN_TILES_ATTRIBUTION;
use observation_mapdata::OSM_ATTRIBUTION;
use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};

/// Ground raster resolution, metres (the client's `GROUND_CELL_M`).
pub const GROUND_CELL_M: f32 = 0.5;
/// Façade patch size, metres (the client's `FACADE_CELL_M`).
pub const FACADE_CELL_M: f32 = 1.0;
/// Most real areas a connection keeps (each with its layers once built).
pub const MAX_CACHED_AREAS: usize = 4;

static DEMO: LazyLock<Result<Arc<AreaBundle>, String>> = LazyLock::new(|| {
    // Built from the committed fixtures, so it needs no network, with the server's default
    // credits. The build time is left at 0 ("unknown"): the data date that matters is the
    // fixture's own.
    observation_mapdata::demo::demo_bundle(OSM_ATTRIBUTION, TERRAIN_TILES_ATTRIBUTION, 0)
        .map(Arc::new)
        .map_err(|e| e.to_string())
});

static DEMO_LAYERS: LazyLock<Result<Arc<StaticLayers>, String>> = LazyLock::new(|| {
    let bundle = DEMO.as_ref().map_err(Clone::clone)?;
    Ok(Arc::new(StaticLayers::new(
        bundle,
        GROUND_CELL_M,
        FACADE_CELL_M,
    )))
});

/// The demo street's bundle, built on first use.
pub fn demo() -> Result<&'static AreaBundle, ToolError> {
    DEMO.as_deref().map_err(|e| ToolError::Demo(e.clone()))
}

/// The demo street's coverage layers (ground raster, façades, occluders),
/// built on first use. Slower than [`demo`]: call it off the async runtime.
pub fn demo_layers() -> Result<&'static StaticLayers, ToolError> {
    DEMO_LAYERS
        .as_deref()
        .map_err(|e| ToolError::Demo(e.clone()))
}

/// One real area held by a connection.
#[derive(Debug)]
struct Entry {
    bbox: BBox,
    bundle: Arc<AreaBundle>,
    layers: OnceLock<Arc<StaticLayers>>,
}

/// The real areas a connection fetched, most recent last, at most
/// [`MAX_CACHED_AREAS`], and the last area that could not be fetched.
#[derive(Debug, Default)]
pub struct AreaCache {
    entries: Mutex<VecDeque<Arc<Entry>>>,
    failed: Mutex<Option<(BBox, String)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AreaCache {
    fn entry(&self, bbox: &BBox) -> Option<Arc<Entry>> {
        lock(&self.entries)
            .iter()
            .find(|e| e.bbox == *bbox)
            .cloned()
    }

    /// Whether `bbox`'s bundle is held.
    pub fn contains(&self, bbox: &BBox) -> bool {
        self.entry(bbox).is_some()
    }

    /// Holds `bundle` for `bbox`, dropping the oldest area beyond the cap.
    pub fn insert(&self, bbox: BBox, bundle: AreaBundle) {
        let mut entries = lock(&self.entries);
        entries.retain(|e| e.bbox != bbox);
        entries.push_back(Arc::new(Entry {
            bbox,
            bundle: Arc::new(bundle),
            layers: OnceLock::new(),
        }));
        while entries.len() > MAX_CACHED_AREAS {
            entries.pop_front();
        }
        let mut failed = lock(&self.failed);
        if failed.as_ref().is_some_and(|(b, _)| *b == bbox) {
            *failed = None;
        }
    }

    /// Records why `bbox` could not be fetched, for the tool that needs it.
    pub fn fail(&self, bbox: BBox, why: String) {
        *lock(&self.failed) = Some((bbox, why));
    }

    /// Forgets every area (the connection ended).
    pub fn clear(&self) {
        lock(&self.entries).clear();
        *lock(&self.failed) = None;
    }

    fn missing(&self, bbox: &BBox) -> ToolError {
        match &*lock(&self.failed) {
            Some((b, why)) if b == bbox => ToolError::AreaFetch(why.clone()),
            _ => ToolError::AreaUnavailable,
        }
    }

    fn bundle(&self, bbox: &BBox) -> Result<Arc<AreaBundle>, ToolError> {
        self.entry(bbox)
            .map(|e| Arc::clone(&e.bundle))
            .ok_or_else(|| self.missing(bbox))
    }

    fn layers(&self, bbox: &BBox) -> Result<Arc<StaticLayers>, ToolError> {
        let entry = self.entry(bbox).ok_or_else(|| self.missing(bbox))?;
        Ok(Arc::clone(entry.layers.get_or_init(|| {
            Arc::new(StaticLayers::new(
                &entry.bundle,
                GROUND_CELL_M,
                FACADE_CELL_M,
            ))
        })))
    }
}

tokio::task_local! {
    /// The connection's areas while a tool runs (see [`with_areas`]).
    static AREAS: Arc<AreaCache>;
}

/// Runs `f` with `areas` as the real areas [`bundle_for`] and [`layers_for`]
/// can see.
pub fn with_areas<R>(areas: Arc<AreaCache>, f: impl FnOnce() -> R) -> R {
    AREAS.sync_scope(areas, f)
}

/// Runs the future `f` with `areas` visible, as [`with_areas`] does.
pub async fn scope_areas<F: std::future::Future>(areas: Arc<AreaCache>, f: F) -> F::Output {
    AREAS.scope(areas, f).await
}

/// The areas visible to the running tool, if any.
pub fn current_areas() -> Option<Arc<AreaCache>> {
    AREAS.try_with(Arc::clone).ok()
}

fn real<T>(
    bbox: &BBox,
    get: impl FnOnce(&AreaCache, &BBox) -> Result<T, ToolError>,
) -> Result<T, ToolError> {
    AREAS
        .try_with(|areas| get(areas, bbox))
        .unwrap_or(Err(ToolError::AreaUnavailable))
}

/// The bundle of the area `scene` is on: the demo street, or a real area
/// the connected app's server sent.
pub fn bundle_for(scene: &SceneFile) -> Result<Arc<AreaBundle>, ToolError> {
    match &scene.area {
        AreaRef::Demo => DEMO.clone().map_err(ToolError::Demo),
        AreaRef::Osm { bbox } => real(bbox, AreaCache::bundle),
    }
}

/// The coverage layers of the area `scene` is on, built on first use.
/// Slow for an area not used before: call it off the async runtime.
pub fn layers_for(scene: &SceneFile) -> Result<Arc<StaticLayers>, ToolError> {
    match &scene.area {
        AreaRef::Demo => DEMO_LAYERS.clone().map_err(ToolError::Demo),
        AreaRef::Osm { bbox } => real(bbox, AreaCache::layers),
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

    fn osm_scene(bbox: BBox) -> SceneFile {
        SceneFile::new(AreaRef::Osm { bbox }, demo().unwrap().origin)
    }

    #[test]
    fn a_real_area_is_seen_only_inside_its_connections_scope() {
        let bbox = BBox::new(47.0, -122.0, 47.002, -121.997).unwrap();
        let scene = osm_scene(bbox);
        assert!(matches!(
            bundle_for(&scene),
            Err(ToolError::AreaUnavailable)
        ));
        let areas = Arc::new(AreaCache::default());
        areas.insert(bbox, demo().unwrap().clone());
        assert!(matches!(
            bundle_for(&scene),
            Err(ToolError::AreaUnavailable)
        ));
        let held = with_areas(Arc::clone(&areas), || bundle_for(&scene)).unwrap();
        assert_eq!(held.buildings.len(), demo().unwrap().buildings.len());
        areas.clear();
        assert!(matches!(
            with_areas(Arc::clone(&areas), || bundle_for(&scene)),
            Err(ToolError::AreaUnavailable)
        ));
        // A failed fetch is reported for its own area only.
        areas.fail(bbox, "the server is busy".into());
        assert!(matches!(
            with_areas(Arc::clone(&areas), || bundle_for(&scene)),
            Err(ToolError::AreaFetch(m)) if m == "the server is busy"
        ));
        let other = osm_scene(BBox::new(48.0, -122.0, 48.002, -121.997).unwrap());
        assert!(matches!(
            with_areas(areas, || bundle_for(&other)),
            Err(ToolError::AreaUnavailable)
        ));
    }

    #[test]
    fn the_cache_keeps_the_newest_areas() {
        let areas = AreaCache::default();
        let boxes: Vec<BBox> = (0..=MAX_CACHED_AREAS)
            .map(|i| {
                BBox::new(
                    47.0 + i as f64 * 0.01,
                    -122.0,
                    47.002 + i as f64 * 0.01,
                    -121.997,
                )
                .unwrap()
            })
            .collect();
        for b in &boxes {
            areas.insert(*b, demo().unwrap().clone());
        }
        assert!(!areas.contains(&boxes[0]));
        assert!(boxes[1..].iter().all(|b| areas.contains(b)));
    }
}
