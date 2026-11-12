//! The workspace version in the root `Cargo.toml`, read and written line by line as the old
//! shell scripts did with sed, not through a TOML parser: the same lines match, so a manifest
//! the scripts refused is refused here too.
//!
//! The lines in play are a sed range: from a line starting `[workspace.package]` up to and
//! including the next empty line (or the end of the file), and again from the next such line.

/// Whether `version` is what the release workflows accept: `X.Y.Z`, or `X.Y.Z-pre` where `pre`
/// is ASCII letters, digits and dots (the scripts' `^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$`).
pub fn is_version(version: &str) -> bool {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && pre.is_none_or(|p| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
        })
}

/// The lines of `text` with their line ends, each with whether it lies in a
/// `[workspace.package]` range.
fn ranged_lines(text: &str) -> impl Iterator<Item = (&str, &str, bool)> {
    let mut in_range = false;
    text.split_inclusive('\n').map(move |line| {
        let body = line.trim_suffix('\n');
        let end = &line[body.len()..];
        let ranged = if in_range {
            // The closing empty line still belongs to the range.
            in_range = !body.is_empty();
            true
        } else {
            in_range = body.starts_with("[workspace.package]");
            in_range
        };
        (body, end, ranged)
    })
}

const PREFIX: &str = "version = \"";

/// What `workspace-version.sh` read as the version: every ranged line that starts
/// `version = "` and has another `"` after it gives what lies between that and the LAST `"`,
/// followed by whatever comes after it (sed's greedy `s/^version = "\(.*\)"/\1/p`). The lines
/// are joined by newlines and the trailing newlines dropped, as `$(...)` did. Anything but a
/// single line that [`is_version`] is refused by the caller.
pub fn workspace_version(text: &str) -> String {
    let mut out = String::new();
    for (body, _, ranged) in ranged_lines(text) {
        let Some(rest) = body.strip_prefix(PREFIX).filter(|_| ranged) else {
            continue;
        };
        if let Some(last) = rest.rfind('"') {
            out.push_str(&rest[..last]);
            out.push_str(&rest[last + 1..]);
            out.push('\n');
        }
    }
    out.truncate(out.trim_end_matches('\n').len());
    out
}

/// `text` with the version set as `set-version.sh` set it: on every ranged line that starts
/// `version = "`, then at least one character other than `"`, then `"`, that much is replaced
/// by `version = "VERSION"` and the rest of the line kept (sed's
/// `s/^version = "[^"]+"/version = "VERSION"/`).
pub fn set_workspace_version(text: &str, version: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (body, end, ranged) in ranged_lines(text) {
        let replaced = body
            .strip_prefix(PREFIX)
            .filter(|_| ranged)
            .and_then(|rest| rest.find('"').filter(|&q| q > 0).map(|q| &rest[q + 1..]));
        match replaced {
            Some(tail) => {
                out.push_str(PREFIX);
                out.push_str(version);
                out.push('"');
                out.push_str(tail);
            }
            None => out.push_str(body),
        }
        out.push_str(end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_semver_cores_with_an_optional_dotted_suffix() {
        for good in [
            "0.1.0",
            "10.20.30",
            "0.2.0-dev.1",
            "1.0.0-rc1",
            "1.0.0-.",
            "01.0.0",
        ] {
            assert!(is_version(good), "{good}");
        }
        for bad in [
            "",
            "1.0",
            "1.0.0.0",
            "v1.0.0",
            "1.0.0-",
            "1.0.0-a-b",
            "1.0.0+b",
            " 1.0.0",
            "1.0.0\n",
            "1..0",
            "1.0.x",
            "١.0.0",
            "1.0.0-é",
        ] {
            assert!(!is_version(bad), "{bad:?}");
        }
    }

    #[test]
    fn reads_the_version_lines_of_the_range_only() {
        let text = "[package]\nversion = \"9.9.9\"\n\n[workspace.package]\nedition = \"2024\"\n\
                    version = \"0.1.0\"\n\nversion = \"8.8.8\"\n";
        assert_eq!(workspace_version(text), "0.1.0");
        // No closing empty line: the range runs to the end.
        assert_eq!(
            workspace_version("[workspace.package]\nversion = \"1.0.0\"\n[x]\nversion = \"2\""),
            "1.0.0\n2"
        );
        // Greedy, and what follows the last quote is kept.
        assert_eq!(
            workspace_version("[workspace.package]\nversion = \"1\" # \"x\" y\n"),
            "1\" # \"x y"
        );
        assert_eq!(workspace_version("[workspace.package]\nversion = \"\n"), "");
        // Trailing empty values vanish, as in `$(...)`; leading ones do not.
        assert_eq!(
            workspace_version("[workspace.package]\nversion = \"1.0.0\"\nversion = \"\"\n"),
            "1.0.0"
        );
        assert_eq!(
            workspace_version("[workspace.package]\nversion = \"\"\nversion = \"1.0.0\"\n"),
            "\n1.0.0"
        );
    }

    #[test]
    fn sets_the_version_lines_of_the_range_only() {
        let text = "[workspace.package]\r\nversion = \"0.1.0\" # c\r\nversion = \"\"\n\n\
                    version = \"0.1.0\"\n[workspace.package] x\nversion = \"\"\"\nversion = \"7\"";
        assert_eq!(
            set_workspace_version(text, "0.2.0"),
            "[workspace.package]\r\nversion = \"0.2.0\" # c\r\nversion = \"\"\n\n\
             version = \"0.1.0\"\n[workspace.package] x\nversion = \"\"\"\nversion = \"0.2.0\""
        );
    }
}
