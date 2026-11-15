//! The coverage summary above the tabs.

use crate::icons;
use crate::state::{InspectorTab, TrackScene};
use crate::theme::{Tokens, strong_family};
use crate::ui::UiData;
use crate::ui::widgets::{bar, hint, section_title, stat};
use bevy_egui::egui::{self, RichText};
use observation_client_model::vm::{CoverageSummaryVm, settings_line};
use observation_client_model::{commands, places};

/// Always in view above the tabs: the figures, the area they are limited
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
    if let Some(level) = pick {
        let mut params = d.scene.params;
        params.dori = level;
        commands::set_params(&mut TrackScene(&mut d.scene), params);
    }
}

/// Which area the figures are limited to: draw one, pick one of the areas,
/// turn the limit off and on, redraw the area or stop limiting.
/// `active`: the figures are limited to an area.
fn region_menu(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens, active: bool) {
    let label = RichText::new(format!("{}  Limit", icons::POLYGON))
        .size(t.font.caption)
        .color(if active { t.accent } else { t.text_muted });
    ui.menu_button(label, |ui| {
        let limit = d
            .scene
            .region()
            .map(|r| (r.id, places::area_title(r), r.area_m2(), r.enabled));
        let others: Vec<(u32, String)> = d
            .scene
            .areas
            .iter()
            .filter(|a| Some(a.id) != d.scene.limit_figures_to && a.problem().is_none())
            .map(|a| (a.id, places::area_title(a)))
            .collect();
        match limit {
            None => {
                if ui
                    .button("Draw area to limit to…")
                    .on_hover_text("Limit the figures to an area you outline on the ground")
                    .clicked()
                {
                    places::draw_limit_area(&mut d.editor);
                }
            }
            Some((_, title, area_m2, enabled)) => {
                let mut on = enabled;
                let toggled = ui
                    .checkbox(&mut on, format!("Only inside {title} ({area_m2:.0} m²)"))
                    .on_hover_text("Count only the street and walls inside the outlined area")
                    .changed();
                if ui
                    .button("Redraw")
                    .on_hover_text("Outline the area again")
                    .clicked()
                {
                    places::draw_limit_area(&mut d.editor);
                } else if ui
                    .button("Stop limiting")
                    .on_hover_text("Count everywhere again; the area stays under Places")
                    .clicked()
                {
                    places::set_figures_limit(&mut TrackScene(&mut d.scene), None);
                } else if toggled {
                    commands::set_region_enabled(&mut TrackScene(&mut d.scene), on);
                }
            }
        }
        if !others.is_empty() {
            ui.separator();
            hint(ui, t, "Limit to one of your areas");
            for (id, title) in others {
                if ui.button(title).clicked() {
                    places::set_figures_limit(&mut TrackScene(&mut d.scene), Some(id));
                }
            }
        }
    })
    .response
    .on_hover_text("Limit the figures to one of your areas");
}
