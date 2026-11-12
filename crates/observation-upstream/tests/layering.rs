//! `GuardedHttp::get_stream` reads a body with no cap and no deadline, so
//! only the region agent's extract download, which bounds both itself, may
//! call it, and only through an adapter private to the agent's crate.
//!
//! This is a text scan. It catches an accidental use, not a determined
//! author: a name built by a macro, for one, is out of its reach.
#![allow(clippy::unwrap_used)] // integration-test helpers live outside `#[test]` fns

use std::path::{Path, PathBuf};

/// The `.rs` files below `dir` (build output left out) that mention `needle`.
fn mentions(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n != "target" && n != ".git")
            {
                mentions(&path, needle, found);
            }
        } else if path.extension().is_some_and(|e| e == "rs")
            && std::fs::read_to_string(&path).unwrap().contains(needle)
        {
            found.push(path);
        }
    }
}

#[test]
fn only_the_region_agent_streams_uncapped_bodies() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let root = crates.parent().unwrap();
    let upstream = crates.join("observation-upstream");
    let agent = crates.join("observation-regions");

    // Every crate of the workspace, the two other workspaces (xtask, fuzz)
    // and the top-level tests.
    let mut found = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap() {
        let dir = entry.unwrap().path();
        if dir.is_dir() && dir != upstream && dir != agent {
            mentions(&dir, "get_stream", &mut found);
        }
    }
    for dir in ["xtask", "fuzz", "tests"] {
        mentions(&root.join(dir), "get_stream", &mut found);
    }
    assert!(found.is_empty(), "get_stream used in {found:?}");

    // In the agent, only its download adapter calls it, and that adapter
    // cannot be named from another crate.
    let mut in_agent = Vec::new();
    mentions(&agent, "get_stream", &mut in_agent);
    let ports = agent.join("src/ports.rs");
    assert_eq!(
        in_agent,
        std::slice::from_ref(&ports),
        "get_stream used in {in_agent:?}"
    );
    let ports = std::fs::read_to_string(&ports).unwrap();
    assert!(ports.contains("pub(crate) struct ExtractDownload(pub(crate) GuardedHttp);"));
    assert!(ports.contains("impl Download for ExtractDownload {"));
    assert!(
        !ports.contains("impl Download for GuardedHttp"),
        "the port is implemented on the public client"
    );
}
