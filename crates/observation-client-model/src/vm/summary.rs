//! The coverage summary above the inspector's tabs.

use crate::coverage::CoverageState;
use crate::document::Document;
use observation_core::camera::{DoriLevel, Lighting};
use observation_core::coverage::{CoverageParams, CoverageStats};

/// The coverage summary.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageSummaryVm {
    pub title: &'static str,
    /// Whether the region menu shows the region as active.
    pub in_region: bool,
    /// The headline figures and the area line; `None` with no cameras.
    pub figures: Option<Figures>,
    /// "Street seen by day: " (or at night), ahead of the ladder.
    pub ladder_lead: String,
    /// The street seen at each detail level; empty before anything is
    /// counted.
    pub ladder: Vec<Rung>,
}

/// The headline figures.
#[derive(Debug, Clone, PartialEq)]
pub struct Figures {
    /// Fraction seen and caption: street, by 2+ cameras, walls.
    pub stats: [(f32, &'static str); 3],
    /// The street fraction, for the bar.
    pub street: f32,
    /// Street surface seen, of all, and open ground seen.
    pub areas: String,
}

/// One detail level in the ladder.
#[derive(Debug, Clone, PartialEq)]
pub struct Rung {
    pub level: DoriLevel,
    /// The level and its street figure.
    pub text: String,
    /// The level the figures above are at.
    pub selected: bool,
    pub tip: String,
}

impl CoverageSummaryVm {
    pub fn new(doc: &Document, coverage: &CoverageState) -> Self {
        let in_region = doc.region.as_ref().is_some_and(|r| r.is_active());
        let s = coverage.stats;
        let figures = (!doc.cameras.is_empty()).then(|| Figures {
            stats: [
                (s.street_fraction(), "of street seen"),
                (s.street_multi_fraction(), "by 2+ cameras"),
                (s.facade_fraction(), "of walls seen"),
            ],
            street: s.street_fraction(),
            areas: format!(
                "{:.0} m² of {:.0} m² street surface · {:.0} m² open ground seen",
                s.street_seen_m2, s.street_m2, s.open_ground_seen_m2
            ),
        });
        Self {
            title: if in_region {
                "Coverage inside region"
            } else {
                "Coverage"
            },
            in_region,
            figures,
            ladder_lead: ladder_lead(&doc.params),
            ladder: ladder(&doc.params, coverage.ladder()),
        }
    }
}

fn ladder_lead(params: &CoverageParams) -> String {
    let light = match params.lighting {
        Lighting::Day => "by day",
        Lighting::Night => "at night",
    };
    format!("Street seen {light}: ")
}

fn ladder<'a>(
    p: &CoverageParams,
    rungs: impl Iterator<Item = (DoriLevel, &'a CoverageStats)>,
) -> Vec<Rung> {
    rungs
        .map(|(level, stats)| {
            let name = match level {
                DoriLevel::Custom => format!("Custom {:.0} px/m", p.pixels_per_metre()),
                l => l.label().to_owned(),
            };
            let selected = level == p.dori;
            let tip = if selected {
                format!("The figures above are at {name}")
            } else {
                format!("Show the figures and overlay at {name}")
            };
            Rung {
                level,
                text: format!("{name} {:.0}%", stats.street_fraction() * 100.0),
                selected,
                tip,
            }
        })
        .collect()
}

/// The settings line under the figures: detail level, lighting and subject
/// height. Separate from [`CoverageSummaryVm`] because the panel shows it
/// after the ladder, whose click may change the level in the same frame.
pub fn settings_line(p: &CoverageParams) -> String {
    let dori = match p.dori {
        DoriLevel::Custom => format!("Custom {:.0} px/m", p.custom_ppm),
        l => l.label().to_owned(),
    };
    let light = match p.lighting {
        Lighting::Day => "Day",
        Lighting::Night => "Night",
    };
    format!("{dori} · {light} · {:.2} m high", p.subject_height_m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::camera::CameraPlacement;
    use observation_core::coverage::Region;

    fn stats(street_seen_m2: f32) -> CoverageStats {
        CoverageStats {
            street_m2: 200.0,
            street_seen_m2,
            street_seen_multi_m2: 48.0,
            open_ground_seen_m2: 12.4,
            facade_m2: 300.0,
            facade_seen_m2: 100.0,
        }
    }

    fn params(dori: DoriLevel, lighting: Lighting) -> CoverageParams {
        CoverageParams {
            dori,
            lighting,
            subject_height_m: 1.6,
            custom_ppm: 125.0,
        }
    }

    fn camera() -> CameraPlacement {
        CameraPlacement {
            id: 0,
            label: String::new(),
            model_id: "m".to_owned(),
            position: [0.0, 3.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        }
    }

    #[test]
    fn without_cameras_there_are_no_figures() {
        let mut coverage = CoverageState::default();
        coverage.stats = stats(102.0);
        let vm = CoverageSummaryVm::new(&Document::default(), &coverage);
        assert_eq!(vm.figures, None);
        assert_eq!((vm.title, vm.in_region), ("Coverage", false));
        assert!(vm.ladder.is_empty(), "nothing counted yet");
    }

    #[test]
    fn the_figures_come_from_the_selected_level() {
        let mut coverage = CoverageState::default();
        coverage.stats = stats(102.0);
        let mut doc = Document::default();
        doc.add_camera(camera());
        let figures = CoverageSummaryVm::new(&doc, &coverage)
            .figures
            .expect("a camera");
        assert_eq!(
            figures.stats,
            [
                (0.51, "of street seen"),
                (0.24, "by 2+ cameras"),
                (1.0 / 3.0, "of walls seen"),
            ]
        );
        assert_eq!(figures.street, 0.51);
        assert_eq!(
            figures.areas,
            "102 m² of 200 m² street surface · 12 m² open ground seen"
        );
    }

    #[test]
    fn an_active_region_titles_the_figures() {
        let mut doc = Document::default();
        doc.region = Some(Region {
            points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
            enabled: true,
        });
        let vm = CoverageSummaryVm::new(&doc, &CoverageState::default());
        assert_eq!((vm.title, vm.in_region), ("Coverage inside region", true));
        doc.region.as_mut().expect("a region").enabled = false;
        let vm = CoverageSummaryVm::new(&doc, &CoverageState::default());
        assert_eq!((vm.title, vm.in_region), ("Coverage", false));
    }

    #[test]
    fn the_settings_line_names_level_light_and_height() {
        assert_eq!(
            settings_line(&params(DoriLevel::Recognise, Lighting::Day)),
            format!("{} · Day · 1.60 m high", DoriLevel::Recognise.label())
        );
        assert_eq!(
            settings_line(&params(DoriLevel::Custom, Lighting::Night)),
            "Custom 125 px/m · Night · 1.60 m high"
        );
        // The value as typed, where the ladder names the clamped density.
        let mut far = params(DoriLevel::Custom, Lighting::Day);
        far.custom_ppm = 100_000.0;
        assert_ne!(far.pixels_per_metre(), far.custom_ppm);
        assert_eq!(
            settings_line(&far),
            "Custom 100000 px/m · Day · 1.60 m high"
        );
        assert_eq!(
            ladder_lead(&params(DoriLevel::Custom, Lighting::Night)),
            "Street seen at night: "
        );
        assert_eq!(
            ladder_lead(&params(DoriLevel::Detect, Lighting::Day)),
            "Street seen by day: "
        );
    }

    #[test]
    fn the_ladder_names_the_clamped_custom_density() {
        let mut p = params(DoriLevel::Custom, Lighting::Day);
        p.custom_ppm = 100_000.0;
        let b = stats(52.0);
        let rungs = ladder(&p, [(DoriLevel::Custom, &b)].into_iter());
        assert_eq!(
            (rungs[0].text.as_str(), rungs[0].tip.as_str()),
            (
                "Custom 2000 px/m 26%",
                "The figures above are at Custom 2000 px/m"
            ),
            "the density coverage is computed at, not the value as typed"
        );
    }

    #[test]
    fn the_rung_and_area_figures_round_not_truncate() {
        let mut coverage = CoverageState::default();
        coverage.stats = CoverageStats {
            open_ground_seen_m2: 12.6,
            ..stats(101.8)
        };
        let mut doc = Document::default();
        doc.add_camera(camera());
        let figures = CoverageSummaryVm::new(&doc, &coverage)
            .figures
            .expect("a camera");
        assert_eq!(
            figures.areas, "102 m² of 200 m² street surface · 13 m² open ground seen",
            "101.8 and 12.6 round up"
        );
        let p = params(DoriLevel::Detect, Lighting::Day);
        let rungs = ladder(&p, [(DoriLevel::Detect, &coverage.stats)].into_iter());
        assert_eq!(
            rungs[0].text,
            format!("{} 51%", DoriLevel::Detect.label()),
            "50.9% rounds up"
        );
    }

    #[test]
    fn the_ladder_marks_the_selected_level() {
        let (a, b) = (stats(150.0), stats(52.0));
        let p = params(DoriLevel::Custom, Lighting::Day);
        let rungs = ladder(
            &p,
            [(DoriLevel::Detect, &a), (DoriLevel::Custom, &b)].into_iter(),
        );
        let detect = DoriLevel::Detect.label();
        let custom = format!("Custom {:.0} px/m", p.pixels_per_metre());
        assert_eq!(
            rungs,
            [
                Rung {
                    level: DoriLevel::Detect,
                    text: format!("{detect} 75%"),
                    selected: false,
                    tip: format!("Show the figures and overlay at {detect}"),
                },
                Rung {
                    level: DoriLevel::Custom,
                    text: format!("{custom} 26%"),
                    selected: true,
                    tip: format!("The figures above are at {custom}"),
                },
            ]
        );
    }
}
