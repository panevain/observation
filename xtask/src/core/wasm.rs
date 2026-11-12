//! A WebAssembly module cut into what scripts/wasm-size-treemap.py attributes: each function
//! body (with the name its `name` section gives it), each data segment, and every other part of
//! the module, as the script reads them, with the same tolerance: the parser trusts the declared
//! counts and sizes and reads on past a section's end, and only running off the end of the file
//! is an error (an IndexError for the script, which crashed). One difference: a LEB128 number
//! above 2^64 - 1 is refused (the script read it as a Python integer).

use std::collections::HashMap;

/// The section names the script prints; any other id is `section N`.
fn section_name(id: u8) -> Option<&'static str> {
    Some(match id {
        1 => "types",
        2 => "imports",
        3 => "function table",
        4 => "tables",
        5 => "memories",
        6 => "globals",
        7 => "exports",
        8 => "start",
        9 => "elements",
        12 => "data count",
        13 => "tags",
        _ => return None,
    })
}

/// What a part of the module is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A function body and its name, if the `name` section gives one.
    Func(Option<String>),
    /// A data segment.
    Data,
    /// A section header or a whole section other than code and data.
    Meta(String),
}

/// The parts of the module with their sizes in bytes (declared, so a truncated module can give
/// sizes beyond its length), and the module without its custom sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    pub items: Vec<(Item, u128)>,
    pub shipped: Vec<u8>,
    /// Whether a `name` section was found (the script warns when not).
    pub has_names: bool,
}

/// Why the module cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unreadable {
    /// The first four bytes are not `\0asm` (the script's own message).
    NotWasm,
    /// A read past the end of the file, at this offset.
    Truncated(u128),
    /// A LEB128 number above 2^64 - 1, at this offset.
    TooLarge(u128),
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWasm => f.write_str("not a wasm module"),
            Self::Truncated(at) => write!(f, "the module ends before offset {at}"),
            Self::TooLarge(at) => write!(f, "a number above 2^64 - 1 at offset {at}"),
        }
    }
}

type Read<T> = Result<T, Unreadable>;

struct Buf<'a>(&'a [u8]);

impl Buf<'_> {
    fn len(&self) -> u128 {
        self.0.len() as u128
    }

    fn at(&self, pos: u128) -> Read<u8> {
        usize::try_from(pos)
            .ok()
            .and_then(|p| self.0.get(p).copied())
            .ok_or(Unreadable::Truncated(pos))
    }

    /// `buf[from:to]`, clamped as a Python slice.
    fn slice(&self, from: u128, to: u128) -> &[u8] {
        let len = self.len();
        let (from, to) = (from.min(len), to.min(len));
        // Both are at most the length, a usize.
        let (from, to) = (from as usize, to as usize);
        self.0.get(from..to.max(from)).unwrap_or_default()
    }

    /// An unsigned LEB128 number at `pos`, and the offset after it.
    fn leb(&self, mut pos: u128) -> Read<(u128, u128)> {
        let start = pos;
        let (mut result, mut shift) = (0u128, 0u32);
        loop {
            let byte = self.at(pos)?;
            pos += 1;
            let bits = u128::from(byte & 0x7F);
            if bits != 0 {
                if shift >= 64 || (bits << shift) >> 64 != 0 {
                    return Err(Unreadable::TooLarge(start));
                }
                result |= bits << shift;
            }
            if byte & 0x80 == 0 {
                return Ok((result, pos));
            }
            shift = shift.saturating_add(7);
        }
    }

    fn sections(&self) -> Read<Vec<(u8, u128, u128, u128)>> {
        if self.0.get(..4) != Some(b"\0asm") {
            return Err(Unreadable::NotWasm);
        }
        let mut pos = 8;
        let mut sections = Vec::new();
        while pos < self.len() {
            let id = self.at(pos)?;
            let (size, body) = self.leb(pos + 1)?;
            sections.push((id, pos, body, body + size));
            pos = body + size;
        }
        Ok(sections)
    }

    /// The function names of a `name` section (the last name of an index wins).
    fn names(&self, start: u128, end: u128) -> Read<HashMap<u128, String>> {
        let (n, mut pos) = self.leb(start)?;
        pos += n; // the custom section's own name
        let mut names = HashMap::new();
        while pos < end {
            let sub = self.at(pos)?;
            let (size, body) = self.leb(pos + 1)?;
            if sub == 1 {
                let (count, mut p) = self.leb(body)?;
                for _ in 0..count {
                    let (idx, q) = self.leb(p)?;
                    let (n, q) = self.leb(q)?;
                    let name = String::from_utf8_lossy(self.slice(q, q + n)).into_owned();
                    names.insert(idx, name);
                    p = q + n;
                }
            }
            pos = body + size;
        }
        Ok(names)
    }

    fn custom_name_is(&self, start: u128, want: &[u8]) -> Read<bool> {
        let (n, p) = self.leb(start)?;
        Ok(self.slice(p, p + n) == want)
    }

    fn imported_funcs(&self, start: u128) -> Read<u128> {
        let (count, mut pos) = self.leb(start)?;
        let mut funcs = 0;
        for _ in 0..count {
            for _ in 0..2 {
                // module, field
                let (n, p) = self.leb(pos)?;
                pos = p + n;
            }
            let kind = self.at(pos)?;
            pos += 1;
            match kind {
                0 => {
                    funcs += 1;
                    pos = self.leb(pos)?.1;
                }
                1 | 2 => {
                    if kind == 1 {
                        pos += 1; // reftype
                    }
                    let flag = self.at(pos)?;
                    pos = self.leb(pos + 1)?.1;
                    if flag & 1 != 0 {
                        pos = self.leb(pos)?.1;
                    }
                }
                3 => pos += 2,                   // valtype, mutability
                _ => pos = self.leb(pos + 1)?.1, // tag
            }
        }
        Ok(funcs)
    }
}

/// The script's `analyse_wasm`.
pub fn analyse(bytes: &[u8]) -> Read<Analysis> {
    let buf = Buf(bytes);
    let sections = buf.sections()?;
    let mut names = HashMap::new();
    let mut imported = 0;
    let mut has_names = false;
    let mut shipped = bytes.get(..8).unwrap_or(bytes).to_vec();
    for &(id, head, body, end) in &sections {
        if id == 0 {
            if buf.custom_name_is(body, b"name")? {
                names = buf.names(body, end)?;
                has_names = true;
            }
            continue;
        }
        shipped.extend_from_slice(buf.slice(head, end));
        if id == 2 {
            imported = buf.imported_funcs(body)?;
        }
    }
    let mut items = Vec::new();
    for &(id, head, body, end) in &sections {
        match id {
            0 => {}
            10 => {
                let (count, mut pos) = buf.leb(body)?;
                items.push((Item::Meta("code section header".into()), pos - head));
                for i in 0..count {
                    let (size, p) = buf.leb(pos)?;
                    let name = names.get(&(imported + i)).cloned();
                    items.push((Item::Func(name), (p - pos) + size));
                    pos = p + size;
                }
            }
            11 => {
                let (count, mut pos) = buf.leb(body)?;
                items.push((Item::Meta("data section header".into()), pos - head));
                for _ in 0..count {
                    let start = pos;
                    let (flag, p) = buf.leb(pos)?;
                    pos = p;
                    if flag == 2 {
                        pos = buf.leb(pos)?.1;
                    }
                    if flag == 0 || flag == 2 {
                        // An active segment: skip its offset expression up to `end`.
                        while buf.at(pos)? != 0x0B {
                            pos += 1;
                        }
                        pos += 1;
                    }
                    let (size, p) = buf.leb(pos)?;
                    pos = p + size;
                    items.push((Item::Data, pos - start));
                }
            }
            _ => {
                let name = section_name(id).map_or_else(|| format!("section {id}"), str::to_owned);
                items.push((Item::Meta(name), end - head));
            }
        }
    }
    Ok(Analysis {
        items,
        shipped,
        has_names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_as_the_script_except_above_64_bits() {
        let buf = Buf(&[0x80, 0x80, 0x80, 0x00, 0xE5, 0x8E, 0x26]);
        assert_eq!(buf.leb(0), Ok((0, 4)));
        assert_eq!(buf.leb(4), Ok((624_485, 7)));
        let max = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        assert_eq!(Buf(&max).leb(0), Ok((u128::from(u64::MAX), 10)));
        let over = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02];
        assert_eq!(Buf(&over).leb(0), Err(Unreadable::TooLarge(0)));
        assert_eq!(Buf(&[0x80]).leb(0), Err(Unreadable::Truncated(1)));
    }

    #[test]
    fn a_module_without_sections_has_no_items() {
        assert_eq!(
            analyse(b"\0asm"),
            Ok(Analysis {
                items: vec![],
                shipped: b"\0asm".to_vec(),
                has_names: false,
            })
        );
        assert_eq!(analyse(b"\0wasm\x01\0\0"), Err(Unreadable::NotWasm));
    }
}
