//! What the coverage figures rest on and leave out: the OSM data date (in
//! the status bar and the Coverage card), the caveats line under the
//! figures, and the notice that a restored or imported scene's map data has
//! changed since it was saved.
//!
//! Tooltips are built only while hovered (`on_hover_ui`), not every frame.
//!
//! The caveat text itself lives in [`observation_core::provenance`], so
//! anything else that prints figures says exactly the same.

use crate::icons;
use crate::state::UiState;
use crate::theme::Tokens;
use bevy_egui::egui::{self, RichText};
use observation_core::bundle::AreaBundle;
use observation_core::provenance::{CAVEATS_SUMMARY, caveats_text, format_date, format_utc_minute};

/// "OSM data as of 2026-10-31", or `None` when the source did not say.
pub fn data_date(bundle: &AreaBundle) -> Option<String> {
    bundle
        .osm_base_unix
        .map(|t| format!("OSM data as of {}", format_date(t)))
}

/// The data date to the minute and when the area was built, for a tooltip.
pub fn data_date_tip(bundle: &AreaBundle) -> String {
    let mut tip = match bundle.osm_base_unix {
        Some(t) => format!(
            "The OpenStreetMap data this area was built from was current at {}. \
             Changes mapped since then are not in it.",
            format_utc_minute(t)
        ),
        None => "The map data source did not say when its data was current.".to_owned(),
    };
    if bundle.generated_unix > 0 {
        tip.push_str(&format!(
            "\nThe server built the area at {}.",
            format_utc_minute(bundle.generated_unix)
        ));
    }
    tip.push_str("\nScenes saved from here record both.");
    tip
}

/// The data date in the status bar, to the right of the attribution.
pub fn status_data_date(ui: &mut egui::Ui, t: &Tokens, bundle: &AreaBundle) {
    if let Some(text) = data_date(bundle) {
        ui.label(RichText::new(text).size(t.font.caption).color(t.text_muted))
            .on_hover_ui(|ui| {
                ui.label(data_date_tip(bundle));
            });
    }
}

/// The data date in the Coverage card; it says so when it is unknown.
pub fn card_data_date(ui: &mut egui::Ui, t: &Tokens, bundle: &AreaBundle) {
    let text = data_date(bundle).unwrap_or_else(|| "OSM data date unknown".to_owned());
    ui.label(
        RichText::new(format!("{}  {text}", icons::DATABASE))
            .size(t.font.caption)
            .color(t.text_muted),
    )
    .on_hover_ui(|ui| {
        ui.label(data_date_tip(bundle));
    });
}

/// The one line of what the figures leave out; hovering it shows the full
/// list.
pub fn caveats(ui: &mut egui::Ui, t: &Tokens) {
    ui.label(
        RichText::new(format!("{}  {CAVEATS_SUMMARY}", icons::INFO))
            .size(t.font.caption)
            .color(t.text_muted),
    )
    .on_hover_ui(|ui| {
        ui.label(caveats_text());
    });
}

/// "Map data has changed since this scene was saved (…)", in the warning
/// colour, with a button to dismiss it.
pub fn data_change_notice(ui: &mut egui::Ui, t: &Tokens, state: &mut UiState) {
    let Some(change) = &state.data_change else {
        return;
    };
    let mut dismiss = false;
    ui.horizontal(|ui| {
        let close = ui
            .add(egui::Button::new(RichText::new(icons::X).size(t.font.caption)).frame(false))
            .on_hover_text("Dismiss");
        dismiss = close.clicked();
        ui.add(
            egui::Label::new(
                RichText::new(format!("{}  {}", icons::WARNING, change.message()))
                    .size(t.font.caption)
                    .color(t.warning),
            )
            .wrap(),
        )
        .on_hover_ui(|ui| {
            ui.label(change.details());
        });
    });
    if dismiss {
        state.data_change = None;
    }
}
