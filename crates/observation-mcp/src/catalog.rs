//! The camera catalogue the app ships, embedded at build time (`build.rs`).

use crate::ToolError;
use observation_core::camera::{CUSTOM_ID_PREFIX, CameraModel, Catalog, fresh_custom_id};
use observation_core::scene::SceneFile;
use std::sync::LazyLock;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/catalog_files.rs"));
}

static CATALOG: LazyLock<Result<Catalog, String>> =
    LazyLock::new(|| Catalog::from_ron_parts(embedded::CATALOG_FILES).map_err(|e| e.to_string()));

/// The curated catalogue, parsed and checked once.
pub fn catalog() -> Result<&'static Catalog, ToolError> {
    CATALOG.as_ref().map_err(|e| ToolError::Catalog(e.clone()))
}

/// The model `id` names: one of the scene's custom models, or else a
/// catalogue model (custom ids cannot collide with catalogue ones).
pub fn model<'a>(
    catalog: &'a Catalog,
    scene: Option<&'a SceneFile>,
    id: &str,
) -> Option<&'a CameraModel> {
    scene
        .and_then(|s| s.custom_models.iter().find(|m| m.id == id))
        .or_else(|| catalog.get(id))
}

/// A fresh custom model id for a model called `name`, unused by the scene.
pub fn new_custom_id(scene: &SceneFile, name: &str) -> String {
    fresh_custom_id(name, 1, |id| scene.custom_models.iter().any(|m| m.id == id))
}

/// Whether `id` names a custom model rather than a catalogue one.
pub fn is_custom(id: &str) -> bool {
    id.starts_with(CUSTOM_ID_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_catalogue_loads() {
        let cat = catalog().unwrap();
        assert!(cat.models.len() > 10, "{} models", cat.models.len());
        assert!(cat.models.iter().all(|m| !is_custom(&m.id)));
    }
}
