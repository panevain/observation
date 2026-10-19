//! Embeds every `.ron` file under `assets/catalog/` (recursively) as `CATALOG_FILES`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "ron") {
            out.push(path);
        }
    }
}

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/catalog");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "no .ron files under {}", root.display());

    let mut src = String::from("pub const CATALOG_FILES: &[&str] = &[\n");
    for f in &files {
        writeln!(src, "    include_str!({:?}),", f.display().to_string()).unwrap();
    }
    src.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("catalog_files.rs");
    std::fs::write(out, src).expect("write catalog_files.rs");
}
