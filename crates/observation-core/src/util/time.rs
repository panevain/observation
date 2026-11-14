//! UTC timestamp parsing and formatting for Overpass's `timestamp_osm_base`
//! (`2026-10-31T19:43:02Z`), with no date library.

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day count since 1970-01-01 (Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Parses an ISO-8601 UTC timestamp as Overpass writes it,
/// `YYYY-MM-DDTHH:MM:SSZ`, into Unix seconds. `None` for anything else,
/// including an empty string (what Overpass reports for a database with no
/// data date) and dates before 1970 or after 9999.
pub fn parse_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (y, mo, d) = (i64::from(num(0..4)?), num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if y < 1970 || !(1..=12).contains(&mo) || d == 0 || d > days_in_month(y, mo) {
        return None;
    }
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = u64::try_from(days_from_civil(y, mo, d)).ok()?;
    // A leap second (`:60`) runs into the next minute, as Unix time does.
    Some(days * 86_400 + u64::from(h * 3600 + mi * 60 + sec))
}

fn split(t: u64) -> ((i64, u32, u32), u64) {
    let days = i64::try_from(t / 86_400).unwrap_or(i64::MAX / 2);
    (civil_from_days(days), t % 86_400)
}

/// `2026-10-31T19:43:02Z`. [`parse_utc`] reads it back for every time up to
/// the end of 9999; later years have more than four digits, which it refuses.
pub fn format_utc(t: u64) -> String {
    let ((y, m, d), s) = split(t);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// `2026-10-31`.
pub fn format_date(t: u64) -> String {
    let ((y, m, d), _) = split(t);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `2026-10-31 19:43 UTC`.
pub fn format_utc_minute(t: u64) -> String {
    let ((y, m, d), s) = split(t);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        s / 3600,
        s / 60 % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2026-10-31T19:43:02Z"), Some(1_793_475_782));
        assert_eq!(format_utc(1_793_475_782), "2026-10-31T19:43:02Z");
        assert_eq!(format_date(1_793_475_782), "2026-10-31");
        assert_eq!(format_utc_minute(1_793_475_782), "2026-10-31 19:43 UTC");
        // Leap days, month and year ends.
        for s in [
            "2000-02-29T12:00:00Z",
            "2024-02-29T23:59:59Z",
            "2026-12-31T23:59:59Z",
            "2027-01-01T00:00:00Z",
            "2100-03-01T00:00:00Z",
            "9999-12-31T23:59:59Z",
        ] {
            let t = parse_utc(s).unwrap();
            assert_eq!(format_utc(t), s);
        }
        // Every day for 30 years round-trips through the civil conversions.
        for day in 0..(30 * 366) {
            let t = day * 86_400 + 3723;
            assert_eq!(parse_utc(&format_utc(t)), Some(t));
        }
    }

    #[test]
    fn rejects_bad_timestamps() {
        for s in [
            "",
            "2026-10-31",
            "2026-10-31T19:43:02",
            "2026-10-31T19:43:02+01:00",
            "2026-10-31 19:43:02Z",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2100-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-10-00T00:00:00Z",
            "2026-10-31T24:00:00Z",
            "2026-10-31T23:60:00Z",
            "1969-12-31T23:59:59Z",
            "+026-10-31T19:43:02Z",
            "2026-1a-31T19:43:02Z",
            "２026-10-31T19:43:0Z",
        ] {
            assert_eq!(parse_utc(s), None, "{s:?}");
        }
    }
}
