//! The coverage summary above the tabs.

use crate::icons;
use crate::state::{InspectorTab, Tool};
use crate::theme::{Tokens, strong_family};
use crate::ui::UiData;
use crate::ui::widgets::{bar, hint, section_title, stat};
use bevy_egui::egui::{self, RichText};
use observation_core::camera::{DoriLevel, Lighting};

/// Always in view above the tabs: the figures, the region they are limited
/// to, what they leave out, the analysis settings they depend on and the
/// date of the map data under them (and whether it changed since a restored
/// or imported scene was saved).
pub(super) fn summary(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let in_region = d.scene.region.as_ref().is_some_and(|r| r.is_active());
    ui.horizontal(|ui| {
        ui.label(section_title(
            t,
            if in_region {
                "Coverage inside region"
            } else {
                "Coverage"
            },
        ));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            region_menu(ui, d, t, in_region);
        });
    });
    crate::ui::provenance::data_change_notice(ui, t, &mut d.ui);
    let s = d.coverage.stats;
    if d.scene.cameras.is_empty() {
        hint(
            ui,
            t,
            "No cameras yet: the figures appear once you place one.",
        );
    } else {
        ui.columns(3, |c| {
            stat(&mut c[0], t, s.street_fraction(), "of street seen", t.text);
            stat(
                &mut c[1],
                t,
                s.street_multi_fraction(),
                "by 2+ cameras",
                t.text,
            );
            stat(&mut c[2], t, s.facade_fraction(), "of walls seen", t.text);
        });
        bar(ui, t, s.street_fraction(), t.coverage_color(1));
        ladder(ui, d, t);
        hint(
            ui,
            t,
            &format!(
                "{:.0} m² of {:.0} m² street surface · {:.0} m² open ground seen",
                s.street_seen_m2, s.street_m2, s.open_ground_seen_m2
            ),
        );
        crate::ui::provenance::caveats(ui, t);
    }
    let p = d.scene.params;
    let dori = match p.dori {
        DoriLevel::Custom => format!("Custom {:.0} px/m", p.custom_ppm),
        l => l.label().to_owned(),
    };
    let light = match p.lighting {
        Lighting::Day => "Day",
        Lighting::Night => "Night",
    };
    let text = format!(
        "{}  {dori} · {light} · {:.2} m high",
        icons::SLIDERS_HORIZONTAL,
        p.subject_height_m
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
fn ladder(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    // Nothing counted yet (no area loaded).
    if d.coverage.ladder().next().is_none() {
        return;
    }
    let p = d.scene.params;
    let light = match p.lighting {
        Lighting::Day => "by day",
        Lighting::Night => "at night",
    };
    let mut pick = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        hint(ui, t, &format!("Street seen {light}: "));
        for (i, (level, stats)) in d.coverage.ladder().enumerate() {
            if i > 0 {
                hint(ui, t, " · ");
            }
            let name = match level {
                DoriLevel::Custom => format!("Custom {:.0} px/m", p.pixels_per_metre()),
                l => l.label().to_owned(),
            };
            let text = format!("{name} {:.0}%", stats.street_fraction() * 100.0);
            let selected = level == p.dori;
            let mut rich = RichText::new(text).size(t.font.caption);
            rich = if selected {
                rich.family(strong_family()).color(t.accent)
            } else {
                rich.color(t.text_muted)
            };
            let tip = if selected {
                format!("The figures above are at {name}")
            } else {
                format!("Show the figures and overlay at {name}")
            };
            if ui
                .add(egui::Button::new(rich).frame(false))
                .on_hover_text(tip)
                .clicked()
            {
                pick = Some(level);
            }
        }
    });
    if let Some(level) = pick.filter(|l| *l != p.dori) {
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
