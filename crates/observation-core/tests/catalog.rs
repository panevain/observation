use observation_core::camera::Catalog;

#[test]
fn shipped_catalog_is_valid() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../observation-client/assets/catalog"
    );
    let main_rs = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../observation-client/src/main.rs"
    ))
    .unwrap();
    let mut parts = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        assert!(
            main_rs.contains(&format!("assets/catalog/{name}")),
            "{name} is not embedded in observation-client/src/main.rs"
        );
        parts.push(std::fs::read_to_string(&path).unwrap());
    }
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
