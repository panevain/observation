//! `cargo xtask release unexpected-files KIND` through the built binary, so that each KIND on
//! the command line reads its own list of `.config/release.toml`: CHANGELOG.md and Dockerfile
//! are on the hotfix list and not on the pre-release list.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn unexpected(kind: &str, input: &str) -> String {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("--repo")
        .arg(&repo)
        .args(["release", "unexpected-files", kind])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{e}"));
    child
        .stdin
        .take()
        .unwrap_or_else(|| panic!("no stdin"))
        .write_all(input.as_bytes())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = child.wait_with_output().unwrap_or_else(|e| panic!("{e}"));
    assert!(out.status.success(), "{kind}: {out:?}");
    String::from_utf8(out.stdout).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn each_kind_reads_its_own_list() {
    let input = "Cargo.toml\nCargo.lock\nfuzz/Cargo.lock\nCHANGELOG.md\nDockerfile\nREADME.md\n";
    assert_eq!(
        unexpected("prerelease", input),
        "CHANGELOG.md\nDockerfile\nREADME.md\n"
    );
    assert_eq!(unexpected("hotfix", input), "README.md\n");
}
