//! Where coverage figures come from, and what they leave out.
//!
//! Anyone who quotes a figure needs to know when the map data was taken and
//! what the figure does not count, and to be able to tell later whether the
//! same scene would still give the same figure. This module holds:
//!
//! - [`Provenance`], the optional block a saved scene records: the OSM data
//!   date, when the server built the area, a fingerprint of the area's map
//!   features, and the app and catalogue versions.
//! - [`CAVEATS_SUMMARY`] and [`CAVEATS`], the one versioned statement of what
//!   the figures leave out ([`CAVEATS_VERSION`]). The app shows it under the
//!   figures, and anything that prints figures (such as an evidence sheet)
//!   must print the same text, so it changes in one place.
//! - UTC timestamp parsing and formatting for Overpass's
//!   `timestamp_osm_base` (`2026-10-31T19:43:02Z`), with no date library.

use crate::bundle::AreaBundle;
use serde::{Deserialize, Deserializer, Serialize};

/// The app and engine version (this crate's, which the whole workspace
/// shares).
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Bumped whenever [`CAVEATS_SUMMARY`], [`CAVEATS`] or [`CAVEATS_BOUND`]
/// change, so a saved scene says which statement its figures were shown with.
pub const CAVEATS_VERSION: u32 = 1;

/// The one line shown under the figures.
pub const CAVEATS_SUMMARY: &str = "Up to: trees, hedges, fences and vehicles are not counted. \
     'Street' includes driveways and footpaths.";

/// Everything the figures leave out or approximate, one point each (the full
/// list behind [`CAVEATS_SUMMARY`]).
pub const CAVEATS: [&str; 7] = [
    "Not modelled: trees, hedges, fences and walls, vehicles, poles, street furniture, \
     awnings, bridges and tunnels. Cameras see straight through them, so the figures \
     can only overstate what is visible past them.",
    "Buildings come from OpenStreetMap. One missing from the map lets cameras see \
     through the gap; one demolished but still mapped blocks a view that is open.",
    "Building heights are inferred where they are not tagged, and roofs are flat, so \
     views over and onto buildings can be wrong in either direction.",
    "Terrain is approximate: ground heights come from a public elevation model sampled \
     every 5 m, so small banks, retaining walls and cuttings are smoothed out.",
    "'Street' includes driveways and footpaths: every mapped road, service road and \
     footpath counts, as a band of estimated width.",
    "Detail levels are pixel-density targets from the published resolution, not a test \
     of what a camera records. Range is capped at 100 m, and floodlights and street \
     lights are ignored at night.",
    "Only the loaded area is analysed, at one subject height: views beyond its edge are \
     cut off.",
];

/// How to read the figures given [`CAVEATS`]: why "up to", and why that is
/// still not a strict upper bound.
pub const CAVEATS_BOUND: &str = "Leaving these out can only overstate what a camera sees, \
     so read the figures as \"up to\". Inferred heights and missing or out-of-date \
     buildings err in both directions, though, so they are not a strict upper bound.";

/// The full statement: the points of [`CAVEATS`] as a bulleted list, then
/// [`CAVEATS_BOUND`].
pub fn caveats_text() -> String {
    let mut out = String::from("What the figures leave out:\n");
    for c in CAVEATS {
        out.push_str("• ");
        out.push_str(c);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(CAVEATS_BOUND);
    out
}

/// Longest version or fingerprint string kept from a file.
const MAX_LABEL_CHARS: usize = 64;

/// Where a scene's figures came from. Saved with the scene (an optional,
/// additive block: older files load without it, and older apps ignore it).
///
/// Times are Unix seconds in memory and ISO-8601 UTC strings in the file,
/// which is meant to be read by people citing it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Provenance {
    /// When the OSM data was current: Overpass's `timestamp_osm_base`.
    #[serde(
        rename = "osm_data_as_of",
        default,
        with = "utc_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub osm_data_unix: Option<u64>,
    /// When the server built the area bundle from that data.
    #[serde(
        rename = "bundle_built_at",
        default,
        with = "utc_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub bundle_built_unix: Option<u64>,
    /// [`AreaBundle::osm_fingerprint`] as 16 hex digits: whether the area's
    /// map features are still the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osm_fingerprint: Option<String>,
    /// [`ENGINE_VERSION`] of the app that saved the scene.
    #[serde(default)]
    pub app_version: String,
    /// [`crate::camera::Catalog::fingerprint`] of the camera catalogue.
    #[serde(default)]
    pub catalog_version: String,
    /// [`CAVEATS_VERSION`] of the caveats shown with the figures.
    #[serde(default)]
    pub caveats_version: u32,
}

impl Provenance {
    /// The provenance of figures computed now, on `bundle`, with a catalogue
    /// whose [`crate::camera::Catalog::fingerprint`] is `catalog`.
    pub fn of(bundle: &AreaBundle, catalog: &str) -> Self {
        Self {
            osm_data_unix: bundle.osm_base_unix,
            bundle_built_unix: Some(bundle.generated_unix).filter(|&t| t > 0),
            osm_fingerprint: Some(fingerprint_hex(bundle.osm_fingerprint)),
            app_version: ENGINE_VERSION.to_owned(),
            catalog_version: catalog.to_owned(),
            caveats_version: CAVEATS_VERSION,
        }
    }

    /// Tidies values read from a file: overlong labels are cut short, and a
    /// fingerprint that is not 16 hex digits is dropped (it could only give
    /// a false warning).
    pub(crate) fn sanitise(&mut self) {
        let cut = |s: &mut String| {
            if s.chars().count() > MAX_LABEL_CHARS {
                *s = s.chars().take(MAX_LABEL_CHARS).collect();
            }
        };
        cut(&mut self.app_version);
        cut(&mut self.catalog_version);
        if self
            .osm_fingerprint
            .as_deref()
            .is_some_and(|f| f.len() != 16 || !f.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            self.osm_fingerprint = None;
        }
        if let Some(f) = &mut self.osm_fingerprint {
            f.make_ascii_lowercase();
        }
    }

    /// How the map data changed between `self` (saved with a scene) and
    /// `now` (the same area as just loaded), or `None` when it did not, or
    /// when either side has no fingerprint to compare.
    ///
    /// The fingerprint decides, not the date: public Overpass's data date
    /// moves every minute whether or not anything in the area changed.
    pub fn data_change(&self, now: &Self) -> Option<DataChange> {
        let saved = self.osm_fingerprint.as_deref()?;
        let current = now.osm_fingerprint.as_deref()?;
        (!saved.eq_ignore_ascii_case(current)).then(|| DataChange {
            saved: self.osm_data_unix,
            now: now.osm_data_unix,
            saved_app: self.app_version.clone(),
            now_app: now.app_version.clone(),
        })
    }
}

/// A scene's area came back with different map features than it was saved
/// with (see [`Provenance::data_change`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataChange {
    /// The saved OSM data date.
    pub saved: Option<u64>,
    /// The OSM data date now.
    pub now: Option<u64>,
    /// The app version the scene was saved with (empty if unknown).
    pub saved_app: String,
    /// The app version now.
    pub now_app: String,
}

impl DataChange {
    /// "Map data has changed since this scene was saved (saved: X, now: Y)",
    /// with dates, or times when both fall on the same day.
    pub fn message(&self) -> String {
        let same_day =
            matches!((self.saved, self.now), (Some(a), Some(b)) if a / 86_400 == b / 86_400);
        let show = |t: Option<u64>| match t {
            Some(t) if same_day => format_utc_minute(t),
            Some(t) => format_date(t),
            None => "date unknown".to_owned(),
        };
        format!(
            "Map data has changed since this scene was saved (saved: {}, now: {})",
            show(self.saved),
            show(self.now)
        )
    }

    /// Longer explanation, for a tooltip.
    pub fn details(&self) -> String {
        let mut out = String::from(
            "The buildings, streets or addresses in this area differ from when the scene \
             was saved, so its figures may differ from the ones saved with it.",
        );
        if !self.saved_app.is_empty() && self.saved_app != self.now_app {
            out.push_str(&format!(
                " The scene was saved with app version {}, and this is {}: the change may \
                 also be in how the app reads the map data.",
                self.saved_app, self.now_app
            ));
        }
        out
    }
}

/// A fingerprint as 16 lower-case hex digits.
pub fn fingerprint_hex(f: u64) -> String {
    format!("{f:016x}")
}

// ----- fingerprints ----------------------------------------------------------

/// 64-bit FNV-1a: small, stable across platforms, Rust versions and runs
/// (unlike `std`'s `DefaultHasher`), which is what a fingerprint saved in a
/// file needs. Not collision-resistant against an adversary, and need not be.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fnv64(u64);

impl Default for Fnv64 {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv64 {
    pub(crate) fn bytes(&mut self, b: &[u8]) -> &mut Self {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    pub(crate) fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn i64(&mut self, v: i64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn f64(&mut self, v: f64) -> &mut Self {
        self.u64(v.to_bits())
    }

    pub(crate) fn f32(&mut self, v: f32) -> &mut Self {
        self.bytes(&v.to_bits().to_le_bytes())
    }

    /// Length-prefixed, so `("ab", "c")` and `("a", "bc")` differ.
    pub(crate) fn str(&mut self, s: &str) -> &mut Self {
        self.u64(s.len() as u64).bytes(s.as_bytes())
    }

    pub(crate) fn opt_str(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            Some(s) => self.bytes(&[1]).str(s),
            None => self.bytes(&[0]),
        }
    }

    pub(crate) fn finish(&self) -> u64 {
        self.0
    }
}

// ----- UTC timestamps --------------------------------------------------------

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

/// `2026-10-31T19:43:02Z`: the inverse of [`parse_utc`].
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

/// Serde for `Option<u64>` Unix seconds as an ISO-8601 UTC string. Reading
/// is lenient: anything that is not such a string (a number, a typo, `null`)
/// reads as `None` rather than failing the whole file.
mod utc_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(t) => s.serialize_str(&super::format_utc(*t)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        Ok(match super::Lenient::<String>::deserialize(d)? {
            super::Lenient::Value(s) => super::parse_utc(&s),
            super::Lenient::Other(_) => None,
        })
    }
}

/// A value of type `T`, or anything else (ignored).
#[derive(Deserialize)]
#[serde(untagged)]
enum Lenient<T> {
    Value(T),
    Other(serde::de::IgnoredAny),
}

/// Reads an optional [`Provenance`] block, dropping it rather than failing
/// the scene when it is malformed: losing the provenance of a hand-edited
/// file is a small price, losing its cameras is not.
pub(crate) fn lenient_provenance<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Provenance>, D::Error> {
    Ok(match Lenient::<Provenance>::deserialize(d)? {
        Lenient::Value(p) => Some(p),
        Lenient::Other(_) => None,
    })
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

    #[test]
    fn fnv_is_stable() {
        // The published FNV-1a 64 test vectors.
        assert_eq!(Fnv64::default().finish(), 0xcbf2_9ce4_8422_2325);
        assert_eq!(Fnv64::default().bytes(b"a").finish(), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(
            Fnv64::default().bytes(b"foobar").finish(),
            0x8594_4171_f739_67e8
        );
        let a = Fnv64::default().str("ab").str("c").finish();
        let b = Fnv64::default().str("a").str("bc").finish();
        assert_ne!(a, b);
        assert_eq!(fingerprint_hex(0xab), "00000000000000ab");
    }

    fn saved(fingerprint: &str, date: &str) -> Provenance {
        Provenance {
            osm_data_unix: parse_utc(date),
            bundle_built_unix: parse_utc("2026-11-01T08:00:00Z"),
            osm_fingerprint: Some(fingerprint.to_owned()),
            app_version: "0.1.0".into(),
            catalog_version: "0123456789abcdef".into(),
            caveats_version: CAVEATS_VERSION,
        }
    }

    #[test]
    fn data_changes_follow_the_fingerprint_not_the_date() {
        let a = saved("00000000000000aa", "2026-10-31T19:43:02Z");
        // A newer data date with the same features: nothing to say.
        let later = saved("00000000000000aa", "2026-11-07T10:00:00Z");
        assert_eq!(a.data_change(&later), None);
        // Different features.
        let changed = saved("00000000000000bb", "2026-11-07T10:00:00Z");
        let c = a.data_change(&changed).unwrap();
        assert_eq!(
            c.message(),
            "Map data has changed since this scene was saved (saved: 2026-10-31, now: 2026-11-07)"
        );
        assert!(!c.details().contains("app version"), "{}", c.details());
        // On the same day the times tell them apart.
        let same_day = saved("00000000000000bb", "2026-10-31T21:00:00Z");
        assert_eq!(
            a.data_change(&same_day).unwrap().message(),
            "Map data has changed since this scene was saved \
             (saved: 2026-10-31 19:43 UTC, now: 2026-10-31 21:00 UTC)"
        );
        // An unknown date.
        let mut undated = changed.clone();
        undated.osm_data_unix = None;
        assert!(
            a.data_change(&undated)
                .unwrap()
                .message()
                .ends_with("now: date unknown)")
        );
        // Another app version may read the same data differently.
        let mut newer_app = changed;
        newer_app.app_version = "0.2.0".into();
        let details = a.data_change(&newer_app).unwrap().details();
        assert!(
            details.contains("0.1.0") && details.contains("0.2.0"),
            "{details}"
        );
        // Nothing to compare with.
        let none = Provenance::default();
        assert_eq!(none.data_change(&a), None);
        assert_eq!(a.data_change(&none), None);
        // Case does not matter.
        assert_eq!(a.data_change(&saved("00000000000000AA", "")), None);
    }

    #[test]
    fn provenance_is_written_with_iso_times_and_read_leniently() {
        let p = saved("00000000000000aa", "2026-10-31T19:43:02Z");
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            json.contains("\"osm_data_as_of\":\"2026-10-31T19:43:02Z\""),
            "{json}"
        );
        assert!(
            json.contains("\"bundle_built_at\":\"2026-11-01T08:00:00Z\""),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Provenance>(&json).unwrap(), p);
        // Unknown times are left out, and unreadable ones read as unknown.
        let bare = serde_json::to_string(&Provenance::default()).unwrap();
        assert!(
            !bare.contains("as_of") && !bare.contains("built_at"),
            "{bare}"
        );
        let odd = r#"{"osm_data_as_of": 1793475782, "bundle_built_at": "last week",
            "app_version": "0.1.0", "future_field": [1, 2]}"#;
        let read: Provenance = serde_json::from_str(odd).unwrap();
        assert_eq!((read.osm_data_unix, read.bundle_built_unix), (None, None));
        assert_eq!(read.app_version, "0.1.0");
    }

    #[test]
    fn sanitise_cuts_labels_and_drops_bad_fingerprints() {
        let mut p = saved("00000000000000AA", "");
        p.app_version = "x".repeat(1000);
        p.sanitise();
        assert_eq!(p.app_version.len(), MAX_LABEL_CHARS);
        assert_eq!(p.osm_fingerprint.as_deref(), Some("00000000000000aa"));
        for bad in ["", "abc", "zz00000000000000", "00000000000000aaa"] {
            let mut p = saved(bad, "");
            p.sanitise();
            assert_eq!(p.osm_fingerprint, None, "{bad:?}");
        }
    }

    #[test]
    fn caveats_are_complete() {
        let text = caveats_text();
        for word in [
            "trees",
            "hedges",
            "fences",
            "vehicles",
            "driveways",
            "footpaths",
        ] {
            assert!(CAVEATS_SUMMARY.contains(word), "summary lacks {word}");
            assert!(text.contains(word), "list lacks {word}");
        }
        assert!(text.contains("not a strict upper bound"));
        assert_eq!(text.matches('•').count(), CAVEATS.len());
    }
}
