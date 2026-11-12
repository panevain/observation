//! What the ported Python scripts did with their inputs, where a port has to do the same to
//! pass and fail on the same inputs: text-mode newlines, `str.splitlines`, truthiness, `str()`
//! and `repr()` of what `tomllib` and `json` produce, and TOML 1.0 (`tomllib`) as opposed to the
//! TOML 1.1 that the `toml` crate also accepts.

use std::fmt::Write as _;

/// Text as Python reads it in text mode (`read_text`, `subprocess.run(text=True)`): CRLF and
/// a lone CR become LF.
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `str.splitlines()`: lines split at every line boundary Python knows, without their ends.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            continue;
        }
        out.push(&s[start..i]);
        start = i + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
            chars.next();
            start += 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The longest field `csv.reader` takes (`csv.field_size_limit()`'s default), in characters.
pub const CSV_FIELD_LIMIT: usize = 131_072;

/// `list(csv.reader(lines))` with the default (excel) dialect: `,` between fields, `"` around a
/// quoted field and `""` for a `"` inside one, and `strict=False`, so a `"` after a quoted field
/// is kept as text. Each line comes without its line ending, as `str.splitlines()` gives them
/// (so a CR or LF never reaches the reader); a line with no characters is an empty record, a
/// quoted field still open at the end of a line goes on with the next line (without a line
/// break), and one still open at the end of the input ends there. A field longer than
/// [`CSV_FIELD_LIMIT`] is an error, as it is for the reader.
pub fn csv_rows(lines: &[&str]) -> Result<Vec<Vec<String>>, String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        StartRecord,
        StartField,
        InField,
        InQuoted,
        QuoteInQuoted,
    }
    struct Record {
        fields: Vec<String>,
        field: String,
        chars: usize,
    }
    impl Record {
        fn add(&mut self, c: char) -> Result<(), String> {
            if self.chars >= CSV_FIELD_LIMIT {
                return Err(format!("field larger than field limit ({CSV_FIELD_LIMIT})"));
            }
            self.field.push(c);
            self.chars += 1;
            Ok(())
        }
        fn save(&mut self) {
            self.fields.push(std::mem::take(&mut self.field));
            self.chars = 0;
        }
    }
    let mut rows = Vec::new();
    let mut rec = Record {
        fields: Vec::new(),
        field: String::new(),
        chars: 0,
    };
    let mut state = State::StartRecord;
    for line in lines {
        // `None` is the end of the line.
        for c in line.chars().map(Some).chain([None]) {
            state = match (state, c) {
                // An empty line: an empty record.
                (State::StartRecord, None) => State::StartRecord,
                (State::StartField | State::InField | State::QuoteInQuoted, None) => {
                    rec.save();
                    State::StartRecord
                }
                (State::StartRecord | State::StartField, Some('"')) => State::InQuoted,
                (
                    State::StartRecord | State::StartField | State::InField | State::QuoteInQuoted,
                    Some(','),
                ) => {
                    rec.save();
                    State::StartField
                }
                (State::InQuoted, None) => State::InQuoted,
                (State::InQuoted, Some('"')) => State::QuoteInQuoted,
                (State::QuoteInQuoted, Some('"')) => {
                    rec.add('"')?;
                    State::InQuoted
                }
                (State::InQuoted, Some(c)) => {
                    rec.add(c)?;
                    State::InQuoted
                }
                (
                    State::StartRecord | State::StartField | State::InField | State::QuoteInQuoted,
                    Some(c),
                ) => {
                    rec.add(c)?;
                    State::InField
                }
            };
        }
        if state == State::StartRecord {
            rows.push(std::mem::take(&mut rec.fields));
        }
    }
    if state == State::InQuoted {
        rec.save();
        rows.push(rec.fields);
    }
    Ok(rows)
}

/// Python's truth value of what `json.loads` returned.
pub fn json_truthy(v: &serde_json::Value) -> bool {
    use serde_json::Value;
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's truth value of what `tomllib` returned (dates and times are always true).
pub fn toml_truthy(v: &toml::Value) -> bool {
    match v {
        toml::Value::String(s) => !s.is_empty(),
        toml::Value::Integer(i) => *i != 0,
        toml::Value::Float(f) => *f != 0.0,
        toml::Value::Boolean(b) => *b,
        toml::Value::Datetime(_) => true,
        toml::Value::Array(a) => !a.is_empty(),
        toml::Value::Table(t) => !t.is_empty(),
    }
}

/// Whether `str.isprintable()` holds for `c`, as far as it can be told without Unicode's
/// tables: control, format, separator (other than space), surrogate and private-use code
/// points are not printable. Unassigned code points are taken as printable (Python escapes
/// them), so `repr` of such a string differs from Python's; it only appears in messages.
fn printable(c: char) -> bool {
    let n = u32::from(c);
    !matches!(n,
        0x00..=0x1f | 0x7f..=0xa0 | 0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x890..=0x891
        | 0x8e2 | 0x1680 | 0x180e | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064
        | 0x2066..=0x206f | 0x3000 | 0xd800..=0xf8ff | 0xfeff | 0xfff9..=0xfffb
        | 0x110bd | 0x110cd | 0x13430..=0x1343f | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a
        | 0xe0001 | 0xe0020..=0xe007f | 0xf0000..=0x10ffff)
        && n & 0xfffe != 0xfffe
}

/// `repr()` of a str.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if printable(c) => out.push(c),
            c => {
                let n = u32::from(c);
                let _ = match n {
                    0..=0xff => write!(out, "\\x{n:02x}"),
                    0x100..=0xffff => write!(out, "\\u{n:04x}"),
                    _ => write!(out, "\\U{n:08x}"),
                };
            }
        }
    }
    out.push(quote);
    out
}

/// `repr()` of a float (also its `str()`): the shortest digits that read back as the same
/// float, in positional notation for exponents -4 to 15 and in scientific notation otherwise.
pub fn repr_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    // `{:e}` gives the shortest round-trip digits: `-1.5e-7`, `1e16`.
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let len = i32::try_from(digits.len()).unwrap_or(i32::MAX);
        let body = if exp < 0 {
            let zeros = usize::try_from(-exp - 1).unwrap_or(0);
            format!("0.{}{digits}", "0".repeat(zeros))
        } else if len <= exp + 1 {
            let zeros = usize::try_from(exp + 1 - len).unwrap_or(0);
            format!("{digits}{}.0", "0".repeat(zeros))
        } else {
            let point = usize::try_from(exp + 1).unwrap_or(0);
            format!("{}.{}", &digits[..point], &digits[point..])
        };
        format!("{sign}{body}")
    } else {
        let mantissa = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{mantissa}e{esign}{:02}", exp.abs())
    }
}

/// The fields after the minute that Python's date and time reprs show: the second when it or
/// the microsecond is not zero, the microsecond when it is not zero.
fn repr_time_fields(t: &toml::value::Time) -> String {
    let (second, micro) = time_parts(t);
    match (second, micro) {
        (0, 0) => String::new(),
        (s, 0) => format!(", {s}"),
        (s, us) => format!(", {s}, {us}"),
    }
}

fn time_parts(t: &toml::value::Time) -> (u8, u32) {
    (t.second.unwrap_or(0), t.nanosecond.unwrap_or(0) / 1000)
}

/// The offset in seconds (`None` for a local date or time).
fn offset_secs(d: &toml::value::Datetime) -> Option<i32> {
    d.offset.map(|o| match o {
        toml::value::Offset::Z => 0,
        toml::value::Offset::Custom { minutes } => i32::from(minutes) * 60,
    })
}

/// `repr()` of a `datetime.timezone` with this offset.
fn repr_tz(secs: i32) -> String {
    if secs == 0 {
        return "datetime.timezone.utc".to_owned();
    }
    // timedelta's repr normalises to days (possibly negative) and seconds in 0..86400.
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let delta = match (days, rest) {
        (0, s) => format!("seconds={s}"),
        (d, 0) => format!("days={d}"),
        (d, s) => format!("days={d}, seconds={s}"),
    };
    format!("datetime.timezone(datetime.timedelta({delta}))")
}

/// `str()` of a `datetime.timezone` offset as `isoformat` writes it: `+HH:MM`.
fn str_tz(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
}

fn str_time(t: &toml::value::Time) -> String {
    let (second, micro) = time_parts(t);
    let mut s = format!("{:02}:{:02}:{second:02}", t.hour, t.minute);
    if micro != 0 {
        let _ = write!(s, ".{micro:06}");
    }
    s
}

/// `repr()` of what tomllib made of a TOML date, time or date-time.
fn repr_datetime(d: &toml::value::Datetime) -> String {
    match (d.date, d.time) {
        (Some(date), None) => format!("datetime.date({}, {}, {})", date.year, date.month, date.day),
        (None, Some(t)) => format!(
            "datetime.time({}, {}{})",
            t.hour,
            t.minute,
            repr_time_fields(&t)
        ),
        (Some(date), Some(t)) => {
            let tz = offset_secs(d).map_or(String::new(), |s| format!(", tzinfo={}", repr_tz(s)));
            format!(
                "datetime.datetime({}, {}, {}, {}, {}{}{tz})",
                date.year,
                date.month,
                date.day,
                t.hour,
                t.minute,
                repr_time_fields(&t)
            )
        }
        (None, None) => String::new(),
    }
}

/// `str()` of what tomllib made of a TOML date, time or date-time (`isoformat`, with a space
/// between date and time).
fn str_datetime(d: &toml::value::Datetime) -> String {
    match (d.date, d.time) {
        (Some(date), None) => format!("{:04}-{:02}-{:02}", date.year, date.month, date.day),
        (None, Some(t)) => str_time(&t),
        (Some(date), Some(t)) => format!(
            "{:04}-{:02}-{:02} {}{}",
            date.year,
            date.month,
            date.day,
            str_time(&t),
            offset_secs(d).map_or(String::new(), str_tz)
        ),
        (None, None) => String::new(),
    }
}

/// `repr()` of what tomllib made of a TOML value (tables keep their order, as dicts do).
pub fn repr_toml(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => repr_str(s),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => repr_float(*f),
        toml::Value::Boolean(b) => if *b { "True" } else { "False" }.to_owned(),
        toml::Value::Datetime(d) => repr_datetime(d),
        toml::Value::Array(a) => {
            format!(
                "[{}]",
                a.iter().map(repr_toml).collect::<Vec<_>>().join(", ")
            )
        }
        toml::Value::Table(t) => repr_table(t),
    }
}

/// `repr()` of a TOML table, as the dict tomllib returns.
pub fn repr_table(t: &toml::Table) -> String {
    format!(
        "{{{}}}",
        t.iter()
            .map(|(k, v)| format!("{}: {}", repr_str(k), repr_toml(v)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// `str()` of what tomllib made of a TOML value, as an f-string writes it.
pub fn str_toml(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Datetime(d) => str_datetime(d),
        other => repr_toml(other),
    }
}

/// Parses TOML as tomllib did: text-mode newlines, then only TOML 1.0. The `toml` crate reads
/// TOML 1.1, whose additions tomllib refuses: newlines, comments and a trailing comma inside
/// an inline table, the `\x` and `\e` escapes, non-ASCII bare keys, control characters in
/// comments, and times without seconds; a leap second (`:60`) and the year 0 are refused too,
/// as Python's datetime refused them.
pub fn parse_toml(text: &str) -> std::result::Result<toml::Table, String> {
    let text = universal_newlines(text);
    toml_1_0_syntax(&text)?;
    let table: toml::Table = toml::from_str(&text).map_err(|e| e.to_string())?;
    check_times(&toml::Value::Table(table.clone()))?;
    Ok(table)
}

/// Every date-time and time has seconds (required before TOML 1.1), none is a leap second, and
/// no date is in the year 0 (Python's dates start at the year 1).
fn check_times(v: &toml::Value) -> std::result::Result<(), String> {
    match v {
        toml::Value::Datetime(d) => match (d.date, d.time) {
            (_, Some(t)) if t.second.is_none() => {
                Err(format!("a time without seconds (TOML 1.1): {d}"))
            }
            (_, Some(t)) if t.second == Some(60) => Err(format!("a leap second: {d}")),
            (Some(date), _) if date.year == 0 => Err(format!("the year 0: {d}")),
            _ => Ok(()),
        },
        toml::Value::Array(a) => a.iter().try_for_each(check_times),
        toml::Value::Table(t) => t.values().try_for_each(check_times),
        _ => Ok(()),
    }
}

/// The TOML 1.1 syntax that TOML 1.0 does not have, found by a scan that tracks strings,
/// comments and brackets (see [`parse_toml`]). What is not TOML at all is left to the parser.
fn toml_1_0_syntax(text: &str) -> std::result::Result<(), String> {
    let chars: Vec<char> = text.chars().collect();
    let mut stack: Vec<char> = Vec::new();
    let mut line = 1;
    let mut i = 0;
    let err = |line: usize, what: &str| Err(format!("line {line}: {what} (TOML 1.1, not 1.0)"));
    let at = |i: usize| chars.get(i).copied();
    while let Some(c) = at(i) {
        let in_table = stack.last() == Some(&'{');
        match c {
            '\n' => {
                if in_table {
                    return err(line, "a newline inside an inline table");
                }
                line += 1;
            }
            '#' => {
                if in_table {
                    return err(line, "a comment inside an inline table");
                }
                while let Some(c) = at(i + 1).filter(|&c| c != '\n') {
                    if c != '\t' && (c < ' ' || c == '\u{7f}') {
                        return err(line, "a control character in a comment");
                    }
                    i += 1;
                }
            }
            '[' | '{' => stack.push(c),
            ']' | '}' => {
                stack.pop();
            }
            ',' if in_table => {
                let mut j = i + 1;
                while matches!(at(j), Some(' ' | '\t')) {
                    j += 1;
                }
                if at(j) == Some('}') {
                    return err(line, "a trailing comma in an inline table");
                }
            }
            '"' | '\'' => {
                let multi = at(i + 1) == Some(c) && at(i + 2) == Some(c);
                i += if multi { 3 } else { 1 };
                loop {
                    let Some(s) = at(i) else {
                        return Ok(()); // unterminated: the parser reports it
                    };
                    if s == '\n' {
                        if !multi {
                            break; // the parser reports it
                        }
                        line += 1;
                    } else if s == '\\' && c == '"' {
                        if matches!(at(i + 1), Some('x' | 'e')) {
                            return err(line, "a \\x or \\e escape");
                        }
                        if at(i + 1) == Some('\n') {
                            line += 1;
                        }
                        i += 1;
                    } else if s == c {
                        if !multi {
                            break;
                        }
                        if at(i + 1) == Some(c) && at(i + 2) == Some(c) {
                            // Up to two more quotes belong to the string.
                            i += 2;
                            while at(i + 1) == Some(c) {
                                i += 1;
                            }
                            break;
                        }
                    }
                    i += 1;
                }
            }
            c if !c.is_ascii() => return err(line, "a non-ASCII character outside a string"),
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// `str.isspace()` of one character, which is also what `\s` matches in a `str` pattern of
/// `re`: Unicode's White_Space plus the information separators U+001C to U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// The whitespace `int()` and `float()` strip around a number: [`is_space`] without U+001C to
/// U+001F, which is Unicode's White_Space.
fn number_space(c: char) -> bool {
    is_space(c) && !('\u{1c}'..='\u{1f}').contains(&c)
}

/// `int(text)` as argparse's `type=int` reads an option: [`argparse_value`]'s rule for a value
/// that starts with `-`, then [`int`].
pub fn int_arg(text: &str) -> Result<i128, String> {
    argparse_value(text)?;
    int(text)
}

/// `int(text)` of a str: Python whitespace around (but not U+001C to U+001F, which `int()` does
/// not strip although `str.isspace()` is true for them), an optional sign, decimal digits with
/// single `_` between them, at most 4,300 digits. Refused where Python reads it too: digits other
/// than ASCII (Python takes any Unicode decimal digit) and values outside the `i128` range.
pub fn int(text: &str) -> Result<i128, String> {
    let invalid = || format!("invalid int value: {}", repr_str(text));
    let t = text.trim_matches(number_space);
    let (negative, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_')
    {
        return Err(invalid());
    }
    let digits = digits.replace('_', "");
    if digits.len() > 4300 {
        return Err(invalid());
    }
    let sign = if negative { "-" } else { "" };
    format!("{sign}{digits}").parse().map_err(|_| invalid())
}

/// The option strings of scripts/wasm-size-treemap.py's parser, the only one whose values
/// [`argparse_value`] reads.
const SCRIPT_OPTIONS: [&str; 12] = [
    "-h",
    "--help",
    "--wasm",
    "--twiggy-csv",
    "--out",
    "--shipped-raw",
    "--shipped-gzip",
    "--commit",
    "--date",
    "--stage",
    "--threshold",
    "--table",
];

/// What argparse (3.11 to 3.13, `_parse_optional`) takes as an option's separate value rather
/// than as an option, for the script's options: anything not starting with `-`; `-` alone; and
/// a string starting with `-` that is not an option (exactly, before an `=`, or as an
/// abbreviation: `--da=x`, `-hx`) and either looks like a negative number (`^-\d+$|^-\d*\.\d+$`,
/// where `$` also matches before a final newline) or contains a space. Refused where argparse
/// took it: a negative number with digits other than ASCII (`-١`, `\d` being Unicode), a value
/// starting with `-=` that has a space (Python 3.11 and 3.12 take it, 3.13 does not), and any
/// value given as `--opt=-x`.
pub fn argparse_value(text: &str) -> Result<String, String> {
    let refuse = || {
        Err(format!(
            "{} starts with `-` and argparse would read it as an option",
            repr_str(text)
        ))
    };
    let Some(rest) = text.strip_prefix('-') else {
        return Ok(text.to_owned());
    };
    if rest.is_empty() {
        return Ok(text.to_owned());
    }
    let prefix = text.split_once('=').map_or(text, |(p, _)| p);
    let option = if rest.starts_with('-') {
        SCRIPT_OPTIONS.iter().any(|o| o.starts_with(prefix))
    } else {
        // A short option and what follows it (`-hx`), or (3.13) every option for `-=…`.
        text.starts_with("-h") || prefix == "-"
    };
    if option || SCRIPT_OPTIONS.contains(&text) {
        return refuse();
    }
    let number = rest.trim_suffix('\n');
    let (int, frac) = number
        .split_once('.')
        .map_or((number, None), |(i, f)| (i, Some(f)));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let negative_number = match frac {
        None => digits(int),
        Some(frac) => (int.is_empty() || digits(int)) && digits(frac),
    };
    if negative_number || text.contains(' ') {
        Ok(text.to_owned())
    } else {
        refuse()
    }
}

/// `float(text)` as argparse's `type=float` reads an option: Python whitespace around (but not
/// U+001C to U+001F, as for [`int`]), an optional sign, `_` only between digits, `inf`,
/// `infinity` and `nan` in any case. Refused where Python reads it too: digits other than ASCII.
pub fn float_arg(text: &str) -> Result<f64, String> {
    argparse_value(text)?;
    let invalid = || format!("invalid float value: {}", repr_str(text));
    let t = text.trim_matches(number_space);
    let b = t.as_bytes();
    let digit = |i: Option<usize>| i.and_then(|i| b.get(i)).is_some_and(u8::is_ascii_digit);
    let underscores_ok = (0..b.len())
        .filter(|&i| b[i] == b'_')
        .all(|i| digit(i.checked_sub(1)) && digit(Some(i + 1)));
    if !t.is_ascii() || !underscores_ok {
        return Err(invalid());
    }
    t.replace('_', "").parse().map_err(|_| invalid())
}

/// `json.dumps()` of a string with the default `ensure_ascii`: `"` and `\` escaped, `\n`, `\r`,
/// `\t`, `\b` and `\f` written short, and everything else outside space to `~` as `\uXXXX`
/// (UTF-16 surrogate pairs beyond the BMP).
pub fn json_str(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

/// `str()` of a scalar that `json.loads` returned (`None` for an array or object, whose
/// `str()` is not reproduced): `None`, `True`/`False`, the integer's digits, the float's
/// `repr`, or the string itself.
pub fn str_json_scalar(v: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    match v {
        Value::Null => Some("None".to_owned()),
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_owned()),
        Value::Number(n) if n.is_f64() => n.as_f64().map(repr_float),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every document of toml.jsonl (generated with Python's tomllib, the cooldown script's
    /// reader): what `parse_toml` accepts, tomllib accepted, and `repr_table` writes what
    /// Python's `repr` wrote for it. The reverse does not hold everywhere: an integer beyond 64
    /// bits and a float beyond f64 (infinity to tomllib) are refused.
    #[test]
    fn toml_is_never_read_where_tomllib_refused() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/toml.jsonl");
        let corpus = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{e}"));
        let mut refused = Vec::new();
        let mut wrong = Vec::new();
        for line in corpus.lines() {
            let doc: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
            let text = doc["toml"].as_str().unwrap_or_default();
            match parse_toml(text) {
                Ok(table) => {
                    if doc["ok"] != true
                        || Some(repr_table(&table).as_str()) != doc["repr"].as_str()
                    {
                        wrong.push((text.to_owned(), repr_table(&table), doc["repr"].clone()));
                    }
                }
                Err(_) if doc["ok"] == true => refused.push(text.to_owned()),
                Err(_) => {}
            }
        }
        assert_eq!(wrong, [], "read where tomllib refused, or read differently");
        assert_eq!(
            refused,
            ["a = 9223372036854775808", "a = 1e400"],
            "refused although tomllib read them"
        );
    }

    #[test]
    fn splits_lines_as_python() {
        let s = "a\nb\r\nc\rd\u{0b}e\u{0c}f\u{1c}g\u{1d}h\u{1e}i\u{85}j\u{2028}k\u{2029}l\n\nm\n";
        assert_eq!(
            splitlines(s),
            [
                "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "", "m"
            ]
        );
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("x"), ["x"]);
        assert_eq!(splitlines("\r\r\n"), ["", ""]);
        assert_eq!(universal_newlines("a\r\r\nb\rc\r\n"), "a\n\nb\nc\n");
    }

    #[test]
    fn floats_print_as_python() {
        let cases = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-0.0, "-0.0"),
            (1.5e-7, "1.5e-07"),
            (123_456_789_012_345_680.0, "1.2345678901234568e+17"),
            (2.5, "2.5"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            (1e300, "1e+300"),
        ];
        for (f, want) in cases {
            assert_eq!(repr_float(f), want, "{f}");
        }
    }

    #[test]
    fn strings_repr_as_python() {
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("both ' and \""), "'both \\' and \"'");
        assert_eq!(
            repr_str("\u{7f}\u{1}é\u{2028}\u{e000}\u{10ffff}"),
            "'\\x7f\\x01é\\u2028\\ue000\\U0010ffff'"
        );
    }

    #[test]
    fn json_strings_and_scalars_as_python_writes_them() {
        // From Python 3.13: json.dumps(s) and str(v).
        let strings = [
            ("abc", r#""abc""#),
            ("a\"b\\c", r#""a\"b\\c""#),
            ("\n\r\t\u{8}\u{c}\u{1}\u{1f}", r#""\n\r\t\b\f\u0001\u001f""#),
            ("\u{7f}", r#""\u007f""#),
            ("\u{e9}", r#""\u00e9""#),
            ("\u{1F600}", r#""\ud83d\ude00""#),
            ("\u{2028}", r#""\u2028""#),
        ];
        for (s, want) in strings {
            assert_eq!(json_str(s), want, "{s:?}");
        }
        let scalars = [
            ("5", "5"),
            ("-3", "-3"),
            ("5.0", "5.0"),
            ("1e20", "1e+20"),
            ("true", "True"),
            ("false", "False"),
            ("null", "None"),
            (r#""x""#, "x"),
            ("0.1", "0.1"),
        ];
        for (json, want) in scalars {
            let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
            assert_eq!(str_json_scalar(&v).as_deref(), Some(want), "{json}");
        }
        assert_eq!(str_json_scalar(&serde_json::json!([1])), None);
        // Python's str.isspace() over U+0000..U+3000.
        let spaces: Vec<char> = (0..=0x3000u32)
            .filter_map(char::from_u32)
            .filter(|&c| is_space(c))
            .collect();
        let mut want: Vec<char> = "\t\n\u{b}\u{c}\r\u{1c}\u{1d}\u{1e}\u{1f} \u{85}\u{a0}\u{1680}"
            .chars()
            .collect();
        want.extend('\u{2000}'..='\u{200a}');
        want.extend(['\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}']);
        assert_eq!(spaces, want);
    }
}
