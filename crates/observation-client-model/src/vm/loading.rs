//! The card shown while an area loads.

use crate::net::AreaRequests;
use observation_core::scene::AreaRef;

/// Seconds after which a slow OSM area offers the demo street, and the
/// card stops saying the server is merely fetching.
const SLOW_SECS: f64 = 5.0;
/// Seconds after which the card blames the map-data service.
const VERY_SLOW_SECS: f64 = 25.0;

/// The loading card.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadingVm {
    /// What is loading, for the heading.
    pub headline: String,
    /// Seconds waited so far, as shown.
    pub elapsed: String,
    /// What is happening, given how long it has taken.
    pub stage: &'static str,
    /// Offer Cancel: only a load that replaces the cameras can be dropped.
    pub can_cancel: bool,
    /// Offer the demo street instead: an OSM area that is slow.
    pub offer_demo: bool,
}

impl LoadingVm {
    /// The card for the area request in flight, if any, at `now` seconds
    /// since start-up (the clock [`AreaRequests::start`] was given).
    pub fn new(requests: &AreaRequests, now: f64) -> Option<Self> {
        let area = requests.area()?;
        let elapsed = now - requests.started();
        let headline = match area {
            AreaRef::Demo => "Loading the demo street…".to_owned(),
            AreaRef::Osm { bbox } => format!(
                "Loading buildings for {:.0} × {:.0} m…",
                bbox.width_m(),
                bbox.height_m()
            ),
        };
        let stage = match (area, elapsed) {
            (AreaRef::Demo, _) => "Built into the server; this should take a moment.",
            (_, e) if e < SLOW_SECS => {
                "Fetching map data and preparing the 3D scene on the server."
            }
            (_, e) if e < VERY_SLOW_SECS => {
                "Still working: a new place waits on OpenStreetMap's public Overpass service, which can take up to a minute. Places loaded before come from the server's cache."
            }
            _ => {
                "The map-data service is slow right now. You can keep waiting, cancel and try a smaller area, or explore the demo street meanwhile."
            }
        };
        Some(Self {
            headline,
            elapsed: format!("{elapsed:.0} s"),
            stage,
            can_cancel: requests.request().is_some_and(|r| !r.keep_cameras),
            offer_demo: area != AreaRef::Demo && elapsed >= SLOW_SECS,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::LoadArea;
    use observation_core::geo::BBox;

    fn osm() -> AreaRef {
        AreaRef::Osm {
            bbox: BBox::new(51.5, -0.001, 51.5009, 0.001).expect("valid box"),
        }
    }

    fn loading(area: AreaRef, keep_cameras: bool, started: f64) -> AreaRequests {
        let mut r = AreaRequests::default();
        r.start(LoadArea { area, keep_cameras }, started);
        r
    }

    const FETCHING: &str = "Fetching map data and preparing the 3D scene on the server.";
    const OVERPASS: &str = "Still working: a new place waits on OpenStreetMap's public Overpass service, which can take up to a minute. Places loaded before come from the server's cache.";
    const SLOW: &str = "The map-data service is slow right now. You can keep waiting, cancel and try a smaller area, or explore the demo street meanwhile.";

    #[test]
    fn nothing_loading_shows_no_card() {
        assert_eq!(LoadingVm::new(&AreaRequests::default(), 10.0), None);
    }

    #[test]
    fn an_osm_area_says_its_size_and_the_seconds_waited() {
        let vm = LoadingVm::new(&loading(osm(), false, 100.0), 103.4).expect("loading");
        assert_eq!(vm.headline, "Loading buildings for 138 × 100 m…");
        assert_eq!(vm.elapsed, "3 s", "measured from the start of the request");
        let later = LoadingVm::new(&loading(osm(), false, 100.0), 103.6).expect("loading");
        assert_eq!(later.elapsed, "4 s", "rounded, not truncated");
        assert_eq!(vm.stage, FETCHING);
        assert!(vm.can_cancel);
        assert!(!vm.offer_demo);
    }

    #[test]
    fn the_stage_and_the_demo_offer_change_at_5_and_25_seconds() {
        let at = |s: f64| LoadingVm::new(&loading(osm(), false, 0.0), s).expect("loading");
        let before = at(4.999);
        assert_eq!((before.stage, before.offer_demo), (FETCHING, false));
        let five = at(5.0);
        assert_eq!((five.stage, five.offer_demo), (OVERPASS, true));
        assert_eq!(at(24.999).stage, OVERPASS);
        let late = at(25.0);
        assert_eq!((late.stage, late.offer_demo), (SLOW, true));
    }

    #[test]
    fn the_demo_street_never_offers_itself() {
        let vm = LoadingVm::new(&loading(AreaRef::Demo, false, 0.0), 60.0).expect("loading");
        assert_eq!(vm.headline, "Loading the demo street…");
        assert_eq!(
            vm.stage,
            "Built into the server; this should take a moment."
        );
        assert!(!vm.offer_demo);
        assert!(vm.can_cancel);
    }

    #[test]
    fn a_restore_that_keeps_its_cameras_cannot_be_cancelled() {
        let vm = LoadingVm::new(&loading(osm(), true, 0.0), 30.0).expect("loading");
        assert!(!vm.can_cancel);
        assert!(vm.offer_demo);
    }
}
