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

#[cfg(test)]
mod tests {
    use super::*;

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
}
