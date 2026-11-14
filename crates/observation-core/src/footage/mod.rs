//! Where footage lands (issue #89): each camera's visible ground split over
//! the areas the user drew and labelled, the wall it sees per building, and
//! verdicts on named targets and on single points; and the gaps, the
//! connected runs of street no camera sees.
//!
//! Everything here is computed from the same sight lines as the coverage
//! overlay ([`StaticLayers`](crate::coverage::StaticLayers)), deterministically
//! and without an engine, so the client, the evidence sheet and the MCP
//! server give the same answers. Two rules hold throughout:
//!
//! - **As drawn.** Areas and their labels are the user's own outlines; no
//!   property data is used. Each area figure is a [`SeenRange`] over the
//!   outlines moved in and out ([`DEFAULT_OFFSET_M`]), with the outline's own
//!   area beside it, so a generous outline cannot quietly give "0 m² of the
//!   neighbour's".
//! - **One-sided.** A verdict is "not ruled out" or "ruled out" with a
//!   [`Reason`], never "seen": what the model leaves out (hedges, fences,
//!   vehicles) can only block more.
//!
//! Buildings are named by their index in the bundle and their distance only,
//! never by address: a neighbour's address is a guess from the nearest
//! address point, so nothing here prints one. Callers add the data date and
//! the caveats line (from the `provenance` module) wherever they show
//! these figures.

mod area;
mod gaps;
mod split;
mod verdict;
mod wall;

#[cfg(test)]
mod tests;

pub use area::{
    AreaLabel, LabelledArea, MAX_AREAS, MAX_NAME_CHARS, MAX_TARGET_POINTS, MAX_TARGET_SPAN_M,
    MAX_TARGETS, Target, TargetPlace, TargetProblem, WALL_TARGET_SIZE_M, area_by_id,
    area_by_id_mut, fresh_id, set_figures_area,
};
pub use gaps::{Gap, gaps_where, seen_ground, street_gaps, unseen_street};
pub use split::{
    AreaMasks, AreaSeen, CameraSplit, DEFAULT_OFFSET_M, LabelSeen, SeenRange, Split, WallSeen,
    camera_splits, split_seen,
};
pub use verdict::{
    CameraVerdicts, LevelVerdict, MAX_TARGET_SAMPLES, Probe, Reach, Reason, TargetVerdicts,
    Verdict, camera_verdicts, point_verdicts, target_verdicts,
};
pub use wall::{WallSpot, nearest_wall};

use crate::camera::{CameraModel, CameraPlacement};

/// A camera as the footage figures need it: where it is and its model. The
/// functions here skip cameras that are not enabled.
#[derive(Debug, Clone, Copy)]
pub struct FootageCamera<'a> {
    pub placement: &'a CameraPlacement,
    pub model: &'a CameraModel,
}
