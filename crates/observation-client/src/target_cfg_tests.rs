//! Code that differs by target lives in `src/platform/`, behind the ports,
//! so that the rest of the client builds and is tested the same way on
//! every target. This test reads the crate's sources and fails on a target
//! condition anywhere else.

use std::path::{Path, PathBuf};

/// What gives target-specific code away: the target `cfg` keys and the
/// shorthand families.
const TARGET_CONDITIONS: [&str; 10] = [
    "target_arch",
    "target_family",
    "target_os",
    "target_env",
    "target_vendor",
    "target_pointer_width",
    "target_endian",
    "cfg(wasm",
    "cfg(unix",
    "cfg(windows",
];

/// Files outside `src/platform/` that may name a target condition, and why.
const ALLOWED: [(&str, &str); 2] = [
    (
        "bin/coverage_worker.rs",
        "the coverage worker is its own binary, kept small by not linking the \
         library (and so not `platform`): it is a no-op `main` off the web",
    ),
    (
        "target_cfg_tests.rs",
        "this test, which names the conditions it looks for",
    ),
];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_the_platform_module_tests_the_target() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    files.sort();
    assert!(
        files.iter().any(|f| f.ends_with("platform/mod.rs")),
        "the walk found the platform module: {files:?}"
    );

    let mut found = Vec::new();
    let mut allowed_used = Vec::new();
    for file in &files {
        let relative = file.strip_prefix(&src).expect("under src");
        let name = relative.to_string_lossy().replace('\\', "/");
        if name.starts_with("platform/") {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("readable source");
        let hits: Vec<_> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| TARGET_CONDITIONS.iter().any(|c| line.contains(c)))
            .map(|(i, line)| format!("src/{name}:{}: {}", i + 1, line.trim()))
            .collect();
        if hits.is_empty() {
            continue;
        }
        if ALLOWED.iter().any(|(allowed, _)| *allowed == name) {
            allowed_used.push(name);
        } else {
            found.extend(hits);
        }
    }

    assert!(
        found.is_empty(),
        "target-specific code outside src/platform/:\n  {}\n\
         Put it behind the platform module instead: a function or port with \
         an implementation in platform/web.rs and one in platform/native.rs, \
         chosen in platform/mod.rs. If it truly cannot live there, add the file \
         to ALLOWED in this test with the reason.",
        found.join("\n  ")
    );
    let unused: Vec<_> = ALLOWED
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !allowed_used.iter().any(|used| used == name))
        .collect();
    assert!(
        unused.is_empty(),
        "allowed but no longer naming a target condition (or moved): {unused:?}; \
         remove them from ALLOWED"
    );
}
