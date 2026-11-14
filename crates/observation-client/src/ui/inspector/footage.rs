//! Where footage lands: the selected camera's split and its verdicts on the
//! targets (Camera tab), every camera's split side by side (Analysis tab),
//! and the verdict rows the Places tab shares. Every one of them ends with
//! the data date and the caveats line, as the Coverage card does.

use crate::icons;
use crate::theme::{Tokens, strong_family};
use crate::ui::UiData;
use crate::ui::widgets::{hint, section, swatch};
use bevy_egui::egui::{self, RichText};
use observation_client_model::vm::{
    AS_DRAWN_NOTE, ONE_SIDED_NOTE, SplitRowKind, SplitTableVm, SplitVm, VerdictVm, settings_line,
};

/// The selected camera's split, then its verdict on each target.
pub(super) fn camera_footage(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens, id: u32) {
    let Some(camera) = d.scene.camera(id).cloned() else {
        return;
    };
    section(ui, t, "Where its footage lands");
    let Some(area) = d.area.as_deref() else {
        hint(ui, t, "Load an area first.");
        return;
    };
    hint(ui, t, &settings_line(&d.scene.params));
    if !camera.enabled {
        hint(
            ui,
            t,
            "Left out of coverage: include it to see where its footage lands.",
        );
        return;
    }
    d.footage
        .update_splits(&d.scene, &d.models, &area.layers, &d.coverage);
    match d.footage.split(id) {
        None => hint(ui, t, "Working out…"),
        Some(split) => {
            let vm = SplitVm::new(&d.scene, split);
            ui.label(RichText::new(&vm.ground).family(strong_family()));
            egui::Grid::new(("split", id))
                .num_columns(2)
                .spacing([t.space.sm, t.space.xs])
                .show(ui, |ui| {
                    for row in &vm.rows {
                        ui.horizontal(|ui| {
                            match row.kind {
                                SplitRowKind::Label(l) => {
                                    swatch(ui, t, t.area_color(Some(l)));
                                }
                                SplitRowKind::Area(l) => {
                                    ui.add_space(t.space.md);
                                    swatch(ui, t, t.area_color(l));
                                }
                                SplitRowKind::Street | SplitRowKind::Unlabelled => {}
                            }
                            let text = RichText::new(&row.what).size(t.font.caption);
                            let text = match row.kind {
                                SplitRowKind::Area(_) => text.color(t.text_muted),
                                _ => text.color(t.text),
                            };
                            ui.add(egui::Label::new(text).wrap())
                                .on_hover_text(&row.tip);
                        });
                        ui.label(RichText::new(&row.value).size(t.font.caption))
                            .on_hover_text(&row.tip);
                        ui.end_row();
                    }
                });
            if d.scene.areas.is_empty() {
                hint(
                    ui,
                    t,
                    "Outline your land, a neighbour's or public ground with the Areas tool (R) to split this up.",
                );
            }
            ui.label(RichText::new(&vm.walls_total).family(strong_family()));
            if !vm.walls.is_empty() {
                egui::Grid::new(("walls", id))
                    .num_columns(2)
                    .spacing([t.space.sm, t.space.xs])
                    .show(ui, |ui| {
                        for w in &vm.walls {
                            ui.label(
                                RichText::new(&w.what)
                                    .size(t.font.caption)
                                    .color(t.text_muted),
                            );
                            ui.label(RichText::new(&w.value).size(t.font.caption));
                            ui.end_row();
                        }
                    });
                hint(
                    ui,
                    t,
                    "Buildings are numbered as in the loaded area, with the distance to the nearest wall seen; addresses are not shown, since they are only a guess.",
                );
            }
            hint(ui, t, AS_DRAWN_NOTE);
        }
    }
    if !d.scene.targets.is_empty() {
        section(ui, t, "Its view of your targets");
        d.footage.update_verdicts(&d.scene, &d.models, &area.layers);
        for target in &d.scene.targets {
            if let Some(v) = d.footage.verdicts(target.id, id) {
                verdict_row(
                    ui,
                    t,
                    &VerdictVm::new(&d.scene.params, &camera, target, v),
                    true,
                );
            }
        }
        hint(ui, t, ONE_SIDED_NOTE);
    }
    footer(ui, d, t);
}

/// Every enabled camera's split side by side.
pub(super) fn split_table(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    section(ui, t, "Where footage lands");
    let Some(area) = d.area.as_deref() else {
        hint(ui, t, "Load an area first.");
        return;
    };
    if !d.scene.cameras.iter().any(|c| c.enabled) {
        hint(ui, t, "No cameras in coverage yet.");
        return;
    }
    d.footage
        .update_splits(&d.scene, &d.models, &area.layers, &d.coverage);
    let footage = &d.footage;
    let vm = SplitTableVm::new(&d.scene, |id| footage.split(id));
    hint(
        ui,
        t,
        &format!(
            "Ground and walls seen by each camera, m², at {}. Cameras overlap, so the rows do not add up.",
            settings_line(&d.scene.params)
        ),
    );
    egui::ScrollArea::horizontal()
        .id_salt("split-table")
        .show(ui, |ui| {
            egui::Grid::new("split-table-grid")
                .num_columns(vm.columns.len() + 1)
                .striped(true)
                .spacing([t.space.md, t.space.xs])
                .show(ui, |ui| {
                    let head = |ui: &mut egui::Ui, text: &str| {
                        ui.label(
                            RichText::new(text)
                                .size(t.font.caption)
                                .family(strong_family())
                                .color(t.text_muted),
                        );
                    };
                    head(ui, "Camera");
                    for c in &vm.columns {
                        head(ui, c);
                    }
                    ui.end_row();
                    for row in &vm.rows {
                        ui.add(
                            egui::Label::new(RichText::new(&row.camera).size(t.font.caption))
                                .truncate(),
                        );
                        for cell in &row.cells {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.label(RichText::new(&cell.main).size(t.font.caption));
                                if let Some(sub) = &cell.sub {
                                    ui.label(
                                        RichText::new(sub)
                                            .size(t.font.caption - 2.0)
                                            .color(t.text_muted),
                                    );
                                }
                            });
                        }
                        ui.end_row();
                    }
                });
        });
    if vm.pending > 0 {
        hint(ui, t, "Working out the rest…");
    }
    if d.scene.areas.is_empty() {
        hint(
            ui,
            t,
            "Outline and label areas with the Areas tool (R) to see how much falls on your land, a neighbour's or public ground.",
        );
    }
    hint(ui, t, AS_DRAWN_NOTE);
    footer(ui, d, t);
}

/// One camera's verdict on one target: the verdict at the current settings,
/// then the ladder across levels and the other lighting. `by_target`: the
/// row names the target (the Camera tab), else the camera (Places tab).
pub(super) fn verdict_row(ui: &mut egui::Ui, t: &Tokens, vm: &VerdictVm, by_target: bool) {
    let who = if by_target { &vm.target } else { &vm.camera };
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(who)
                .size(t.font.caption)
                .family(strong_family()),
        );
        let color = if vm.ruled_out_now {
            t.text_muted
        } else {
            t.text
        };
        ui.label(RichText::new(&vm.now).size(t.font.caption).color(color))
            .on_hover_text(format!("At the current settings; {}", vm.nearest));
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, rung) in vm.ladder.iter().enumerate() {
            if i > 0 {
                hint(ui, t, " · ");
            }
            let mark = if rung.ruled_out {
                icons::X
            } else {
                icons::CHECK
            };
            let mut text = RichText::new(format!("{} {mark}", rung.name)).size(t.font.caption);
            text = if rung.selected {
                text.family(strong_family()).color(t.accent)
            } else {
                text.color(t.text_muted)
            };
            ui.label(text).on_hover_text(&rung.tip);
        }
    });
}

/// The data date and the caveats line, under every split and verdict.
pub(super) fn footer(ui: &mut egui::Ui, d: &UiData, t: &Tokens) {
    crate::ui::provenance::caveats(ui, t);
    if let Some(a) = &d.area {
        crate::ui::provenance::card_data_date(ui, t, &a.bundle);
    }
}
