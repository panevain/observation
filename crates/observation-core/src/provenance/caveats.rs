//! The one statement of what the figures leave out.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::hash::{Fnv64, fingerprint_hex};

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
