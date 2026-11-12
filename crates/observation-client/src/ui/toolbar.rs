//! Top bar, tool rail and status bar.

use super::UiData;
use super::widgets::{RailKind, hint, keycap, rail_button, secondary};
use crate::history::{HistoryCommand, HistoryOp};
use crate::icons;
use crate::persist::PersistAction;
use crate::state::{LoadArea, Tool};
use crate::theme::{Tokens, UiFont, strong_family};
use bevy::prelude::*;
use bevy_egui::egui::{self, Align, Layout, RichText, vec2};
use observation_client_model::vm::{HistoryVm, SaveVm, StatusVm};
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
            d.panels.map_open = true;
        }
        if ui
            .add(secondary(&t, "Demo street"))
            .on_hover_text("Load the bundled demo street")
            .clicked()
        {
            load.write(LoadArea::demo());
        }
        if d.regions.enabled {
            let progress = d
                .regions
                .active_job()
                .and_then(super::regions::progress_label);
            let text = match &progress {
                Some(p) => format!("{} {p}", icons::DATABASE),
                None => icons::DATABASE.to_owned(),
            };
            let tip = match &progress {
                Some(_) => "Importing map data on the server (click for details)",
                None => "Local map data: regions this server answers itself",
            };
            if ui.add(secondary(&t, &text)).on_hover_text(tip).clicked() {
                d.panels.regions_open.toggle();
            }
            if progress.is_some() {
                ui.spinner();
            }
        }
        ui.separator();
        for button in HistoryVm::new(&d.history).0 {
            let glyph = match button.op {
                HistoryOp::Undo => icons::ARROW_COUNTER_CLOCKWISE,
                HistoryOp::Redo => icons::ARROW_CLOCKWISE,
            };
            if ui
                .add_enabled(button.enabled, secondary(&t, glyph))
                .on_hover_text(&button.tip)
                .on_disabled_hover_text(&button.tip)
                .clicked()
            {
                d.history_commands.write(HistoryCommand(button.op));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(secondary(&t, "?"))
                .on_hover_text("Help and shortcuts (?)")
                .clicked()
            {
                d.panels.help_open.toggle();
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
            let on = d.editor.tool == tool;
            let disabled = tool.needs_selection() && d.editor.selected.is_none();
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
                d.editor.tool = tool;
            }
        }
        ui.add_space(t.space.md);
        ui.separator();
        ui.add_space(t.space.md);
        let panels = &mut **d.panels;
        let toggles: [(&str, &str, &str, &mut bool); 2] = [
            (
                icons::TRIANGLE_DASHED,
                "Cones",
                "Show every camera's view cone (the selected one always shows)",
                &mut panels.show_frustums,
            ),
            (
                icons::WALL,
                "Walls",
                "Show coverage on building walls",
                &mut panels.show_facades,
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

/// Shortcuts for the active tool on the left; the coverage figures, the
/// attribution and the OSM data date on the right, so the figures stay in
/// view while the inspector is scrolled.
pub fn status_bar(ui: &mut egui::Ui, d: &UiData) {
    let t = *d.tokens;
    let vm = StatusVm::new(
        d.area.as_ref().map(|a| &a.bundle),
        &d.config.tile_attribution,
        d.save.status(),
        d.time.elapsed_secs_f64(),
        &d.coverage.stats,
    );
    ui.horizontal_centered(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Right to left: the date ends the line, after the credit.
            if let Some(a) = &d.area {
                super::provenance::status_data_date(ui, &t, &a.bundle);
            }
            ui.hyperlink_to(
                RichText::new(&vm.attribution)
                    .size(t.font.caption)
                    .color(t.text_muted),
                "https://www.openstreetmap.org/copyright",
            );
            if let Some(dem) = &vm.dem {
                let text = RichText::new(&dem.text)
                    .size(t.font.caption)
                    .color(t.text_muted);
                match dem.link {
                    Some(url) => {
                        ui.hyperlink_to(text, url);
                    }
                    None => {
                        ui.label(text);
                    }
                }
            }
            ui.separator();
            save_status(ui, &t, vm.save.as_ref());
            ui.separator();
            for (caption, value) in &vm.figures {
                ui.label(
                    RichText::new(*caption)
                        .size(t.font.caption)
                        .color(t.text_muted),
                );
                ui.label(
                    RichText::new(value)
                        .size(t.font.caption)
                        .family(strong_family())
                        .color(t.text),
                );
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = t.space.xs;
                for (key, what) in d.editor.tool.keymap() {
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

/// Whether the scene is safely in this browser's storage.
fn save_status(ui: &mut egui::Ui, t: &Tokens, save: Option<&SaveVm>) {
    let Some(save) = save else {
        return;
    };
    let (icon, color) = if save.failed {
        (icons::CLOUD_WARNING, t.warning)
    } else {
        (icons::CLOUD_CHECK, t.text_muted)
    };
    ui.label(
        RichText::new(format!("{icon} {}", save.text))
            .size(t.font.caption)
            .color(color),
    )
    .on_hover_text(&save.tip);
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
            ui.separator();
            hint(ui, &t, "Address tool");
            let mut jump = d.address_jump.0;
            if ui
                .checkbox(&mut jump, "Click jumps to the camera list")
                .on_hover_text(
                    "Clicking an address also scrolls the camera list to the cameras at it",
                )
                .changed()
            {
                d.address_jump.0 = jump;
            }
        });
}
