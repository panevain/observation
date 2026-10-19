//! Catalog file discovery, shared by `build.rs` (embedding) and the core catalog test
//! (validation) via `#[path]`, so both always see the same set of files.

use std::path::{Path, PathBuf};

/// Every `.ron` file under `root` (recursively, extension matched case-insensitively),
/// sorted, plus every directory visited. Symlinks are rejected rather than followed.
pub fn discover(root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    walk(root, &mut files, &mut dirs);
    files.sort();
    (files, dirs)
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>) {
    dirs.push(dir.to_path_buf());
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("read catalog dir entry");
        let path = entry.path();
        let kind = entry.file_type().expect("catalog entry file type");
        assert!(
            !kind.is_symlink(),
            "symlink in catalog directory: {}",
            path.display()
        );
        if kind.is_dir() {
            walk(&path, files, dirs);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ron"))
        {
            files.push(path);
        }
    }
}
