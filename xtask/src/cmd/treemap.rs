//! `cargo xtask treemap`: the size of the client's WebAssembly module per crate, as
//! scripts/wasm-size-treemap.py measures it: every function body is credited to the crate of its
//! symbol ([`crate::core::demangle`]), data segments and the rest of the module are items of
//! their own ([`crate::core::wasm`]), and custom sections (names, debug info) are left out of
//! the shipped size. `--table N` prints the N largest crates and the totals, as the script does.
//! `--twiggy-csv` reads the items from `twiggy top --format csv` instead ([`twiggy_items`]).
//!
//! The gzip figure is measured in process (flate2's default backend at level 9, zlib framing as
//! the script's `zlib.compress(…, 9)`), so it can differ from the script's by a little; pass
//! `--shipped-gzip` to print a measured figure instead.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::cmd::wasm_budget;
use crate::core::demangle::{Refused, crate_of_symbol};
use crate::core::py::{csv_rows, int, is_space, splitlines, universal_newlines};
use crate::core::report::thousands;
use crate::core::treemap::{self, Crash, Meta};
use crate::core::wasm::{self, Item};
use crate::{Error, Result};

/// Where the items come from: exactly one of `--wasm` and `--twiggy-csv`.
#[derive(Debug, Clone)]
pub enum Source {
    /// A module that still has its `name` section.
    Wasm(PathBuf),
    /// The output of `twiggy top -n 1000000 --format csv`.
    TwiggyCsv(PathBuf),
}

impl Default for Source {
    fn default() -> Self {
        Self::Wasm(PathBuf::new())
    }
}

/// The command line, as the script took it.
#[derive(Debug, Clone, Default)]
pub struct Args {
    pub source: Source,
    /// Print the N largest crates (0: no table).
    pub table: usize,
    /// The raw and gzip sizes to print instead of the measured ones (0 counts as not given).
    pub shipped_raw: Option<i128>,
    pub shipped_gzip: Option<i128>,
    /// Where the SVG goes.
    pub out: PathBuf,
    pub commit: String,
    pub date: String,
    pub stage: String,
    /// Crates below this share of the total are grouped.
    pub threshold: f64,
}

/// The script's defaults.
pub const UNKNOWN: &str = "unknown";
pub const STAGE: &str = "after wasm-bindgen and wasm-opt -Os";
pub const THRESHOLD: &str = "0.005";

/// What the command prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

/// Bytes per crate, with the script's names for what is not a function.
pub fn per_crate(items: &[(Item, i128)]) -> Result<HashMap<String, i128>> {
    let mut per_crate = HashMap::new();
    for (item, size) in items {
        let crate_ = match item {
            Item::Data => "data segments".to_owned(),
            Item::Meta(_) => "module structure".to_owned(),
            Item::Func(None) => "other / unnamed".to_owned(),
            Item::Func(Some(sym)) => crate_of_symbol(sym)
                .map_err(|Refused(why)| {
                    Error::new(format!("cannot attribute the function {sym:?}: {why}"))
                })?
                .unwrap_or_else(|| "other / unnamed".to_owned()),
        };
        // The sizes' magnitudes add up to at most 2^64 + the file's length for a module (it has
        // fewer items than bytes), and to at most MAX_TWIGGY_BYTES from a CSV.
        *per_crate.entry(crate_).or_default() += *size;
    }
    Ok(per_crate)
}

/// The script's `top_table`: the `n` largest crates, largest first, then by name.
pub fn top_table(per_crate: &HashMap<String, i128>, total: i128, n: usize) -> Result<String> {
    let mut rows: Vec<_> = per_crate.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut lines = vec![format!(
        "{:<4} {:<30} {:>12} {:>8}",
        "#", "crate", "bytes", "share"
    )];
    for (i, (crate_, &bytes)) in rows.into_iter().take(n).enumerate() {
        if total == 0 {
            return Err(Error::new(
                "float division by zero (the crates total 0 bytes)",
            ));
        }
        let share = 100.0 * bytes as f64 / total as f64;
        lines.push(format!(
            "{:<4} {crate_:<30} {:>12} {share:>7.2}%",
            i + 1,
            thousands(bytes)
        ));
    }
    Ok(lines.join("\n"))
}

/// `zlib.compress(module, 9)`, with flate2's backend: zlib framing, at the level and with the
/// library of wasm-budget's gzip figure ([`wasm_budget::GZIP_LEVEL`]), so that the two agree.
pub fn zlib(module: &[u8]) -> Result<Vec<u8>> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::new(wasm_budget::GZIP_LEVEL));
    z.write_all(module)
        .and_then(|()| z.finish())
        .map_err(|e| Error::new(format!("gzip: {e}")))
}

fn zlib_size(module: &[u8]) -> Result<i128> {
    zlib(module).map(|v| i128::try_from(v.len()).unwrap_or(i128::MAX))
}

/// The largest sum of the sizes' magnitudes `--twiggy-csv` takes: 2^53, the integers a float
/// holds exactly. The script reads any size Python's `int()` reads, and its shares and
/// coordinates then lose precision in float arithmetic; the port refuses such a file instead.
pub const MAX_TWIGGY_BYTES: u128 = 1 << 53;

/// The script's `parse_twiggy_csv`: the file as UTF-8 text with universal newlines, split by
/// `str.splitlines()` and read by `csv.reader`. The header's first `Shallow Bytes` column gives
/// the size (else column 0) and its `Item` column the item (else the last column), compared
/// after stripping and lowercasing, the last of equal names winning. Rows too short for both
/// columns are skipped. An item `data[…` is a data segment; a custom section, `.debug…` or
/// `"name"` is skipped; a section header (`type`, `table`, `elem`, `export`, `import`, `global`,
/// `memory`, `code`, `function`, `... and`, `wasm …`) is module structure; anything else is a
/// function, credited by its name. Sizes are read by `int()` (negative ones included).
pub fn twiggy_items(text: &str) -> Result<Vec<(Item, i128)>> {
    let text = universal_newlines(text);
    let lines = splitlines(&text);
    let mut rows = csv_rows(&lines).map_err(Error::new)?.into_iter();
    let header = rows
        .next()
        .ok_or_else(|| Error::new("the CSV has no header line"))?;
    let mut col = HashMap::new();
    for (i, h) in header.iter().enumerate() {
        col.insert(h.trim_matches(is_space).to_lowercase(), i);
    }
    // Python indexes: the item column of an empty header is -1, the row's last field.
    let size_col = col.get("shallow bytes").copied().unwrap_or(0);
    let item_col = col.get("item").copied().or(header.len().checked_sub(1));
    let mut items = Vec::new();
    let mut magnitude: u128 = 0;
    for (n, row) in rows.enumerate() {
        if row.len() <= size_col.max(item_col.unwrap_or(0)) {
            continue;
        }
        let item = &row[item_col.unwrap_or(row.len() - 1)];
        let size = int(&row[size_col])
            .map_err(|e| Error::new(format!("row {} of the CSV: {e}", n + 2)))?;
        let kind = if item.starts_with("data[") {
            Item::Data
        } else if ["custom section", ".debug", "\"name\""]
            .iter()
            .any(|p| item.starts_with(p))
        {
            continue;
        } else if META.iter().any(|p| item.starts_with(p)) || item.starts_with("wasm ") {
            Item::Meta(item.split('[').next().unwrap_or_default().to_owned())
        } else {
            Item::Func(Some(item.clone()))
        };
        magnitude = magnitude.saturating_add(size.unsigned_abs());
        if magnitude > MAX_TWIGGY_BYTES {
            return Err(Error::new(format!(
                "the CSV's sizes add up to more than 2^53 bytes (row {}), which the treemap's \
                 float arithmetic cannot hold exactly",
                n + 2
            )));
        }
        items.push((kind, size));
    }
    Ok(items)
}

/// The prefixes of the section headers twiggy lists.
const META: [&str; 10] = [
    "type", "table", "elem", "export", "import", "global", "memory", "code", "function", "... and",
];

pub fn run(args: &Args) -> Result<Output> {
    let mut stderr = String::new();
    // The script's `raw` and `gz`: measured from a module, none from a CSV.
    let (items, measured_raw, measured_gzip) = match &args.source {
        Source::Wasm(path) => {
            let bytes =
                std::fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
            let analysis = wasm::analyse(&bytes).map_err(|e| Error::new(e.to_string()))?;
            if !analysis.has_names {
                stderr.push_str("warning: no `name` section; functions cannot be attributed\n");
            }
            let items = analysis
                .items
                .into_iter()
                // At most 2^64 + the file's length (a section size is a u64 LEB128).
                .map(|(item, size)| (item, i128::try_from(size).unwrap_or(i128::MAX)))
                .collect();
            let raw = i128::try_from(analysis.shipped.len()).unwrap_or(i128::MAX);
            (items, Some(raw), Some(zlib_size(&analysis.shipped)?))
        }
        Source::TwiggyCsv(path) => {
            let bytes =
                std::fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
            let text = String::from_utf8(bytes)
                .map_err(|e| Error::new(format!("{}: not UTF-8: {e}", path.display())))?;
            (twiggy_items(&text)?, None, None)
        }
    };
    let per_crate = per_crate(&items)?;
    let total: i128 = per_crate.values().sum();
    // `args.shipped_raw or raw`, then `raw or total`; `args.shipped_gzip or gz`, then `or 0`:
    // a given 0 counts as not given.
    let raw = match args.shipped_raw {
        Some(n) if n != 0 => n,
        _ => measured_raw.filter(|&n| n != 0).unwrap_or(total),
    };
    let gzip = match args.shipped_gzip {
        Some(n) if n != 0 => n,
        _ => measured_gzip.unwrap_or(0),
    };
    let families = treemap::aggregate(&per_crate, total, args.threshold);
    let meta = Meta {
        raw,
        gzip,
        commit: args.commit.clone(),
        date: args.date.clone(),
        stage: args.stage.clone(),
    };
    let svg = treemap::render(&families, total, &meta)
        .map_err(|Crash(why)| Error::new(format!("cannot draw the treemap: {why}")))?;
    std::fs::write(&args.out, svg)
        .map_err(|e| Error::new(format!("{}: {e}", args.out.display())))?;
    let mut stdout = String::new();
    if args.table > 0 {
        stdout = format!(
            "{}\n\nattributed total {} B; shipped-equivalent module {} B; gzip -9 {} B\n",
            top_table(&per_crate, total, args.table)?,
            thousands(total),
            thousands(raw),
            thousands(gzip),
        );
    }
    Ok(Output { stdout, stderr })
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    fn ok<T, E: std::fmt::Display>(r: std::result::Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    /// The script's `zlib.compress(…, 9)`: zlib framing whose header says deflate with a 32 KiB
    /// window (0x78) at "maximum compression" (FLEVEL 3: 0xDA), and the input back.
    #[test]
    fn compresses_as_zlib_at_level_9() {
        let noise: Vec<u8> = (0..20_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        for input in [&b""[..], b"\0asm\x01\0\0\0", &[7; 5000], &noise] {
            let z = ok(zlib(input));
            assert_eq!(z.get(..2), Some(&[0x78, 0xDA][..]), "{} bytes", input.len());
            let mut back = Vec::new();
            ok(flate2::read::ZlibDecoder::new(z.as_slice()).read_to_end(&mut back));
            assert_eq!(back, input);
        }
    }

    /// The figure printed is that of the whole shipped module (its first 8 bytes included).
    #[test]
    fn prints_the_zlib_size_of_the_shipped_module() {
        let module = [
            &b"\0asm\x01\0\0\0"[..],
            &[1, 4, 1, 0x60, 0, 0],
            &[3, 2, 1, 0],
            &[10, 4, 1, 2, 0, 0x0B],
            &[0, 5, 4, b'n', b'o', b'p', b'e'],
        ]
        .concat();
        let dir = ok(tempfile::tempdir());
        let wasm = dir.path().join("m.wasm");
        ok(std::fs::write(&wasm, &module));
        let args = Args {
            source: Source::Wasm(wasm),
            table: 1,
            out: dir.path().join("m.svg"),
            threshold: 0.005,
            ..Args::default()
        };
        let shipped = ok(wasm::analyse(&module)).shipped;
        assert_eq!(shipped.len(), module.len() - 7);
        let want = format!("gzip -9 {} B\n", thousands(ok(zlib_size(&shipped))));
        let out = ok(run(&args)).stdout;
        assert!(out.ends_with(&want), "{out:?} should end with {want:?}");
    }
}
