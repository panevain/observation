#!/usr/bin/env python3
"""Render a size-per-crate treemap (SVG) of a WebAssembly module.

Stdlib only, deterministic (same input and arguments give byte-identical
output). Run it in isolated mode:

    python3 -I scripts/wasm-size-treemap.py --wasm NAMED.wasm --out treemap.svg \\
        [--shipped-raw BYTES --shipped-gzip BYTES] [--commit HASH] [--date YYYY-MM-DD] \\
        [--table 15]

    python3 -I scripts/wasm-size-treemap.py --twiggy-csv top.csv --out treemap.svg

Inputs (exactly one):
  --wasm FILE         A module that still has its `name` section (build it with
                      CARGO_PROFILE_<PROFILE>_STRIP=false and, if you post-process
                      it, keep names, e.g. `wasm-opt --debuginfo`). The script
                      parses the binary directly: every function body is
                      attributed to a crate by demangling its symbol (legacy
                      `_ZN` and v0 `_R` manglings); data segments and the
                      remaining sections are separate items.
  --twiggy-csv FILE   Output of `twiggy top -n 1000000 --format csv`
                      (columns: Shallow Bytes, Shallow Percent, Item).

Sizes count what is shipped: all sections except custom ones (`name`, debug
info, `producers`, ...). Unless --shipped-raw/--shipped-gzip are given, the raw
and gzip(-9) figures in the title are computed from the module with its custom
sections removed (only possible with --wasm).

Attribution is by symbol name, after the optimiser has inlined and merged
code, so a crate's figure is "bytes of functions whose symbol belongs to the
crate", not "what you save by removing the dependency". Generic code from
core/alloc/std stays in the `core`/`alloc`/`std` groups even when instantiated
for another crate's types.
"""

import argparse
import re
import sys
import zlib

# --------------------------------------------------------------------------
# Wasm binary parsing
# --------------------------------------------------------------------------


def leb_u(buf, pos):
    result = shift = 0
    while True:
        byte = buf[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, pos
        shift += 7


SECTION_NAMES = {
    1: "types", 2: "imports", 3: "function table", 4: "tables", 5: "memories",
    6: "globals", 7: "exports", 8: "start", 9: "elements", 12: "data count",
    13: "tags",
}


def parse_sections(buf):
    if buf[:4] != b"\0asm":
        raise SystemExit("not a wasm module")
    pos = 8
    sections = []
    while pos < len(buf):
        sid = buf[pos]
        size, body = leb_u(buf, pos + 1)
        sections.append((sid, pos, body, body + size))
        pos = body + size
    return sections


def read_name_section(buf, start, end):
    """Return {function index: mangled name} from a `name` custom section."""
    n, pos = leb_u(buf, start)
    pos += n  # skip the custom-section name ("name")
    names = {}
    while pos < end:
        sub_id = buf[pos]
        size, body = leb_u(buf, pos + 1)
        if sub_id == 1:
            count, p = leb_u(buf, body)
            for _ in range(count):
                idx, p = leb_u(buf, p)
                n, p = leb_u(buf, p)
                names[idx] = buf[p:p + n].decode("utf-8", "replace")
                p += n
        pos = body + size
    return names


def custom_section_name(buf, start):
    n, p = leb_u(buf, start)
    return buf[p:p + n].decode("utf-8", "replace")


def count_imported_funcs(buf, start):
    count, pos = leb_u(buf, start)
    funcs = 0
    for _ in range(count):
        for _ in range(2):  # module, field
            n, pos = leb_u(buf, pos)
            pos += n
        kind = buf[pos]
        pos += 1
        if kind == 0:
            funcs += 1
            _, pos = leb_u(buf, pos)
        elif kind == 1:  # table: reftype, limits
            pos += 1
            flag = buf[pos]
            pos += 1
            _, pos = leb_u(buf, pos)
            if flag & 1:
                _, pos = leb_u(buf, pos)
        elif kind == 2:  # memory
            flag = buf[pos]
            pos += 1
            _, pos = leb_u(buf, pos)
            if flag & 1:
                _, pos = leb_u(buf, pos)
        elif kind == 3:  # global: valtype, mutability
            pos += 2
        else:  # tag
            pos += 1
            _, pos = leb_u(buf, pos)
    return funcs


def analyse_wasm(buf):
    """Return (items, shipped_buf).

    items: list of (kind, label, size). kind is 'func', 'data' or 'meta'.
    shipped_buf: module bytes with every custom section removed.
    """
    sections = parse_sections(buf)
    names = {}
    imported = 0
    items = []
    kept = [buf[:8]]
    has_names = False
    for sid, head, body, end in sections:
        if sid == 0:
            if custom_section_name(buf, body) == "name":
                names = read_name_section(buf, body, end)
                has_names = True
            continue
        kept.append(buf[head:end])
        if sid == 2:
            imported = count_imported_funcs(buf, body)
    if not has_names:
        print("warning: no `name` section; functions cannot be attributed",
              file=sys.stderr)
    for sid, head, body, end in sections:
        if sid == 0:
            continue
        if sid == 10:
            count, pos = leb_u(buf, body)
            items.append(("meta", "code section header", pos - head))
            for i in range(count):
                size, p = leb_u(buf, pos)
                items.append(("func", names.get(imported + i), (p - pos) + size))
                pos = p + size
        elif sid == 11:
            count, pos = leb_u(buf, body)
            items.append(("meta", "data section header", pos - head))
            for _ in range(count):
                start = pos
                flag, pos = leb_u(buf, pos)
                if flag == 2:
                    _, pos = leb_u(buf, pos)
                if flag in (0, 2):  # active segment: offset init expression
                    while buf[pos] != 0x0B:
                        pos += 1
                    pos += 1
                size, pos = leb_u(buf, pos)
                pos += size
                items.append(("data", "data", pos - start))
        else:
            items.append(("meta", SECTION_NAMES.get(sid, "section %d" % sid),
                          end - head))
    return items, b"".join(kept)


def parse_twiggy_csv(text):
    import csv
    items = []
    reader = csv.reader(text.splitlines())
    header = next(reader)
    col = {h.strip().lower(): i for i, h in enumerate(header)}
    size_col = col.get("shallow bytes", 0)
    item_col = col.get("item", len(header) - 1)
    for row in reader:
        if len(row) <= max(size_col, item_col):
            continue
        item, size = row[item_col], int(row[size_col])
        if item.startswith("data["):
            items.append(("data", "data", size))
        elif re.match(r"^(custom section|\.debug|\"name\")", item):
            continue
        elif re.match(r"^(type|table|elem|export|import|global|memory|code|"
                      r"function|\.\.\. and)", item) or item.startswith("wasm "):
            items.append(("meta", item.split("[")[0], size))
        else:
            items.append(("func", item, size))
    return items


# --------------------------------------------------------------------------
# Symbol demangling (only as far as needed to find the owning crate)
# --------------------------------------------------------------------------

_ESC = {
    "SP": "@", "BP": "*", "RF": "&", "LT": "<", "GT": ">", "LP": "(",
    "RP": ")", "C": ",",
}


def _unescape_legacy(comp):
    comp = comp.replace("..", "::")

    def rep(m):
        tok = m.group(1)
        if tok.startswith("u"):
            try:
                return chr(int(tok[1:], 16))
            except ValueError:
                return m.group(0)
        return _ESC.get(tok, m.group(0))

    return re.sub(r"\$([A-Za-z0-9]+)\$", rep, comp)


def demangle_legacy(sym):
    """`_ZN...E` -> readable path string without the hash, or None."""
    if not sym.startswith("_ZN"):
        return None
    pos = 3
    comps = []
    while pos < len(sym) and sym[pos].isdigit():
        m = re.match(r"\d+", sym[pos:])
        n = int(m.group(0))
        pos += len(m.group(0))
        comps.append(_unescape_legacy(sym[pos:pos + n]))
        pos += n
    if comps and re.fullmatch(r"h[0-9a-f]{16}", comps[-1]):
        comps.pop()
    return "::".join(comps) if comps else None


_B62 = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
_DIGITS = re.compile(r"\d+")


class _V0:
    """Just enough of the v0 grammar to find the crate a symbol belongs to."""

    def __init__(self, s):
        self.s = s
        self.i = 0

    def peek(self):
        return self.s[self.i] if self.i < len(self.s) else ""

    def take(self):
        c = self.peek()
        self.i += 1
        return c

    def base62(self):  # terminated by '_'
        v = 0
        while self.peek() != "_":
            c = self.take()
            if c == "":
                raise ValueError
            v = v * 62 + _B62.index(c)
        self.i += 1
        return v

    def opt_disambig(self):
        if self.peek() == "s":
            self.i += 1
            self.base62()

    def ident(self):
        if self.peek() == "u":
            self.i += 1
        m = _DIGITS.match(self.s, self.i)
        if not m:
            raise ValueError
        n = int(m.group(0))
        self.i = m.end()
        if self.peek() == "_":
            self.i += 1
        out = self.s[self.i:self.i + n]
        self.i += n
        return out

    def path(self):
        """Return the leading crate of a path (None for back-references)."""
        c = self.take()
        if c == "C":
            self.opt_disambig()
            return self.ident()
        if c == "N":
            self.take()  # namespace
            crate = self.path()
            self.opt_disambig()
            self.ident()
            return crate
        if c == "M":
            self.opt_disambig()
            crate = self.path()
            self.type_()
            return crate
        if c == "X":
            self.opt_disambig()
            self.path()  # impl-path
            crate = self.type_()
            self.path()  # trait
            return crate
        if c == "Y":
            crate = self.type_()
            self.path()
            return crate
        if c == "I":
            crate = self.path()
            while self.peek() != "E":
                self.generic_arg()
            self.i += 1
            return crate
        if c == "B":
            return self.backref(self.path)
        raise ValueError

    def backref(self, parse):
        """Parse the construct at an earlier offset, then resume here."""
        target = self.base62()
        if target >= self.i:
            raise ValueError
        saved, self.i = self.i, target
        try:
            return parse()
        finally:
            self.i = saved

    def generic_arg(self):
        c = self.peek()
        if c == "L":
            self.i += 1
            self.base62()
        elif c == "K":
            self.i += 1
            self.const_()
        else:
            self.type_()

    def const_(self):
        c = self.peek()
        if c == "p":
            self.i += 1
        elif c == "B":
            self.i += 1
            self.base62()
        else:
            if c.islower():
                self.i += 1
            if self.peek() == "n":
                self.i += 1
            while self.peek() != "_":
                self.i += 1
            self.i += 1

    def type_(self):
        """Return the crate of the type if it is a path, else None."""
        c = self.peek()
        if c in "CNIMXY":
            return self.path()
        if c == "B":
            self.i += 1
            return self.backref(self.type_)
        self.i += 1
        if c.islower():
            return None  # basic type
        if c in "RQ":
            if self.peek() == "L":
                self.i += 1
                self.base62()
            return self.type_()
        if c in "POS":
            return self.type_()
        if c == "A":
            crate = self.type_()
            self.const_()
            return crate
        if c == "T":
            first = None
            while self.peek() != "E":
                t = self.type_()
                first = first or t
            self.i += 1
            return first
        if c == "F":
            if self.peek() == "G":
                self.i += 1
                self.base62()
            if self.peek() == "U":
                self.i += 1
            if self.peek() == "K":
                self.i += 1
                if self.peek() == "C":
                    self.i += 1
                else:
                    self.ident()
            while self.peek() != "E":
                self.type_()
            self.i += 1
            self.type_()
            return None
        if c == "D":
            first = None
            if self.peek() == "G":
                self.i += 1
                self.base62()
            while self.peek() != "E":
                t = self.path()
                first = first or t
                while self.peek() == "p":
                    self.i += 1
                    self.ident()
                    self.type_()
            self.i += 1
            if self.peek() == "L":
                self.i += 1
                self.base62()
            return first
        raise ValueError


_CRATE_ROOT = re.compile(r"C(?:s[0-9a-zA-Z]*_)?(\d+)")
_CRATE_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def v0_crate(sym):
    """Crate owning a v0 symbol (`_R...`), or None if it cannot be decoded."""
    body = sym[2:]
    while body[:1].isdigit():  # optional version digits
        body = body[1:]
    try:
        crate = _V0(body).path()
    except (ValueError, IndexError):
        crate = None
    if crate:
        return crate
    # Fallback: the first crate root `C[s<disambiguator>_]<len><name>`.
    for m in _CRATE_ROOT.finditer(body):
        n = int(m.group(1))
        name = body[m.end():m.end() + n]
        if len(name) == n and _CRATE_NAME.fullmatch(name):
            return name
    return None


_PRIMS = {
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64", "bool", "char", "str", "dyn", "unsafe",
    "extern", "fn", "mut", "const", "impl", "Self", "T",
}
_LEAD = re.compile(r"^(<|&|\*const |\*mut |mut |dyn |\(|\[|'[a-z_]+ )")
_HASH = re.compile(r"\[[0-9a-f]{1,16}\]")  # crate disambiguator in demangled v0
_PRIM_IMPL = tuple(p + ">" for p in sorted(_PRIMS)) + ("T]>",)
_IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?=::)")


def crate_from_readable(text):
    """Leading crate of a demangled path such as
    `<bevy_ecs::a::B<T> as core::fmt::Debug>::fmt` -> bevy_ecs."""
    t = _HASH.sub("", text.strip())
    while True:
        t2 = _LEAD.sub("", t)
        if t2 == t:
            break
        t = t2
    m = _IDENT.match(t)
    if m and m.group(0) not in _PRIMS:
        return m.group(0)
    # Impl on a primitive or tuple (`<f32 as bevy_reflect::Reflect>::m`):
    # credit the trait's crate instead.
    head, sep, trait = text.partition(" as ")
    if sep and head.startswith("<"):
        return crate_from_readable(trait)
    if t.startswith(_PRIM_IMPL):  # `<str>::trim`, `<[T]>::sort`, `<u128>::fmt`
        return "core"
    return None


def crate_of_symbol(sym):
    """Map a (mangled or twiggy-demangled) symbol to a crate name or None."""
    if sym is None:
        return None
    if sym.startswith("_R"):
        return v0_crate(sym)
    if sym.startswith("_ZN"):
        return crate_from_readable(demangle_legacy(sym) or "")
    if "::" not in sym:
        # Unmangled C-ABI names: wasm-bindgen glue, compiler-rt, allocator.
        if sym.startswith(("__wbg_", "__wbindgen", "wasm_bindgen")):
            return "wasm_bindgen"
        if sym.startswith(("__rust_", "__rdl_", "__rg_")):
            return "alloc"
        if re.match(r"^(memcpy|memmove|memset|memcmp|bcmp|fmodf?|sqrtf?|powf?|fmaf?|"
                    r"floorf?|ceilf?|truncf?|roundf?|rintf?|__\w+i[34]\b)",
                    sym):
            return "compiler_builtins"
        if sym in ("malloc", "free", "calloc", "realloc", "dlmalloc",
                   "dlfree", "dlrealloc", "dlcalloc"):
            return "dlmalloc"
        return None
    return crate_from_readable(sym)


# --------------------------------------------------------------------------
# Grouping into families
# --------------------------------------------------------------------------

STD_CRATES = {"core", "alloc", "std", "compiler_builtins", "dlmalloc",
              "panic_abort", "panic_unwind", "unwind",
              "rustc_std_workspace_core", "rustc_std_workspace_alloc"}
EGUI_CRATES = {"egui", "epaint", "emath", "ecolor", "bevy_egui", "egui_extras",
               "epaint_default_fonts", "ab_glyph", "ab_glyph_rasterizer",
               "ttf_parser", "owned_ttf_parser"}
WGPU_PREFIXES = ("wgpu", "naga", "glow", "khronos_egl")
NONCODE = ("data segments", "other / unnamed", "module structure")
NONCODE_FAMILY = "Data and module structure"


def family_of(crate):
    if crate in NONCODE:
        return NONCODE_FAMILY
    if crate.startswith("observation"):
        return "observation"
    if crate in EGUI_CRATES:
        return "egui"
    if crate.startswith(("bevy", "encase")):
        return "Bevy"
    if crate.startswith(WGPU_PREFIXES):
        return "wgpu"
    if crate in STD_CRATES:
        return "Rust std"
    return "Other dependencies"


FAMILY_ORDER = ["Bevy", "egui", "wgpu", "Rust std", "observation",
                "Other dependencies", NONCODE_FAMILY]
# (leaf fill, family backdrop); white text on every leaf fill has contrast >= 4.5
FAMILY_COLOURS = {
    "Bevy": ("#9a4b00", "#5c2d00"),
    "egui": ("#0b5f8a", "#06374f"),
    "wgpu": ("#6a3fb0", "#3d2368"),
    "Rust std": ("#55606b", "#2f363d"),
    "observation": ("#1a7332", "#0d4019"),
    "Other dependencies": ("#a1294f", "#5e1730"),
    NONCODE_FAMILY: ("#3f4e5e", "#222c36"),
}


def aggregate(items, threshold):
    """Return ({family: [(label, bytes)]}, total, {crate: bytes})."""
    per_crate = {}
    for kind, label, size in items:
        if kind == "data":
            crate = "data segments"
        elif kind == "meta":
            crate = "module structure"
        else:
            crate = crate_of_symbol(label) or "other / unnamed"
        per_crate[crate] = per_crate.get(crate, 0) + size
    total = sum(per_crate.values())
    fam = {}
    for crate, size in per_crate.items():
        fam.setdefault(family_of(crate), []).append((crate, size))
    out = {}
    for f, members in fam.items():
        big = sorted([m for m in members if m[1] >= threshold * total],
                     key=lambda cs: (-cs[1], cs[0]))
        small = [m for m in members if m[1] < threshold * total]
        if len(small) == 1:
            big.append(small[0])
        elif small:
            big.append(("other crates (%d)" % len(small),
                        sum(s for _, s in small)))
        out[f] = big
    return out, total, per_crate


# --------------------------------------------------------------------------
# Squarified treemap
# --------------------------------------------------------------------------


def squarify(sizes, x, y, w, h):
    """sizes: list of (key, value) sorted descending. Return {key: rect}."""
    rects = {}
    total = float(sum(v for _, v in sizes))
    if total <= 0 or w <= 0 or h <= 0:
        return rects
    scale = w * h / total
    queue = [(k, v * scale) for k, v in sizes if v > 0]

    def worst(row, side):
        s = sum(a for _, a in row)
        return max(max(side * side * a / (s * s), (s * s) / (side * side * a))
                   for _, a in row)

    while queue:
        side = min(w, h)
        row = [queue[0]]
        i = 1
        while i < len(queue) and worst(row + [queue[i]], side) <= worst(row, side):
            row.append(queue[i])
            i += 1
        queue = queue[i:]
        s = sum(a for _, a in row)
        if w >= h:  # the row becomes a column on the left
            cw = s / h
            cy = y
            for k, a in row:
                ch = a / cw
                rects[k] = (x, cy, cw, ch)
                cy += ch
            x += cw
            w -= cw
        else:  # the row runs along the top
            rh = s / w
            cx = x
            for k, a in row:
                rw = a / rh
                rects[k] = (cx, y, rw, rh)
                cx += rw
            y += rh
            h -= rh
    return rects


# --------------------------------------------------------------------------
# SVG
# --------------------------------------------------------------------------

BG = "#f6f8fa"
INK = "#1f2328"
MUTED = "#57606a"
FONT = "-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif"


def esc(s):
    return (s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
            .replace('"', "&quot;"))


def kib(n):
    if n >= 1024 * 1024:
        return "%.2f MiB" % (n / 1048576.0)
    return "%s KiB" % format(int(round(n / 1024.0)), ",")


def text_w(s, size, bold=False):
    """Conservative width estimate for a sans-serif string."""
    return len(s) * size * (0.62 if bold else 0.58)


def shade(hexcol, factor):
    r, g, b = (int(hexcol[i:i + 2], 16) for i in (1, 3, 5))
    return "#%02x%02x%02x" % tuple(int(round(c * factor)) for c in (r, g, b))


def render(families, total, meta):
    width, pad, head_h, map_h, foot_h = 1200, 16, 78, 720, 34
    height = head_h + map_h + foot_h
    body = []
    out = body.append

    fam_sizes = [(f, sum(s for _, s in families[f]))
                 for f in FAMILY_ORDER if f in families]
    code = sorted([fs for fs in fam_sizes if fs[0] != NONCODE_FAMILY],
                  key=lambda fs: (-fs[1], fs[0]))
    rest = [fs for fs in fam_sizes if fs[0] == NONCODE_FAMILY]
    fam_sizes = code + rest
    fam_rects = squarify(sorted(fam_sizes, key=lambda fs: (-fs[1], fs[0])),
                         pad, head_h, width - 2 * pad, map_h)

    head, gap = 22, 2
    for f, fsize in fam_sizes:
        fx, fy, fw, fh = fam_rects[f]
        fill, backdrop = FAMILY_COLOURS[f]
        out('<g><title>%s: %s (%.1f%%)</title>' % (
            esc(f), kib(fsize), 100.0 * fsize / total))
        out('<rect x="%.1f" y="%.1f" width="%.1f" height="%.1f" fill="%s"/>' % (
            fx, fy, fw, fh, backdrop))
        label = "%s  %s  %.1f%%" % (f, kib(fsize), 100.0 * fsize / total)
        if text_w(label, 13, True) > fw - 10:
            label = f
        if text_w(label, 13, True) <= fw - 10 and fh > head + 4:
            out('<text x="%.1f" y="%.1f" font-size="13" font-weight="700" '
                'fill="#ffffff">%s</text>' % (fx + 6, fy + 15, esc(label)))
        out('</g>')
        leaves = families[f]
        rects = squarify(leaves, fx + gap, fy + head, fw - 2 * gap,
                         fh - head - gap)
        n = len(leaves)
        for rank, (crate, size) in enumerate(leaves):
            if crate not in rects:  # degenerate container: nothing to draw
                continue
            rx, ry, rw, rh = rects[crate]
            # Darken slightly down the ranking; white text stays above 4.5:1.
            col = shade(fill, 1.0 - 0.25 * (rank / max(n - 1, 1)))
            pct = 100.0 * size / total
            out('<g><title>%s: %s (%s bytes, %.2f%%)</title>' % (
                esc(crate), kib(size), format(size, ","), pct))
            out('<rect x="%.1f" y="%.1f" width="%.1f" height="%.1f" '
                'fill="%s" stroke="%s" stroke-width="1.5"/>' % (
                    rx + 0.75, ry + 0.75, max(rw - 1.5, 0), max(rh - 1.5, 0),
                    col, backdrop))
            line1 = crate
            line2 = "%s · %.1f%%" % (kib(size), pct)
            if rw > 24 and rh > 18:
                if text_w(line1, 12, True) > rw - 10:
                    fit = int((rw - 10) / (12 * 0.62)) - 1
                    line1 = line1[:fit] + "…" if fit >= 5 else None
                if line1:
                    out('<text x="%.1f" y="%.1f" font-size="12" '
                        'font-weight="700" fill="#ffffff">%s</text>' % (
                            rx + 5, ry + 16, esc(line1)))
                    if rh > 36:
                        if text_w(line2, 11) > rw - 10:
                            line2 = kib(size)  # drop the percentage
                        if text_w(line2, 11) <= rw - 10:
                            out('<text x="%.1f" y="%.1f" font-size="11" '
                                'fill="#ffffff">%s</text>' % (
                                    rx + 5, ry + 31, esc(line2)))
            out('</g>')

    title = "WebAssembly bundle by crate"
    sub1 = ("Total %.2f MB raw (%s) · %.2f MB gzip · rectangle area is "
            "proportional to bytes of code and data" % (
                meta["raw"] / 1e6, kib(meta["raw"]), meta["gzip"] / 1e6))
    sub2 = "observation-client, wasm-release profile, %s · commit %s · %s" % (
        meta["stage"], meta["commit"], meta["date"])
    foot = ("Attributed from symbol names after wasm-opt; inlined and merged "
            "code is credited to the surviving symbol. Percentages are of "
            "code, data and module structure (names excluded).")
    svg = [
        '<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" '
        'viewBox="0 0 %d %d" role="img" aria-labelledby="t d" '
        'font-family="%s">' % (width, height, width, height, esc(FONT)),
        '<title id="t">WebAssembly bundle size by crate (treemap)</title>',
        '<desc id="d">%s</desc>' % esc(
            "Treemap of the shipped WebAssembly module, one rectangle per "
            "dependency crate, grouped by family. " + sub1 + "."),
        '<rect width="%d" height="%d" fill="%s"/>' % (width, height, BG),
        '<text x="%d" y="30" font-size="21" font-weight="700" fill="%s">%s</text>' % (
            pad, INK, esc(title)),
        '<text x="%d" y="50" font-size="13" fill="%s">%s</text>' % (
            pad, MUTED, esc(sub1)),
        '<text x="%d" y="68" font-size="12" fill="%s">%s</text>' % (
            pad, MUTED, esc(sub2)),
    ] + body + [
        '<text x="%d" y="%d" font-size="11" fill="%s">%s</text>' % (
            pad, head_h + map_h + 22, MUTED, esc(foot)),
        '</svg>', '']
    return "\n".join(svg)


def top_table(per_crate, total, n):
    rows = sorted(per_crate.items(), key=lambda cs: (-cs[1], cs[0]))[:n]
    lines = ["%-4s %-30s %12s %8s" % ("#", "crate", "bytes", "share")]
    for i, (c, s) in enumerate(rows, 1):
        lines.append("%-4d %-30s %12s %7.2f%%" % (i, c, format(s, ","),
                                                  100.0 * s / total))
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description="Wasm size-per-crate treemap (SVG)")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--wasm")
    src.add_argument("--twiggy-csv")
    ap.add_argument("--out", required=True)
    ap.add_argument("--shipped-raw", type=int, help="override raw bytes shown")
    ap.add_argument("--shipped-gzip", type=int, help="override gzip bytes shown")
    ap.add_argument("--commit", default="unknown")
    ap.add_argument("--date", default="unknown")
    ap.add_argument("--stage", default="after wasm-bindgen and wasm-opt -Os")
    ap.add_argument("--threshold", type=float, default=0.005,
                    help="crates below this share are grouped (default 0.005)")
    ap.add_argument("--table", type=int, default=0,
                    help="print a top-N table to stdout")
    args = ap.parse_args()

    raw = gz = None
    if args.wasm:
        with open(args.wasm, "rb") as fh:
            buf = fh.read()
        items, shipped = analyse_wasm(buf)
        raw = len(shipped)
        gz = len(zlib.compress(shipped, 9))
    else:
        with open(args.twiggy_csv, encoding="utf-8") as fh:
            items = parse_twiggy_csv(fh.read())
    raw = args.shipped_raw or raw
    gz = args.shipped_gzip or gz
    families, total, per_crate = aggregate(items, args.threshold)
    meta = {"raw": raw or total, "gzip": gz or 0, "commit": args.commit,
            "date": args.date, "stage": args.stage}
    with open(args.out, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(render(families, total, meta))
    if args.table:
        print(top_table(per_crate, total, args.table))
        print("\nattributed total %s B; shipped-equivalent module %s B; "
              "gzip -9 %s B" % (format(total, ","), format(meta["raw"], ","),
                                format(meta["gzip"], ",")))


if __name__ == "__main__":
    main()
