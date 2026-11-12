//! The crate a wasm function's symbol belongs to, as scripts/wasm-size-treemap.py finds it: the
//! symbol's leading crate after demangling just enough of it (legacy `_ZN…E` and v0 `_R…`
//! manglings), or of the readable path wasm-bindgen and twiggy write
//! (`<bevy_ecs::a::B<T> as core::fmt::Debug>::fmt`), or a fixed crate for the unmangled names of
//! glue, compiler-rt and the allocator.
//!
//! [`crate_of_symbol`] gives what the script's `crate_of_symbol` gives, with these differences
//! (each one a symbol rustc does not emit):
//! - the port follows at most [`MAX_NESTING`] nested steps (a v0 path, type, generic argument
//!   or back-reference; a readable path's ` as `), each one a Python frame in the script, and at
//!   most [`V0_STEPS`] v0 steps in all. Past either it gives up: on a v0 symbol it takes the
//!   script's own fallback for a symbol it cannot decode, on a readable path it fails. The
//!   script crashes on deeper symbols (Python's recursion limit: from about 990 nested steps,
//!   which is 330 levels of generic arguments or 495 back-references in a chain, a
//!   back-reference that leads back to itself, see [`V0`]) and runs without end on symbols
//!   whose back-references fan out; between [`MAX_NESTING`] and its limit it still decodes, and
//!   gets a crate the port does not. Real symbols are far below both: the 51,334 v0 names of a
//!   native build of `observation-server` and its dependencies (axum, tokio, tower, hyper, serde,
//!   ron) with v0 mangling nest at most 22 steps and take at most 84 in all. The limit sits well
//!   below Python's so that it does not depend on the interpreter's version or on its caller's
//!   stack;
//! - where the script failed (a v0 constant that runs to the end of the symbol, where it loops
//!   forever; a number of more than 4,300 digits; a `$u…$` escape above 0x7FFFFFFF, which Python
//!   3.12 cannot pass to `chr`), the port fails too;
//! - it refuses a `_R` or `_ZN` symbol, or an unmangled one starting with `__` that is not
//!   glue or allocator, that is not ASCII (the script read them with Unicode digits and word
//!   characters).

use std::sync::LazyLock;

use regex::Regex;

use crate::core::py;

/// Nested steps after which the port gives up on a symbol: a v0 path, type, generic argument or
/// back-reference, or a readable path's ` as ` (one Python frame each in the script). In steps,
/// less than a sixth of where the script can reach Python's recursion limit (about 995 steps
/// on every recursive path); in levels of generic arguments (three steps each), 50 against 331.
pub const MAX_NESTING: usize = 150;

/// Steps of the v0 parser per symbol (calls, as counted for [`MAX_NESTING`]) after which the
/// port gives up, so that back-references fanning out cannot make it run without end.
pub const V0_STEPS: usize = 10_000;

/// Python 3.12's limit on the digits `int()` converts from decimal text.
const INT_MAX_STR_DIGITS: usize = 4300;

/// Why the port cannot attribute a symbol at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

type Found = Result<Option<String>, Refused>;

/// The crate of a symbol (mangled, or demangled by wasm-bindgen or twiggy), or `None`.
pub fn crate_of_symbol(sym: &str) -> Found {
    if (sym.starts_with("_R") || sym.starts_with("_ZN")) && !sym.is_ascii() {
        return Err(not_ascii());
    }
    if sym.starts_with("_R") {
        return v0_crate(sym.as_bytes());
    }
    if sym.starts_with("_ZN") {
        return crate_from_readable(&demangle_legacy(sym.as_bytes())?.unwrap_or_default());
    }
    if !sym.contains("::") {
        return unmangled(sym).map(|c| c.map(str::to_owned));
    }
    crate_from_readable(sym)
}

fn re(pattern: &str) -> Regex {
    // Constant patterns, each compiled by the tests: the panic cannot happen.
    Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"))
}

fn not_ascii() -> Refused {
    Refused("a mangled symbol that is not ASCII, which rustc does not emit".into())
}

/// Unmangled C-ABI names: wasm-bindgen glue, compiler-rt, the allocator.
fn unmangled(sym: &str) -> Result<Option<&'static str>, Refused> {
    static BUILTIN: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"^(memcpy|memmove|memset|memcmp|bcmp|fmodf?|sqrtf?|powf?|fmaf?|floorf?|ceilf?|truncf?|roundf?|rintf?|__\w+i[34]\b)",
        )
    });
    const DLMALLOC: [&str; 8] = [
        "malloc",
        "free",
        "calloc",
        "realloc",
        "dlmalloc",
        "dlfree",
        "dlrealloc",
        "dlcalloc",
    ];
    Ok(
        if ["__wbg_", "__wbindgen", "wasm_bindgen"]
            .iter()
            .any(|p| sym.starts_with(p))
        {
            Some("wasm_bindgen")
        } else if ["__rust_", "__rdl_", "__rg_"]
            .iter()
            .any(|p| sym.starts_with(p))
        {
            Some("alloc")
        } else if sym.starts_with("__") && !sym.is_ascii() {
            // The script's `__\w+i[34]\b` reads Unicode word characters as Python does.
            return Err(not_ascii());
        } else if BUILTIN.is_match(sym) {
            Some("compiler_builtins")
        } else if DLMALLOC.contains(&sym) {
            Some("dlmalloc")
        } else {
            None
        },
    )
}

/// `int()` of ASCII digits, saturated (every use only compares it with a length), or `None`
/// beyond Python's digit limit.
fn py_int(digits: &[u8]) -> Option<usize> {
    if digits.len() > INT_MAX_STR_DIGITS {
        return None;
    }
    Some(digits.iter().fold(0usize, |v, d| {
        v.saturating_mul(10).saturating_add(usize::from(d - b'0'))
    }))
}

fn digits_at(s: &[u8], at: usize) -> &[u8] {
    let rest = s.get(at..).unwrap_or_default();
    let n = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    &rest[..n]
}

fn slice(s: &[u8], from: usize, len: usize) -> &[u8] {
    let from = from.min(s.len());
    &s[from..from.saturating_add(len).min(s.len())]
}

fn too_long() -> Refused {
    Refused(format!(
        "a number of more than {INT_MAX_STR_DIGITS} digits, which the script cannot read"
    ))
}

/// `_ZN…E` as a readable path without the hash, or `None`.
fn demangle_legacy(sym: &[u8]) -> Result<Option<String>, Refused> {
    static HASH: LazyLock<Regex> = LazyLock::new(|| re(r"^h[0-9a-f]{16}$"));
    let mut pos = 3;
    let mut comps = Vec::new();
    while sym.get(pos).is_some_and(u8::is_ascii_digit) {
        let digits = digits_at(sym, pos);
        let n = py_int(digits).ok_or_else(too_long)?;
        pos += digits.len();
        // ASCII (checked by the caller), so any slice is a str.
        let comp = String::from_utf8_lossy(slice(sym, pos, n));
        comps.push(unescape_legacy(&comp)?);
        pos = pos.saturating_add(n);
    }
    if comps.last().is_some_and(|c| HASH.is_match(c)) {
        comps.pop();
    }
    Ok((!comps.is_empty()).then(|| comps.join("::")))
}

fn unescape_legacy(comp: &str) -> Result<String, Refused> {
    static ESCAPE: LazyLock<Regex> = LazyLock::new(|| re(r"\$([A-Za-z0-9]+)\$"));
    let comp = comp.replace("..", "::");
    let mut out = String::with_capacity(comp.len());
    let mut last = 0;
    for m in ESCAPE.captures_iter(&comp) {
        let (whole, tok) = (&m[0], &m[1]);
        out.push_str(&comp[last..m.get(0).map_or(0, |g| g.start())]);
        last = m.get(0).map_or(0, |g| g.end());
        let fixed = match tok {
            "SP" => "@",
            "BP" => "*",
            "RF" => "&",
            "LT" => "<",
            "GT" => ">",
            "LP" => "(",
            "RP" => ")",
            "C" => ",",
            _ => "",
        };
        if !fixed.is_empty() {
            out.push_str(fixed);
        } else if let Some(hex) = tok.strip_prefix('u') {
            match hex_char(hex)? {
                Some(c) => out.push(c),
                None => out.push_str(whole),
            }
        } else {
            out.push_str(whole);
        }
    }
    out.push_str(&comp[last..]);
    Ok(out)
}

/// `chr(int(hex, 16))`: `None` where that raises ValueError (the script keeps the escape), a
/// refusal above 0x7FFFFFFF (OverflowError on Python 3.12, which the script does not catch). A
/// surrogate becomes U+FFFD: only ASCII in a path changes which crate it names.
fn hex_char(hex: &str) -> Result<Option<char>, Refused> {
    let digits = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let digits = digits.trim_start_matches('0');
    let value = if digits.len() > 8 {
        u64::MAX
    } else {
        u64::from_str_radix(if digits.is_empty() { "0" } else { digits }, 16).unwrap_or(u64::MAX)
    };
    if value > 0x7FFF_FFFF {
        return Err(Refused(
            "a `$u…$` escape above 0x7FFFFFFF, on which the script crashes".into(),
        ));
    }
    if value > 0x10_FFFF {
        return Ok(None);
    }
    let value = u32::try_from(value).unwrap_or(0);
    Ok(Some(char::from_u32(value).unwrap_or('\u{fffd}')))
}

/// The leading crate of a readable path, crediting the trait's crate for an impl on a primitive
/// or a tuple (`<f32 as bevy_reflect::Reflect>::m`), and `core` for `<str>::trim` and the like.
fn crate_from_readable(text: &str) -> Found {
    static HASH: LazyLock<Regex> = LazyLock::new(|| re(r"\[[0-9a-f]{1,16}\]"));
    static LEAD: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(<|&|\*const |\*mut |mut |dyn |\(|\[|'[a-z_]+ )"));
    const PRIMS: [&str; 26] = [
        "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
        "f32", "f64", "bool", "char", "str", "dyn", "unsafe", "extern", "fn", "mut", "const",
        "impl", "Self", "T",
    ];
    let mut text = text;
    for _ in 0..MAX_NESTING {
        let mut t = HASH
            .replace_all(text.trim_matches(py::is_space), "")
            .into_owned();
        while let Some(m) = LEAD.find(&t) {
            if m.is_empty() {
                break;
            }
            t.drain(..m.end());
        }
        // `[A-Za-z_][A-Za-z0-9_]*(?=::)`: only the longest such run can be followed by `::`.
        let ident_len = if t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            t.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(t.len())
        } else {
            0
        };
        let ident = &t[..ident_len];
        if ident_len > 0 && t[ident_len..].starts_with("::") && !PRIMS.contains(&ident) {
            return Ok(Some(ident.to_owned()));
        }
        if let Some((head, trait_)) = text.split_once(" as ")
            && head.starts_with('<')
        {
            text = trait_;
            continue;
        }
        let prim_impl = PRIMS
            .iter()
            .map(|p| format!("{p}>"))
            .chain(["T]>".to_owned()])
            .any(|p| t.starts_with(&p));
        return Ok(prim_impl.then(|| "core".to_owned()));
    }
    Err(Refused(format!(
        "a readable path nested more than {MAX_NESTING} times (` as `), more than the port follows"
    )))
}

/// The crate owning a v0 symbol (`_R…`), or `None` if it cannot be decoded.
fn v0_crate(sym: &[u8]) -> Found {
    static CRATE_ROOT: LazyLock<Regex> = LazyLock::new(|| re(r"C(?:s[0-9a-zA-Z]*_)?([0-9]+)"));
    static CRATE_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z_][A-Za-z0-9_]*$"));
    let mut body = &sym[2..];
    while body.first().is_some_and(u8::is_ascii_digit) {
        body = &body[1..];
    }
    let mut parser = V0 {
        s: body,
        i: 0,
        depth: 0,
        steps: 0,
    };
    match parser.path() {
        Ok(Some(c)) if !c.is_empty() => return Ok(Some(String::from_utf8_lossy(&c).into_owned())),
        Ok(_) | Err(V0Error::Invalid | V0Error::GaveUp) => {}
        Err(V0Error::Hang) => {
            return Err(Refused(
                "a v0 constant that runs to the end of the symbol, on which the script loops \
                 forever"
                    .into(),
            ));
        }
    }
    // The first crate root `C[s<disambiguator>_]<len><name>`. ASCII, checked by the caller.
    let text = String::from_utf8_lossy(body);
    for m in CRATE_ROOT.captures_iter(&text) {
        let (Some(whole), Some(len)) = (m.get(0), m.get(1)) else {
            continue;
        };
        let n = py_int(len.as_str().as_bytes()).ok_or_else(too_long)?;
        let name = slice(body, whole.end(), n);
        if name.len() == n {
            let name = String::from_utf8_lossy(name);
            if CRATE_NAME.is_match(&name) {
                return Ok(Some(name.into_owned()));
            }
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum V0Error {
    /// Python's ValueError or IndexError: the script falls back.
    Invalid,
    /// Deeper than [`MAX_NESTING`] or longer than [`V0_STEPS`]: the port takes the fallback.
    GaveUp,
    /// The script loops forever.
    Hang,
}

type Crate = Result<Option<Vec<u8>>, V0Error>;

/// `first or t`.
fn or(first: Option<Vec<u8>>, t: Option<Vec<u8>>) -> Option<Vec<u8>> {
    if first.as_ref().is_some_and(|f| !f.is_empty()) {
        first
    } else {
        t
    }
}

/// The script's `_V0`: just enough of the v0 grammar to find the crate a symbol belongs to,
/// with its quirks. Its base-62 numbers lack the grammar's `+ 1` for a non-empty number, so a
/// back-reference points one byte early; on some real symbols it leads back to itself, which
/// the script followed until Python's recursion limit (and crashed), and the port until
/// [`MAX_NESTING`] (and falls back).
struct V0<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
    steps: usize,
}

const B62: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

impl V0<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let c = self.peek();
        self.i = self.i.saturating_add(1);
        c
    }

    fn skip(&mut self) {
        self.i = self.i.saturating_add(1);
    }

    /// Runs one nested step (a frame of the script's parser), within the limits.
    fn nested(&mut self, step: impl FnOnce(&mut Self) -> Crate) -> Crate {
        self.steps += 1;
        if self.depth >= MAX_NESTING || self.steps > V0_STEPS {
            return Err(V0Error::GaveUp);
        }
        self.depth += 1;
        let crate_ = step(self);
        self.depth -= 1;
        crate_
    }

    /// A base-62 number terminated by `_` (saturated: it is only compared with a position).
    fn base62(&mut self) -> Result<usize, V0Error> {
        let mut v = 0usize;
        while self.peek() != Some(b'_') {
            let c = self.take().ok_or(V0Error::Invalid)?;
            let d = B62.iter().position(|&b| b == c).ok_or(V0Error::Invalid)?;
            v = v.saturating_mul(62).saturating_add(d);
        }
        self.skip();
        Ok(v)
    }

    fn opt_disambig(&mut self) -> Result<(), V0Error> {
        if self.peek() == Some(b's') {
            self.skip();
            self.base62()?;
        }
        Ok(())
    }

    fn ident(&mut self) -> Result<Vec<u8>, V0Error> {
        if self.peek() == Some(b'u') {
            self.skip();
        }
        let digits = digits_at(self.s, self.i);
        if digits.is_empty() {
            return Err(V0Error::Invalid);
        }
        let n = py_int(digits).ok_or(V0Error::Invalid)?;
        self.i += digits.len();
        if self.peek() == Some(b'_') {
            self.skip();
        }
        let out = slice(self.s, self.i, n).to_vec();
        self.i = self.i.saturating_add(n);
        Ok(out)
    }

    /// The leading crate of a path.
    fn path(&mut self) -> Crate {
        self.nested(Self::path_inner)
    }

    fn path_inner(&mut self) -> Crate {
        match self.take() {
            Some(b'C') => {
                self.opt_disambig()?;
                Ok(Some(self.ident()?))
            }
            Some(b'N') => {
                self.skip(); // namespace
                let crate_ = self.path()?;
                self.opt_disambig()?;
                self.ident()?;
                Ok(crate_)
            }
            Some(b'M') => {
                self.opt_disambig()?;
                let crate_ = self.path()?;
                self.type_()?;
                Ok(crate_)
            }
            Some(b'X') => {
                self.opt_disambig()?;
                self.path()?; // impl-path
                let crate_ = self.type_()?;
                self.path()?; // trait
                Ok(crate_)
            }
            Some(b'Y') => {
                let crate_ = self.type_()?;
                self.path()?;
                Ok(crate_)
            }
            Some(b'I') => {
                let crate_ = self.path()?;
                while self.peek() != Some(b'E') {
                    self.generic_arg()?;
                }
                self.skip();
                Ok(crate_)
            }
            Some(b'B') => self.backref(Self::path),
            _ => Err(V0Error::Invalid),
        }
    }

    /// Parses the construct at an earlier offset, then resumes here.
    fn backref(&mut self, parse: fn(&mut Self) -> Crate) -> Crate {
        self.nested(|p| p.backref_inner(parse))
    }

    fn backref_inner(&mut self, parse: fn(&mut Self) -> Crate) -> Crate {
        let target = self.base62()?;
        if target >= self.i {
            return Err(V0Error::Invalid);
        }
        let saved = std::mem::replace(&mut self.i, target);
        let crate_ = parse(self);
        self.i = saved;
        crate_
    }

    fn generic_arg(&mut self) -> Result<(), V0Error> {
        self.nested(Self::generic_arg_inner).map(drop)
    }

    fn generic_arg_inner(&mut self) -> Crate {
        match self.peek() {
            Some(b'L') => {
                self.skip();
                self.base62()?;
            }
            Some(b'K') => {
                self.skip();
                self.const_()?;
            }
            _ => {
                self.type_()?;
            }
        }
        Ok(None)
    }

    fn const_(&mut self) -> Result<(), V0Error> {
        match self.peek() {
            Some(b'p') => self.skip(),
            Some(b'B') => {
                self.skip();
                self.base62()?;
            }
            c => {
                if c.is_some_and(|c| c.is_ascii_lowercase()) {
                    self.skip();
                }
                if self.peek() == Some(b'n') {
                    self.skip();
                }
                // The script steps until it sees `_`, forever if there is none.
                let rest = self.s.get(self.i..).unwrap_or_default();
                let n = rest.iter().position(|&c| c == b'_').ok_or(V0Error::Hang)?;
                self.i += n + 1;
            }
        }
        Ok(())
    }

    /// The crate of the type if it is a path, else `None`.
    fn type_(&mut self) -> Crate {
        self.nested(Self::type_inner)
    }

    fn type_inner(&mut self) -> Crate {
        let c = self.peek();
        // `"" in "CNIMXY"` is true in Python: at the end, the path fails.
        if c.is_none_or(|c| b"CNIMXY".contains(&c)) {
            return self.path();
        }
        if c == Some(b'B') {
            self.skip();
            return self.backref(Self::type_);
        }
        self.skip();
        let Some(c) = c else {
            return Err(V0Error::Invalid);
        };
        if c.is_ascii_lowercase() {
            return Ok(None); // a basic type
        }
        match c {
            b'R' | b'Q' => {
                if self.peek() == Some(b'L') {
                    self.skip();
                    self.base62()?;
                }
                self.type_()
            }
            b'P' | b'O' | b'S' => self.type_(),
            b'A' => {
                let crate_ = self.type_()?;
                self.const_()?;
                Ok(crate_)
            }
            b'T' => {
                let mut first = None;
                while self.peek() != Some(b'E') {
                    let t = self.type_()?;
                    first = or(first, t);
                }
                self.skip();
                Ok(first)
            }
            b'F' => {
                if self.peek() == Some(b'G') {
                    self.skip();
                    self.base62()?;
                }
                if self.peek() == Some(b'U') {
                    self.skip();
                }
                if self.peek() == Some(b'K') {
                    self.skip();
                    if self.peek() == Some(b'C') {
                        self.skip();
                    } else {
                        self.ident()?;
                    }
                }
                while self.peek() != Some(b'E') {
                    self.type_()?;
                }
                self.skip();
                self.type_()?;
                Ok(None)
            }
            b'D' => {
                let mut first = None;
                if self.peek() == Some(b'G') {
                    self.skip();
                    self.base62()?;
                }
                while self.peek() != Some(b'E') {
                    let t = self.path()?;
                    first = or(first, t);
                    while self.peek() == Some(b'p') {
                        self.skip();
                        self.ident()?;
                        self.type_()?;
                    }
                }
                self.skip();
                if self.peek() == Some(b'L') {
                    self.skip();
                    self.base62()?;
                }
                Ok(first)
            }
            _ => Err(V0Error::Invalid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(sym: &str) -> Option<String> {
        crate_of_symbol(sym).unwrap_or_else(|Refused(e)| panic!("{sym}: {e}"))
    }

    #[test]
    fn finds_the_crate_of_each_kind_of_name() {
        for (sym, want) in [
            (
                "<bevy_ecs::a::B<T> as core::fmt::Debug>::fmt",
                Some("bevy_ecs"),
            ),
            ("<f32 as bevy_reflect::Reflect>::m", Some("bevy_reflect")),
            ("<str>::trim", Some("core")),
            ("<[T]>::sort", Some("core")),
            (
                "alloc[42d2b33bd937efdb]::alloc::handle_alloc_error",
                Some("alloc"),
            ),
            ("_ZN4core3fmt5write17h0123456789abcdefE", Some("core")),
            // The script keeps rustc's `_` before a leading `$`, so this impl names no crate.
            (
                "_ZN40_$LT$f32$u20$as$u20$serde..Serialize$GT$9serialize17h0123456789abcdefE",
                None,
            ),
            ("_RNvCs5JHfeWeQK0x_5alloc3foo", Some("alloc")),
            ("__wbg_log_1", Some("wasm_bindgen")),
            ("__rust_alloc", Some("alloc")),
            ("memcpy", Some("compiler_builtins")),
            ("__multi3", Some("compiler_builtins")),
            ("dlfree", Some("dlmalloc")),
            ("main", None),
        ] {
            assert_eq!(found(sym).as_deref(), want, "{sym}");
        }
    }

    #[test]
    fn falls_back_where_a_back_reference_leads_back_to_itself() {
        // `B_` points at offset 0 (the script's base-62 lacks the `+ 1`): `I` then `B_` again.
        assert_eq!(found("_RINvB_C3foo").as_deref(), Some("foo"));
    }

    #[test]
    fn falls_back_past_the_nesting_limit() {
        // Generic arguments nested `d` levels in an impl's self type: `bar`, or the fallback `foo`.
        let sym = |d| format!("_RXC3foo{}l{}C3baz", "INtC3bar1S".repeat(d), "E".repeat(d));
        assert_eq!(found(&sym(49)).as_deref(), Some("bar"));
        assert_eq!(found(&sym(50)).as_deref(), Some("foo"));
    }

    #[test]
    fn fails_where_the_script_fails() {
        for sym in [
            "_RIC3fooKj",         // loops forever
            "_ZN12_$u80000000$E", // OverflowError on Python 3.12
            "_ZN3fé",             // not ASCII
            "__mul\u{e9}i3",      // not ASCII
        ] {
            assert!(crate_of_symbol(sym).is_err(), "{sym}");
        }
        let deep = format!("{}u8 as a::b>::c", "<u8 as ".repeat(MAX_NESTING));
        assert!(crate_of_symbol(&deep).is_err());
        let long = format!("_ZN{}a", "9".repeat(INT_MAX_STR_DIGITS + 1));
        assert!(crate_of_symbol(&long).is_err());
    }
}
