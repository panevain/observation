//! Which building and address a point in the area belongs to.

use super::AreaBundle;
use crate::address::{
    Address, AddressPoint, AddressSource, MAX_ADDRESS_DISTANCE_M, distance_to_footprint,
    near_bounds,
};
use crate::util::planar::dist;

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
}
