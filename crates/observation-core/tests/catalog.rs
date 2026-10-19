use observation_core::camera::Catalog;

// The same discovery code build.rs uses to embed the files in the client.
#[path = "../../observation-client/catalog_walk.rs"]
mod catalog_walk;

#[test]
fn shipped_catalog_is_valid() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../observation-client/assets/catalog"
    );
    let (files, _) = catalog_walk::discover(std::path::Path::new(dir));
    assert!(!files.is_empty(), "no catalog files found in {dir}");
    let parts: Vec<String> = files
        .iter()
        .map(|f| std::fs::read_to_string(f).unwrap())
        .collect();
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let cat = Catalog::from_ron_parts(&parts).expect("catalog parses and validates");
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
