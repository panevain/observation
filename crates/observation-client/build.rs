//! Embeds every `.ron` file under `assets/catalog/` (recursively) as `CATALOG_FILES`.

use std::fmt::Write as _;
use std::path::Path;

#[path = "catalog_walk.rs"]
mod catalog_walk;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let root = Path::new(&manifest).join("assets/catalog");
    let (files, dirs) = catalog_walk::discover(&root);
    assert!(!files.is_empty(), "no .ron files under {}", root.display());
    println!("cargo:rerun-if-changed=catalog_walk.rs");
    for d in &dirs {
        println!("cargo:rerun-if-changed={}", d.display());
    }

    let mut src = String::from("pub const CATALOG_FILES: &[&str] = &[\n");
    for f in &files {
        let path = f.to_str().expect("catalog path is valid UTF-8");
        writeln!(src, "    include_str!({path:?}),").expect("write to String");
    }
    src.push_str("];\n");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set");
    std::fs::write(Path::new(&out_dir).join("catalog_files.rs"), src)
        .expect("write catalog_files.rs");
}
