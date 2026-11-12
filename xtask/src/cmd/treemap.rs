//! `cargo xtask treemap`: the size of the client's WebAssembly module per crate, as
//! scripts/wasm-size-treemap.py measures it: every function body is credited to the crate of its
//! symbol ([`crate::core::demangle`]), data segments and the rest of the module are items of
//! their own ([`crate::core::wasm`]), and custom sections (names, debug info) are left out of
//! the shipped size. `--table N` prints the N largest crates and the totals, as the script does.
//!
//! The gzip figure is measured in process (flate2's default backend at level 9, zlib framing as
//! the script's `zlib.compress(…, 9)`), so it can differ from the script's by a little; pass
//! `--shipped-gzip` to print a measured figure instead.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::core::demangle::{Refused, crate_of_symbol};
use crate::core::report::thousands;
use crate::core::wasm::{self, Item};
use crate::{Error, Result};

/// The command line, as the script took it.
#[derive(Debug, Clone, Default)]
pub struct Args {
    /// A module that still has its `name` section.
    pub wasm: PathBuf,
    /// Print the N largest crates (0: no table).
    pub table: usize,
    /// The raw and gzip sizes to print instead of the measured ones (0 counts as not given).
    pub shipped_raw: Option<i128>,
    pub shipped_gzip: Option<i128>,
}

/// What the command prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

/// Bytes per crate, with the script's names for what is not a function.
pub fn per_crate(items: &[(Item, u128)]) -> Result<HashMap<String, u128>> {
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
        *per_crate.entry(crate_).or_default() += size;
    }
    Ok(per_crate)
}

fn size(n: u128) -> String {
    thousands(i128::try_from(n).unwrap_or(i128::MAX))
}

/// The script's `top_table`: the `n` largest crates, largest first, then by name.
pub fn top_table(per_crate: &HashMap<String, u128>, total: u128, n: usize) -> String {
    let mut rows: Vec<_> = per_crate.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut lines = vec![format!(
        "{:<4} {:<30} {:>12} {:>8}",
        "#", "crate", "bytes", "share"
    )];
    for (i, (crate_, &bytes)) in rows.into_iter().take(n).enumerate() {
        let share = 100.0 * bytes as f64 / total as f64;
        lines.push(format!(
            "{:<4} {crate_:<30} {:>12} {share:>7.2}%",
            i + 1,
            size(bytes)
        ));
    }
    lines.join("\n")
}

/// `len(zlib.compress(module, 9))`, with flate2's backend.
fn zlib_size(module: &[u8]) -> Result<u128> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::new(9));
    z.write_all(module)
        .and_then(|()| z.finish())
        .map(|v| v.len() as u128)
        .map_err(|e| Error::new(format!("gzip: {e}")))
}

pub fn run(args: &Args) -> Result<Output> {
    let bytes = std::fs::read(&args.wasm)
        .map_err(|e| Error::new(format!("{}: {e}", args.wasm.display())))?;
    let analysis = wasm::analyse(&bytes).map_err(|e| Error::new(e.to_string()))?;
    let mut stderr = String::new();
    if !analysis.has_names {
        stderr.push_str("warning: no `name` section; functions cannot be attributed\n");
    }
    let per_crate = per_crate(&analysis.items)?;
    let total: u128 = per_crate.values().sum();
    // `args.shipped_raw or len(shipped)`: 0 is not given.
    let raw = match args.shipped_raw {
        Some(n) if n != 0 => thousands(n),
        _ => size(analysis.shipped.len() as u128),
    };
    let gzip = match args.shipped_gzip {
        Some(n) if n != 0 => thousands(n),
        _ => size(zlib_size(&analysis.shipped)?),
    };
    let mut stdout = String::new();
    if args.table > 0 {
        stdout = format!(
            "{}\n\nattributed total {} B; shipped-equivalent module {raw} B; gzip -9 {gzip} B\n",
            top_table(&per_crate, total, args.table),
            size(total),
        );
    }
    Ok(Output { stdout, stderr })
}
