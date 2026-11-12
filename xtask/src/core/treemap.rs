//! The treemap of scripts/wasm-size-treemap.py: crates grouped into families, families and
//! crates tiled by the squarified algorithm, and the SVG, all computed as the script computes
//! them (Python's float arithmetic, its `sum()` of floats, `round()`, `%.Nf` and `max()`/`min()`
//! on ties and NaN), so that the same input gives the same bytes. Where the script divided by
//! zero or looked up a family it had not tiled, and crashed, [`render`] fails.

use std::collections::HashMap;

use crate::core::report::thousands;

pub const NONCODE: [&str; 3] = ["data segments", "other / unnamed", "module structure"];
const NONCODE_FAMILY: &str = "Data and module structure";
const STD_CRATES: [&str; 10] = [
    "core",
    "alloc",
    "std",
    "compiler_builtins",
    "dlmalloc",
    "panic_abort",
    "panic_unwind",
    "unwind",
    "rustc_std_workspace_core",
    "rustc_std_workspace_alloc",
];
const EGUI_CRATES: [&str; 11] = [
    "egui",
    "epaint",
    "emath",
    "ecolor",
    "bevy_egui",
    "egui_extras",
    "epaint_default_fonts",
    "ab_glyph",
    "ab_glyph_rasterizer",
    "ttf_parser",
    "owned_ttf_parser",
];
const FAMILY_ORDER: [&str; 7] = [
    "Bevy",
    "egui",
    "wgpu",
    "Rust std",
    "observation",
    "Other dependencies",
    NONCODE_FAMILY,
];

/// (leaf fill, family backdrop); white text on every leaf fill has contrast >= 4.5.
fn colours(family: &str) -> (&'static str, &'static str) {
    match family {
        "Bevy" => ("#9a4b00", "#5c2d00"),
        "egui" => ("#0b5f8a", "#06374f"),
        "wgpu" => ("#6a3fb0", "#3d2368"),
        "Rust std" => ("#55606b", "#2f363d"),
        "observation" => ("#1a7332", "#0d4019"),
        "Other dependencies" => ("#a1294f", "#5e1730"),
        _ => ("#3f4e5e", "#222c36"),
    }
}

/// Why the script would have crashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crash(pub String);

type Out<T> = Result<T, Crash>;

pub fn family_of(crate_: &str) -> &'static str {
    if NONCODE.contains(&crate_) {
        NONCODE_FAMILY
    } else if crate_.starts_with("observation") {
        "observation"
    } else if EGUI_CRATES.contains(&crate_) {
        "egui"
    } else if crate_.starts_with("bevy") || crate_.starts_with("encase") {
        "Bevy"
    } else if ["wgpu", "naga", "glow", "khronos_egl"]
        .iter()
        .any(|p| crate_.starts_with(p))
    {
        "wgpu"
    } else if STD_CRATES.contains(&crate_) {
        "Rust std"
    } else {
        "Other dependencies"
    }
}

/// Python's `int >= float`, which compares the exact values.
fn int_ge_float(i: i128, f: f64) -> bool {
    if f.is_nan() {
        return false;
    }
    if f >= 2f64.powi(127) {
        return false;
    }
    if f < -(2f64.powi(127)) {
        return true;
    }
    // |floor| < 2^127, an integer, so the cast is exact.
    let floor = f.floor();
    let fi = floor as i128;
    i > fi || (i == fi && floor == f)
}

/// Each family's members, largest first (by name on ties), the crates below `threshold` of the
/// total folded into one `other crates (N)` member (or kept, if only one).
pub fn aggregate(
    per_crate: &HashMap<String, i128>,
    total: i128,
    threshold: f64,
) -> HashMap<&'static str, Vec<(String, i128)>> {
    let mut fam: HashMap<&'static str, Vec<(String, i128)>> = HashMap::new();
    for (crate_, &size) in per_crate {
        fam.entry(family_of(crate_))
            .or_default()
            .push((crate_.clone(), size));
    }
    // `threshold * total`: the int converted to a float first.
    let limit = threshold * total as f64;
    let mut out = HashMap::new();
    for (f, members) in fam {
        // Not a partition: with a NaN threshold a crate is in neither list.
        let mut big: Vec<_> = members
            .iter()
            .filter(|m| int_ge_float(m.1, limit))
            .cloned()
            .collect();
        let small: Vec<_> = members
            .into_iter()
            .filter(|m| !limit.is_nan() && !int_ge_float(m.1, limit))
            .collect();
        big.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if small.len() == 1 {
            big.extend(small);
        } else if !small.is_empty() {
            let sum = small.iter().map(|s| s.1).sum();
            big.push((format!("other crates ({})", small.len()), sum));
        }
        out.insert(f, big);
    }
    out
}

/// Python's `sum()` of floats (3.12 and later): the first value, then Neumaier's compensated
/// sum of the rest, the compensation added at the end if it is finite and not zero.
pub fn py_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut values = values.into_iter();
    let Some(first) = values.next() else {
        return 0.0;
    };
    let (mut f, mut c) = (0.0 + first, 0.0);
    for x in values {
        let t = f + x;
        if f.abs() >= x.abs() {
            c += (f - t) + x;
        } else {
            c += (x - t) + f;
        }
        f = t;
    }
    if c != 0.0 && c.is_finite() {
        f += c;
    }
    f
}

fn div(a: f64, b: f64) -> Out<f64> {
    if b == 0.0 {
        return Err(Crash("float division by zero".into()));
    }
    Ok(a / b)
}

/// Python's `max(a, b)` and `min(a, b)`: the first unless the second is strictly beyond it.
fn py_max(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

fn py_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

pub type Rect = (f64, f64, f64, f64);

fn worst(row: &[(&str, f64)], side: f64) -> Out<f64> {
    let s = py_sum(row.iter().map(|r| r.1));
    let mut out: Option<f64> = None;
    for &(_, a) in row {
        let m = py_max(div(side * side * a, s * s)?, div(s * s, side * side * a)?);
        out = Some(out.map_or(m, |o| py_max(o, m)));
    }
    out.ok_or_else(|| Crash("max() arg is an empty sequence".into()))
}

/// The squarified treemap of `sizes` (sorted descending) in the rectangle.
pub fn squarify(
    sizes: &[(&str, i128)],
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Out<HashMap<String, Rect>> {
    let (mut x, mut y, mut w, mut h) = (x, y, w, h);
    let mut rects = HashMap::new();
    let total = sizes.iter().map(|s| s.1).sum::<i128>() as f64;
    if total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return Ok(rects);
    }
    let scale = div(w * h, total)?;
    let mut queue: Vec<(&str, f64)> = sizes
        .iter()
        .filter(|s| s.1 > 0)
        .map(|&(k, v)| (k, v as f64 * scale))
        .collect();
    while !queue.is_empty() {
        let side = py_min(w, h);
        let mut row = vec![queue[0]];
        let mut i = 1;
        while i < queue.len() {
            let mut longer = row.clone();
            longer.push(queue[i]);
            if worst(&longer, side)? <= worst(&row, side)? {
                row = longer;
                i += 1;
            } else {
                break;
            }
        }
        queue.drain(..i);
        let s = py_sum(row.iter().map(|r| r.1));
        if w >= h {
            // The row becomes a column on the left.
            let cw = div(s, h)?;
            let mut cy = y;
            for (k, a) in row {
                let ch = div(a, cw)?;
                rects.insert(k.to_owned(), (x, cy, cw, ch));
                cy += ch;
            }
            x += cw;
            w -= cw;
        } else {
            // The row runs along the top.
            let rh = div(s, w)?;
            let mut cx = x;
            for (k, a) in row {
                let rw = div(a, rh)?;
                rects.insert(k.to_owned(), (cx, y, rw, rh));
                cx += rw;
            }
            y += rh;
            h -= rh;
        }
    }
    Ok(rects)
}

/// `"%.Nf" % x`. Every value drawn is finite: sizes are integers below 2^127, and each division
/// by zero fails before it.
fn f(x: f64, n: usize) -> String {
    format!("{x:.n$}")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn kib(n: i128) -> String {
    if n >= 1024 * 1024 {
        format!("{} MiB", f(n as f64 / 1_048_576.0, 2))
    } else {
        // `int(round(x))`: ties to even. |n / 1024| < 2^127, so the cast is exact.
        thousands((n as f64 / 1024.0).round_ties_even() as i128) + " KiB"
    }
}

fn text_w(s: &str, size: usize, bold: bool) -> f64 {
    (s.chars().count() * size) as f64 * if bold { 0.62 } else { 0.58 }
}

fn shade(hex: &str, factor: f64) -> String {
    let c = |i: usize| i64::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64;
    let r = |v: f64| (v * factor).round_ties_even() as i64;
    format!("#{:02x}{:02x}{:02x}", r(c(1)), r(c(3)), r(c(5)))
}

/// What the title says besides the tiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub raw: i128,
    pub gzip: i128,
    pub commit: String,
    pub date: String,
    pub stage: String,
}

fn pct(size: i128, total: i128) -> Out<f64> {
    div(100.0 * size as f64, total as f64)
}

/// The script's `render`: the SVG text.
pub fn render(
    families: &HashMap<&'static str, Vec<(String, i128)>>,
    total: i128,
    meta: &Meta,
) -> Out<String> {
    let (width, pad, head_h, map_h, foot_h) = (1200usize, 16usize, 78usize, 720usize, 34usize);
    let height = head_h + map_h + foot_h;
    let mut body = Vec::new();

    let fam_sizes: Vec<(&str, i128)> = FAMILY_ORDER
        .iter()
        .filter_map(|&f| {
            families
                .get(f)
                .map(|m| (f, m.iter().map(|s| s.1).sum::<i128>()))
        })
        .collect();
    let by_size = |a: &(&str, i128), b: &(&str, i128)| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0));
    let mut code: Vec<_> = fam_sizes
        .iter()
        .copied()
        .filter(|fs| fs.0 != NONCODE_FAMILY)
        .collect();
    code.sort_by(by_size);
    let rest = fam_sizes
        .iter()
        .copied()
        .filter(|fs| fs.0 == NONCODE_FAMILY);
    let ordered: Vec<_> = code.into_iter().chain(rest).collect();
    let mut sorted = ordered.clone();
    sorted.sort_by(by_size);
    let fam_rects = squarify(
        &sorted,
        pad as f64,
        head_h as f64,
        (width - 2 * pad) as f64,
        map_h as f64,
    )?;

    let (head, gap) = (22.0, 2.0);
    for (fam, fsize) in ordered {
        let &(fx, fy, fw, fh) = fam_rects
            .get(fam)
            .ok_or_else(|| Crash(format!("KeyError: {fam:?}")))?;
        let (fill, backdrop) = colours(fam);
        let share = pct(fsize, total)?;
        body.push(format!(
            "<g><title>{}: {} ({}%)</title>",
            esc(fam),
            kib(fsize),
            f(share, 1)
        ));
        body.push(format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{backdrop}"/>"#,
            f(fx, 1),
            f(fy, 1),
            f(fw, 1),
            f(fh, 1)
        ));
        let mut label = format!("{fam}  {}  {}%", kib(fsize), f(share, 1));
        if text_w(&label, 13, true) > fw - 10.0 {
            label = fam.to_owned();
        }
        if text_w(&label, 13, true) <= fw - 10.0 && fh > head + 4.0 {
            body.push(format!(
                r##"<text x="{}" y="{}" font-size="13" font-weight="700" fill="#ffffff">{}</text>"##,
                f(fx + 6.0, 1),
                f(fy + 15.0, 1),
                esc(&label)
            ));
        }
        body.push("</g>".into());
        let leaves = &families[fam];
        let keyed: Vec<(&str, i128)> = leaves.iter().map(|(c, s)| (c.as_str(), *s)).collect();
        let rects = squarify(&keyed, fx + gap, fy + head, fw - 2.0 * gap, fh - head - gap)?;
        let n = leaves.len();
        for (rank, (crate_, size)) in leaves.iter().enumerate() {
            let Some(&(rx, ry, rw, rh)) = rects.get(crate_) else {
                continue; // a degenerate container: nothing to draw
            };
            // Darken slightly down the ranking; white text stays above 4.5:1.
            let col = shade(
                fill,
                1.0 - 0.25 * (rank as f64 / n.saturating_sub(1).max(1) as f64),
            );
            let share = pct(*size, total)?;
            body.push(format!(
                "<g><title>{}: {} ({} bytes, {}%)</title>",
                esc(crate_),
                kib(*size),
                thousands(*size),
                f(share, 2)
            ));
            body.push(format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{col}" stroke="{backdrop}" stroke-width="1.5"/>"#,
                f(rx + 0.75, 1),
                f(ry + 0.75, 1),
                f(py_max(rw - 1.5, 0.0), 1),
                f(py_max(rh - 1.5, 0.0), 1)
            ));
            let mut line1 = Some(crate_.clone());
            let mut line2 = format!("{} · {}%", kib(*size), f(share, 1));
            if rw > 24.0 && rh > 18.0 {
                if text_w(crate_, 12, true) > rw - 10.0 {
                    // `int(…)` truncates towards zero.
                    let fit = ((rw - 10.0) / (12.0 * 0.62)).trunc() as i64 - 1;
                    line1 = (fit >= 5).then(|| {
                        let cut: String = crate_.chars().take(fit as usize).collect();
                        cut + "…"
                    });
                }
                if let Some(line1) = line1.filter(|l| !l.is_empty()) {
                    body.push(format!(
                        r##"<text x="{}" y="{}" font-size="12" font-weight="700" fill="#ffffff">{}</text>"##,
                        f(rx + 5.0, 1),
                        f(ry + 16.0, 1),
                        esc(&line1)
                    ));
                    if rh > 36.0 {
                        if text_w(&line2, 11, false) > rw - 10.0 {
                            line2 = kib(*size); // drop the percentage
                        }
                        if text_w(&line2, 11, false) <= rw - 10.0 {
                            body.push(format!(
                                r##"<text x="{}" y="{}" font-size="11" fill="#ffffff">{}</text>"##,
                                f(rx + 5.0, 1),
                                f(ry + 31.0, 1),
                                esc(&line2)
                            ));
                        }
                    }
                }
            }
            body.push("</g>".into());
        }
    }

    let font = "-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif";
    let (bg, ink, muted) = ("#f6f8fa", "#1f2328", "#57606a");
    let sub1 = format!(
        "Total {} MB raw ({}) · {} MB gzip · rectangle area is proportional to bytes of code and data",
        f(meta.raw as f64 / 1e6, 2),
        kib(meta.raw),
        f(meta.gzip as f64 / 1e6, 2)
    );
    let sub2 = format!(
        "observation-client, wasm-release profile, {} · commit {} · {}",
        meta.stage, meta.commit, meta.date
    );
    let foot = "Attributed from symbol names after wasm-opt; inlined and merged code is credited \
                to the surviving symbol. Percentages are of code, data and module structure \
                (names excluded).";
    let mut svg = vec![
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-labelledby="t d" font-family="{}">"#,
            esc(font)
        ),
        r#"<title id="t">WebAssembly bundle size by crate (treemap)</title>"#.to_owned(),
        format!(
            r#"<desc id="d">{}</desc>"#,
            esc(&format!(
                "Treemap of the shipped WebAssembly module, one rectangle per dependency crate, \
                 grouped by family. {sub1}."
            ))
        ),
        format!(r#"<rect width="{width}" height="{height}" fill="{bg}"/>"#),
        format!(
            r#"<text x="{pad}" y="30" font-size="21" font-weight="700" fill="{ink}">{}</text>"#,
            esc("WebAssembly bundle by crate")
        ),
        format!(
            r#"<text x="{pad}" y="50" font-size="13" fill="{muted}">{}</text>"#,
            esc(&sub1)
        ),
        format!(
            r#"<text x="{pad}" y="68" font-size="12" fill="{muted}">{}</text>"#,
            esc(&sub2)
        ),
    ];
    svg.extend(body);
    svg.push(format!(
        r#"<text x="{pad}" y="{}" font-size="11" fill="{muted}">{}</text>"#,
        head_h + map_h + 22,
        esc(foot)
    ));
    svg.push("</svg>".into());
    svg.push(String::new());
    Ok(svg.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_floats_as_python() {
        assert_eq!(py_sum([0.1, 0.2, 0.3]), 0.6);
        assert_eq!(py_sum([1e100, 1.0, -1e100]), 1.0);
        assert_eq!(py_sum([]), 0.0);
    }

    #[test]
    fn compares_ints_and_floats_exactly() {
        let big = 1i128 << 53;
        assert!(int_ge_float(big + 1, big as f64));
        assert!(!int_ge_float(big, (big + 2) as f64));
        assert!(int_ge_float(3, 2.5) && !int_ge_float(2, 2.5) && int_ge_float(2, 2.0));
        assert!(!int_ge_float(0, f64::NAN) && int_ge_float(-5, f64::NEG_INFINITY));
    }
}
