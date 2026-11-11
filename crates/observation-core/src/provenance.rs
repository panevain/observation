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
//! - The serde helpers that write times as ISO-8601 UTC strings. Parsing and
//!   formatting them ([`parse_utc`], [`format_utc`] and the rest) live in
//!   `util::time` and are re-exported here.

use crate::bundle::AreaBundle;
pub use crate::util::hash::fingerprint_hex;
pub use crate::util::time::{format_date, format_utc, format_utc_minute, parse_utc};
use serde::{Deserialize, Deserializer, Serialize};

/// The app and engine version (this crate's, which the whole workspace
/// shares).
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Bumped whenever [`CAVEATS_SUMMARY`], [`CAVEATS`] or [`CAVEATS_BOUND`]
/// change, so a saved scene says which statement its figures were shown with
/// (a test pins a hash of the text, so a change without a bump fails it).
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
        if self.same_data() {
            return "This app reads the map data differently from when this scene was \
                    saved (same data date)"
                .to_owned();
        }
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

    /// Whether both sides have the same known data date: the same OSM data,
    /// so the fingerprint changed because the app reads it differently
    /// (height inference, road widths), not because the map did.
    pub fn same_data(&self) -> bool {
        self.saved.is_some() && self.saved == self.now
    }

    /// Longer explanation, for a tooltip.
    pub fn details(&self) -> String {
        if self.same_data() {
            return "The map data has the same date as when the scene was saved, but this \
                    version of the app builds the area from it differently (for example \
                    building heights or street widths), so the figures may differ from the \
                    ones saved with it."
                .to_owned();
        }
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
    use crate::util::hash::Fnv64;

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
        // The same data date: the app, not the map, changed.
        let reread = saved("00000000000000bb", "2026-10-31T19:43:02Z");
        let c = a.data_change(&reread).unwrap();
        assert!(c.same_data());
        assert!(
            c.message()
                .starts_with("This app reads the map data differently")
        );
        assert!(c.details().contains("same date"), "{}", c.details());
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

    #[test]
    fn caveats_version_follows_the_text() {
        // Saved scenes record CAVEATS_VERSION as the statement their figures
        // were shown with. If this fails, the caveats changed: bump
        // CAVEATS_VERSION and pin the new hash here.
        let mut h = Fnv64::default();
        h.str(CAVEATS_SUMMARY).str(CAVEATS_BOUND);
        for c in CAVEATS {
            h.str(c);
        }
        assert_eq!(
            (CAVEATS_VERSION, fingerprint_hex(h.finish())),
            (1, "46ff2a019c5ab692".to_owned())
        );
    }
}
