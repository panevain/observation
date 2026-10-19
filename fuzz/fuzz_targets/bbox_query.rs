//! `BBox::parse` and the helpers applied to the `?bbox=` query parameter of
//! `/api/area`, which is attacker-controlled.
#![no_main]

use libfuzzer_sys::fuzz_target;
use observation_core::geo::{BBox, Projection};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(b) = BBox::parse(text) else { return };
    assert!(b.south < b.north && b.west < b.east);
    let _ = b.validate_size();
    let key = b.snapped(0.0001).cache_key();
    // Cache keys become file names: they must never contain path separators.
    assert!(
        !key.contains('/') && !key.contains('\\') && !key.contains(".."),
        "{key}"
    );
    let p = Projection::new(b.center());
    let _ = p.to_latlon(p.to_world(b.center()));
});
