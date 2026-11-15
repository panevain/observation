//! Which building and address a point in the area belongs to.

use super::AreaBundle;
use crate::address::{
    Address, AddressPoint, AddressSource, MAX_ADDRESS_DISTANCE_M, distance_to_footprint,
    near_bounds,
};
use crate::road::Road;
use crate::util::planar::{closest_on_segment, dist};
use glam::Vec2;

/// A road near a point (see [`AreaBundle::nearest_road`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoadNear {
    /// The road's index in [`AreaBundle::roads`].
    pub index: usize,
    /// The point of its centre line nearest the point asked about, world
    /// `[x, z]`.
    pub point: [f32; 2],
    /// The distance between the two, metres.
    pub distance_m: f32,
}

impl AreaBundle {
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
    /// own, else the closest to `p` of the address points it owns (the one
    /// each lies in, else the nearest building within
    /// [`MAX_ADDRESS_DISTANCE_M`]). Never a neighbour's, unlike
    /// [`Self::address_at`].
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

    /// The road whose centre line passes nearest world `[x, z]` `p`, among
    /// those `accept` accepts (the first in bundle order on a tie), if any.
    /// Street names are public; this never gives a building's address.
    pub fn nearest_road(&self, p: [f32; 2], accept: impl Fn(&Road) -> bool) -> Option<RoadNear> {
        let at = Vec2::from_array(p);
        let mut best: Option<RoadNear> = None;
        for (index, road) in self.roads.iter().enumerate() {
            if !accept(road) {
                continue;
            }
            for seg in road.points.windows(2) {
                let c = closest_on_segment(at, Vec2::from_array(seg[0]), Vec2::from_array(seg[1]));
                let distance_m = c.distance(at);
                if best.is_none_or(|b| distance_m < b.distance_m) {
                    best = Some(RoadNear {
                        index,
                        point: c.to_array(),
                        distance_m,
                    });
                }
            }
        }
        best
    }

    /// Indices of the buildings at an address `is` accepts: those tagged with
    /// it, and those owning an address point with it (as in
    /// [`Self::building_address`]). Ascending, no repeats.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::building::MeshData;
    use crate::bundle::Building;
    use crate::geo::{BBox, LatLon};
    use crate::terrain::Terrain;
    use crate::testing::box_footprint;

    /// Two houses side by side: `1` tagged on its building, `2` only as a
    /// node in its yard, nearer house 1's wall than the node is.
    fn neighbours() -> AreaBundle {
        let square = |x: f32| box_footprint(x, 0.0, x + 10.0, 10.0, 6.0);
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
    fn the_nearest_road_and_the_nearest_named_one() {
        let mut b = neighbours();
        assert_eq!(b.nearest_road([0.0, 0.0], |_| true), None);
        let road = |name: Option<&str>, z: f32| Road {
            osm_id: 1,
            highway: "residential".into(),
            name: name.map(Into::into),
            width_m: 6.0,
            points: vec![[-50.0, z], [0.0, z], [50.0, z]],
        };
        b.roads = vec![road(None, -5.0), road(Some("Main St"), -20.0)];
        let near = b.nearest_road([30.0, -8.0], |_| true).expect("a road");
        assert_eq!(near.index, 0);
        assert!((Vec2::from_array(near.point) - Vec2::new(30.0, -5.0)).length() < 1e-4);
        assert!((near.distance_m - 3.0).abs() < 1e-4);
        let named = b
            .nearest_road([30.0, -8.0], |r| r.name.is_some())
            .expect("a named road");
        assert_eq!(named.index, 1);
        assert!((named.distance_m - 12.0).abs() < 1e-4);
    }
}
