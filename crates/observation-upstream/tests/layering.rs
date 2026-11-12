//! `GuardedHttp::get_stream` reads a body with no cap and no deadline, so
//! only the region agent's extract download, which bounds both itself, may
//! call it.
#![allow(clippy::unwrap_used)] // integration-test helpers live outside `#[test]` fns

use std::path::Path;

/// The `.rs` files below `dir` that mention `needle`.
fn mentions(dir: &Path, needle: &str, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            mentions(&path, needle, found);
        } else if path.extension().is_some_and(|e| e == "rs")
            && std::fs::read_to_string(&path).unwrap().contains(needle)
        {
            found.push(path.display().to_string());
        }
    }
}

#[test]
fn only_the_region_agent_streams_uncapped_bodies() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap() {
        let dir = entry.unwrap().path();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        if dir.is_dir() && !["observation-upstream", "observation-regions"].contains(&&*name) {
            mentions(&dir, "get_stream", &mut found);
        }
    }
    assert!(found.is_empty(), "get_stream used in {found:?}");
    let mut agent = Vec::new();
    mentions(
        &crates.join("observation-regions"),
        "get_stream",
        &mut agent,
    );
    assert!(!agent.is_empty(), "the check looks in the wrong place");
}
