//! Whether a stored bundle or tile can still be served as it is.

use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use observation_core::bundle::{BUNDLE_MAGIC, BUNDLE_VERSION};

use crate::clock::unix_secs;
use crate::ports::{StoredBundle, TileMeta};

/// Whether `bytes` start like a bundle of the current format version.
pub fn is_current_bundle(bytes: &[u8]) -> bool {
    bytes.starts_with(&BUNDLE_MAGIC) && bytes.get(4..6) == Some(&BUNDLE_VERSION.to_le_bytes()[..])
}

/// Whether a bundle written at `written` is fresh at `now`: younger than
/// `ttl` and, given `valid_after`, written no earlier (a region imported later
/// has newer data). A write time in the future (clock skew) or an unknown one
/// counts as young.
pub fn bundle_is_fresh(
    written: Option<SystemTime>,
    now: SystemTime,
    ttl: Duration,
    valid_after: Option<SystemTime>,
) -> bool {
    let age = written.and_then(|m| now.duration_since(m).ok());
    age.is_none_or(|a| a < ttl) && valid_after.is_none_or(|v| written.is_none_or(|m| m >= v))
}

/// A stored bundle as callers use it: its bytes and whether it is fresh, or
/// `None` (logged) if it is not a bundle of the current version. Expired
/// bundles are returned too, so callers can serve them when the upstream is
/// down.
pub fn usable_bundle(
    key: &str,
    stored: StoredBundle,
    now: SystemTime,
    ttl: Duration,
    valid_after: Option<SystemTime>,
) -> Option<(Bytes, bool)> {
    if !is_current_bundle(&stored.bytes) {
        tracing::warn!(key, "ignoring cached bundle with bad magic or old version");
        return None;
    }
    let fresh = bundle_is_fresh(stored.written, now, ttl, valid_after);
    Some((stored.bytes, fresh))
}

/// Whether a tile with `meta` can be served without asking upstream at `now`.
pub fn tile_is_fresh(meta: &TileMeta, now: SystemTime) -> bool {
    meta.expires_unix > unix_secs(now)
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    const HOUR: Duration = Duration::from_secs(3600);
    const T: u64 = 1_700_000_000;

    #[test]
    fn bundle_freshness_truth_table() {
        let written = Some(at(T));
        // (now, ttl, valid_after, fresh)
        let cases = [
            (T, HOUR, None, true),
            (T + 3599, HOUR, None, true),
            (T + 3600, HOUR, None, false), // exactly the TTL is stale
            (T + 3601, HOUR, None, false),
            (T - 60, HOUR, None, true), // written in the future: fresh
            (T, Duration::ZERO, None, false), // zero TTL: stale at once
            (T, HOUR, Some(T + 60), false), // newer local data
            (T, HOUR, Some(T), true),   // data from the same second
            (T, HOUR, Some(T - 60), true),
            (T + 3600, HOUR, Some(T - 60), false), // either rule makes it stale
        ];
        for (now, ttl, valid_after, fresh) in cases {
            assert_eq!(
                bundle_is_fresh(written, at(now), ttl, valid_after.map(at)),
                fresh,
                "now {now}, ttl {ttl:?}, valid_after {valid_after:?}"
            );
        }
        // Unknown write time (a file system without mtimes): always fresh.
        assert!(bundle_is_fresh(None, at(T), HOUR, Some(at(T + 60))));
        assert!(bundle_is_fresh(None, at(T), Duration::ZERO, None));
    }

    fn bundle_bytes() -> Vec<u8> {
        let mut v = BUNDLE_MAGIC.to_vec();
        v.extend_from_slice(&BUNDLE_VERSION.to_le_bytes());
        v.extend_from_slice(&[9, 9]);
        v
    }

    #[test]
    fn only_current_bundles_are_usable() {
        let stored = |bytes: Vec<u8>| StoredBundle {
            bytes: Bytes::from(bytes),
            written: Some(at(T)),
        };
        let usable = |bytes| usable_bundle("k", stored(bytes), at(T + 1), HOUR, None);
        assert_eq!(
            usable(bundle_bytes()),
            Some((Bytes::from(bundle_bytes()), true))
        );
        assert_eq!(usable(b"<html>oops</html>".to_vec()), None);
        let mut old = bundle_bytes();
        old[4..6].copy_from_slice(&(BUNDLE_VERSION.wrapping_sub(1)).to_le_bytes());
        assert_eq!(usable(old), None);
        assert_eq!(usable(BUNDLE_MAGIC.to_vec()), None, "too short");
        let expired = usable_bundle("k", stored(bundle_bytes()), at(T + 3600), HOUR, None);
        assert_eq!(expired, Some((Bytes::from(bundle_bytes()), false)));
    }

    #[test]
    fn tiles_are_fresh_until_expires_unix() {
        let meta = TileMeta {
            expires_unix: T + 100,
            ..TileMeta::default()
        };
        assert!(tile_is_fresh(&meta, at(T + 99)));
        assert!(
            !tile_is_fresh(&meta, at(T + 100)),
            "expired at exactly expires_unix"
        );
        assert!(!tile_is_fresh(&meta, at(T + 101)));
        assert!(!tile_is_fresh(&TileMeta::default(), UNIX_EPOCH));
    }
}
