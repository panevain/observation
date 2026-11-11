//! The terrain height field.

use crate::config;
use glam::{Vec2, Vec3};
use observation_core::terrain::Terrain;
use proptest::prelude::*;

/// A valid grid of up to 12 × 12 samples, anywhere near the origin.
fn terrain() -> impl Strategy<Value = Terrain> {
    (
        1..=12u32,
        1..=12u32,
        1.0..=50.0f32,
        -100.0..=0.0f32,
        -100.0..=0.0f32,
    )
        .prop_flat_map(|(cols, rows, cell_m, x, z)| {
            prop::collection::vec(-3000..=3000i16, (cols * rows) as usize).prop_map(
                move |heights_dm| Terrain {
                    min: [x, z],
                    cell_m,
                    cols,
                    rows,
                    heights_dm,
                    datum_m: 50.0,
                    attribution: String::new(),
                },
            )
        })
}

fn point() -> impl Strategy<Value = Vec2> {
    (-300.0..=300.0f32, -300.0..=300.0f32).prop_map(|(x, z)| Vec2::new(x, z))
}

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
