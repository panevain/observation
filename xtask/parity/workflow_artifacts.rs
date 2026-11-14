//! The artifacts that a bot job commits (hotfix.yml's `hotfix`, pins.yml's `pins`) are bound by
//! a digest: the producing job, which builds nothing, outputs the sha256 of its files, and the
//! bot job's "Check the artifact" step compares it before anything reads them and before the
//! token. Both workflows' steps are cut out of the files and run with bash: a genuine artifact
//! passes; a replaced, swapped, missing or symlinked file and a malformed digest fail.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

fn workflow(name: &str) -> String {
    ok(fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../.github/workflows")
            .join(name),
    ))
}

/// The step named `name`: its lines, and the name of the step after it.
fn step<'a>(wf: &'a str, name: &str) -> (Vec<&'a str>, &'a str) {
    let lines: Vec<&str> = wf.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == format!("- name: {name}"))
        .unwrap_or_else(|| panic!("no step {name}"));
    let indent = lines[start].len() - lines[start].trim_start().len();
    let end = (start + 1..lines.len())
        .find(|&i| {
            let l = lines[i];
            let at = l.len() - l.trim_start().len();
            !l.trim().is_empty() && !l.trim_start().starts_with('#') && at <= indent
        })
        .unwrap_or(lines.len());
    let next = lines[end].trim().strip_prefix("- name: ").unwrap_or("");
    (lines[start..end].to_vec(), next)
}

/// The lines of the step right before the step named `name` (comments left out).
fn previous_step<'a>(wf: &'a str, name: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = wf.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == format!("- name: {name}"))
        .unwrap_or_else(|| panic!("no step {name}"));
    let indent = lines[start].len() - lines[start].trim_start().len();
    let at = |l: &str| l.len() - l.trim_start().len();
    let begin = (0..start)
        .rev()
        .find(|&i| at(lines[i]) == indent && lines[i].trim_start().starts_with("- "))
        .unwrap_or_else(|| panic!("no step before {name}"));
    lines[begin..start]
        .iter()
        .copied()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect()
}

/// A step's `run: |` block, unindented, and the value of `key` in its `env:`.
fn run_and_env(step: &[&str], key: &str) -> (String, String) {
    let at = step
        .iter()
        .position(|l| l.trim() == "run: |")
        .unwrap_or_else(|| panic!("no run block"));
    let indent = step[at + 1].len() - step[at + 1].trim_start().len();
    let run = step[at + 1..]
        .iter()
        .map(|l| l.get(indent..).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let prefix = format!("{key}: ");
    let value = step
        .iter()
        .find_map(|l| l.trim().strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("no {key}"));
    (run, value.to_owned())
}

fn bash(script: &str, dir: &Path, env: &[(&str, &str)]) -> (i32, String) {
    let out = ok(Command::new("bash")
        .args(["--noprofile", "--norc", "-eo", "pipefail", "-c", script])
        .current_dir(dir)
        .envs(env.iter().copied())
        .output());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.code().unwrap_or(-1), text)
}

/// Writes the artifact's files (each file's text from `texts`) into a fresh `dir`.
fn artifact(dir: &Path, files: &[&str], texts: &[&str]) {
    if dir.exists() {
        ok(fs::remove_dir_all(dir));
    }
    ok(fs::create_dir_all(dir));
    for (f, t) in files.iter().zip(texts) {
        ok(fs::write(dir.join(f), t));
    }
}

#[test]
fn bot_jobs_commit_only_the_artifact_their_producer_wrote() {
    let mut check_texts = Vec::new();
    for (name, producer) in [("hotfix.yml", "prepare"), ("pins.yml", "bump")] {
        let wf = workflow(name);
        let (check, next) = step(&wf, "Check the artifact");
        assert_eq!(
            next, "Bot token",
            "{name}: the check is not right before the token"
        );
        // Nothing runs between the download and the check: the step before it is the
        // download of this artifact, into the directory the check reads.
        let artifact_name = name.trim_end_matches(".yml");
        let download: Vec<&str> = previous_step(&wf, "Check the artifact")
            .iter()
            .map(|l| l.trim())
            .collect();
        assert!(
            download[0].starts_with("- uses: actions/download-artifact@"),
            "{name}: the step before the check is not the download: {download:?}"
        );
        let path = format!("${{{{ runner.temp }}}}/{artifact_name}");
        assert_eq!(
            download[1..],
            [
                "with:",
                &format!("name: {artifact_name}"),
                &format!("path: {path}")
            ],
            "{name}"
        );
        assert_eq!(run_and_env(&check, "DIR").1, path, "{name}");
        let (run, files) = run_and_env(&check, "FILES");
        let sha_binding = format!("SHA256: ${{{{ needs.{producer}.outputs.sha256 }}}}");
        assert!(
            check.iter().any(|l| l.trim() == sha_binding),
            "{name}: {sha_binding}"
        );
        check_texts.push(run.clone());
        let files: Vec<&str> = files.split(' ').collect();
        let produce = wf
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("echo \"sha256=$(cd \"${out}\""))
            .unwrap_or_else(|| panic!("{name}: no producer line"));
        let listed = format!("sha256sum -- {} |", files.join(" "));
        assert!(
            produce.contains(&listed),
            "{name}: {produce} hashes other files"
        );

        let tmp = ok(tempfile::tempdir());
        let dir = tmp.path().join("artifact");
        let texts = ["diff --git a/Cargo.toml b/Cargo.toml\n", "Body text.\n"];
        artifact(&dir, &files, &texts);
        let output = tmp.path().join("output");
        ok(fs::write(&output, ""));
        let out = output.to_string_lossy().into_owned();
        let d = dir.to_string_lossy().into_owned();
        assert_eq!(
            bash(produce, tmp.path(), &[("out", &d), ("GITHUB_OUTPUT", &out)]).0,
            0
        );
        let written = ok(fs::read_to_string(&output));
        let sha = written
            .trim()
            .strip_prefix("sha256=")
            .unwrap_or_else(|| panic!("{name}: wrote {written:?}"))
            .to_owned();
        let joined = files.join(" ");
        let check = |sha: &str| {
            let env = [("DIR", d.as_str()), ("FILES", &joined), ("SHA256", sha)];
            bash(&run, tmp.path(), &env)
        };

        assert_eq!(check(&sha), (0, String::new()), "{name}: genuine artifact");
        let refused = |what: &str, got: (i32, String), message: &str| {
            assert_eq!(got.0, 1, "{name}: {what}: {got:?}");
            assert!(got.1.starts_with("::error::"), "{name}: {what}: {got:?}");
            assert!(got.1.contains(message), "{name}: {what}: {got:?}");
        };
        let replaced = format!("the artifact is not the one {producer} wrote");
        for (what, texts) in [
            (
                "patch replaced",
                ["diff --git a/README.md b/README.md\n", texts[1]],
            ),
            ("body replaced", [texts[0], "Body text.\nmore\n"]),
            ("files swapped", [texts[1], texts[0]]),
        ] {
            artifact(&dir, &files, &texts);
            refused(what, check(&sha), &replaced);
        }
        artifact(&dir, &files[..1], &texts);
        refused("a file missing", check(&sha), "is not a regular file");
        artifact(&dir, &files, &texts);
        ok(fs::rename(dir.join(files[1]), tmp.path().join("elsewhere")));
        ok(std::os::unix::fs::symlink(
            tmp.path().join("elsewhere"),
            dir.join(files[1]),
        ));
        refused("a symlink", check(&sha), "is not a regular file");
        artifact(&dir, &files, &texts);
        for bad in ["", "abc", &sha.to_uppercase(), &format!("{sha}\n")] {
            refused("a malformed digest", check(bad), "bad digest");
        }
    }
    assert_eq!(check_texts[0].replace("prepare", "bump"), check_texts[1]);
}
