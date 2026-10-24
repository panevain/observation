//! The server's Overpass pipeline on arbitrary upstream JSON: parse, then
//! project/extrude/pack into a bundle. Upstream data is untrusted, so neither
//! step may panic, and the resulting bundle must decode.
#![no_main]

use libfuzzer_sys::fuzz_target;
use observation_core::bundle::{AreaBundle, build_bundle};
use observation_core::geo::{BBox, LatLon};
use observation_server::overpass::parse_response;

fuzz_target!(|data: &[u8]| {
    let Ok(osm) = parse_response(data) else {
        return;
    };
    let bbox = BBox::around(LatLon::new(47.7086, -122.206), 300.0);
    let bundle = build_bundle(bbox, &osm.buildings, &osm.roads, &osm.addresses, "fuzz", 0);
    AreaBundle::decode(&bundle.encode()).expect("built bundle decodes");
});
