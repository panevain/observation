//! Small helpers with no domain knowledge: stable hashing, UTC times,
//! plan-view geometry and URL checks. Nothing here imports from the rest of
//! the crate.

pub mod hash;
pub mod planar;
pub mod time;
pub mod url;

/// Serde default for flags that are on unless a file says otherwise.
pub(crate) fn default_true() -> bool {
    true
}
