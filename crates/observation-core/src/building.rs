//! Building footprints and their extrusion into a mesh.

pub use crate::osm::tags::{STOREY_M, default_height_for, infer_heights, parse_length_m};
pub use crate::util::planar::{clean_ring, signed_area2};
use serde::{Deserialize, Serialize};

/// Render/occlusion-ready mesh in the world frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn push_vertex(&mut self, p: [f32; 3], n: [f32; 3]) -> u32 {
        let i = self.positions.len() as u32;
        self.positions.push(p);
        self.normals.push(n);
        i
    }

    /// Pushes a triangle, flipping winding if needed so it is
    /// counter-clockwise when viewed from the side `n` points to.
    fn push_tri(&mut self, a: u32, b: u32, c: u32, n: [f32; 3]) {
        let p = |i: u32| glam::Vec3::from_array(self.positions[i as usize]);
        let face = (p(b) - p(a)).cross(p(c) - p(a));
        if face.dot(glam::Vec3::from_array(n)) >= 0.0 {
            self.indices.extend([a, b, c]);
        } else {
            self.indices.extend([a, c, b]);
        }
    }
}

/// A building footprint in world `[x, z]` coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    pub outer: Vec<[f32; 2]>,
    #[serde(default)]
    pub holes: Vec<Vec<[f32; 2]>>,
    /// Bottom of the building part, metres above [`Self::base_m`].
    pub min_height_m: f32,
    /// Roof, metres above [`Self::base_m`].
    pub height_m: f32,
    /// World `y` of the ground the building stands on: the lowest ground
    /// under the footprint, so it never floats.
    #[serde(default)]
    pub base_m: f32,
}

impl Footprint {
    /// World `y` of the bottom of the solid.
    pub fn bottom_y(&self) -> f32 {
        self.base_m + self.min_height_m
    }

    /// World `y` of the roof.
    pub fn roof_y(&self) -> f32 {
        self.base_m + self.height_m
    }
}

/// Appends walls and a flat roof (plus underside for raised parts) to `mesh`.
/// Returns `false` (and appends nothing) for degenerate footprints.
pub fn extrude(fp: &Footprint, mesh: &mut MeshData) -> bool {
    let outer = clean_ring(&fp.outer);
    if outer.len() < 3 || signed_area2(&outer).abs() < 1e-3 || fp.height_m <= fp.min_height_m {
        return false;
    }
    let holes: Vec<Vec<[f32; 2]>> = fp
        .holes
        .iter()
        .map(|h| clean_ring(h))
        .filter(|h| h.len() >= 3)
        .collect();
    let (y0, y1) = (fp.bottom_y(), fp.roof_y());

    // Walls: outward normal; holes face into the hole.
    let mut wall = |ring: &[[f32; 2]], is_hole: bool| {
        let sign = signed_area2(ring).signum() * if is_hole { -1.0 } else { 1.0 };
        for i in 0..ring.len() {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len = (dx * dx + dz * dz).sqrt();
            if len < 1e-4 {
                continue;
            }
            let n = [sign * dz / len, 0.0, -sign * dx / len];
            let v = [
                mesh.push_vertex([a[0], y0, a[1]], n),
                mesh.push_vertex([b[0], y0, b[1]], n),
                mesh.push_vertex([b[0], y1, b[1]], n),
                mesh.push_vertex([a[0], y1, a[1]], n),
            ];
            mesh.push_tri(v[0], v[1], v[2], n);
            mesh.push_tri(v[0], v[2], v[3], n);
        }
    };
    wall(&outer, false);
    for h in &holes {
        wall(h, true);
    }

    // Roof (and underside for raised building parts) via earcut.
    let mut flat: Vec<[f64; 2]> = outer
        .iter()
        .map(|p| [f64::from(p[0]), f64::from(p[1])])
        .collect();
    let mut hole_idx: Vec<u32> = Vec::with_capacity(holes.len());
    for h in &holes {
        hole_idx.push(flat.len() as u32);
        flat.extend(h.iter().map(|p| [f64::from(p[0]), f64::from(p[1])]));
    }
    let mut tris: Vec<u32> = Vec::new();
    earcut::Earcut::new().earcut(flat.iter().copied(), &hole_idx, &mut tris);
    let mut cap = |y: f32, n: [f32; 3]| {
        let base = mesh.positions.len() as u32;
        for p in &flat {
            mesh.push_vertex([p[0] as f32, y, p[1] as f32], n);
        }
        for t in tris.as_chunks::<3>().0 {
            mesh.push_tri(base + t[0], base + t[1], base + t[2], n);
        }
    };
    cap(y1, [0.0, 1.0, 0.0]);
    if fp.min_height_m > 0.05 {
        cap(y0, [0.0, -1.0, 0.0]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    fn square(s: f32) -> Vec<[f32; 2]> {
        vec![[0.0, 0.0], [s, 0.0], [s, s], [0.0, s], [0.0, 0.0]]
    }

    #[test]
    fn extrude_box_has_outward_normals_and_ccw_faces() {
        for ring in [square(10.0), square(10.0).into_iter().rev().collect()] {
            let fp = Footprint {
                outer: ring,
                holes: vec![],
                min_height_m: 0.0,
                height_m: 5.0,
                base_m: 0.0,
            };
            let mut m = MeshData::default();
            assert!(extrude(&fp, &mut m));
            // 4 walls * 2 tris + 2 roof tris
            assert_eq!(m.indices.len(), (4 * 2 + 2) * 3);
            let centre = glam::Vec3::new(5.0, 2.5, 5.0);
            for t in m.indices.as_chunks::<3>().0 {
                let p = |i: u32| glam::Vec3::from_array(m.positions[i as usize]);
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                let face = (b - a).cross(c - a).normalize();
                let n = glam::Vec3::from_array(m.normals[t[0] as usize]);
                assert!(face.dot(n) > 0.99, "winding matches normal");
                assert!(
                    n.dot((a + b + c) / 3.0 - centre) > 0.0,
                    "normal points outward"
                );
            }
        }
    }

    #[test]
    fn extrude_with_hole_and_raised_part() {
        let fp = Footprint {
            outer: square(20.0),
            holes: vec![vec![[5.0, 5.0], [15.0, 5.0], [15.0, 15.0], [5.0, 15.0]]],
            min_height_m: 3.0,
            height_m: 9.0,
            base_m: 0.0,
        };
        let mut m = MeshData::default();
        assert!(extrude(&fp, &mut m));
        assert!(m.positions.iter().all(|p| p[1] == 3.0 || p[1] == 9.0));
        assert!(m.normals.iter().any(|n| n[1] < -0.5), "underside present");
        let degenerate = Footprint {
            outer: vec![[0.0, 0.0], [1.0, 1.0]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 3.0,
            base_m: 0.0,
        };
        assert!(!extrude(&degenerate, &mut m));
    }
}
