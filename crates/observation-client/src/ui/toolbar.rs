//! Top bar, tool rail and status bar.

use super::UiData;
use super::widgets::{hint, secondary};
use crate::persist::PersistAction;
use crate::state::{LoadArea, Tool};
use bevy::prelude::*;
use bevy_egui::egui::{self, Align, Layout, RichText, Stroke, vec2};
use observation_core::scene::AreaRef;

pub fn top_bar(
    ui: &mut egui::Ui,
    d: &mut UiData,
    load: &mut MessageWriter<LoadArea>,
    persist: &mut MessageWriter<PersistAction>,
) {
    let t = *d.tokens;
    ui.horizontal_centered(|ui| {
        ui.label(RichText::new("Observation").size(t.font.title).strong());
        ui.add_space(t.space.md);
        ui.separator();
        let area = match (&d.area, d.scene.area) {
            (Some(a), _) => match a.area {
                AreaRef::Demo => "Demo street".to_owned(),
                AreaRef::Osm { bbox } => format!(
                    "OSM area · {:.0} × {:.0} m",
                    bbox.width_m(),
                    bbox.height_m()
                ),
            },
            (None, _) => "No area loaded".to_owned(),
        };
        ui.label(RichText::new(area).color(t.text_muted));
        if ui
            .add(secondary(&t, "Change area…"))
            .on_hover_text("Pick a street on the map")
            .clicked()
        {
            d.picker
                .open_at(d.scene.origin, d.area.as_ref().map(|a| a.bundle.bbox));
            d.ui.map_open = true;
        }
        if ui
            .add(secondary(&t, "Demo street"))
            .on_hover_text("Load the bundled demo street")
            .clicked()
        {
            load.write(LoadArea::demo());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(secondary(&t, "?"))
                .on_hover_text("Help and shortcuts (?)")
                .clicked()
            {
                d.ui.help_open = !d.ui.help_open;
            }
            if ui
                .add(secondary(&t, "Export"))
                .on_hover_text("Download the scene as JSON")
                .clicked()
            {
                persist.write(PersistAction::Export);
            }
            if ui
                .add(secondary(&t, "Import"))
                .on_hover_text("Open a scene JSON file")
                .clicked()
            {
                persist.write(PersistAction::Import);
            }
        });
    });
}

pub fn tool_rail(ui: &mut egui::Ui, d: &mut UiData) {
    let t = *d.tokens;
    let size = vec2(
        t.layout.toolbar_width - 2.0 * t.space.sm,
        t.layout.hit_target + t.space.md,
    );
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = t.space.xs;
        for tool in Tool::ALL {
            let on = d.ui.tool == tool;
            let disabled = tool.needs_selection() && d.ui.selected.is_none();
            let text = RichText::new(tool.label())
                .size(t.font.caption)
                .color(if on { t.on_accent } else { t.text });
            let btn = egui::Button::new(text)
                .fill(if on { t.accent } else { t.surface })
                .stroke(Stroke::new(
                    t.stroke_width,
                    if on { t.accent } else { t.border },
                ))
                .min_size(size);
            let resp = ui
                .add_enabled(!disabled, btn)
                .on_hover_text(format!(
                    "{} ({})\n{}",
                    tool.label(),
                    tool.shortcut(),
                    tool.hint()
                ))
                .on_disabled_hover_text("Select a camera first");
            if resp.clicked() {
                d.ui.tool = tool;
            }
        }
        ui.add_space(t.space.md);
        ui.separator();
        ui.add_space(t.space.md);
        let ui_state = &mut *d.ui;
        let toggles: [(&str, &str, &mut bool); 2] = [
            (
                "Cones",
                "Show view-cone outlines",
                &mut ui_state.show_frustums,
            ),
            (
                "Walls",
                "Show coverage on building walls",
                &mut ui_state.show_facades,
            ),
        ];
        for (label, tip, value) in toggles {
            let on = *value;
            let text = RichText::new(label).size(t.font.caption).color(if on {
                t.text
            } else {
                t.text_muted
            });
            let btn = egui::Button::new(text)
                .fill(if on { t.surface_sunken } else { t.surface })
                .stroke(Stroke::new(t.stroke_width, t.border))
                .min_size(size);
            if ui.add(btn).on_hover_text(tip).clicked() {
                *value = !on;
            }
        }
    });
}

pub fn status_bar(ui: &mut egui::Ui, d: &UiData) {
    let t = *d.tokens;
    ui.horizontal_centered(|ui| {
        hint(ui, &t, d.ui.tool.hint());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let attribution = d.area.as_ref().map_or("© OpenStreetMap contributors", |a| {
                a.bundle.attribution.as_str()
            });
            ui.hyperlink_to(
                RichText::new(attribution)
                    .size(t.font.caption)
                    .color(t.text_muted),
                "https://www.openstreetmap.org/copyright",
            );
        });
    });
}
