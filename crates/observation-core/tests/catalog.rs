use observation_core::camera::Catalog;

#[test]
fn shipped_catalog_is_valid() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../observation-client/assets/catalog"
    );
    let mut parts = Vec::new();
    collect(std::path::Path::new(dir), &mut parts);
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

fn collect(dir: &std::path::Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "ron") {
            out.push(std::fs::read_to_string(&path).unwrap());
        }
    }
}
