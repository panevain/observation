//! The Evidence sheet dialog: choosing a tier, composing the sheet from the
//! open scene and the coverage already computed, and handing it to the
//! browser as SVG, PNG or print (see [`observation_core::evidence`]).

use crate::coverage::CoverageState;
use crate::document::{Document, Models};
use crate::persist::file_stem;
use crate::platform::{FileDialogs, SheetOutput};
use observation_core::bundle::AreaBundle;
use observation_core::coverage::StaticLayers;
use observation_core::evidence::{SheetCamera, SheetInputs, SheetSvg, Tier, evidence_sheet};
use observation_core::provenance::{Provenance, format_date};

/// MIME type of a downloaded sheet.
pub const SHEET_MIME: &str = "image/svg+xml";

/// How the sheet leaves the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetFormat {
    /// Download the SVG.
    Svg,
    /// Download a PNG the browser draws from the SVG.
    Png,
    /// The browser's print dialog, which also saves PDF.
    Print,
}

impl SheetFormat {
    pub const ALL: [Self; 3] = [Self::Svg, Self::Png, Self::Print];

    /// The button's text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Svg => "Download SVG",
            Self::Png => "Download PNG",
            Self::Print => "Print or save as PDF…",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Self::Svg => "A vector image that opens in any browser and scales without blurring",
            Self::Png => "A picture at twice the page size, for documents and messages",
            Self::Print => {
                "Opens the browser's print dialog, one A4 page per page of the sheet; \
                 choose \"Save as PDF\" there for a PDF"
            }
        }
    }
}

/// The dialog's explanation of the tiers.
pub const TIER_NOTE: &str = "Coverage shapes give camera positions away: the edges of what a \
    camera sees meet where it is. So the Aggregate sheet keeps to numbers, and is the one to \
    share. Exact adds the map, for your own records.";

/// The download name, without its extension, for a sheet of `scene` made at
/// `generated_unix`: `Front of house evidence 2026-11-14 (aggregate)`.
pub fn sheet_file_stem(scene: &str, generated_unix: u64, tier: Tier) -> String {
    let stem = file_stem(scene).unwrap_or_else(|| "observation".to_owned());
    format!(
        "{stem} evidence {} ({})",
        format_date(generated_unix),
        tier.key()
    )
}

/// Why no sheet can be made yet, or `None` when one can: an area must be
/// loaded and every enabled camera's coverage worked out far enough.
pub fn not_ready(
    doc: &Document,
    models: &Models,
    coverage: &CoverageState,
    area_loaded: bool,
) -> Option<&'static str> {
    if !area_loaded || doc.area.is_none() {
        return Some("Load an area first.");
    }
    let behind = doc.cameras.iter().filter(|c| c.enabled).any(|c| {
        models.get(&c.model_id).is_some_and(|m| {
            coverage
                .sight(c.id)
                .is_none_or(|s| !s.covers(doc.params.sight_range_m(m)))
        })
    });
    behind.then_some("Coverage is still being worked out; the sheet can be made once it is done.")
}

/// The open scene's sheet at `tier`, from the coverage already computed for
/// it (no camera is computed again). `scene` is the scene's name.
#[allow(clippy::too_many_arguments)]
pub fn build_sheet(
    doc: &Document,
    models: &Models,
    coverage: &CoverageState,
    bundle: &AreaBundle,
    layers: &StaticLayers,
    facade_cell_m: f32,
    scene: &str,
    generated_unix: u64,
    tier: Tier,
) -> Result<SheetSvg, String> {
    if let Some(why) = not_ready(doc, models, coverage, true) {
        return Err(why.to_owned());
    }
    let cameras: Vec<SheetCamera> = doc
        .cameras
        .iter()
        .filter(|c| c.enabled)
        .filter_map(|c| {
            Some(SheetCamera {
                placement: c,
                model: models.get(&c.model_id)?,
                sight: coverage.sight(c.id)?,
            })
        })
        .collect();
    // The figures come from the data on screen, whatever a loaded file said.
    let provenance = Provenance::of(bundle, &models.catalog_version);
    let inputs = SheetInputs {
        title: scene,
        generated_unix,
        bundle,
        layers,
        facade_cell_m,
        provenance: &provenance,
        params: doc.params,
        cameras: &cameras,
        areas: &doc.areas,
        limit_figures_to: doc.limit_figures_to,
        targets: &doc.targets,
        agent_log: &doc.agent_log,
    };
    evidence_sheet(&inputs, tier).map_err(|e| format!("Could not make the sheet: {e}"))
}

/// Hands `sheet` to the browser in `format` under `name` (without its
/// extension, which the format adds). A PNG is drawn later: its outcome goes
/// to `done`, as does every other format's at once.
pub fn export_sheet(
    format: SheetFormat,
    sheet: &SheetSvg,
    name: &str,
    files: &dyn FileDialogs,
    out: &dyn SheetOutput,
    done: Box<dyn FnOnce(Result<(), String>) + Send>,
) {
    match format {
        SheetFormat::Svg => done(files.download(&format!("{name}.svg"), SHEET_MIME, &sheet.svg)),
        SheetFormat::Png => out.download_png(&format!("{name}.png"), sheet, done),
        SheetFormat::Print => done(out.print(sheet)),
    }
}

/// The message for a failed export.
pub fn failure_text(format: SheetFormat, why: &str) -> String {
    match format {
        SheetFormat::Svg => format!("Could not save the sheet: {why}"),
        SheetFormat::Png => format!("Could not make the PNG: {why}"),
        SheetFormat::Print => format!("Could not print the sheet: {why}"),
    }
}

#[cfg(test)]
mod tests;
