use observation_core::camera::Catalog;

#[test]
fn shipped_catalog_is_valid() {
    let src = include_str!("../../observation-client/assets/catalog.ron");
    let cat = Catalog::from_ron(src).expect("catalog parses and validates");
    assert!(cat.models.len() >= 12);
    for m in &cat.models {
        if !m.vendor.is_empty() {
            assert!(
                m.source
                    .as_deref()
                    .is_some_and(|s| s.starts_with("https://")),
                "{} needs a source",
                m.id
            );
        }
    }
}
