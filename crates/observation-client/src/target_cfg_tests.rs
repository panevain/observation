//! Code that differs by target lives in `src/platform/`, behind the ports,
//! so that the rest of the client builds and is tested the same way on
//! every target. This test reads every Rust file of the client crate (its
//! sources, `build.rs` and what it includes, `tests/`) and of the model
//! crate, and fails on a target condition outside `src/platform/`.

use std::path::{Path, PathBuf};

/// Rust's target cfg keys (`target_has_` covers `target_has_atomic` and the
/// like).
const TARGET_KEYS: [&str; 10] = [
    "target_arch",
    "target_family",
    "target_os",
    "target_env",
    "target_vendor",
    "target_pointer_width",
    "target_endian",
    "target_feature",
    "target_abi",
    "target_has_",
];

/// Whether `line` names a target condition: a target cfg key anywhere, a
/// target family (`unix`, `windows`, `wasm…`) on a line that uses `cfg`, or
/// what lets a build script set a cfg from the target (`CARGO_CFG_*`,
/// `rustc-cfg`). A family name on its own line inside a multi-line
/// `cfg(any(…))` is not seen.
fn names_target(line: &str) -> bool {
    let words = || {
        line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
    };
    words().any(|w| TARGET_KEYS.iter().any(|k| w.starts_with(k)) || w.starts_with("CARGO_CFG_"))
        || line.contains("rustc-cfg")
        || line.contains("rustc-check-cfg")
        || (line.contains("cfg")
            && words().any(|w| w == "unix" || w == "windows" || w.starts_with("wasm")))
}

/// Files outside `observation-client/src/platform/` that may name a target
/// condition, and why.
const ALLOWED: [(&str, &str); 2] = [
    (
        "observation-client/src/bin/coverage_worker.rs",
        "the coverage worker is its own binary, kept small by not linking the \
         library (and so not `platform`): it is a no-op `main` off the web",
    ),
    (
        "observation-client/src/target_cfg_tests.rs",
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
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory");
    let mut files = Vec::new();
    for krate in ["observation-client", "observation-client-model"] {
        sources(&crates.join(krate), &mut files);
    }
    files.sort();
    for expected in [
        "observation-client/src/platform/mod.rs",
        "observation-client/build.rs",
        "observation-client/catalog_walk.rs",
        "observation-client-model/src/lib.rs",
    ] {
        assert!(
            files.iter().any(|f| f.ends_with(expected)),
            "the walk found {expected}: {files:?}"
        );
    }

    let mut found = Vec::new();
    let mut allowed_used = Vec::new();
    for file in &files {
        let relative = file.strip_prefix(crates).expect("under crates/");
        let name = relative.to_string_lossy().replace('\\', "/");
        if name.starts_with("observation-client/src/platform/") {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("readable source");
        let hits: Vec<_> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| names_target(line))
            .map(|(i, line)| format!("{name}:{}: {}", i + 1, line.trim()))
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
        "target-specific code outside observation-client/src/platform/:\n  {}\n\
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

#[test]
fn every_kind_of_target_condition_is_seen() {
    for line in [
        "#[cfg(target_arch = \"wasm32\")]",
        "if cfg!(target_family = \"wasm\") {",
        "#[cfg(not(unix))]",
        "#[cfg(any(windows, unix))]",
        "cfg!(windows) || cfg!(unix)",
        "#[cfg(target_feature = \"atomics\")]",
        "#[cfg(target_has_atomic = \"64\")]",
        "#[cfg_attr(target_abi = \"eabihf\", allow(dead_code))]",
        "#[cfg(wasm)]",
        "let arch = std::env::var(\"CARGO_CFG_TARGET_ARCH\");",
        "println!(\"cargo::rustc-cfg=web\");",
    ] {
        assert!(names_target(line), "{line}");
    }
    for line in [
        "#[cfg(test)]",
        "#[cfg(any(test, feature = \"testing\"))]",
        "// Runs in the web worker.",
        "let target = camera.position;",
        "mod target_cfg_tests;",
    ] {
        assert!(!names_target(line), "{line}");
    }
}
