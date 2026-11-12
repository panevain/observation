//! The curated camera catalogue and custom model ids.

use super::{CameraKind, CameraModel};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog parse error: {0}")]
    Parse(String),
    #[error("model `{id}`: {why}")]
    Invalid { id: String, why: String },
    #[error("duplicate model id `{0}`")]
    Duplicate(String),
}

/// Prefix of every custom model id. Catalogue ids may not use it, so a custom
/// model can never shadow a catalogue one (custom models win the lookup).
pub const CUSTOM_ID_PREFIX: &str = "custom-";

/// An id `custom-<slug>-<n>` for a custom model named `name`, with the
/// smallest `n` from `first` up for which `taken` is false.
pub fn fresh_custom_id(name: &str, first: usize, taken: impl Fn(&str) -> bool) -> String {
    let slug: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    (first..)
        .map(|n| format!("{CUSTOM_ID_PREFIX}{slug}-{n}"))
        .find(|id| !taken(id))
        .unwrap_or_default()
}

/// The curated model list shipped with the app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Catalog {
    pub version: u32,
    pub models: Vec<CameraModel>,
}

impl Catalog {
    /// Parses and validates a RON catalog.
    pub fn from_ron(s: &str) -> Result<Self, CatalogError> {
        let cat: Self = ron::from_str(s).map_err(|e| CatalogError::Parse(e.to_string()))?;
        check_catalog_models(&cat.models)?;
        Ok(cat)
    }

    /// Parses per-vendor RON files (each a bare list of models) into one validated
    /// catalog, ordered by kind, then vendor, then name.
    pub fn from_ron_parts(parts: &[&str]) -> Result<Self, CatalogError> {
        let mut models = Vec::new();
        for part in parts {
            let list: Vec<CameraModel> =
                ron::from_str(part).map_err(|e| CatalogError::Parse(e.to_string()))?;
            models.extend(list);
        }
        let kind_rank = |k: CameraKind| CameraKind::ALL.iter().position(|a| *a == k);
        models.sort_by(|a, b| {
            (kind_rank(a.kind), &a.vendor, &a.name).cmp(&(kind_rank(b.kind), &b.vendor, &b.name))
        });
        check_catalog_models(&models)?;
        Ok(Self { version: 1, models })
    }

    pub fn get(&self, id: &str) -> Option<&CameraModel> {
        self.models.iter().find(|m| m.id == id)
    }

    /// The catalogue's version for a scene's provenance: 16 hex digits
    /// hashed from every model's values, so it changes whenever any model
    /// does (and with nothing to bump by hand), but not with comments or
    /// file layout.
    pub fn fingerprint(&self) -> String {
        let mut h = crate::util::hash::Fnv64::default();
        h.u64(self.models.len() as u64);
        for m in &self.models {
            // Plain data: serialising cannot fail.
            h.bytes(&serde_json::to_vec(m).unwrap_or_default());
        }
        crate::util::hash::fingerprint_hex(h.finish())
    }
}

/// Validates catalogue models: each is sane, no two share an id, and none
/// uses the [`CUSTOM_ID_PREFIX`] reserved for custom models.
fn check_catalog_models(models: &[CameraModel]) -> Result<(), CatalogError> {
    let mut seen = std::collections::HashSet::new();
    for m in models {
        m.validate()?;
        if m.id.starts_with(CUSTOM_ID_PREFIX) {
            return Err(CatalogError::Invalid {
                id: m.id.clone(),
                why: format!("ids starting with `{CUSTOM_ID_PREFIX}` are kept for custom models"),
            });
        }
        if !seen.insert(m.id.as_str()) {
            return Err(CatalogError::Duplicate(m.id.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::model;
    use super::*;

    #[test]
    fn catalog_parse_and_validate() {
        let ron = r#"(version: 1, models: [(id: "a-b", vendor: "V", name: "N", kind: Doorbell,
            hfov_deg: 150.0, vfov_deg: 150.0, resolution: (1536, 1536), ir_range_m: Some(5.0),
            source: Some("https://example.com"))])"#;
        let c = Catalog::from_ron(ron).unwrap();
        assert_eq!(c.get("a-b").unwrap().kind, CameraKind::Doorbell);
        // The fingerprint follows the models' values, not the file's layout.
        let fp = c.fingerprint();
        assert_eq!(fp.len(), 16);
        let relaid =
            Catalog::from_ron(&format!("// a comment\n{}", ron.replace(", ", ",\n  "))).unwrap();
        assert_eq!(relaid.fingerprint(), fp);
        let wider = Catalog::from_ron(&ron.replace("hfov_deg: 150.0", "hfov_deg: 151.0")).unwrap();
        assert_ne!(wider.fingerprint(), fp);
        assert_ne!(Catalog::default().fingerprint(), fp);
        let dup = ron.replace("models: [(", "models: [(id: \"a-b\", vendor: \"V\", name: \"N\", kind: Doorbell, hfov_deg: 150.0, vfov_deg: 150.0, resolution: (1536, 1536)), (");
        assert!(matches!(
            Catalog::from_ron(&dup),
            Err(CatalogError::Duplicate(_))
        ));
        // The prefix is kept for custom models, so they never shadow one.
        let reserved = |r: Result<Catalog, CatalogError>, want: &str| match r {
            Err(CatalogError::Invalid { id, why }) => {
                assert_eq!(id, want);
                assert!(why.contains(CUSTOM_ID_PREFIX), "{why}");
            }
            other => panic!("expected the reserved prefix to be refused, got {other:?}"),
        };
        reserved(
            Catalog::from_ron(&ron.replace("\"a-b\"", "\"custom-a-b\"")),
            "custom-a-b",
        );
        reserved(
            Catalog::from_ron_parts(&[r#"[(id: "custom-x", vendor: "", name: "N",
                kind: Bullet, hfov_deg: 90.0, vfov_deg: 50.0, resolution: (1920, 1080))]"#]),
            "custom-x",
        );
    }

    #[test]
    fn fresh_custom_ids() {
        assert_eq!(
            fresh_custom_id("Front Cam!", 1, |_| false),
            "custom-front-cam-1"
        );
        let taken = |id: &str| id == "custom-x-2" || id == "custom-x-3";
        assert_eq!(fresh_custom_id("X", 2, taken), "custom-x-4");
        let id = fresh_custom_id("Größe", 1, |_| false);
        assert!(model_with_id(&id).validate().is_ok(), "{id}");
    }

    fn model_with_id(id: &str) -> CameraModel {
        CameraModel {
            id: id.into(),
            ..model()
        }
    }
}
