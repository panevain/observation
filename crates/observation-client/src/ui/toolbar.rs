//! Top bar, tool rail and status bar.

use super::UiData;
use super::widgets::{RailKind, hint, keycap, rail_button, secondary};
use crate::icons;
use crate::persist::PersistAction;
use crate::state::{LoadArea, Tool};
use crate::theme::{UiFont, strong_family};
use bevy::prelude::*;
use bevy_egui::egui::{self, Align, Layout, RichText, vec2};
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
            settings_menu(ui, d);
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
        ui.spacing_mut().button_padding = vec2(t.space.xs, t.space.xs);
        for tool in Tool::ALL {
            let on = d.ui.tool == tool;
            let disabled = tool.needs_selection() && d.ui.selected.is_none();
            // The active tool is the one accent-filled button, with the filled
            // icon, so the mode never depends on hue alone.
            let glyph = icons::tool(tool);
            let icon = if on {
                icons::fill::rich(glyph)
            } else {
                RichText::new(glyph)
            };
            let tip = if disabled {
                format!(
                    "{} ({})\nSelect a camera first",
                    tool.label(),
                    tool.shortcut()
                )
            } else {
                format!("{} ({})\n{}", tool.label(), tool.shortcut(), tool.hint())
            };
            let resp = rail_button(
                ui,
                &t,
                icon,
                tool.label(),
                Some(tool.shortcut()),
                size,
                RailKind::Tool,
                on,
                !disabled,
            )
            .on_hover_text(tip);
            if resp.clicked() {
                d.ui.tool = tool;
            }
        }
        ui.add_space(t.space.md);
        ui.separator();
        ui.add_space(t.space.md);
        let ui_state = &mut *d.ui;
        let toggles: [(&str, &str, &str, &mut bool); 2] = [
            (
                icons::TRIANGLE_DASHED,
                "Cones",
                "Show every camera's view cone (the selected one always shows)",
                &mut ui_state.show_frustums,
            ),
            (
                icons::WALL,
                "Walls",
                "Show coverage on building walls",
                &mut ui_state.show_facades,
            ),
        ];
        for (glyph, label, tip, value) in toggles {
            let on = *value;
            let state = if on { "on" } else { "off" };
            if rail_button(
                ui,
                &t,
                RichText::new(glyph),
                label,
                None,
                size,
                RailKind::Toggle,
                on,
                true,
            )
            .on_hover_text(format!("{tip} ({state})"))
            .clicked()
            {
                *value = !on;
            }
        }
    });
}

/// Shortcuts for the active tool on the left; the coverage figures and the
/// attribution on the right, so the figures stay in view while the inspector
/// is scrolled.
pub fn status_bar(ui: &mut egui::Ui, d: &UiData) {
    let t = *d.tokens;
    ui.horizontal_centered(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let data = d.area.as_ref().map_or("© OpenStreetMap contributors", |a| {
                a.bundle.attribution.as_str()
            });
            let tiles = d.config.tile_attribution.as_str();
            let attribution = if tiles == data {
                data.to_owned()
            } else {
                format!("{data} · Tiles {tiles}")
            };
            ui.hyperlink_to(
                RichText::new(attribution)
                    .size(t.font.caption)
                    .color(t.text_muted),
                "https://www.openstreetmap.org/copyright",
            );
            ui.separator();
            let s = d.coverage.stats;
            for (value, caption) in [
                (s.facade_fraction(), "walls"),
                (s.street_multi_fraction(), "2+ cams"),
                (s.street_fraction(), "street"),
            ] {
                ui.label(
                    RichText::new(caption)
                        .size(t.font.caption)
                        .color(t.text_muted),
                );
                ui.label(
                    RichText::new(format!("{:.0}%", value * 100.0))
                        .size(t.font.caption)
                        .family(strong_family())
                        .color(t.text),
                );
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = t.space.xs;
                for (key, what) in d.ui.tool.keymap() {
                    // Drop the rest rather than run into the figures.
                    if ui.available_width() < 140.0 {
                        break;
                    }
                    keycap(ui, &t, key);
                    ui.label(
                        RichText::new(*what)
                            .size(t.font.caption)
                            .color(t.text_muted),
                    );
                    ui.add_space(t.space.sm);
                }
            });
        });
    });
}

/// Display preferences, remembered in this browser.
fn settings_menu(ui: &mut egui::Ui, d: &mut UiData) {
    let t = *d.tokens;
    let button = secondary(&t, icons::GEAR_SIX);
    let resp = ui.add(button).on_hover_text("Settings");
    egui::Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(200.0);
            hint(ui, &t, "Font");
            for f in UiFont::ALL {
                let on = *d.font == f;
                if ui.selectable_label(on, f.label()).clicked() && !on {
                    *d.font = f;
                }
            }
        });
}
