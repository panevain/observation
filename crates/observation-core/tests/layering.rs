//! The crate's modules form layers, and a module may only use its own layer
//! or lower ones (see the crate documentation). This test reads every source
//! file, finds each path into another top-level module (`crate::…`, and
//! `super::…` chains that climb out of one), and fails on an upward edge
//! that is not in [`ALLOWED`]. It also fails when an [`ALLOWED`] entry no
//! longer matches anything, so the list can only shrink, and when a file
//! belongs to a module no layer names.

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
            && (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_'))
        {
            // A raw string: r"…", r#"…"#, and so on.
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
/// top-level module, as segments starting with that module's name.
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

#[test]
fn modules_only_use_their_own_layer_or_lower() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    let mut problems = Vec::new();
    let mut used = BTreeSet::new();
    for file in &files {
        let rel = file.strip_prefix(&src).expect("under src/");
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str == "lib.rs" {
            continue;
        }
        // The module path: `coverage/sight.rs` is coverage::sight,
        // `coverage/mod.rs` is coverage.
        let mut module: Vec<String> = rel
            .with_extension("")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if module.last().is_some_and(|m| m == "mod") {
            module.pop();
        }
        let top = module[0].clone();
        if top == "testing" {
            continue;
        }
        let Some(from) = layer_of(&top) else {
            problems.push(format!(
                "{rel_str}: assign this file to a layer (module `{top}` is in no layer)"
            ));
            continue;
        };
        let code = code_only(&std::fs::read_to_string(file).expect("read a source file"));
        for path in edges(&module, &code) {
            let Some(target) = path.first() else { continue };
            if *target == top || target == "testing" {
                continue;
            }
            let Some(to) = layer_of(target) else {
                problems.push(format!(
                    "{rel_str}: uses `{}`, and module `{target}` is in no layer",
                    path.join("::")
                ));
                continue;
            };
            if to > from {
                let path = path.join("::");
                match ALLOWED.iter().find(|(f, p, _)| *f == rel_str && *p == path) {
                    Some(entry) => {
                        used.insert(*entry);
                    }
                    None => problems.push(format!(
                        "{rel_str} (layer {from}) uses `{path}` (layer {to}): an upward edge"
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
    assert!(problems.is_empty(), "layering:\n{}", problems.join("\n"));
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
