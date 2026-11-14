//! UTC timestamps.

use crate::config;
use observation_core::provenance::{format_utc, parse_utc};
use proptest::prelude::*;

/// The last second of 9999-12-31, the latest time [`format_utc`] writes in
/// the form [`parse_utc`] reads.
const LAST_UTC: u64 = 253_402_300_799;

/// 2100-01-01T00:00:00Z.
const YEAR_2100: u64 = 4_102_444_800;

/// The Gregorian leap rule, written out here rather than taken from the
/// code under test.
fn leap(y: u32) -> bool {
    y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400))
}

fn days_in(y: u32, m: u32) -> u32 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The calendar day after `(y, m, d)`.
fn next_day((y, m, d): (u32, u32, u32)) -> (u32, u32, u32) {
    if d < days_in(y, m) {
        (y, m, d + 1)
    } else if m < 12 {
        (y, m + 1, 1)
    } else {
        (y + 1, 1, 1)
    }
}

/// A date at a boundary that calendar code gets wrong: the end of February
/// and of the year, in years around the century and 400-year rules (and
/// sometimes any year).
fn boundary_date() -> impl Strategy<Value = (u32, u32, u32)> {
    let year = prop_oneof![
        3 => prop::sample::select(vec![
            1970u32, 1971, 1972, 1999, 2000, 2001, 2023, 2024, 2025, 2099, 2100, 2101, 2399,
            2400, 2401, 9996, 9998,
        ]),
        1 => 1970..=9998u32,
    ];
    (year, 0..5usize).prop_map(|(y, which)| match which {
        0 => (y, 2, 28),
        1 => (y, 2, days_in(y, 2)),
        2 => (y, 3, 1),
        3 => (y, 12, 31),
        _ => (y, 1, 1),
    })
}

fn text((y, m, d): (u32, u32, u32), s: u32) -> String {
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn prop_utc_round_trips_up_to_9999(
        // Mostly the years real data has, sometimes any year the format holds.
        t in prop_oneof![3 => 0..=YEAR_2100, 1 => 0..=LAST_UTC],
    ) {
        let s = format_utc(t);
        prop_assert_eq!(s.len(), 20);
        prop_assert_eq!(parse_utc(&s), Some(t), "{}", s);
    }

    #[test]
    fn prop_utc_boundary_dates_are_a_day_apart(
        date in boundary_date(),
        second in 0..86_400u32,
    ) {
        let s = text(date, second);
        let t = parse_utc(&s);
        prop_assert!(t.is_some(), "{} does not parse", s);
        let t = t.unwrap_or_default();
        prop_assert_eq!(format_utc(t), s.clone());
        let next = text(next_day(date), second);
        prop_assert_eq!(parse_utc(&next), Some(t + 86_400), "{} then {}", s, next);
        // A day past the end of the month is refused.
        let (y, m, _) = date;
        let past = text((y, m, days_in(y, m) + 1), second);
        prop_assert_eq!(parse_utc(&past), None, "{}", past);
    }
}

#[test]
fn prop_utc_bound_is_where_four_digit_years_end() {
    assert_eq!(format_utc(LAST_UTC), "9999-12-31T23:59:59Z");
    assert_eq!(parse_utc(&format_utc(LAST_UTC + 1)), None);
}
