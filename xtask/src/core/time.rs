//! Dates and instants without a date-time crate: civil-date arithmetic and the ISO 8601 forms
//! the scripts read (crates.io's `pubtime` and `created_at`, and `--now`).

use std::fmt;

const MICROS_PER_DAY: i64 = 86_400_000_000;

/// A proleptic Gregorian calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    year: i32,
    month: u32,
    day: u32,
}

impl Date {
    pub fn year(self) -> i32 {
        self.year
    }

    pub fn month(self) -> u32 {
        self.month
    }

    pub fn day(self) -> u32 {
        self.day
    }

    /// The date, if it exists.
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let last = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            1..=12 => 31,
            _ => return None,
        };
        (1..=last)
            .contains(&day)
            .then_some(Self { year, month, day })
    }

    /// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
    pub fn days_since_epoch(self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(self.day) - 1;
        era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
    }

    /// The date `days` after 1970-01-01 (`civil_from_days`).
    pub fn from_days_since_epoch(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        // In range for every i64 day count a Timestamp can hold.
        Self {
            year: i32::try_from(year).unwrap_or(i32::MAX),
            month: u32::try_from(month).unwrap_or(1),
            day: u32::try_from(day).unwrap_or(1),
        }
    }
}

/// `YYYY-MM-DD`.
impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// An instant, to the microsecond, with the UTC offset it was written in (which only
/// [`Timestamp::date`] uses). Equality and order compare instants.
#[derive(Debug, Clone, Copy)]
pub struct Timestamp {
    micros: i64,
    offset_secs: i32,
}

impl PartialEq for Timestamp {
    fn eq(&self, other: &Self) -> bool {
        self.micros == other.micros
    }
}
impl Eq for Timestamp {}
impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Timestamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.micros.cmp(&other.micros)
    }
}

impl Timestamp {
    /// Microseconds since 1970-01-01T00:00:00Z, in UTC.
    pub fn from_unix_micros(micros: i64) -> Self {
        Self {
            micros,
            offset_secs: 0,
        }
    }

    /// `YYYY-MM-DD` (year 0001 or later), optionally followed by `T` (or a space), `HH:MM`,
    /// optional `:SS` and fraction (`.` or `,`, digits beyond six dropped), and `Z` or `±HH:MM`
    /// (or `±HHMM`). Without an offset the time is UTC, as the scripts treated a naive `--now`.
    ///
    /// This is a subset of what Python's `datetime.fromisoformat` accepts: the forms crates.io's
    /// `pubtime` and `created_at` use, and a hand-written `--now`. It rejects the rest
    /// (`20261111`, `2026-W46-3`, `T12`, `T1200`, a `t` separator, `+03`, `+03:00:30`).
    pub fn parse(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        let num = |range: std::ops::Range<usize>| -> Option<u32> {
            let t = s.get(range)?;
            t.bytes()
                .all(|c| c.is_ascii_digit())
                .then(|| t.parse().ok())?
        };
        if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
            return None;
        }
        let year = i32::try_from(num(0..4)?).ok().filter(|&y| y >= 1)?;
        let date = Date::new(year, num(5..7)?, num(8..10)?)?;
        let (mut secs, mut micros, mut i) = (0_i64, 0_i64, 10);
        if matches!(b.get(10), Some(b'T' | b' ')) {
            let (h, m) = (num(11..13)?, num(14..16)?);
            if b.get(13) != Some(&b':') || h > 23 || m > 59 {
                return None;
            }
            secs = i64::from(h * 3600 + m * 60);
            i = 16;
            if b.get(16) == Some(&b':') {
                let sec = num(17..19)?;
                if sec > 59 {
                    return None;
                }
                secs += i64::from(sec);
                i = 19;
                if matches!(b.get(19), Some(b'.' | b',')) {
                    let n = b[20..].iter().take_while(|c| c.is_ascii_digit()).count();
                    if n == 0 {
                        return None;
                    }
                    // Microseconds: further digits are dropped.
                    let digits = &s[20..20 + n.min(6)];
                    micros = digits.parse::<i64>().ok()? * 10_i64.pow(6 - digits.len() as u32);
                    i = 20 + n;
                }
            }
        }
        let offset_secs = match &s[i..] {
            "" | "Z" => 0,
            rest => {
                let sign = match rest.as_bytes()[0] {
                    b'+' => 1,
                    b'-' => -1,
                    _ => return None,
                };
                // Exactly HH:MM or HHMM.
                let digits = match rest.as_bytes()[1..] {
                    [a, b, b':', c, d] | [a, b, c, d] => [a, b, c, d],
                    _ => return None,
                };
                if !digits.iter().all(u8::is_ascii_digit) {
                    return None;
                }
                let n = |i: usize| i32::from(digits[i] - b'0');
                let (h, m) = (n(0) * 10 + n(1), n(2) * 10 + n(3));
                if h > 23 || m > 59 {
                    return None;
                }
                sign * (h * 3600 + m * 60)
            }
        };
        let local = date.days_since_epoch() * MICROS_PER_DAY + secs * 1_000_000 + micros;
        Some(Self {
            micros: local - i64::from(offset_secs) * 1_000_000,
            offset_secs,
        })
    }

    /// The calendar date in the offset the instant was written in.
    pub fn date(self) -> Date {
        let local = self.micros + i64::from(self.offset_secs) * 1_000_000;
        Date::from_days_since_epoch(local.div_euclid(MICROS_PER_DAY))
    }

    /// The same instant `days` whole days earlier.
    pub fn minus_days(self, days: i64) -> Self {
        Self {
            micros: self.micros - days * MICROS_PER_DAY,
            ..self
        }
    }

    /// Whole days from `earlier` to `self`, rounded down (negative when `earlier` is later).
    pub fn days_since(self, earlier: Self) -> i64 {
        (self.micros - earlier.micros).div_euclid(MICROS_PER_DAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        Timestamp::parse(s).unwrap_or_else(|| panic!("{s} does not parse"))
    }

    #[test]
    fn civil_dates_round_trip() {
        for days in [-719_468, -1, 0, 1, 11_016, 20_768, 2_932_896] {
            let d = Date::from_days_since_epoch(days);
            assert_eq!(d.days_since_epoch(), days, "{d}");
        }
        assert_eq!(
            Date::from_days_since_epoch(20_768).to_string(),
            "2026-11-11"
        );
        assert_eq!(
            Date::new(2024, 2, 29).map(Date::days_since_epoch),
            Some(19_782)
        );
        assert_eq!(Date::new(2026, 2, 29), None);
        assert_eq!(Date::new(2026, 13, 1), None);
    }

    #[test]
    fn parses_the_forms_crates_io_and_now_use() {
        assert_eq!(
            ts("2026-11-11"),
            Timestamp::from_unix_micros(20_768 * MICROS_PER_DAY)
        );
        assert_eq!(ts("2026-07-21T21:59:23Z"), ts("2026-07-21 23:59:23+02:00"));
        assert_eq!(
            ts("2018-11-07T23:28:13.006296+00:00"),
            ts("2018-11-07T23:28:13.0062961Z")
        );
        assert!(ts("2018-11-07T23:28:13.1Z") > ts("2018-11-07T23:28:13.09Z"));
        assert_eq!(ts("2026-11-11T01:00-0300").date().to_string(), "2026-11-11");
        assert_eq!(ts("2026-11-11T01:00+0300").date().to_string(), "2026-11-11");
        assert_eq!(
            ts("2026-11-11T01:00+03:00")
                .minus_days(7)
                .date()
                .to_string(),
            "2026-11-04"
        );
        for bad in [
            "",
            "2026-1-11",
            "2026-11-31",
            "2026-11-11T24:00",
            "2026-11-11T1:00",
            "2026-11-11Q",
            "2026-11-11T10:00:00.Z",
            "2026-11-11T10:00+5",
            "2026-11-11T10:00+0:300",
            "2026-11-11T10:00+030:0",
            "0000-01-01",
        ] {
            assert_eq!(Timestamp::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn whole_days_round_down_like_python_timedelta() {
        let now = ts("2026-11-11T12:00:00Z");
        assert_eq!(now.days_since(ts("2026-11-04T12:00:00Z")), 7);
        assert_eq!(now.days_since(ts("2026-11-04T12:00:00.000001Z")), 6);
        assert_eq!(now.days_since(ts("2026-11-11T12:00:01Z")), -1);
    }
}
