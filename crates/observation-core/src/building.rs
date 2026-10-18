//! Building height inference from OSM tags and footprint extrusion.

use serde::{Deserialize, Serialize};

/// Assumed storey height, metres.
pub const STOREY_M: f32 = 3.0;

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
    pub min_height_m: f32,
    pub height_m: f32,
}

/// Parses an OSM length value (`"12"`, `"12 m"`, `"12.5m"`, `"40'"`, `"40 ft"`, `"12'6\""`) to metres.
pub fn parse_length_m(raw: &str) -> Option<f32> {
    let s = raw.trim().replace(',', ".");
    if let Some((ft, rest)) = s.split_once('\'') {
        let ft: f32 = ft.trim().parse().ok()?;
        let inches = rest.trim().trim_end_matches('"').trim();
        let inches: f32 = if inches.is_empty() {
            0.0
        } else {
            inches.parse().ok()?
        };
        return Some(ft * 0.3048 + inches * 0.0254);
    }
    let lower = s.to_ascii_lowercase();
    let (num, factor) = if let Some(n) = lower.strip_suffix("ft") {
        (n, 0.3048)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 1.0)
    } else {
        (lower.as_str(), 1.0)
    };
    let v: f32 = num.trim().parse().ok()?;
    (v.is_finite() && v >= 0.0).then_some(v * factor)
}

/// Default total height for a `building=*` value when no height tags exist.
pub fn default_height_for(building: &str) -> f32 {
    match building {
        "shed" | "garage" | "garages" | "carport" | "hut" | "roof" | "kiosk" | "cabin" => 3.0,
        "bungalow" | "static_caravan" => 4.0,
        "house" | "detached" | "semidetached_house" | "terrace" | "residential" | "farm" => 6.5,
        "apartments" | "dormitory" | "hotel" => 10.0,
        "church" | "cathedral" => 12.0,
        _ => 6.0,
    }
}

/// Infers `(min_height, height)` in metres from OSM tags.
///
/// Priority: `height` → `building:levels`(+`roof:levels`) → building-type default.
/// `min_height` / `building:min_level` give the base for building parts.
pub fn infer_heights<'a>(tag: impl Fn(&str) -> Option<&'a str>) -> (f32, f32) {
    let levels = |k: &str| {
        tag(k)
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    let min = tag("min_height")
        .and_then(parse_length_m)
        .or_else(|| levels("building:min_level").map(|l| l * STOREY_M))
        .unwrap_or(0.0);
    let height = tag("height")
        .and_then(parse_length_m)
        .filter(|h| *h > 0.5)
        .or_else(|| {
            levels("building:levels").filter(|l| *l > 0.0).map(|l| {
                let roof = levels("roof:levels").unwrap_or(0.0);
                (l + roof) * STOREY_M + if roof == 0.0 { 1.0 } else { 0.0 }
            })
        })
        .unwrap_or_else(|| default_height_for(tag("building").unwrap_or("yes")));
    let height = height.clamp(1.0, 300.0);
    (min.min(height - 0.5).max(0.0), height)
}

/// Removes a closing duplicate point and consecutive duplicates.
pub fn clean_ring(ring: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(ring.len());
    for &p in ring {
        if out
            .last()
            .is_none_or(|q| (q[0] - p[0]).abs() > 1e-4 || (q[1] - p[1]).abs() > 1e-4)
        {
            out.push(p);
        }
    }
    while out.len() > 1 {
        let (f, l) = (out[0], out[out.len() - 1]);
        if (f[0] - l[0]).abs() <= 1e-4 && (f[1] - l[1]).abs() <= 1e-4 {
            out.pop();
        } else {
            break;
        }
    }
    out
}

/// Twice the signed area of a ring in the `[x, z]` plane.
pub fn signed_area2(ring: &[[f32; 2]]) -> f32 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
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
    let (y0, y1) = (fp.min_height_m, fp.height_m);

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
        for t in tris.chunks_exact(3) {
            mesh.push_tri(base + t[0], base + t[1], base + t[2], n);
        }
    };
    cap(y1, [0.0, 1.0, 0.0]);
    if y0 > 0.05 {
        cap(y0, [0.0, -1.0, 0.0]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn infer(t: &HashMap<String, String>) -> (f32, f32) {
        infer_heights(|k| t.get(k).map(String::as_str))
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_length_m("12"), Some(12.0));
        assert_eq!(parse_length_m("12.5 m"), Some(12.5));
        assert!((parse_length_m("10'").unwrap() - 3.048).abs() < 1e-4);
        assert!((parse_length_m("10 ft").unwrap() - 3.048).abs() < 1e-4);
        assert!((parse_length_m("5'6\"").unwrap() - 1.6764).abs() < 1e-3);
        assert_eq!(parse_length_m("tall"), None);
    }

    #[test]
    fn height_priority() {
        assert_eq!(
            infer(&tags(&[("building", "house"), ("height", "8")])),
            (0.0, 8.0)
        );
        assert_eq!(
            infer(&tags(&[("building", "house"), ("building:levels", "2")])),
            (0.0, 7.0)
        );
        assert_eq!(
            infer(&tags(&[
                ("building", "house"),
                ("building:levels", "2"),
                ("roof:levels", "1")
            ])),
            (0.0, 9.0)
        );
        assert_eq!(infer(&tags(&[("building", "garage")])), (0.0, 3.0));
        assert_eq!(infer(&tags(&[("building", "yes")])), (0.0, 6.0));
        assert_eq!(
            infer(&tags(&[
                ("building:part", "yes"),
                ("height", "10"),
                ("min_height", "4")
            ])),
            (4.0, 10.0)
        );
    }

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
            };
            let mut m = MeshData::default();
            assert!(extrude(&fp, &mut m));
            // 4 walls * 2 tris + 2 roof tris
            assert_eq!(m.indices.len(), (4 * 2 + 2) * 3);
            let centre = glam::Vec3::new(5.0, 2.5, 5.0);
            for t in m.indices.chunks_exact(3) {
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
        };
        assert!(!extrude(&degenerate, &mut m));
    }
}
