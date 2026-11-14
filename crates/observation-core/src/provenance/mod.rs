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

mod caveats;
mod record;

pub use crate::util::hash::fingerprint_hex;
pub use crate::util::time::{format_date, format_utc, format_utc_minute, parse_utc};
pub use caveats::{CAVEATS, CAVEATS_BOUND, CAVEATS_SUMMARY, CAVEATS_VERSION, caveats_text};
pub(crate) use record::lenient_provenance;
pub use record::{DataChange, ENGINE_VERSION, Provenance};
