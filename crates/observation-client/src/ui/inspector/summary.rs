//! The coverage summary above the tabs.

use crate::icons;
use crate::state::{InspectorTab, Tool};
use crate::theme::{Tokens, strong_family};
use crate::ui::UiData;
use crate::ui::widgets::{bar, hint, section_title, stat};
use bevy_egui::egui::{self, RichText};
use observation_client_model::vm::{CoverageSummaryVm, settings_line};

/// Always in view above the tabs: the figures, the region they are limited
/// to, what they leave out, the analysis settings they depend on and the
/// date of the map data under them (and whether it changed since a restored
/// or imported scene was saved).
pub(super) fn summary(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let vm = CoverageSummaryVm::new(&d.scene, &d.coverage);
    ui.horizontal(|ui| {
        ui.label(section_title(t, vm.title));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            region_menu(ui, d, t, vm.in_region);
        });
    });
    crate::ui::provenance::data_change_notice(ui, t, &mut d.data_change);
    match &vm.figures {
        None => hint(
            ui,
            t,
            "No cameras yet: the figures appear once you place one.",
        ),
        Some(figures) => {
            let [street, multi, walls] = figures.stats;
            ui.columns(3, |c| {
                stat(&mut c[0], t, street.0, street.1, t.text);
                stat(&mut c[1], t, multi.0, multi.1, t.text);
                stat(&mut c[2], t, walls.0, walls.1, t.text);
            });
            bar(ui, t, figures.street, t.coverage_color(1));
            ladder(ui, d, t, &vm);
            hint(ui, t, &figures.areas);
            crate::ui::provenance::caveats(ui, t);
        }
    }
    let text = format!(
        "{}  {}",
        icons::SLIDERS_HORIZONTAL,
        settings_line(&d.scene.params)
    );
    if ui
        .add(
            egui::Button::new(RichText::new(text).size(t.font.caption).color(t.text_muted))
                .frame(false)
                .truncate(),
        )
        .on_hover_text("Detail level, lighting and subject height: change them under Analysis")
        .clicked()
    {
        d.panels.inspector_tab = InspectorTab::Analysis;
    }
    if let Some(a) = &d.area {
        crate::ui::provenance::card_data_date(ui, t, &a.bundle);
    }
}

/// The street seen at every detail level under the current lighting, from
/// Detect down to Identify (and Custom when selected), with the selected level
/// highlighted. Clicking a level selects it.
fn ladder(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens, vm: &CoverageSummaryVm) {
    // Nothing counted yet (no area loaded).
    if vm.ladder.is_empty() {
        return;
    }
    let mut pick = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        hint(ui, t, &vm.ladder_lead);
        for (i, rung) in vm.ladder.iter().enumerate() {
            if i > 0 {
                hint(ui, t, " · ");
            }
            let mut rich = RichText::new(&rung.text).size(t.font.caption);
            rich = if rung.selected {
                rich.family(strong_family()).color(t.accent)
            } else {
                rich.color(t.text_muted)
            };
            if ui
                .add(egui::Button::new(rich).frame(false))
                .on_hover_text(&rung.tip)
                .clicked()
            {
                pick = Some(rung.level);
            }
        }
    });
    if let Some(level) = pick.filter(|l| *l != d.scene.params.dori) {
        d.scene.params.dori = level;
        d.scene.touch();
    }
}

/// Draw, toggle, redraw or clear the region the figures are limited to.
/// `active`: the figures are limited to the region.
fn region_menu(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens, active: bool) {
    let label = RichText::new(format!("{}  Region", icons::POLYGON))
        .size(t.font.caption)
        .color(if active { t.accent } else { t.text_muted });
    ui.menu_button(label, |ui| {
        let Some(r) = &d.scene.region else {
            if ui
                .button("Draw region…")
                .on_hover_text("Limit the figures to an area you outline on the ground (R)")
                .clicked()
            {
                d.editor.tool = Tool::Region;
            }
            return;
        };
        let mut on = r.enabled;
        let toggled = ui
            .checkbox(
                &mut on,
                format!("Only inside region ({:.0} m²)", r.area_m2()),
            )
            .on_hover_text("Count only the street and walls inside the outlined area")
            .changed();
        if ui
            .button("Redraw")
            .on_hover_text("Outline a new region (R)")
            .clicked()
        {
            d.editor.tool = Tool::Region;
        } else if ui.button("Clear").clicked() {
            d.scene.region = None;
            d.scene.touch();
        } else if toggled && let Some(r) = &mut d.scene.region {
            r.enabled = on;
            d.scene.touch();
        }
    })
    .response
    .on_hover_text("Limit the figures to part of the area");
}
