//! `Catalog::from_ron_parts` and `Catalog::from_ron` on arbitrary text: the
//! catalogue is shipped with the app, but its RON files are hand-edited, and
//! a bad edit must give an error rather than a panic. An accepted catalogue
//! holds only valid models with unique ids outside the custom-model prefix,
//! ordered by kind, vendor and name, and its fingerprint does not change when
//! an empty part is added.
#![no_main]

use libfuzzer_sys::fuzz_target;
use observation_core::camera::{CUSTOM_ID_PREFIX, CameraKind, CameraModel, Catalog};

fn check_models(models: &[CameraModel]) {
    let mut ids = std::collections::HashSet::new();
    for m in models {
        assert!(m.validate().is_ok(), "{}", m.id);
        assert!(!m.id.starts_with(CUSTOM_ID_PREFIX), "{}", m.id);
        assert!(ids.insert(m.id.as_str()), "duplicate {}", m.id);
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(catalog) = Catalog::from_ron_parts(&[text]) {
        check_models(&catalog.models);
        let rank = |k: CameraKind| CameraKind::ALL.iter().position(|a| *a == k);
        assert!(catalog.models.is_sorted_by_key(|m| (
            rank(m.kind),
            m.vendor.clone(),
            m.name.clone()
        )));
        let fingerprint = catalog.fingerprint();
        assert_eq!(fingerprint.len(), 16);
        assert!(fingerprint.bytes().all(|b| b.is_ascii_hexdigit()));
        let merged = Catalog::from_ron_parts(&[text, "[]"]).expect("an empty part changes nothing");
        assert_eq!(merged.fingerprint(), fingerprint);
    }
    if let Ok(catalog) = Catalog::from_ron(text) {
        check_models(&catalog.models);
    }
});
