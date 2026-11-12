//! A fingerprint of the OSM features an area is built from.

use super::features::{OsmAddressPoint, OsmBuilding, OsmRoad};
use crate::address::Address;
use crate::geo::{BBox, LatLon};
use crate::util::hash::Fnv64;

/// How far beyond the area a road's geometry still counts towards
/// [`osm_fingerprint`]: half the widest street band ([`road_width_m`] allows
/// up to 60 m), so a road just outside whose band reaches in is covered.
const FINGERPRINT_ROAD_MARGIN_M: f64 = 30.0;

/// A fingerprint of the map features an area in `bbox` is built from: every
/// building's id, rings, heights and address, every road's id, type, name,
/// width and the stretch of it near the area, and every address point. Equal
/// for the same features in any order, and different (barring a 1 in 2^64
/// chance) when any of them changes, so a saved scene can tell whether its
/// area's map data changed.
///
/// Overpass returns whole ways, so a road through the area comes with its
/// geometry kilometres beyond it. Only its segments that come within 30 m
/// of the box count (a road with none is left out), so an edit far along a road does not read as a change here. Buildings
/// count whole: every building returned touches the area, and all its walls
/// count towards the figures.
///
/// It covers what the app read from OSM, after height inference and road
/// widths: a version of the app that reads the same data differently gives a
/// different fingerprint. Terrain is not included.
pub fn osm_fingerprint(
    bbox: &BBox,
    buildings: &[OsmBuilding],
    roads: &[OsmRoad],
    addresses: &[OsmAddressPoint],
) -> u64 {
    fn points(h: &mut Fnv64, ring: &[LatLon]) {
        h.u64(ring.len() as u64);
        for p in ring {
            h.f64(p.lat).f64(p.lon);
        }
    }
    fn address(h: &mut Fnv64, a: &Address) {
        h.opt_str(a.housenumber.as_deref())
            .opt_str(a.street.as_deref())
            .opt_str(a.housename.as_deref())
            .opt_str(a.unit.as_deref());
    }
    let buildings = buildings.iter().map(|b| {
        let mut h = Fnv64::default();
        h.bytes(b"building").i64(b.osm_id);
        points(&mut h, &b.outer);
        h.u64(b.holes.len() as u64);
        for hole in &b.holes {
            points(&mut h, hole);
        }
        h.f32(b.min_height_m).f32(b.height_m);
        match &b.address {
            Some(a) => address(h.bytes(&[1]), a),
            None => {
                h.bytes(&[0]);
            }
        }
        h.finish()
    });
    let near = bbox.expanded_m(FINGERPRINT_ROAD_MARGIN_M);
    let roads = roads.iter().filter_map(|r| {
        let mut h = Fnv64::default();
        h.bytes(b"road")
            .i64(r.osm_id)
            .str(&r.highway)
            .opt_str(r.name.as_deref())
            .f32(r.width_m);
        // Each run of consecutive segments near the area, as its points.
        let mut runs = 0u64;
        let mut last: Option<usize> = None;
        for (i, seg) in r.points.windows(2).enumerate() {
            if !segment_near(&near, seg[0], seg[1]) {
                continue;
            }
            if last != Some(i) {
                runs += 1;
                h.bytes(b"run").f64(seg[0].lat).f64(seg[0].lon);
            }
            h.f64(seg[1].lat).f64(seg[1].lon);
            last = Some(i + 1);
        }
        (runs > 0).then(|| h.u64(runs).finish())
    });
    let addresses = addresses.iter().map(|a| {
        let mut h = Fnv64::default();
        h.bytes(b"address").f64(a.point.lat).f64(a.point.lon);
        address(&mut h, &a.address);
        h.finish()
    });
    // Sorted, so the order Overpass lists features in does not matter.
    let mut each: Vec<u64> = buildings.chain(roads).chain(addresses).collect();
    each.sort_unstable();
    let mut h = Fnv64::default();
    h.u64(each.len() as u64);
    for f in each {
        h.u64(f);
    }
    h.finish()
}

/// Whether the segment `a`-`b` may come into `bbox`: its bounds overlap the
/// box (conservative: a long diagonal can count without entering it).
fn segment_near(bbox: &BBox, a: LatLon, b: LatLon) -> bool {
    a.lat.min(b.lat) <= bbox.north
        && a.lat.max(b.lat) >= bbox.south
        && a.lon.min(b.lon) <= bbox.east
        && a.lon.max(b.lon) >= bbox.west
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change to [`sample_features`].
    type Edit = dyn Fn(&mut Vec<OsmBuilding>, &mut Vec<OsmRoad>, &mut Vec<OsmAddressPoint>);

    /// Two buildings, a road and an address point, unprojected.
    fn sample_features() -> (Vec<OsmBuilding>, Vec<OsmRoad>, Vec<OsmAddressPoint>) {
        let p = |lat, lon| LatLon::new(lat, lon);
        let building = |id, d: f64| OsmBuilding {
            osm_id: id,
            outer: vec![p(0.0, 0.0), p(0.0, d), p(d, d), p(d, 0.0), p(0.0, 0.0)],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.5,
            address: None,
        };
        // Through the area, then on for over 100 km.
        let road = OsmRoad {
            osm_id: 7,
            highway: "residential".into(),
            name: Some("Test St".into()),
            width_m: 6.0,
            points: vec![p(0.0, -0.002), p(0.0, 0.003), p(0.0, 0.5), p(0.0, 1.0)],
        };
        let address = OsmAddressPoint {
            address: Address {
                housenumber: Some("12".into()),
                ..Address::default()
            },
            point: p(0.5, 0.5),
        };
        (
            vec![building(1, 0.001), building(2, 0.002)],
            vec![road],
            vec![address],
        )
    }

    #[test]
    fn fingerprint_ignores_order_and_follows_every_feature() {
        let (b, r, a) = sample_features();
        let area = sample_area();
        let base = osm_fingerprint(&area, &b, &r, &a);
        // Pinned: a fingerprint is saved in scene files, so it must not drift
        // between releases for the same features.
        assert_eq!(base, 0xf95b_dbfe_9344_f0c5, "{base:016x}");
        let mut swapped = b.clone();
        swapped.reverse();
        assert_eq!(osm_fingerprint(&area, &swapped, &r, &a), base);

        let changed = |f: &Edit| {
            let (mut b, mut r, mut a) = sample_features();
            f(&mut b, &mut r, &mut a);
            osm_fingerprint(&area, &b, &r, &a)
        };
        let variants = [
            changed(&|b, _, _| b[0].height_m = 9.0),
            changed(&|b, _, _| b[0].outer[1].lon += 1e-7),
            changed(&|b, _, _| {
                let ring = b[0].outer.clone();
                b[1].holes.push(ring);
            }),
            changed(&|b, _, _| b[0].address = Some(Address::default())),
            changed(&|b, _, _| b.truncate(1)),
            changed(&|_, r, _| r[0].name = None),
            changed(&|_, r, _| r[0].highway = "service".into()),
            changed(&|_, r, _| r[0].width_m = 4.0),
            // The segment leaving the area ends there, so moving it moves the
            // road inside.
            changed(&|_, r, _| r[0].points[1].lon = 0.004),
            changed(&|_, _, a| a[0].address.unit = Some("B".into())),
            changed(&|_, _, a| a.clear()),
        ];
        let mut all: Vec<u64> = variants.to_vec();
        all.push(base);
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), variants.len() + 1, "every change shows");

        // Far along the road, beyond the segment that leaves the area, and
        // a road that never comes near: no change here.
        let unchanged = [
            changed(&|_, r, _| r[0].points[2].lon = 0.4),
            changed(&|_, r, _| r[0].points.push(LatLon::new(1.0, 1.0))),
            changed(&|_, r, _| {
                let mut far = r[0].clone();
                far.osm_id = 8;
                far.points = vec![LatLon::new(1.0, 1.0), LatLon::new(1.0, 2.0)];
                r.push(far);
            }),
        ];
        assert_eq!(unchanged, [base; 3]);
        // An empty area differs from this one, and is pinned too.
        let empty = osm_fingerprint(&area, &[], &[], &[]);
        assert_ne!(empty, base);
        assert_eq!(empty, 0xa8c7_f832_281a_39c5, "{empty:016x}");
    }

    /// A 300 m box around [`sample_features`]' buildings.
    fn sample_area() -> BBox {
        BBox::around(LatLon::new(0.0005, 0.0005), 300.0)
    }
}
