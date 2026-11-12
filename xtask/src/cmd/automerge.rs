//! `cargo xtask automerge list`: the open Dependabot pull requests that may merge themselves,
//! and those on hold (the Pins pull request too, whose auto-merge pins.yml sets). Ported from
//! scripts/dependabot-automerge.py (since removed), whose output, summary, messages and exit
//! status it keeps; dependabot-automerge.yml enables or disables auto-merge on the result as the
//! bot app.
//!
//! - merge: each open, non-draft Dependabot pull request into develop, without auto-merge yet
//!   and not labelled `hold`, whose commits are all Dependabot's and whose every updated
//!   dependency (from the commits' `updated-dependencies` metadata) is a semver patch update, a
//!   Docker base image digest refresh, or the fuzz targets' dated nightly (the last two carry
//!   no update type).
//! - hold: each of them, or the Pins pull request, labelled `hold` with auto-merge on, to switch
//!   it off.
//!
//! It reads pull request metadata through `gh` only, and checks out nothing. Pull request data
//! is read on Python's terms where the script read it: where the script raised on a shape (a
//! missing key, a value it could not iterate or compare), this fails too.

use serde_json::{Map, Value};

use crate::core::py;
use crate::ports::GitHub;
use crate::{Error, Result};

/// The author of every commit of a pull request that may merge itself.
pub const BOT: &str = "dependabot[bot]";
/// The label that keeps a pull request from merging itself (open-pr's `squash-unless-hold`
/// reads the same label for the Pins pull request).
pub const HOLD: &str = "hold";
/// The Pins pull request's branch (pins.yml).
pub const PINS: &str = "chore/pin-bumps";
/// The branch Dependabot's pull requests target (dependabot.yml sets no `target-branch`).
pub const BASE: &str = "develop";
/// The update type that may merge itself.
pub const PATCH: &str = "version-update:semver-patch";
/// Branch prefixes of Dependabot updates that carry no update type and may still merge: the
/// base image digests (dependabot.yml's `docker` entry, which lets only digests through) and
/// the fuzz targets' nightly (its `rust-toolchain` entry for /fuzz, which builds only the fuzz
/// runs).
pub const UNTYPED: [&str; 2] = ["dependabot/docker/", "dependabot/rust_toolchain/fuzz/"];
/// The fields read from each pull request.
pub const FIELDS: &str =
    "number,title,headRefName,headRefOid,baseRefName,isDraft,autoMergeRequest,labels,commits";

/// The command's inputs besides the port.
#[derive(Debug, Clone)]
pub struct Args {
    pub repo: String,
    pub dry_run: bool,
}

/// What the command prints: the JSON line on stdout and the Markdown summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// `{"merge":[...],"hold":[...]}` (compact, as Python's `json.dumps` wrote it) and a newline.
    pub stdout: String,
    pub summary: String,
}

fn shape(what: &str) -> Error {
    Error::new(format!("gh: {what}"))
}

fn key<'a>(pr: &'a Map<String, Value>, k: &str) -> Result<&'a Value> {
    pr.get(k)
        .ok_or_else(|| shape(&format!("a pull request has no `{k}`")))
}

/// The items of `for x in value` where the script then subscripted each `x`: an array's items;
/// an object's keys or a string's characters (as strings, which cannot be subscripted by a
/// key, so they fail where they are used); anything else could not be iterated.
fn iterate(value: &Value, what: &str) -> Result<Vec<Value>> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.into())).collect()),
        _ => Err(shape(&format!("{what} cannot be iterated"))),
    }
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| shape(&format!("{what} is not an object")))
}

/// `[(name, update type or None)]` from a Dependabot commit message's metadata block: the
/// first `---\nupdated-dependencies:\n` at a line start, up to the first line that is `...`.
pub fn dependencies(body: &str) -> Vec<(String, Option<String>)> {
    const HEAD: &str = "---\nupdated-dependencies:\n";
    let line_end = |start: usize| -> Option<usize> {
        let mut q = start;
        loop {
            let rest = &body[q..];
            if rest.starts_with("...") && matches!(rest.as_bytes().get(3), None | Some(b'\n')) {
                return Some(q);
            }
            q += rest.find('\n')? + 1;
        }
    };
    let mut from = 0;
    let block = loop {
        let Some(i) = body[from..].find(HEAD) else {
            return Vec::new();
        };
        let p = from + i;
        if p == 0 || body.as_bytes()[p - 1] == b'\n' {
            let start = p + HEAD.len();
            if let Some(end) = line_end(start) {
                break &body[start..end];
            }
        }
        from = p + 1;
    };
    let mut deps: Vec<(String, Option<String>)> = Vec::new();
    for line in py::splitlines(block) {
        if let Some(name) = line
            .strip_prefix("- dependency-name: ")
            .filter(|n| !n.is_empty())
        {
            deps.push((name.to_owned(), None));
        } else if let Some(last) = deps.last_mut() {
            let rest = line.trim_start_matches(py::is_space);
            if rest.len() < line.len()
                && let Some(kind) = rest.strip_prefix("update-type: ")
                && !kind.is_empty()
                && !kind.chars().any(py::is_space)
            {
                last.1 = Some(kind.to_owned());
            }
        }
    }
    deps
}

/// `text` as a Markdown table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Whether the pull request is labelled `hold` (every label must have a hashable `name`).
fn on_hold(pr: &Map<String, Value>) -> Result<bool> {
    let mut hold = false;
    for label in iterate(key(pr, "labels")?, "`labels`")? {
        match object(&label, "a label")?.get("name") {
            None => return Err(shape("a label has no `name`")),
            Some(Value::Array(_) | Value::Object(_)) => {
                return Err(shape("a label's `name` is an array or object"));
            }
            Some(Value::String(s)) if s == HOLD => hold = true,
            Some(_) => {}
        }
    }
    Ok(hold)
}

/// `None` if the pull request may merge itself, else the reason it may not.
pub fn eligible(pr: &Map<String, Value>) -> Result<Option<String>> {
    if on_hold(pr)? {
        return Ok(Some(format!("labelled {HOLD}")));
    }
    if py::json_truthy(key(pr, "isDraft")?)
        || py::json_truthy(key(pr, "autoMergeRequest")?)
        || key(pr, "baseRefName")?.as_str() != Some(BASE)
    {
        return Ok(Some(
            "draft, already set to auto-merge, or not into develop".to_owned(),
        ));
    }
    let not_bot = || Ok(Some("has commits not by Dependabot".to_owned()));
    let commits = key(pr, "commits")?;
    if !py::json_truthy(commits) {
        return not_bot();
    }
    let commits = iterate(commits, "`commits`")?;
    let mut bodies = Vec::new();
    for c in &commits {
        let c = object(c, "a commit")?;
        let mut logins = Vec::new();
        for a in iterate(key(c, "authors")?, "a commit's `authors`")? {
            logins.push(key(object(&a, "an author")?, "login")?.clone());
        }
        if logins.len() != 1 || logins[0].as_str() != Some(BOT) {
            return not_bot();
        }
        bodies.push(c);
    }
    let untyped = match key(pr, "headRefName")? {
        Value::String(s) => UNTYPED.iter().any(|p| s.starts_with(p)),
        _ => return Err(shape("`headRefName` is not a string")),
    };
    let mut deps = Vec::new();
    for c in bodies {
        match key(c, "messageBody")? {
            Value::String(body) => deps.extend(dependencies(body)),
            _ => return Err(shape("a commit's `messageBody` is not a string")),
        }
    }
    if deps.is_empty() {
        return Ok(Some("no updated-dependencies metadata".to_owned()));
    }
    for (name, kind) in deps {
        if kind.as_deref() != Some(PATCH) && !(kind.is_none() && untyped) {
            let kind = kind.unwrap_or_else(|| "not a patch update".to_owned());
            return Ok(Some(format!("{name} is {kind}")));
        }
    }
    Ok(None)
}

/// The JSON `gh` printed; a failure prints gh's stderr to `diagnostics` and stops, as the script
/// did.
fn gh(port: &dyn GitHub, args: &[&str], diagnostics: &mut String) -> Result<Value> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let out = port.gh(&args)?;
    if out.code != 0 {
        diagnostics.push_str(&out.stderr);
        return Err(Error::new(format!(
            "gh {} failed ({})",
            args.join(" "),
            out.code
        )));
    }
    serde_json::from_str(&out.stdout)
        .map_err(|e| Error::new(format!("gh {}: not JSON: {e}", args.join(" "))))
}

/// Lists the pull requests and decides. `diagnostics` receives what goes to stderr: one line
/// per pull request, or gh's stderr before an error.
pub fn run(args: &Args, port: &dyn GitHub, diagnostics: &mut String) -> Result<Outcome> {
    let repo = args.repo.as_str();
    // Commits are fetched per pull request: asking for them in the list exceeds GitHub's
    // GraphQL node limit beyond about 20 pull requests.
    let bots = gh(
        port,
        &[
            "pr",
            "list",
            "--repo",
            repo,
            "--author",
            "app/dependabot",
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number",
        ],
        diagnostics,
    )?;
    let pins = gh(
        port,
        &[
            "pr", "list", "--repo", repo, "--head", PINS, "--state", "open", "--json", "number",
        ],
        diagnostics,
    )?;
    // `listed = bots; listed += pins`: a list extends with anything iterable; two strings
    // concatenate; anything else is a TypeError.
    let listed = match (&bots, &pins) {
        (Value::Array(_), _) => {
            let mut all = iterate(&bots, "`gh pr list`")?;
            all.extend(iterate(&pins, "`gh pr list --head`")?);
            all
        }
        (Value::String(a), Value::String(b)) => {
            iterate(&Value::String(format!("{a}{b}")), "`gh pr list`")?
        }
        _ => return Err(shape("`gh pr list` printed neither a list nor two strings")),
    };
    let mut prs = Vec::new();
    for p in &listed {
        let number = key(object(p, "a listed pull request")?, "number")?;
        let number = py::str_json_scalar(number)
            .ok_or_else(|| shape("a listed pull request's `number` is an array or object"))?;
        prs.push(gh(
            port,
            &["pr", "view", &number, "--repo", repo, "--json", FIELDS],
            diagnostics,
        )?);
    }
    let would = if args.dry_run { "would " } else { "" };
    let (mut merge, mut hold, mut rows) = (Vec::new(), Vec::new(), Vec::new());
    for pr in &prs {
        let pr = object(pr, "`gh pr view`'s answer")?;
        let number = key(pr, "number")?
            .as_i64()
            .ok_or_else(|| shape("`number` is not an integer"))?;
        let item = match key(pr, "headRefOid")? {
            Value::String(sha) => format!(r#"{{"number":{number},"sha":{}}}"#, py::json_str(sha)),
            _ => return Err(shape("`headRefOid` is not a string")),
        };
        let verdict = if on_hold(pr)? && py::json_truthy(key(pr, "autoMergeRequest")?) {
            hold.push(item);
            format!("labelled {HOLD}: {would}disable auto-merge")
        } else if key(pr, "headRefName")?.as_str() == Some(PINS) {
            "left alone: the Pins PR, whose auto-merge pins.yml sets".to_owned()
        } else if let Some(why) = eligible(pr)? {
            format!("left alone: {why}")
        } else {
            merge.push(item);
            format!("{would}enable auto-merge")
        };
        let Value::String(title) = key(pr, "title")? else {
            return Err(shape("`title` is not a string"));
        };
        diagnostics.push_str(&format!("#{number} {title}: {verdict}\n"));
        rows.push(format!(
            "| #{number} | {} | {} |",
            cell(title),
            cell(&verdict)
        ));
    }
    let mut summary = format!(
        "### Open Dependabot and Pins pull requests{}\n\n",
        if args.dry_run {
            " (dry run: nothing changed)"
        } else {
            ""
        }
    );
    if rows.is_empty() {
        summary.push_str("None.\n");
    } else {
        summary.push_str("| PR | Title | Auto-merge |\n| --- | --- | --- |\n");
        summary.push_str(&rows.join("\n"));
        summary.push('\n');
    }
    Ok(Outcome {
        stdout: format!(
            r#"{{"merge":[{}],"hold":[{}]}}"#,
            merge.join(","),
            hold.join(",")
        ) + "\n",
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::FakeGitHub;
    use crate::ports::GhOutput;
    use serde::Deserialize;
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::PathBuf;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    fn repo_file(path: &str) -> String {
        ok(fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(path),
        ))
    }

    #[derive(Deserialize)]
    struct Cases {
        case: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        #[serde(default = "yes")]
        summary: bool,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        gh: BTreeMap<String, Answer>,
    }

    fn yes() -> bool {
        true
    }

    #[derive(Deserialize)]
    struct Answer {
        #[serde(default)]
        code: i32,
        #[serde(default)]
        stdout: String,
        #[serde(default)]
        stderr: String,
    }

    /// One case on the fakes: exit status, stdout, stderr, summary (as the CLI writes them).
    fn run_case(case: &Case) -> (u8, String, String, String) {
        let answers: HashMap<String, GhOutput> = case
            .gh
            .iter()
            .map(|(args, a)| {
                let out = GhOutput {
                    code: a.code,
                    stdout: a.stdout.clone(),
                    stderr: a.stderr.clone(),
                };
                (args.clone(), out)
            })
            .collect();
        let port = FakeGitHub {
            answers,
            ..FakeGitHub::default()
        };
        let args = Args {
            repo: "o/r".into(),
            dry_run: case.dry_run,
        };
        let mut diagnostics = String::new();
        match run(&args, &port, &mut diagnostics) {
            Ok(out) => {
                let summary = if case.summary {
                    out.summary
                } else {
                    String::new()
                };
                (0, out.stdout, diagnostics, summary)
            }
            Err(e) => (
                1,
                String::new(),
                format!("{diagnostics}{e}\n"),
                String::new(),
            ),
        }
    }

    /// The cases on which the port fails although the script passed, on purpose: a pull
    /// request `number` that is not an integer, or a `headRefOid` that is not a string, which
    /// the script copied into its JSON as they were.
    const STRICTER: &[&str] = &["number string in view", "headRefOid int"];

    /// Every case of automerge/cases.toml against what the script did with it
    /// (automerge/golden, captured from the script with the same recorded gh answers): where
    /// the script failed, the port fails, with the same output where the script failed with a
    /// message of its own; where it passed, the port gives the same stdout, stderr and summary,
    /// except for the cases in [`STRICTER`], where it fails.
    #[test]
    fn matches_the_script_and_never_passes_where_it_failed() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/automerge");
        let cases: Cases = ok(toml::from_str(&ok(fs::read_to_string(
            dir.join("cases.toml"),
        ))));
        let golden = ok(fs::read_to_string(dir.join("golden")));
        let mut want = BTreeMap::new();
        for block in golden.split("\n== ") {
            // Each section ends in a newline, which the split took.
            let block = format!(
                "{}\n",
                block.trim_start_matches("== ").trim_end_matches('\n')
            );
            let block = block.as_str();
            let (name, rest) = block.split_once('\n').unwrap_or((block, ""));
            let (code, rest) = rest.split_once('\n').unwrap_or((rest, ""));
            let rest = rest.strip_prefix("-- stdout\n").unwrap_or(rest);
            let (stdout, rest) = rest.split_once("-- stderr\n").unwrap_or((rest, ""));
            let (stderr, summary) = rest.split_once("-- summary\n").unwrap_or((rest, ""));
            let summary = summary.strip_suffix('\n').unwrap_or(summary);
            let owned = [code, stdout, stderr, summary].map(str::to_owned);
            want.insert(name.to_owned(), owned);
        }
        assert_eq!(
            want.len(),
            cases.case.len(),
            "cases and golden differ in length"
        );
        let mut problems = Vec::new();
        for case in &cases.case {
            let (code, stdout, stderr, summary) = run_case(case);
            let Some([py_code, py_stdout, py_stderr, py_summary]) = want.get(&case.name) else {
                problems.push(format!("{}: not in the golden", case.name));
                continue;
            };
            // The golden's sections end in a newline of the format, not of the summary.
            let summary = summary.strip_suffix('\n').unwrap_or(&summary).to_owned();
            let same = stdout == *py_stdout && stderr == *py_stderr && summary == *py_summary;
            let stricter = STRICTER.contains(&case.name.as_str());
            let problem = match (py_code.as_str(), code) {
                ("exit 0", 0) if stricter => "passes, but is listed as stricter",
                ("exit 0", 0) if !same => "passes with different output",
                ("exit 0", 0) => "",
                ("exit 0", _) if !stricter => {
                    "fails where the script passed (not listed as stricter)"
                }
                ("exit 0", _) => "",
                (_, 0) => "PASSES WHERE THE SCRIPT FAILED",
                _ if !py_stderr.contains("<traceback>") && !same => {
                    "fails with output other than the script's message"
                }
                _ => "",
            };
            if !problem.is_empty() {
                problems.push(format!(
                    "{}: {problem}\n  script: {py_code} {py_stdout:?} {py_stderr:?} {py_summary:?}\n  port: exit {code} {stdout:?} {stderr:?} {summary:?}",
                    case.name
                ));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn reads_the_metadata_block() {
        let body = "Bumps.\n---\nupdated-dependencies:\n- dependency-name: a\n  update-type: \
                    version-update:semver-patch\n- dependency-name: b\n...\n";
        assert_eq!(
            dependencies(body),
            [
                ("a".to_owned(), Some(PATCH.to_owned())),
                ("b".to_owned(), None)
            ]
        );
        assert!(dependencies("no block").is_empty());
    }

    /// The constants are couplings with other files: each is checked against the file that
    /// relies on the same value.
    #[test]
    fn constants_agree_with_the_files_that_share_them() {
        let pins = repo_file(".github/workflows/pins.yml");
        assert!(
            pins.contains(&format!("git switch -c {PINS}\n")),
            "pins.yml's branch"
        );
        assert!(
            pins.contains(&format!("branch: {PINS}\n")),
            "pins.yml's branch"
        );
        let open_pr = repo_file(".github/actions/open-pr/action.yml");
        assert!(
            open_pr.contains(&format!(r#"any(.name == "{HOLD}")"#)),
            "open-pr's hold label"
        );
        // Dependabot: no target-branch (so the default branch, develop), a docker entry and a
        // rust-toolchain entry for /fuzz, whose branches are UNTYPED's prefixes.
        let dependabot = repo_file(".github/dependabot.yml");
        let config: Vec<&str> = dependabot
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        assert!(!config.iter().any(|l| l.contains("target-branch")));
        assert_eq!(BASE, "develop");
        assert!(config.contains(&"  - package-ecosystem: docker"));
        let toolchain = config
            .iter()
            .position(|l| *l == "  - package-ecosystem: rust-toolchain");
        assert_eq!(
            toolchain.and_then(|i| config.get(i + 1)).copied(),
            Some("    directory: /fuzz")
        );
        assert_eq!(
            UNTYPED,
            ["dependabot/docker/", "dependabot/rust_toolchain/fuzz/"]
        );
        // The workflow reads the JSON's keys and has_work.
        let workflow = repo_file(".github/workflows/dependabot-automerge.yml");
        for needle in [
            ".hold[].number",
            r#".merge[] | "\(.number) \(.sha)""#,
            "has_work",
        ] {
            assert!(
                workflow.contains(needle),
                "dependabot-automerge.yml: {needle}"
            );
        }
    }
}
