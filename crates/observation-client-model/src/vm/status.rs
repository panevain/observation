//! The status bar along the bottom of the window.

use crate::persist::SaveState;
use observation_core::bundle::AreaBundle;
use observation_core::coverage::CoverageStats;
use observation_core::terrain::{TERRAIN_TILES_ATTRIBUTION, TERRAIN_TILES_CREDITS_URL};

/// The map data's credit before an area has loaded.
const OSM_CREDIT: &str = "© OpenStreetMap contributors";

/// The status bar: credits, whether the scene is saved, and the headline
/// figures.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusVm {
    /// The map data's credit, and the map tiles' when they differ.
    pub attribution: String,
    /// The elevation data's credit, when the area has one.
    pub dem: Option<DemCredit>,
    /// Whether the scene is in this browser's storage; nothing before the
    /// first write.
    pub save: Option<SaveVm>,
    /// Caption and percentage of each figure, in the order laid out right
    /// to left.
    pub figures: [(&'static str, String); 3],
}

/// The elevation data's credit.
#[derive(Debug, Clone, PartialEq)]
pub struct DemCredit {
    pub text: String,
    /// Where the full credits are listed: only for the default source; an
    /// operator's own source has only the text it was given.
    pub link: Option<&'static str>,
}

/// The save status and its tooltip.
#[derive(Debug, Clone, PartialEq)]
pub struct SaveVm {
    /// The browser refused the write.
    pub failed: bool,
    pub text: &'static str,
    pub tip: String,
}

impl StatusVm {
    /// The status bar for the loaded area's `bundle` (if any), the map
    /// tiles' credit, the autosave's `save` state, `now` in seconds since
    /// start-up (the clock the save was stamped with) and the coverage
    /// figures.
    pub fn new(
        bundle: Option<&AreaBundle>,
        tile_attribution: &str,
        save: SaveState,
        now: f64,
        stats: &CoverageStats,
    ) -> Self {
        let data = bundle.map_or(OSM_CREDIT, |b| b.attribution.as_str());
        let attribution = if tile_attribution == data {
            data.to_owned()
        } else {
            format!("{data} · Tiles {tile_attribution}")
        };
        let dem = bundle
            .map(|b| b.terrain.attribution.as_str())
            .filter(|a| !a.is_empty())
            .map(|text| DemCredit {
                text: text.to_owned(),
                link: (text == TERRAIN_TILES_ATTRIBUTION).then_some(TERRAIN_TILES_CREDITS_URL),
            });
        let percent = |f: f32| format!("{:.0}%", f * 100.0);
        Self {
            attribution,
            dem,
            save: save_vm(save, now),
            figures: [
                ("walls", percent(stats.facade_fraction())),
                ("2+ cams", percent(stats.street_multi_fraction())),
                ("street", percent(stats.street_fraction())),
            ],
        }
    }
}

fn save_vm(save: SaveState, now: f64) -> Option<SaveVm> {
    match save {
        SaveState::Saved(at) => {
            let ago = (now - at).max(0.0);
            let when = if ago < 60.0 {
                "just now".to_owned()
            } else {
                format!("{:.0} min ago", ago / 60.0)
            };
            Some(SaveVm {
                failed: false,
                text: "Saved",
                tip: format!("Saved in this browser {when}. Use Export to keep a copy elsewhere."),
            })
        }
        SaveState::Failed(why) => Some(SaveVm {
            failed: true,
            text: "Not saved",
            tip: format!(
                "{} The scene is not saved; use Export to keep your work.",
                why.reason()
            ),
        }),
        SaveState::Pending => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::SaveFailure;
    use observation_core::bundle::build_bundle;
    use observation_core::geo::{BBox, LatLon};
    use observation_core::terrain::Terrain;

    fn stats() -> CoverageStats {
        CoverageStats {
            street_m2: 200.0,
            street_seen_m2: 102.0,
            street_seen_multi_m2: 48.0,
            facade_m2: 300.0,
            facade_seen_m2: 100.0,
            ..CoverageStats::default()
        }
    }

    fn status(save: SaveState, now: f64) -> StatusVm {
        StatusVm::new(None, "© Tiles Inc", save, now, &stats())
    }

    #[test]
    fn before_an_area_the_osm_credit_and_the_tiles_credit_are_shown() {
        let vm = status(SaveState::Pending, 0.0);
        assert_eq!(
            vm.attribution,
            "© OpenStreetMap contributors · Tiles © Tiles Inc"
        );
        assert_eq!(vm.dem, None);
        assert_eq!(vm.save, None, "nothing before the first write");
    }

    #[test]
    fn a_tiles_credit_equal_to_the_data_credit_is_shown_once() {
        let vm = StatusVm::new(
            None,
            "© OpenStreetMap contributors",
            SaveState::Pending,
            0.0,
            &stats(),
        );
        assert_eq!(vm.attribution, "© OpenStreetMap contributors");
    }

    fn bundle(data: &str, dem: &str) -> AreaBundle {
        let bbox = BBox::around(LatLon::new(51.5, 0.0), 100.0);
        let mut b = build_bundle(bbox, &[], &[], &[], Terrain::default(), "test", 7, Some(5));
        b.attribution = data.to_owned();
        b.terrain.attribution = dem.to_owned();
        b
    }

    fn with(bundle: &AreaBundle, tiles: &str) -> StatusVm {
        StatusVm::new(Some(bundle), tiles, SaveState::Pending, 0.0, &stats())
    }

    #[test]
    fn a_loaded_area_credits_its_own_data_and_elevation() {
        let b = bundle("© Server OSM copy", "Elevation by Someone");
        let vm = with(&b, "© Tiles Inc");
        assert_eq!(vm.attribution, "© Server OSM copy · Tiles © Tiles Inc");
        assert_eq!(
            vm.dem,
            Some(DemCredit {
                text: "Elevation by Someone".to_owned(),
                link: None,
            }),
            "an operator's own source: its text only"
        );
        assert_eq!(
            with(&b, "© Server OSM copy").attribution,
            "© Server OSM copy"
        );
    }

    #[test]
    fn the_default_elevation_source_links_to_its_credits() {
        let b = bundle("d", TERRAIN_TILES_ATTRIBUTION);
        let dem = with(&b, "t").dem.expect("a credit");
        assert_eq!(dem.link, Some(TERRAIN_TILES_CREDITS_URL));
        assert_eq!(with(&bundle("d", ""), "t").dem, None, "no credit, no label");
    }

    #[test]
    fn the_figures_are_whole_percentages_walls_first() {
        let vm = status(SaveState::Pending, 0.0);
        assert_eq!(
            vm.figures,
            [
                ("walls", "33%".to_owned()),
                ("2+ cams", "24%".to_owned()),
                ("street", "51%".to_owned()),
            ]
        );
    }

    #[test]
    fn a_save_is_just_now_for_a_minute_then_counted_in_minutes() {
        let tip = |now: f64| {
            let save = status(SaveState::Saved(100.0), now).save.expect("saved");
            assert!(!save.failed);
            assert_eq!(save.text, "Saved");
            save.tip
        };
        let just_now = "Saved in this browser just now. Use Export to keep a copy elsewhere.";
        assert_eq!(tip(159.999), just_now);
        assert_eq!(
            tip(160.0),
            "Saved in this browser 1 min ago. Use Export to keep a copy elsewhere."
        );
        assert_eq!(
            tip(100.0 + 150.0),
            "Saved in this browser 2 min ago. Use Export to keep a copy elsewhere.",
            "rounded"
        );
    }

    #[test]
    fn a_refused_save_says_why_and_to_export() {
        let save = status(SaveState::Failed(SaveFailure::Full), 0.0)
            .save
            .expect("failed");
        assert!(save.failed);
        assert_eq!(save.text, "Not saved");
        assert_eq!(
            save.tip,
            "This browser's storage for the site is full. The scene is not saved; use Export to keep your work."
        );
    }
}
