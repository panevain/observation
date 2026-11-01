//! The `AreaBundle` wire format the server sends and the client renders.
//!
//! Layout: `b"OBSB"` magic, `u16` little-endian format version, then a
//! postcard-encoded [`AreaBundle`].
//!
//! Version 3 added the [`Terrain`] grid and [`Footprint::base_m`]; version 4
//! the OSM data date ([`AreaBundle::osm_base_unix`]) and
//! [`AreaBundle::osm_fingerprint`].

use crate::address::{
    Address, AddressPoint, AddressSource, MAX_ADDRESS_DISTANCE_M, building_of, dist,
    distance_to_footprint, near_bounds, ring_bounds,
};
use crate::building::{Footprint, MeshData, extrude};
use crate::geo::{BBox, LatLon, Projection};
use crate::provenance::Fnv64;
use crate::terrain::Terrain;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const BUNDLE_MAGIC: [u8; 4] = *b"OBSB";
pub const BUNDLE_VERSION: u16 = 4;
/// MIME type used on the wire.
pub const BUNDLE_CONTENT_TYPE: &str = "application/vnd.observation.bundle";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Building {
    pub osm_id: i64,
    pub footprint: Footprint,
    /// `[first_index, index_count]` into [`AreaBundle::mesh`] indices.
    pub index_range: [u32; 2],
    /// From the building's own `addr:*` tags.
    pub address: Option<Address>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Road {
    pub osm_id: i64,
    /// OSM `highway=*` value.
    pub highway: String,
    pub name: Option<String>,
    /// Carriageway width used for street-coverage statistics, metres.
    pub width_m: f32,
    /// Polyline in world `[x, z]`.
    pub points: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaBundle {
    pub bbox: BBox,
    /// World-frame origin (the bbox centre).
    pub origin: LatLon,
    pub buildings: Vec<Building>,
    pub roads: Vec<Road>,
    pub mesh: MeshData,
    /// Addresses mapped as nodes rather than on a building.
    pub addresses: Vec<AddressPoint>,
    /// Ground heights over the area and its margin (flat without
    /// elevation data).
    pub terrain: Terrain,
    pub attribution: String,
    /// When the server built this bundle, Unix seconds (0 if unknown). Not
    /// when the map data was current: see [`Self::osm_base_unix`].
    pub generated_unix: u64,
    /// When the OSM data was current, Unix seconds: Overpass's
    /// `osm3s.timestamp_osm_base`, the time of the last change its database
    /// holds. `None` when the source did not say.
    pub osm_base_unix: Option<u64>,
    /// [`osm_fingerprint`] of the map features the bundle was built from, so
    /// a saved scene can tell whether its area changed (the data date alone
    /// cannot: public Overpass's moves every minute).
    pub osm_fingerprint: u64,
}

#[derive(Debug, Error, PartialEq)]
pub enum BundleError {
    #[error("the reply is not an area bundle (a proxy or sign-in page may have answered)")]
    Magic,
    #[error("the server sent bundle version {0}, but this app reads version {BUNDLE_VERSION}")]
    Version(u16),
    #[error("the area data is damaged or cut short ({0})")]
    Decode(String),
}

impl AreaBundle {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.mesh.positions.len() * 24);
        out.extend_from_slice(&BUNDLE_MAGIC);
        out.extend_from_slice(&BUNDLE_VERSION.to_le_bytes());
        // Serialising plain data into a Vec cannot fail.
        postcard::to_extend(self, out).unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, BundleError> {
        if bytes.len() < 6 || bytes[..4] != BUNDLE_MAGIC {
            return Err(BundleError::Magic);
        }
        let v = u16::from_le_bytes([bytes[4], bytes[5]]);
        if v != BUNDLE_VERSION {
            return Err(BundleError::Version(v));
        }
        let bundle: Self =
            postcard::from_bytes(&bytes[6..]).map_err(|e| BundleError::Decode(e.to_string()))?;
        bundle
            .terrain
            .validate()
            .map_err(|_| BundleError::Decode("inconsistent terrain grid".to_owned()))?;
        Ok(bundle)
    }

    pub fn projection(&self) -> Projection {
        Projection::new(self.origin)
    }

    /// Footprints only (for occlusion), in bundle order.
    pub fn footprints(&self) -> impl Iterator<Item = &Footprint> {
        self.buildings.iter().map(|b| &b.footprint)
    }

    /// The address a camera at world `[x, z]` belongs to (see
    /// [`crate::address::address_at`]).
    pub fn address_at(&self, p: [f32; 2]) -> Option<&Address> {
        let footprint = |i: u32| self.buildings.get(i as usize).map(|b| &b.footprint);
        let buildings = self.buildings.iter().filter_map(|b| {
            b.address
                .as_ref()
                .map(|a| AddressSource::Building(&b.footprint, a))
        });
        let points = self.addresses.iter().map(AddressSource::Point);
        crate::address::address_at(p, footprint, buildings.chain(points))
    }

    /// The building nearest world `[x, z]` within `max` metres (0 inside
    /// it), if any.
    pub fn building_near(&self, p: [f32; 2], max: f32) -> Option<usize> {
        self.buildings
            .iter()
            .enumerate()
            .filter(|(_, b)| near_bounds(p, &b.footprint.outer, max))
            .map(|(i, b)| (i, distance_to_footprint(p, &b.footprint)))
            .filter(|(_, d)| *d <= max)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The building an address point belongs to: the one it lies in, else
    /// (a node in the yard, as some imports place them) the nearest building
    /// within [`MAX_ADDRESS_DISTANCE_M`].
    fn owner_of(&self, a: &AddressPoint) -> Option<usize> {
        match a.building {
            Some(i) => Some(i as usize).filter(|&i| i < self.buildings.len()),
            None => self.building_near(a.position, MAX_ADDRESS_DISTANCE_M),
        }
    }

    /// The address of building `i` itself, for a pointer at `p` on it: its
    /// own, else the closest to `p` of the address points it owns (see
    /// [`Self::owner_of`]). Never a neighbour's, unlike [`Self::address_at`].
    pub fn building_address(&self, i: usize, p: [f32; 2]) -> Option<&Address> {
        let b = self.buildings.get(i)?;
        if let Some(a) = &b.address {
            return Some(a);
        }
        // Points inside the building first, then those in its yard.
        self.addresses
            .iter()
            .filter(|a| a.building == Some(i as u32))
            .min_by(|a, b| dist(p, a.position).total_cmp(&dist(p, b.position)))
            .or_else(|| {
                self.addresses
                    .iter()
                    .filter(|a| a.building.is_none())
                    .filter(|a| near_bounds(a.position, &b.footprint.outer, MAX_ADDRESS_DISTANCE_M))
                    .filter(|a| self.owner_of(a) == Some(i))
                    .min_by(|a, c| {
                        distance_to_footprint(a.position, &b.footprint)
                            .total_cmp(&distance_to_footprint(c.position, &b.footprint))
                    })
            })
            .map(|a| &a.address)
    }

    /// The address under a pointer at world `[x, z]`: on a building (within
    /// `on_building` metres of it, so a ray that met a wall counts), that
    /// building's own address or none; elsewhere the nearest, as
    /// [`Self::address_at`].
    pub fn address_pointed_at(&self, p: [f32; 2], on_building: f32) -> Option<&Address> {
        match self.building_near(p, on_building) {
            Some(i) => self.building_address(i, p),
            None => self.address_at(p),
        }
    }

    /// Indices of the buildings at an address `is` accepts: those tagged with
    /// it, and those owning an address point with it (see
    /// [`Self::owner_of`]). Ascending, no repeats.
    pub fn buildings_at(&self, is: impl Fn(&Address) -> bool) -> Vec<usize> {
        let tagged = self
            .buildings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.address.as_ref().is_some_and(&is))
            .map(|(i, _)| i);
        let owning = self
            .addresses
            .iter()
            .filter(|p| is(&p.address))
            .filter_map(|p| self.owner_of(p));
        let mut out: Vec<usize> = tagged.chain(owning).collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// A building as parsed from OSM, before projection.
#[derive(Debug, Clone, PartialEq)]
pub struct OsmBuilding {
    pub osm_id: i64,
    pub outer: Vec<LatLon>,
    pub holes: Vec<Vec<LatLon>>,
    pub min_height_m: f32,
    pub height_m: f32,
    pub address: Option<Address>,
}

/// An address node as parsed from OSM, before projection.
#[derive(Debug, Clone, PartialEq)]
pub struct OsmAddressPoint {
    pub address: Address,
    pub point: LatLon,
}

/// A road as parsed from OSM, before projection.
#[derive(Debug, Clone, PartialEq)]
pub struct OsmRoad {
    pub osm_id: i64,
    pub highway: String,
    pub name: Option<String>,
    pub width_m: f32,
    pub points: Vec<LatLon>,
}

/// `highway=*` values for ways people walk or cycle on rather than drive.
pub const FOOT_WAYS: [&str; 5] = ["footway", "path", "pedestrian", "cycleway", "steps"];

/// Whether a road of this `highway=*` type can give a house its street name:
/// not motorways or their links, and of the ways people walk or cycle on, only
/// pedestrian streets.
fn names_addresses(highway: &str) -> bool {
    !(highway == "motorway"
        || highway.ends_with("_link")
        || (FOOT_WAYS.contains(&highway) && highway != "pedestrian"))
}

/// Estimated carriageway width from OSM `highway`, `lanes` and `width` tags.
/// Returns `None` for highway types that are not part of the street surface.
pub fn road_width_m(highway: &str, lanes: Option<&str>, width: Option<&str>) -> Option<f32> {
    let base = match highway {
        "motorway" | "trunk" => 14.0,
        "primary" => 10.0,
        "secondary" => 8.0,
        "tertiary" => 7.0,
        "residential" | "unclassified" => 6.0,
        "living_street" => 5.0,
        "service" => 4.0,
        h if FOOT_WAYS.contains(&h) => 2.0,
        "motorway_link" | "trunk_link" | "primary_link" | "secondary_link" | "tertiary_link" => 6.0,
        _ => return None,
    };
    if let Some(w) = width
        .and_then(crate::building::parse_length_m)
        .filter(|w| (1.0..60.0).contains(w))
    {
        return Some(w);
    }
    if let Some(l) = lanes
        .and_then(|l| l.trim().parse::<f32>().ok())
        .filter(|l| (1.0..12.0).contains(l))
    {
        return Some(l * 3.25);
    }
    Some(base)
}

/// Projects, extrudes and packs OSM features into a bundle, standing each
/// building on the lowest ground under it.
pub fn build_bundle(
    bbox: BBox,
    buildings: &[OsmBuilding],
    roads: &[OsmRoad],
    addresses: &[OsmAddressPoint],
    terrain: Terrain,
    attribution: impl Into<String>,
    generated_unix: u64,
    osm_base_unix: Option<u64>,
) -> AreaBundle {
    let osm_fingerprint = osm_fingerprint(buildings, roads, addresses);
    let origin = bbox.center();
    let proj = Projection::new(origin);
    let to_w = |p: &LatLon| {
        let w = proj.to_world(*p);
        [w[0] as f32, w[1] as f32]
    };
    let mut mesh = MeshData::default();
    let mut out = Vec::with_capacity(buildings.len());
    for b in buildings {
        let outer: Vec<[f32; 2]> = b.outer.iter().map(to_w).collect();
        let footprint = Footprint {
            base_m: terrain.min_under(&outer),
            outer,
            holes: b
                .holes
                .iter()
                .map(|h| h.iter().map(to_w).collect())
                .collect(),
            min_height_m: b.min_height_m,
            height_m: b.height_m,
        };
        let start = mesh.indices.len() as u32;
        if extrude(&footprint, &mut mesh) {
            let footprint = Footprint {
                outer: crate::building::clean_ring(&footprint.outer),
                holes: footprint
                    .holes
                    .iter()
                    .map(|h| crate::building::clean_ring(h))
                    .collect(),
                ..footprint
            };
            out.push(Building {
                osm_id: b.osm_id,
                footprint,
                index_range: [start, mesh.indices.len() as u32 - start],
                address: b.address.clone(),
            });
        }
    }
    let roads: Vec<Road> = roads
        .iter()
        .filter(|r| r.points.len() >= 2)
        .map(|r| Road {
            osm_id: r.osm_id,
            highway: r.highway.clone(),
            name: r.name.clone(),
            width_m: r.width_m,
            points: r.points.iter().map(to_w).collect(),
        })
        .collect();
    let streets: Vec<(&str, &[[f32; 2]])> = roads
        .iter()
        .filter(|r| names_addresses(&r.highway))
        .filter_map(|r| Some((r.name.as_deref()?, r.points.as_slice())))
        .collect();
    for b in &mut out {
        if let Some(a) = &mut b.address {
            let (lo, hi) = ring_bounds(&b.footprint.outer);
            let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
            a.fill_street(centre, streets.iter().copied());
        }
    }
    let footprints: Vec<&Footprint> = out.iter().map(|b| &b.footprint).collect();
    let bounds: Vec<_> = footprints.iter().map(|f| ring_bounds(&f.outer)).collect();
    let addresses = addresses
        .iter()
        .map(|a| {
            let position = to_w(&a.point);
            let building = building_of(position, &footprints, &bounds);
            let mut address = a.address.clone();
            address.fill_street(position, streets.iter().copied());
            AddressPoint {
                address,
                position,
                building: building.map(|i| i as u32),
            }
        })
        .collect();
    AreaBundle {
        bbox,
        origin,
        buildings: out,
        roads,
        mesh,
        addresses,
        terrain,
        attribution: attribution.into(),
        generated_unix,
        osm_base_unix,
        osm_fingerprint,
    }
}

/// A fingerprint of the map features an area is built from: every
/// building's id, rings, heights and address, every road's id, type, name,
/// width and points, and every address point. Equal for the same features in
/// any order, and different (barring a 1 in 2^64 chance) when any of them
/// changes, so a saved scene can tell whether its area's map data changed.
///
/// It covers what the app read from OSM, after height inference and road
/// widths: a version of the app that reads the same data differently gives a
/// different fingerprint. Terrain is not included.
pub fn osm_fingerprint(
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
    let roads = roads.iter().map(|r| {
        let mut h = Fnv64::default();
        h.bytes(b"road")
            .i64(r.osm_id)
            .str(&r.highway)
            .opt_str(r.name.as_deref())
            .f32(r.width_m);
        points(&mut h, &r.points);
        h.finish()
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample() -> AreaBundle {
        sample_on(Terrain::default())
    }

    /// [`sample`] on the given ground.
    pub(crate) fn sample_on(terrain: Terrain) -> AreaBundle {
        let bbox = BBox::around(LatLon::new(51.5, -0.12), 200.0);
        let c = bbox.center();
        let d = 0.0001;
        let b = OsmBuilding {
            osm_id: 42,
            outer: vec![
                LatLon::new(c.lat, c.lon),
                LatLon::new(c.lat, c.lon + d),
                LatLon::new(c.lat + d, c.lon + d),
                LatLon::new(c.lat + d, c.lon),
                LatLon::new(c.lat, c.lon),
            ],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.5,
            address: None,
        };
        let r = OsmRoad {
            osm_id: 7,
            highway: "residential".into(),
            name: Some("Test St".into()),
            width_m: 6.0,
            points: vec![
                LatLon::new(c.lat - d, c.lon - d),
                LatLon::new(c.lat - d, c.lon + 2.0 * d),
            ],
        };
        let a = OsmAddressPoint {
            address: Address {
                housenumber: Some("12".into()),
                street: Some("Test St".into()),
                ..Address::default()
            },
            point: LatLon::new(c.lat + d / 2.0, c.lon + d / 2.0),
        };
        build_bundle(
            bbox,
            &[b],
            &[r],
            &[a],
            terrain,
            "© OpenStreetMap contributors",
            1_700_000_000,
            Some(1_699_990_000),
        )
    }

    #[test]
    fn round_trip() {
        let b = sample();
        assert_eq!(b.buildings.len(), 1);
        assert_eq!(b.buildings[0].index_range[0], 0);
        assert_eq!(b.buildings[0].index_range[1] as usize, b.mesh.indices.len());
        // The address node lies inside the building.
        assert_eq!(b.addresses[0].building, Some(0));
        let wall = b.buildings[0].footprint.outer[0];
        assert_eq!(
            b.address_at([wall[0], wall[1] + 0.15]).map(Address::label),
            Some("12 Test St".to_owned())
        );
        assert_eq!(b.buildings_at(|a| a.label() == "12 Test St"), vec![0]);
        assert!(b.buildings_at(|a| a.label() == "14 Test St").is_empty());
        // A building tagged with the address itself, also holding the point.
        let mut tagged = b.clone();
        let own = Address {
            housenumber: Some("12".into()),
            street: Some("Test St".into()),
            ..Address::default()
        };
        tagged.buildings[0].address = Some(own.clone());
        tagged.addresses.clear();
        assert_eq!(tagged.buildings_at(|a| *a == own), vec![0]);
        let bytes = b.encode();
        assert_eq!(&bytes[..4], b"OBSB");
        assert_eq!(AreaBundle::decode(&bytes).unwrap(), b);
        assert_eq!(b.osm_base_unix, Some(1_699_990_000));
        assert_ne!(b.osm_fingerprint, 0);
    }

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
        let road = OsmRoad {
            osm_id: 7,
            highway: "residential".into(),
            name: Some("Test St".into()),
            width_m: 6.0,
            points: vec![p(0.0, 0.0), p(0.0, 1.0)],
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
        let base = osm_fingerprint(&b, &r, &a);
        // Pinned: a fingerprint is saved in scene files, so it must not drift
        // between releases for the same features.
        assert_eq!(base, 0x6499_895e_529d_4893, "{base:016x}");
        let mut swapped = b.clone();
        swapped.reverse();
        assert_eq!(osm_fingerprint(&swapped, &r, &a), base);

        let changed = |f: &Edit| {
            let (mut b, mut r, mut a) = sample_features();
            f(&mut b, &mut r, &mut a);
            osm_fingerprint(&b, &r, &a)
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
            changed(&|_, _, a| a[0].address.unit = Some("B".into())),
            changed(&|_, _, a| a.clear()),
        ];
        let mut all: Vec<u64> = variants.to_vec();
        all.push(base);
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), variants.len() + 1, "every change shows");
        assert_eq!(
            osm_fingerprint(&[], &[], &[]),
            osm_fingerprint(&[], &[], &[])
        );
    }

    #[test]
    fn buildings_stand_on_the_lowest_ground_under_them() {
        // Falling 1 m per 10 m eastward; the house spans x = 0 to about 7.4.
        let terrain = Terrain::sample_from(
            glam::Vec2::splat(-300.0),
            glam::Vec2::splat(300.0),
            |p| Some(50.0 - p.x / 10.0),
            "dem",
        )
        .unwrap();
        let b = sample_on(terrain);
        let fp = &b.buildings[0].footprint;
        let east = fp.outer.iter().map(|p| p[0]).fold(f32::MIN, f32::max);
        assert!((fp.base_m + east / 10.0).abs() < 0.06, "{}", fp.base_m);
        let ys = || b.mesh.positions.iter().map(|p| p[1]);
        assert!((ys().fold(f32::MAX, f32::min) - fp.base_m).abs() < 1e-4);
        assert!((ys().fold(f32::MIN, f32::max) - fp.roof_y()).abs() < 1e-4);
        assert_eq!(AreaBundle::decode(&b.encode()).unwrap(), b);
        // A grid whose size does not match its samples is refused.
        let mut bad = b.clone();
        bad.terrain.heights_dm.pop();
        assert!(matches!(
            AreaBundle::decode(&bad.encode()),
            Err(BundleError::Decode(_))
        ));
    }

    /// Two houses side by side: `1` tagged on its building, `2` only as a
    /// node in its yard, nearer house 1's wall than the node is.
    fn neighbours() -> AreaBundle {
        let square = |x: f32| Footprint {
            outer: vec![[x, 0.0], [x + 10.0, 0.0], [x + 10.0, 10.0], [x, 10.0]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.0,
            base_m: 0.0,
        };
        let addr = |n: &str| Address {
            housenumber: Some(n.into()),
            street: Some("Main St".into()),
            ..Address::default()
        };
        let building = |x: f32, address| Building {
            osm_id: 1,
            footprint: square(x),
            index_range: [0, 0],
            address,
        };
        AreaBundle {
            bbox: BBox::around(LatLon::new(51.5, -0.12), 200.0),
            origin: LatLon::new(51.5, -0.12),
            buildings: vec![building(0.0, Some(addr("1"))), building(12.0, None)],
            roads: vec![],
            mesh: MeshData::default(),
            addresses: vec![AddressPoint {
                address: addr("2"),
                position: [17.0, -6.0],
                building: None,
            }],
            terrain: Terrain::default(),
            attribution: String::new(),
            generated_unix: 0,
            osm_base_unix: None,
            osm_fingerprint: 0,
        }
    }

    #[test]
    fn pointing_at_a_building_gives_its_own_address() {
        let b = neighbours();
        let label = |a: Option<&Address>| a.map(Address::label);
        // The camera rule picks house 1's wall, 7 m away, over the node 11 m away.
        assert_eq!(label(b.address_at([17.0, 5.0])), Some("1 Main St".into()));
        assert_eq!(
            label(b.address_pointed_at([17.0, 5.0], 0.5)),
            Some("2 Main St".into())
        );
        // On a wall, and on the first house.
        assert_eq!(
            label(b.address_pointed_at([12.0, 5.0], 0.5)),
            Some("2 Main St".into())
        );
        assert_eq!(
            label(b.address_pointed_at([5.0, 5.0], 0.5)),
            Some("1 Main St".into())
        );
        // Off the buildings: the nearest address.
        assert_eq!(
            label(b.address_pointed_at([17.0, -7.0], 0.5)),
            Some("2 Main St".into())
        );
        assert_eq!(b.buildings_at(|a| a.label() == "2 Main St"), vec![1]);
        assert_eq!(b.buildings_at(|a| a.label() == "1 Main St"), vec![0]);
        // A house with nothing of its own is not given a neighbour's.
        let mut bare = b.clone();
        bare.addresses.clear();
        assert_eq!(bare.address_pointed_at([17.0, 5.0], 0.5), None);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(AreaBundle::decode(b"nope"), Err(BundleError::Magic));
        let mut bytes = sample().encode();
        bytes[4] = 99;
        assert_eq!(AreaBundle::decode(&bytes), Err(BundleError::Version(99)));
        let mut bytes = sample().encode();
        bytes.truncate(20);
        assert!(matches!(
            AreaBundle::decode(&bytes),
            Err(BundleError::Decode(_))
        ));
    }

    #[test]
    fn road_widths() {
        assert_eq!(road_width_m("residential", None, None), Some(6.0));
        assert_eq!(road_width_m("residential", Some("2"), None), Some(6.5));
        assert_eq!(road_width_m("primary", Some("4"), Some("12 m")), Some(12.0));
        assert_eq!(road_width_m("proposed", None, None), None);
    }
}
