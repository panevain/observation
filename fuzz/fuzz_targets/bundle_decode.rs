//! `AreaBundle::decode` on arbitrary bytes: bundles come over the network
//! and from the server's on-disk cache, so decoding must never panic, and
//! anything that decodes must survive an encode/decode round trip.
#![no_main]

use libfuzzer_sys::fuzz_target;
use observation_core::bundle::AreaBundle;

fuzz_target!(|data: &[u8]| {
    if let Ok(bundle) = AreaBundle::decode(data) {
        let again = AreaBundle::decode(&bundle.encode()).expect("re-encoded bundle decodes");
        assert_eq!(again.buildings.len(), bundle.buildings.len());
        assert_eq!(again.mesh.indices.len(), bundle.mesh.indices.len());
    }
});
