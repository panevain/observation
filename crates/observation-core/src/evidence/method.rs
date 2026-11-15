//! The sheet's methodology, written from the constants the figures are
//! computed with, so that the text cannot drift from the code.

use crate::camera::{ASSUMED_IR_RANGE_M, DoriLevel, MAX_FOV_DEG, MAX_RANGE_M};
use crate::coverage::CoverageParams;
use crate::footage::DEFAULT_OFFSET_M;
use crate::osm::tags::{FOOT_WAYS, LANE_WIDTH_M, LANES_TAG, ROAD_WIDTHS, SERVICE_WAY, WIDTH_TAG_M};
use crate::provenance::{CAVEATS, CAVEATS_BOUND, CAVEATS_VERSION, format_utc_minute};

use super::Tier;

/// One part of the methodology: a heading and its paragraphs (a paragraph
/// starting with "• " is a list item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub paragraphs: Vec<String>,
}

/// The heading the agent log will be printed under (see [`methodology`]).
pub const AGENT_LOG_HEADING: &str = "Changes proposed by an AI agent and accepted by the user";

/// One change an AI agent proposed and the user accepted, as the sheet
/// prints it: an entry of the scene's change log.
pub use crate::scene::agent_log::AgentLogEntry as AgentChange;

/// The methodology for figures computed with `params` on ground cells of
/// `ground_cell_m` and wall patches of about `facade_cell_m`, followed by
/// the agent log when there is one, as a sheet at `tier` may print it.
pub fn methodology(
    params: &CoverageParams,
    ground_cell_m: f32,
    facade_cell_m: f32,
    agent_log: &[AgentChange],
    tier: Tier,
) -> Vec<Section> {
    let mut out = vec![
        street(),
        seen(params, ground_cell_m, facade_cell_m),
        places(),
        caveats(),
    ];
    // A scene without an agent log prints no section.
    if !agent_log.is_empty() {
        out.push(agent_changes(agent_log, tier));
    }
    out
}

/// The changes an AI agent proposed and the user accepted (#259), from the
/// scene's `agent_log`, under [`AGENT_LOG_HEADING`]. The Exact sheet lists
/// each with the time it was accepted and the agent's summary. The summaries
/// are free text from the agent and may name camera positions, labels or
/// addresses, which the Aggregate sheet leaves out; so it lists only how
/// many there are and when each was accepted.
fn agent_changes(log: &[AgentChange], tier: Tier) -> Section {
    let paragraphs = if tier.shows_positions() {
        log.iter()
            .map(|c| {
                format!(
                    "• {}: {}",
                    format_utc_minute(c.accepted_unix),
                    c.summary.trim()
                )
            })
            .collect()
    } else {
        let n = log.len();
        let changes = if n == 1 {
            "1 change".to_owned()
        } else {
            format!("{n} changes")
        };
        std::iter::once(format!(
            "An AI agent proposed {changes} to this scene that the user accepted, at the \
             times below. What each change did is left out of this sheet: the agent \
             describes it in its own words, which may name camera positions, labels or \
             addresses. The descriptions appear on the Exact sheet."
        ))
        .chain(
            log.iter()
                .map(|c| format!("• Accepted {}", format_utc_minute(c.accepted_unix))),
        )
        .collect()
    };
    Section {
        heading: AGENT_LOG_HEADING.to_owned(),
        paragraphs,
    }
}

/// "a, b and c".
fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn street() -> Section {
    let widths: Vec<String> = ROAD_WIDTHS
        .iter()
        .map(|(types, w)| format!("{} ({w} m)", types.join(", ")))
        .collect();
    let minor: Vec<&str> = std::iter::once(SERVICE_WAY)
        .chain(FOOT_WAYS.iter().copied())
        .collect();
    Section {
        heading: "What “street” means".to_owned(),
        paragraphs: vec![
            format!(
                "Street is the ground on every road, path and service way mapped in \
                 OpenStreetMap whose highway type is one of these, each as a band of the \
                 width in brackets centred on the mapped line: {}. A way tagged with a width \
                 of at least {} m and below {} m uses that width; failing that, a lanes \
                 tag of at least {} and below {} gives {LANE_WIDTH_M} m a lane. Ground under a \
                 building is never street.",
                widths.join("; "),
                WIDTH_TAG_M.start,
                WIDTH_TAG_M.end,
                LANES_TAG.start,
                LANES_TAG.end,
            ),
            format!(
                "Carriageway is the street on every type above but {}. Footpath and \
                 service is the rest of the street: ground on one of those and on no \
                 carriageway. Driveways are usually mapped as service ways, so they count \
                 here.",
                list(&minor)
            ),
        ],
    }
}

fn seen(params: &CoverageParams, ground_cell_m: f32, facade_cell_m: f32) -> Section {
    let levels: Vec<String> = DoriLevel::ALL
        .iter()
        .filter_map(|l| {
            Some(format!(
                "{} {} px/m ({})",
                l.label(),
                l.pixels_per_metre()?,
                l.reading()?
            ))
        })
        .collect();
    Section {
        heading: "What “seen” means".to_owned(),
        paragraphs: vec![
            format!(
                "A spot counts as seen by a camera when a point {} m above the ground there \
                 (the subject height) lies inside the camera's field of view and within its \
                 range, and the straight line from the camera to it passes through no \
                 building and stays clear of the ground. Ground is tested at the centre of \
                 every {ground_cell_m} m square cell outside buildings, walls in patches of \
                 about {facade_cell_m} m. A figure is the area of the cells or patches seen.",
                super::render::metres(params.subject_height_m),
            ),
            format!(
                "Range at a detail level of D pixels per metre is R = W ÷ (2 × D × \
                 tan(H ÷ 2)), where W is the camera's published horizontal resolution in \
                 pixels and H its horizontal field of view (taken as at most {MAX_FOV_DEG}°), \
                 and never more than {MAX_RANGE_M} m. At night R is also no more than the \
                 camera's published infrared range, or {ASSUMED_IR_RANGE_M} m where none is \
                 published. The detail levels (DORI, after EN 62676-4) are {}; Custom is a \
                 density the user sets.",
                list(&levels.iter().map(String::as_str).collect::<Vec<_>>()),
            ),
        ],
    }
}

fn places() -> Section {
    Section {
        heading: "Areas, targets and verdicts".to_owned(),
        paragraphs: vec![
            format!(
                "Areas and their labels (own, neighbour, public, other) are outlines the \
                 user drew, as drawn: they are not property boundaries, and no property data \
                 is used. Each area figure is given as drawn, with the range it takes when \
                 every outline is moved {DEFAULT_OFFSET_M} m in and {DEFAULT_OFFSET_M} m \
                 out, and each outline's own area is printed beside it. The scene, area and \
                 target names are printed as the user typed them."
            ),
            "A verdict on a target is one-sided. Not ruled out: some of the target is \
             visible in the model. Ruled out: the model rules it out, for the reason given. \
             What the model leaves out can only block more, so it never says that a camera \
             sees a place."
                .to_owned(),
        ],
    }
}

fn caveats() -> Section {
    let mut paragraphs: Vec<String> = CAVEATS.iter().map(|c| format!("• {c}")).collect();
    paragraphs.push(CAVEATS_BOUND.to_owned());
    Section {
        heading: format!("What the figures leave out (caveats version {CAVEATS_VERSION})"),
        paragraphs,
    }
}
