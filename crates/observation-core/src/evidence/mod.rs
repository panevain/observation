//! The evidence sheet (issue #91): a dated, self-describing summary of a
//! scene's figures that can leave the app, as SVG.
//!
//! [`evidence_sheet`] composes it from what the app already holds: the area,
//! each enabled camera's sight lines, the labelled areas and targets, the
//! analysis settings and the provenance record. It prints
//!
//! - who made it and from what: the date, the app version, the OSM data date
//!   and bundle build time, the OSM fingerprint, the catalogue and caveats
//!   versions, and the exact analysis settings;
//! - the detail × lighting grid ([`detail_grid`]), with the area every figure
//!   is out of, worked out from the sight lines the app already has (range
//!   only cuts them by distance), so it casts no extra sight line;
//! - where each camera's footage lands and every camera's verdicts on each
//!   target, from [`crate::footage`];
//! - the methodology ([`methodology`]), written from the same constants the
//!   figures are computed with, and the versioned caveats.
//!
//! # Tiers
//!
//! Coverage shapes give camera positions away: every side plane of a view
//! cone passes through the eye, so two straight edges of a footprint meet at
//! the camera. So the default, [`Tier::Aggregate`], holds numbers only: no
//! map, no coverage raster, no positions, no camera labels (they often name
//! a house), no addresses, and no distance from a camera to a building or a
//! target. [`Tier::Exact`] adds the map with the cameras, the coverage and
//! the walls seen per building, for the owner's own use, and is watermarked
//! "shows camera positions". The tier is applied when the figures are
//! gathered, so what an Aggregate sheet leaves out never reaches its
//! renderer.
//!
//! The output is deterministic: the same inputs give the same bytes.

mod figures;
mod method;
mod render;
mod svg;

#[cfg(test)]
mod tests;

pub use figures::{DetailGrid, GridCell, GridRow, StreetKinds, detail_grid};
pub use method::{AGENT_LOG_HEADING, AgentChange, Section, methodology};

use crate::bundle::AreaBundle;
use crate::camera::{CameraModel, CameraPlacement};
use crate::coverage::{CoverageParams, Sight, StaticLayers};
use crate::footage::{LabelledArea, Target};
use crate::provenance::Provenance;
use thiserror::Error;

/// Width of a page of the sheet, CSS px: A4 at 96 px to the inch.
pub const PAGE_WIDTH: f32 = 794.0;
/// Height of a page, CSS px (A4). Pages are stacked top to bottom in one
/// SVG, so page `k` starts at `k * PAGE_HEIGHT`.
pub const PAGE_HEIGHT: f32 = 1123.0;

/// What a sheet may show.
///
/// Each tier says what it shows through its methods rather than by being
/// matched on, so that a further tier (a map aggregated to blocks, which
/// needs its own leak test first) is one more variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Tier {
    /// Numbers only, for sharing: the default.
    #[default]
    Aggregate,
    /// Adds the map with camera positions and coverage, for the owner.
    Exact,
}

impl Tier {
    pub const ALL: [Self; 2] = [Self::Aggregate, Self::Exact];

    pub fn label(self) -> &'static str {
        match self {
            Self::Aggregate => "Aggregate",
            Self::Exact => "Exact",
        }
    }

    /// For file names: `aggregate`, `exact`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Aggregate => "aggregate",
            Self::Exact => "exact",
        }
    }

    /// The running header's note on what the sheet shows.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Aggregate => "no camera positions",
            Self::Exact => "shows camera positions",
        }
    }

    /// What the tier holds, and who it is for.
    pub fn summary(self) -> &'static str {
        match self {
            Self::Aggregate => {
                "Numbers only, for sharing: the detail × lighting grid, where each camera's \
                 footage lands and the verdicts on the targets. It has no map, no coverage \
                 shapes, no camera positions or labels, no addresses and no distances from a \
                 camera, because the shape of what a camera sees gives away where it is."
            }
            Self::Exact => {
                "For the owner's own use: everything on the Aggregate sheet, plus a map with \
                 every camera's position and coverage, camera labels and the walls each camera \
                 sees per building. It shows where cameras are: do not share it without the \
                 agreement of everyone whose camera is on it."
            }
        }
    }

    /// Whether the sheet shows where cameras are: the map, the coverage,
    /// labels and distances.
    pub fn shows_positions(self) -> bool {
        match self {
            Self::Aggregate => false,
            Self::Exact => true,
        }
    }
}

/// An enabled camera as the sheet needs it.
#[derive(Debug, Clone, Copy)]
pub struct SheetCamera<'a> {
    pub placement: &'a CameraPlacement,
    pub model: &'a CameraModel,
    /// Its sight lines ([`StaticLayers::sight`]), reaching at least
    /// [`CoverageParams::sight_range_m`] for its model.
    pub sight: &'a Sight,
}

/// Everything a sheet is composed from.
#[derive(Debug, Clone, Copy)]
pub struct SheetInputs<'a> {
    /// The scene's name, as the user typed it.
    pub title: &'a str,
    /// When the sheet is made, Unix seconds.
    pub generated_unix: u64,
    pub bundle: &'a AreaBundle,
    /// The layers built from `bundle`.
    pub layers: &'a StaticLayers,
    /// The size the layers' wall patches were made at, metres.
    pub facade_cell_m: f32,
    /// Where the figures come from ([`Provenance::of`] the bundle).
    pub provenance: &'a Provenance,
    pub params: CoverageParams,
    /// The scene's cameras in order; disabled ones are skipped.
    pub cameras: &'a [SheetCamera<'a>],
    pub areas: &'a [LabelledArea],
    /// The area the figures are limited to (while it is enabled and usable).
    pub limit_figures_to: Option<u32>,
    pub targets: &'a [Target],
    /// Changes an AI agent proposed and the user accepted (see
    /// [`methodology`]).
    pub agent_log: &'a [AgentChange],
}

/// Why no sheet could be made.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SheetError {
    /// A camera's sight lines do not reach as far as the figures need yet.
    #[error("camera {camera_id}'s coverage is still being worked out")]
    NotComputed { camera_id: u32 },
}

/// A sheet: SVG text of `pages` A4 pages ([`PAGE_WIDTH`] × [`PAGE_HEIGHT`]
/// each) stacked top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetSvg {
    pub svg: String,
    pub pages: u32,
}

/// The sheet for `inputs` at `tier`.
pub fn evidence_sheet(inputs: &SheetInputs, tier: Tier) -> Result<SheetSvg, SheetError> {
    let figures = figures::Figures::compute(inputs, tier)?;
    Ok(render::render(&figures))
}
