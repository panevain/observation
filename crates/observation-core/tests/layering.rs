//! The crate's modules form layers, and a module may only use its own layer
//! or lower ones. [`LAYERS`] is the source of truth; the table in the crate
//! documentation must list the same modules per layer (checked here). This
//! test reads every source file, finds each path into another top-level
//! module (`crate::…`, and `super::…` chains that climb out of one), and
//! fails on an upward edge that is not in [`ALLOWED`]. It also fails:
//! - when an [`ALLOWED`] entry no longer matches anything, so the list can
//!   only shrink;
//! - when code inside the crate imports one of the compatibility re-exports
//!   [`ALLOWED`] exists for (such as `building::infer_heights`) rather than
//!   the item's home, since that would carry the upward edge into a module
//!   the allowlist does not name;
//! - on `use crate::*`, `use crate as …` and a `super::*` or `super as …`
//!   that climbs out of a top-level module, which hide the module used;
//! - when a file belongs to a module no layer names, when a [`LAYERS`] entry
//!   has no file, and on an inline module in `lib.rs`.
//!
//! `#[cfg(test)]` modules are scanned too, on purpose: a test inside a module
//! is part of that module and keeps to its layer (fixtures belong in
//! `testing`, which may be used from anywhere).
//!
//! `lib.rs` itself is the crate root, in no layer: it only declares the
//! modules (and re-exports `glam`), so only inline modules are refused there.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every top-level module and its layer. `testing` is not a layer: it holds
/// fixture builders for tests and may use anything, and anything may use it
/// (it is compiled only for tests).
const LAYERS: &[(&str, u8)] = &[
    // 0: helpers with no domain knowledge.
    ("util", 0),
    // 1: coordinates and map tiles.
    ("geo", 1),
    ("tiles", 1),
    // 2: the world model.
    ("building", 2),
    ("road", 2),
    ("address", 2),
    ("terrain", 2),
    ("camera", 2),
    // 3: OSM input and the area bundle built from it.
    ("osm", 3),
    ("bundle", 3),
    // 4: analysis.
    ("frustum", 4),
    ("occlusion", 4),
    ("coverage", 4),
    ("example", 4),
    // 5: messages and records exchanged with the outside.
    ("coverage_job", 5),
    ("regions", 5),
    ("provenance", 5),
    ("scene", 5),
];

/// Upward edges that remain, each with the reason: `(file under src/, path
/// used, why)`. Remove an entry once its edge is gone (the test says so).
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "building.rs",
        "osm::tags::STOREY_M",
        "compatibility re-export: building::STOREY_M keeps its path",
    ),
    (
        "building.rs",
        "osm::tags::default_height_for",
        "compatibility re-export: building::default_height_for keeps its path",
    ),
    (
        "building.rs",
        "osm::tags::infer_heights",
        "compatibility re-export: building::infer_heights keeps its path (the server uses it)",
    ),
    (
        "building.rs",
        "osm::tags::parse_length_m",
        "compatibility re-export: building::parse_length_m keeps its path",
    ),
];

fn layer_of(module: &str) -> Option<u8> {
    LAYERS.iter().find(|(m, _)| *m == module).map(|&(_, l)| l)
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read src/")
        .map(|e| e.expect("read a directory entry").path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// Whether the `r` at `c[i]` starts a raw string literal: it is not the end
/// of an identifier, except for the `b` or `c` of a raw byte or C string.
fn raw_string_prefix(c: &[char], i: usize) -> bool {
    let ident = |k: usize| c[k].is_alphanumeric() || c[k] == '_';
    match i {
        0 => true,
        _ if !ident(i - 1) => true,
        1 => c[0] == 'b' || c[0] == 'c',
        _ => (c[i - 1] == 'b' || c[i - 1] == 'c') && !ident(i - 2),
    }
}

/// The source with comments, string literals and character literals blanked
/// out (replaced by spaces), so only code is scanned.
fn code_only(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let blank = |out: &mut String, ch: char| out.push(if ch == '\n' { '\n' } else { ' ' });
    while i < c.len() {
        let next = c.get(i + 1).copied();
        if c[i] == '/' && next == Some('/') {
            while i < c.len() && c[i] != '\n' {
                blank(&mut out, c[i]);
                i += 1;
            }
        } else if c[i] == '/' && next == Some('*') {
            let mut depth = 0;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    blank(&mut out, c[i]);
                    blank(&mut out, c[i + 1]);
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    blank(&mut out, c[i]);
                    blank(&mut out, c[i + 1]);
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    blank(&mut out, c[i]);
                    i += 1;
                }
            }
        } else if c[i] == 'r'
            && (next == Some('"') || next == Some('#'))
            && raw_string_prefix(&c, i)
        {
            // A raw string: r"…", r#"…"#, and so on, also as br"…" or cr"…".
            let mut j = i + 1;
            while c.get(j) == Some(&'#') {
                j += 1;
            }
            if c.get(j) != Some(&'"') {
                out.push(c[i]);
                i += 1;
                continue;
            }
            let hashes = j - i - 1;
            let mut k = j + 1;
            while k < c.len()
                && !(c[k] == '"' && (0..hashes).all(|h| c.get(k + 1 + h) == Some(&'#')))
            {
                k += 1;
            }
            let end = (k + 1 + hashes).min(c.len());
            for &ch in &c[i..end] {
                blank(&mut out, ch);
            }
            i = end;
        } else if c[i] == '"' {
            blank(&mut out, c[i]);
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' {
                    blank(&mut out, c[i]);
                    i += 1;
                }
                if i < c.len() {
                    blank(&mut out, c[i]);
                    i += 1;
                }
            }
            if i < c.len() {
                blank(&mut out, c[i]);
                i += 1;
            }
        } else if c[i] == '\'' {
            // A character literal ('a', '\n', '\u{1F600}'), or a lifetime.
            let close = if next == Some('\\') {
                (i + 2..c.len()).find(|&k| c[k] == '\'')
            } else if c.get(i + 2) == Some(&'\'') {
                Some(i + 2)
            } else {
                None
            };
            match close {
                Some(end) => {
                    for &ch in &c[i..=end] {
                        blank(&mut out, ch);
                    }
                    i = end + 1;
                }
                None => {
                    out.push(c[i]);
                    i += 1;
                }
            }
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    PathSep,
    Punct(char),
}

fn tokens(code: &str) -> Vec<Token> {
    let c: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i].is_alphabetic() || c[i] == '_' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Token::Ident(c[start..i].iter().collect()));
        } else if c[i] == ':' && c.get(i + 1) == Some(&':') {
            out.push(Token::PathSep);
            i += 2;
        } else if c[i].is_whitespace() {
            i += 1;
        } else {
            out.push(Token::Punct(c[i]));
            i += 1;
        }
    }
    out
}

/// Reads a use tree or path starting at `t[*i]` (just after a `::`), and
/// returns every full path it names, each as segments after `prefix`.
fn paths(t: &[Token], i: &mut usize, prefix: &[String], out: &mut Vec<Vec<String>>) {
    match t.get(*i) {
        Some(Token::Punct('{')) => {
            *i += 1;
            loop {
                match t.get(*i) {
                    Some(Token::Punct('}')) => {
                        *i += 1;
                        return;
                    }
                    Some(Token::Punct(',')) => *i += 1,
                    None => return,
                    _ => paths(t, i, prefix, out),
                }
            }
        }
        Some(Token::Ident(name)) => {
            let mut path = prefix.to_vec();
            path.push(name.clone());
            *i += 1;
            if t.get(*i) == Some(&Token::PathSep) {
                *i += 1;
                paths(t, i, &path, out);
            } else {
                if matches!(t.get(*i), Some(Token::Ident(a)) if a == "as") {
                    *i += 2;
                }
                out.push(path);
            }
        }
        Some(Token::Punct('*')) => {
            *i += 1;
            out.push(prefix.to_vec());
        }
        _ => out.push(prefix.to_vec()),
    }
}

/// Every path from `file` (its module path under the crate) into another
/// top-level module, as segments starting with that module's name. A glob or
/// alias of the crate root itself (`crate::*`, `crate as x`, the same through
/// `super`) gives an empty path.
fn edges(module: &[String], code: &str) -> Vec<Vec<String>> {
    let t = tokens(code);
    let mut out = Vec::new();
    // Inline modules (`mod tests { … }`) deepen the module path for `super`.
    let mut inline: Vec<(String, usize)> = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < t.len() {
        match &t[i] {
            Token::Punct('{') => depth += 1,
            Token::Punct('}') => {
                depth = depth.saturating_sub(1);
                if inline.last().is_some_and(|(_, d)| *d == depth) {
                    inline.pop();
                }
            }
            Token::Ident(m) if m == "mod" => {
                if let (Some(Token::Ident(name)), Some(Token::Punct('{'))) =
                    (t.get(i + 1), t.get(i + 2))
                {
                    inline.push((name.clone(), depth));
                }
            }
            // `crate as x` and a top-level `super as x` hide the module
            // used: reported as an empty path.
            Token::Ident(c)
                if (c == "crate" || (c == "super" && module.len() + inline.len() <= 1))
                    && matches!(t.get(i + 1), Some(Token::Ident(a)) if a == "as") =>
            {
                out.push(Vec::new());
            }
            Token::Ident(c) if c == "crate" && t.get(i + 1) == Some(&Token::PathSep) => {
                i += 2;
                paths(&t, &mut i, &[], &mut out);
                continue;
            }
            Token::Ident(s) if s == "super" && t.get(i + 1) == Some(&Token::PathSep) => {
                let here: Vec<&String> =
                    module.iter().chain(inline.iter().map(|(n, _)| n)).collect();
                let mut up = 0;
                let mut j = i;
                while matches!(t.get(j), Some(Token::Ident(s)) if s == "super")
                    && t.get(j + 1) == Some(&Token::PathSep)
                {
                    up += 1;
                    j += 2;
                }
                // Only a climb out of the top-level module leaves its layer.
                if up >= here.len() {
                    i = j;
                    paths(&t, &mut i, &[], &mut out);
                    continue;
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The module path of a file under `src/`: `coverage/sight.rs` is
/// coverage::sight, `coverage/mod.rs` is coverage.
fn module_of(rel: &str) -> Vec<String> {
    let mut module: Vec<String> = rel
        .trim_end_matches(".rs")
        .split('/')
        .map(str::to_owned)
        .collect();
    if module.last().is_some_and(|m| m == "mod") {
        module.pop();
    }
    module
}

/// The paths inside the crate that [`ALLOWED`]'s compatibility re-exports
/// create, each with the item's home: `("building::infer_heights",
/// "osm::tags::infer_heights")`.
fn compat_paths() -> Vec<(String, &'static str)> {
    ALLOWED
        .iter()
        .map(|(file, home, _)| {
            let item = home.rsplit("::").next().unwrap_or(home);
            (format!("{}::{item}", module_of(file).join("::")), *home)
        })
        .collect()
}

/// Every layering problem in `files`: `(path under src/, source)` pairs.
fn check(files: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut used = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let compat = compat_paths();
    for (rel_str, source) in files {
        let rel_str = rel_str.as_str();
        let code = code_only(source);
        if rel_str == "lib.rs" {
            let t = tokens(&code);
            for w in t.windows(3) {
                if let [Token::Ident(m), Token::Ident(name), Token::Punct('{')] = w
                    && m == "mod"
                {
                    problems.push(format!(
                        "lib.rs: inline module `{name}`: move it to src/{name}.rs and give it a layer"
                    ));
                }
            }
            continue;
        }
        let module = module_of(rel_str);
        let top = module[0].clone();
        seen.insert(top.clone());
        if top == "testing" {
            continue;
        }
        let Some(from) = layer_of(&top) else {
            problems.push(format!(
                "{rel_str}: assign this file to a layer (module `{top}` is in no layer)"
            ));
            continue;
        };
        for path in edges(&module, &code) {
            let Some(target) = path.first() else {
                problems.push(format!(
                    "{rel_str}: `crate::*`, `crate as …` or a top-level `super::*` / `super as …` \
                     is not allowed; name the module"
                ));
                continue;
            };
            let joined = path.join("::");
            for (old, home) in &compat {
                let reexporter = ALLOWED.iter().any(|(f, h, _)| f == &rel_str && h == home);
                let hit = joined == *old || joined.starts_with(&format!("{old}::"));
                if hit && !reexporter {
                    problems.push(format!(
                        "{rel_str}: uses `{old}`, a compatibility re-export; use `{home}` (its home)"
                    ));
                }
            }
            if *target == top || target == "testing" {
                continue;
            }
            let Some(to) = layer_of(target) else {
                problems.push(format!(
                    "{rel_str}: uses `{joined}`, and module `{target}` is in no layer"
                ));
                continue;
            };
            if to > from {
                match ALLOWED
                    .iter()
                    .find(|(f, p, _)| *f == rel_str && *p == joined)
                {
                    Some(entry) => {
                        used.insert(*entry);
                    }
                    None => problems.push(format!(
                        "{rel_str} (layer {from}) uses `{joined}` (layer {to}): an upward edge"
                    )),
                }
            }
        }
    }
    for entry in ALLOWED {
        if !used.contains(entry) {
            problems.push(format!(
                "allowed edge {} -> {} no longer exists: remove it from ALLOWED",
                entry.0, entry.1
            ));
        }
    }
    for (name, _) in LAYERS {
        if !seen.contains(*name) {
            problems.push(format!(
                "LAYERS names `{name}`, but there is no src/{name}.rs or src/{name}/: remove it"
            ));
        }
    }
    problems
}

fn crate_sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    files
        .iter()
        .map(|file| {
            let rel = file.strip_prefix(&src).expect("under src/");
            (
                rel.to_string_lossy().replace('\\', "/"),
                std::fs::read_to_string(file).expect("read a source file"),
            )
        })
        .collect()
}

#[test]
fn modules_only_use_their_own_layer_or_lower() {
    let problems = check(&crate_sources());
    assert!(problems.is_empty(), "layering:\n{}", problems.join("\n"));
}

#[test]
fn the_crate_documentation_lists_the_same_layers() {
    let lib = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("read lib.rs");
    let mut documented: Vec<(String, u8)> = Vec::new();
    for line in lib.lines() {
        let Some(row) = line.strip_prefix("//! | ") else {
            continue;
        };
        let mut cells = row.split(" | ");
        let (Some(layer), Some(modules)) = (cells.next(), cells.next()) else {
            continue;
        };
        let Ok(layer) = layer.trim().parse::<u8>() else {
            continue;
        };
        for name in modules.split(',') {
            let name = name
                .trim()
                .trim_matches(|c| c == '[' || c == ']' || c == '`');
            documented.push((name.to_owned(), layer));
        }
    }
    let mut want: Vec<(String, u8)> = LAYERS.iter().map(|(m, l)| ((*m).to_owned(), *l)).collect();
    documented.sort();
    want.sort();
    assert_eq!(
        documented, want,
        "the `# Layers` table in src/lib.rs must list the modules of LAYERS"
    );
}

/// The attacks the scanner must catch, each on a small fake source tree.
#[test]
fn the_check_refuses_hidden_and_laundered_edges() {
    let problems = |rel: &str, src: &str| -> Vec<String> {
        check(&[(rel.to_owned(), src.to_owned())])
            .into_iter()
            .filter(|p| !p.starts_with("allowed edge") && !p.starts_with("LAYERS names"))
            .collect()
    };
    let laundered = problems("address.rs", "use crate::building::infer_heights;");
    assert_eq!(
        laundered,
        [
            "address.rs: uses `building::infer_heights`, a compatibility re-export; \
          use `osm::tags::infer_heights` (its home)"
        ]
    );
    assert_eq!(
        problems(
            "coverage/sight.rs",
            "use super::super::building::{STOREY_M};"
        )
        .len(),
        1
    );
    for hidden in [
        "use crate::*; fn f() { coverage::x(); }",
        "use crate as c; fn f() { c::coverage::x(); }",
        "use super::*; fn f() { coverage::x(); }",
        "use super as s;",
        "use crate::{*};",
    ] {
        let found = problems("address.rs", hidden);
        assert!(
            found.len() == 1 && found[0].contains("name the module"),
            "{hidden}: {found:?}"
        );
    }
    // Inside a module's own tree, `super::*` stays in the module.
    assert!(problems("coverage/sight.rs", "use super::*;").is_empty());
    assert!(problems("address.rs", "mod tests { use super::*; }").is_empty());
    // Raw byte and C strings are not code.
    assert!(
        problems(
            "address.rs",
            r###"const B: &[u8] = br#"a " crate::coverage "#;"###
        )
        .is_empty()
    );
    assert!(
        problems(
            "address.rs",
            r###"const C: &CStr = cr#"a " crate::coverage "#;"###
        )
        .is_empty()
    );
    // An inline module in lib.rs has no layer.
    assert_eq!(
        problems(
            "lib.rs",
            "pub mod inline_new { pub use crate::scene::SceneFile; }"
        ),
        ["lib.rs: inline module `inline_new`: move it to src/inline_new.rs and give it a layer"]
    );
    // A LAYERS entry needs a file.
    assert!(
        check(&[])
            .iter()
            .any(|p| p.starts_with("LAYERS names `geo`"))
    );
    // `pub(crate)` is not a path.
    assert!(problems("address.rs", "pub(crate) fn f() {}").is_empty());
}

#[test]
fn the_scanner_reads_nested_use_lists_and_skips_comments() {
    let code = code_only(
        r##"
        // use crate::scene::SceneFile;
        /* crate::scene /* nested */ crate::scene */
        use crate::{
            geo::{BBox, LatLon as L},
            camera::model::*,
        };
        let s = "crate::scene";
        let r = r#"crate::scene"#;
        let c = '"'; let l: &'static str = "";
        mod tests {
            use super::super::bundle::AreaBundle;
            use super::Local;
        }
        let x = crate::road::Road { };
    "##,
    );
    let found: Vec<String> = edges(&["coverage".to_owned()], &code)
        .iter()
        .map(|p| p.join("::"))
        .collect();
    assert_eq!(
        found,
        [
            "geo::BBox",
            "geo::LatLon",
            "camera::model",
            "bundle::AreaBundle",
            "road::Road"
        ]
    );
}
