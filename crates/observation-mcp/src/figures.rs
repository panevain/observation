//! Coverage figures for a whole scene, computed in one pass: the batch form
//! of the client's incremental `observation_client_model::coverage::update`.

use crate::catalog::model;
use observation_core::camera::{Catalog, DoriLevel};
use observation_core::coverage::{RegionMask, StaticLayers, Tally};
use observation_core::frustum::ViewCone;
use observation_core::scene::SceneFile;

/// The figures at one detail level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelFigures {
    pub level: DoriLevel,
    /// The level's pixel density, px/m.
    pub ppm: f32,
    pub stats: observation_core::coverage::CoverageStats,
}

/// A camera left out of the figures, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub id: u32,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Figures {
    /// One entry per level of [`CoverageParams::ladder`](observation_core::coverage::CoverageParams::ladder).
    pub levels: Vec<LevelFigures>,
    /// Whether the figures are limited to the scene's drawn region.
    pub region_limited: bool,
    pub counted: Vec<u32>,
    pub skipped: Vec<Skipped>,
}

/// What `scene`'s enabled cameras see at every detail level, under the
/// scene's lighting and subject height, on `layers` (the scene's area).
pub fn scene_figures(scene: &SceneFile, catalog: &Catalog, layers: &StaticLayers) -> Figures {
    let params = scene.params;
    let usable = scene
        .region
        .as_ref()
        .filter(|r| r.enabled && r.problem().is_none());
    let mask = usable.map(|r| RegionMask::new(layers, r));
    let ladder = params.ladder();
    let mut tallies: Vec<Tally> = ladder
        .iter()
        .map(|_| Tally::new(layers, mask.as_ref()))
        .collect();
    let (mut counted, mut skipped) = (Vec::new(), Vec::new());
    for c in &scene.cameras {
        if !c.enabled {
            skipped.push(Skipped {
                id: c.id,
                reason: "disabled",
            });
            continue;
        }
        let Some(m) = model(catalog, Some(scene), &c.model_id) else {
            skipped.push(Skipped {
                id: c.id,
                reason: "unknown model",
            });
            continue;
        };
        // One line-of-sight pass out to the longest range any level needs;
        // each level then counts the cells within its own range.
        let cone = ViewCone::new(c, m, params.sight_range_m(m));
        let sight = layers.sight(&cone, params.subject_height_m);
        for ((_, ppm), tally) in ladder.iter().zip(&mut tallies) {
            let range = m.effective_range_m(*ppm, params.lighting);
            tally.add_within(layers, mask.as_ref(), &sight, range);
        }
        counted.push(c.id);
    }
    let levels = ladder
        .iter()
        .zip(&tallies)
        .map(|(&(level, ppm), t)| LevelFigures {
            level,
            ppm,
            stats: t.stats(layers),
        })
        .collect();
    Figures {
        levels,
        region_limited: mask.is_some(),
        counted,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::area::demo_layers;
    use crate::catalog::catalog;
    use crate::edit::{add_example_cameras, new_demo_scene};

    #[test]
    fn more_detail_never_sees_more() {
        let mut s = new_demo_scene().unwrap();
        let cat = catalog().unwrap();
        let layers = demo_layers().unwrap();
        let empty = scene_figures(&s, cat, layers);
        assert!(empty.levels.iter().all(|l| l.stats.street_seen_m2 == 0.0));
        assert!(empty.levels[0].stats.street_m2 > 0.0);

        add_example_cameras(&mut s, cat, 3, &[]).unwrap();
        s.cameras[0].enabled = false;
        let f = scene_figures(&s, cat, layers);
        assert_eq!(f.counted.len(), s.cameras.len() - 1);
        assert_eq!(
            f.skipped,
            vec![Skipped {
                id: 1,
                reason: "disabled"
            }]
        );
        let seen: Vec<f32> = f.levels.iter().map(|l| l.stats.street_seen_m2).collect();
        assert!(seen[0] > 0.0, "{seen:?}");
        assert!(seen.windows(2).all(|w| w[0] >= w[1]), "{seen:?}");
    }
}
