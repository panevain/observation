//! The terrain height field.

use crate::config;
use crate::strategies::{point, terrain};
use glam::{Vec2, Vec3};
use observation_core::terrain::Terrain;
use proptest::prelude::*;

proptest! {
    #![proptest_config(config(128))]

    #[test]
    fn prop_heights_lie_between_bottom_and_top(t in terrain(), p in point()) {
        prop_assert!(t.validate().is_ok());
        let h = t.height_at(p);
        prop_assert!(h >= t.bottom() - 1e-3 && h <= t.top() + 1e-3, "{h} at {p}");
    }

    #[test]
    fn prop_lowest_ground_is_no_higher_than_any_corner(
        t in terrain(),
        ring in prop::collection::vec(point(), 3..8),
    ) {
        let ring: Vec<[f32; 2]> = ring.iter().map(|p| p.to_array()).collect();
        let low = t.min_under(&ring);
        for p in &ring {
            prop_assert!(low <= t.height_at(Vec2::from_array(*p)) + 1e-4, "{low}");
        }
        prop_assert!(low >= t.bottom() - 1e-3);
    }

    #[test]
    fn prop_lowest_ground_is_no_higher_than_any_sample_inside(
        t in terrain(),
        // A rectangle around one sample, often holding only that one, and
        // half the time in the grid's first row or column.
        at in (0..12u32, 0..12u32, any::<bool>()),
        size in (0.1..=1.6f32, 0.1..=1.6f32, 0.1..=1.6f32, 0.1..=1.6f32),
    ) {
        let (col, row) = if at.2 { (at.0 % t.cols, 0) } else { (0, at.1 % t.rows) };
        let centre = Vec2::from_array(t.min) + Vec2::new(col as f32, row as f32) * t.cell_m;
        let lo = centre - Vec2::new(size.0, size.1) * t.cell_m;
        let hi = centre + Vec2::new(size.2, size.3) * t.cell_m;
        let ring = [[lo.x, lo.y], [hi.x, lo.y], [hi.x, hi.y], [lo.x, hi.y]];
        let low = t.min_under(&ring);
        for r in 0..t.rows {
            for c in 0..t.cols {
                let p = Vec2::from_array(t.min) + Vec2::new(c as f32, r as f32) * t.cell_m;
                // Strictly inside, clear of float noise at the edges.
                let margin = 1e-3 * t.cell_m;
                if p.cmpgt(lo + margin).all() && p.cmplt(hi - margin).all() {
                    let sample = f32::from(t.heights_dm[(r * t.cols + c) as usize]) * 0.1;
                    prop_assert!(low <= sample, "{low} above the sample at ({c}, {r}): {sample}");
                }
            }
        }
    }

    #[test]
    fn prop_rays_stop_on_the_ground(
        t in terrain(),
        from in point(),
        up in 0.0..=200.0f32,
        dir in (-1.0..=1.0f32, -1.0..=-0.05f32, -1.0..=1.0f32),
    ) {
        let origin = Vec3::new(from.x, t.height_at(from) + up, from.y);
        let dir = Vec3::new(dir.0, dir.1, dir.2);
        if let Some(hit) = t.ray_hit(origin, dir, 2000.0) {
            prop_assert_eq!(hit.y, t.height_at(Vec2::new(hit.x, hit.z)));
            // On the ray, give or take the bisection's last step.
            let along = (hit - origin).dot(dir.normalize());
            let off = (origin + dir.normalize() * along - hit).length();
            prop_assert!(along >= -1e-3 && off < 0.05 + along * 1e-4, "{hit} {off}");
        }
    }

    #[test]
    fn prop_sampled_grids_validate(
        min in (-2000.0..=0.0f32, -2000.0..=0.0f32),
        size in (0.0..=2500.0f32, 0.0..=2500.0f32),
        slope in (-0.5..=0.5f32, -0.5..=0.5f32),
        base in -400.0..=8000.0f32,
    ) {
        let min = Vec2::new(min.0, min.1);
        let max = min + Vec2::new(size.0, size.1);
        let t = Terrain::sample_from(min, max, |p| Some(base + slope.0 * p.x + slope.1 * p.y), "dem")
            .expect("every sample has a value");
        prop_assert!(t.validate().is_ok(), "{} x {} at {}", t.cols, t.rows, t.cell_m);
        prop_assert_eq!(t.datum_m, base);
    }
}
