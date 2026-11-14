//! The Places tab: the labelled areas and the targets. A row opens when it
//! is picked (and its outline is highlighted in the 3D view); an open area
//! can be named, labelled, switched off, made the one the figures are
//! limited to, redrawn or deleted, and an open target shows every camera's
//! verdict on it.

use super::footage::{footer, verdict_row};
use crate::icons;
use crate::state::{Tool, TrackScene};
use crate::theme::{NO_FILL, Tokens, strong_family};
use crate::ui::UiData;
use crate::ui::widgets::{danger, hint, primary, secondary, segmented, subsection, swatch};
use bevy_egui::egui::{self, RichText, TextEdit};
use observation_client_model::commands::EditCtx;
use observation_client_model::editor::TargetShape;
use observation_client_model::places;
use observation_client_model::vm::{AreaRowVm, ONE_SIDED_NOTE, PlacesVm, TargetRowVm, VerdictVm};
use observation_core::footage::{AreaLabel, MAX_NAME_CHARS};

pub(super) fn places_tab(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let vm = PlacesVm::new(&d.scene, &d.editor);
    subsection(ui, t, "Areas");
    hint(
        ui,
        t,
        "Outline your land, a neighbour's or public ground and label it. The labels are your own statement: no property data is used.",
    );
    if ui
        .add(primary(t, &format!("{}  Draw area…", icons::POLYGON)))
        .on_hover_text("Then click its corners on the ground (R)")
        .clicked()
    {
        places::draw_area(&mut d.editor);
    }
    if vm.areas.is_empty() {
        hint(ui, t, "No areas yet.");
    }
    for row in &vm.areas {
        area_row(ui, d, t, row);
    }
    ui.separator();
    subsection(ui, t, "Targets");
    hint(
        ui,
        t,
        "Mark a door, a gate or a path, then see whether the model rules out each camera seeing it.",
    );
    let mut shape = d.editor.target_shape;
    segmented(
        ui,
        t,
        &mut shape,
        &[
            (TargetShape::Point, "Click a wall or the ground"),
            (TargetShape::Outline, "Outline on the ground"),
        ],
    );
    if shape != d.editor.target_shape {
        d.editor.target_shape = shape;
        d.editor.region_draft.clear();
    }
    if ui
        .add(primary(t, &format!("{}  Mark target…", icons::TARGET)))
        .on_hover_text(match shape {
            TargetShape::Point => {
                "Then click a wall for a 1 m patch of it, or the ground for a 1 m square (T)"
            }
            TargetShape::Outline => "Then click its corners on the ground, at most 30 m across (T)",
        })
        .clicked()
    {
        d.editor.set_tool(Tool::Target);
    }
    if vm.targets.is_empty() {
        hint(ui, t, "No targets yet.");
    } else {
        if let Some(area) = d.area.as_deref() {
            d.footage.update_verdicts(&d.scene, &d.models, &area.layers);
        }
        for row in &vm.targets {
            target_row(ui, d, t, row);
        }
        hint(ui, t, ONE_SIDED_NOTE);
    }
    footer(ui, d, t);
}

/// A row that picks its area or target when clicked, filled while picked.
fn picker_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    selected: bool,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    egui::Frame::new()
        .fill(if selected { t.hover } else { NO_FILL })
        .corner_radius(egui::CornerRadius::same(t.radius.md))
        .inner_margin(egui::Margin::symmetric(t.space.xs as i8, t.space.xs as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(add);
        })
        .response
        .interact(egui::Sense::click())
}

fn area_row(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens, row: &AreaRowVm) {
    let id = row.id;
    let header = picker_row(ui, t, row.selected, |ui| {
        swatch(ui, t, t.area_color(row.label));
        let color = if row.enabled { t.text } else { t.text_muted };
        ui.add(
            egui::Label::new(
                RichText::new(&row.title)
                    .family(strong_family())
                    .color(color),
            )
            .truncate(),
        );
        let mut detail = format!("{} · {}", row.label_text, row.area);
        if row.limits {
            detail.push_str(" · limits figures");
        }
        if !row.enabled {
            detail.push_str(" · off");
        }
        hint(ui, t, &detail);
    });
    if header.clicked() {
        d.editor.selected_area = if row.selected { None } else { Some(id) };
    }
    if !row.selected {
        return;
    }
    ui.indent(("area", id), |ui| {
        let mut name = row.name.clone();
        if ui
            .add(
                TextEdit::singleline(&mut name)
                    .hint_text("Name, e.g. Front garden")
                    .char_limit(MAX_NAME_CHARS)
                    .desired_width(f32::INFINITY),
            )
            .on_hover_text("Area name")
            .changed()
        {
            places::rename_area(&mut TrackScene(&mut d.scene), id, &name);
        }
        let mut label = row.label;
        egui::ComboBox::from_id_salt(("area-label", id))
            .selected_text(row.label_text)
            .show_ui(ui, |ui| {
                for option in std::iter::once(None).chain(AreaLabel::ALL.map(Some)) {
                    ui.selectable_value(&mut label, option, places::label_text(option));
                }
            })
            .response
            .on_hover_text(
                "What this ground is, as you say: your own, a neighbour's, public or other",
            );
        if label != row.label {
            places::set_area_label(&mut TrackScene(&mut d.scene), id, label);
        }
        ui.horizontal_wrapped(|ui| {
            let mut on = row.enabled;
            if ui
                .checkbox(&mut on, "Counts")
                .on_hover_text("Off: kept, but left out of the split and of any limit")
                .changed()
            {
                places::set_area_enabled(&mut TrackScene(&mut d.scene), id, on);
            }
            let mut limits = row.limits;
            if ui
                .checkbox(&mut limits, "Limits the figures")
                .on_hover_text("Count the Coverage figures only inside this area")
                .changed()
            {
                places::set_figures_limit(&mut TrackScene(&mut d.scene), limits.then_some(id));
            }
        });
        if let Some(problem) = row.problem {
            ui.label(
                RichText::new(format!("{}  {problem}", icons::WARNING))
                    .size(t.font.caption)
                    .color(t.warning),
            );
        }
        ui.horizontal(|ui| {
            if ui
                .add(secondary(t, &format!("{}  Redraw", icons::PENCIL_SIMPLE)))
                .on_hover_text("Click a new outline on the ground; the name and label stay")
                .clicked()
            {
                places::redraw_area(&mut d.editor, id);
            }
            if ui
                .add(danger(t, &format!("{}  Delete", icons::TRASH)))
                .clicked()
            {
                places::delete_area(
                    &mut EditCtx {
                        doc: &mut TrackScene(&mut d.scene),
                        editor: &mut d.editor,
                        panels: &mut d.panels,
                        toasts: &mut d.toasts,
                    },
                    id,
                );
            }
        });
    });
}

fn target_row(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens, row: &TargetRowVm) {
    let id = row.id;
    let Some(target) = d.scene.targets.iter().find(|x| x.id == id).cloned() else {
        return;
    };
    let verdicts: Vec<VerdictVm> = d
        .scene
        .cameras
        .iter()
        .filter(|c| c.enabled)
        .filter_map(|c| {
            d.footage
                .verdicts(id, c.id)
                .map(|v| VerdictVm::new(&d.scene.params, c, &target, v))
        })
        .collect();
    let open = verdicts.iter().filter(|v| !v.ruled_out_now).count();
    let header = picker_row(ui, t, row.selected, |ui| {
        ui.label(RichText::new(icons::TARGET).color(t.text_muted));
        ui.add(egui::Label::new(RichText::new(&row.title).family(strong_family())).truncate());
        let summary = match (verdicts.len(), open) {
            (0, _) => row.kind.clone(),
            (n, k) => format!("{} · not ruled out for {k} of {n}", row.kind),
        };
        hint(ui, t, &summary);
    });
    if header.clicked() {
        d.editor.selected_target = if row.selected { None } else { Some(id) };
    }
    if !row.selected {
        return;
    }
    ui.indent(("target", id), |ui| {
        let mut name = row.name.clone();
        if ui
            .add(
                TextEdit::singleline(&mut name)
                    .hint_text("Name, e.g. Front door")
                    .char_limit(MAX_NAME_CHARS)
                    .desired_width(f32::INFINITY),
            )
            .on_hover_text("Target name")
            .changed()
        {
            places::rename_target(&mut TrackScene(&mut d.scene), id, &name);
        }
        if verdicts.is_empty() {
            hint(ui, t, "No cameras in coverage.");
        }
        for v in &verdicts {
            verdict_row(ui, t, v, false);
        }
        if ui
            .add(danger(t, &format!("{}  Delete", icons::TRASH)))
            .clicked()
        {
            places::delete_target(
                &mut EditCtx {
                    doc: &mut TrackScene(&mut d.scene),
                    editor: &mut d.editor,
                    panels: &mut d.panels,
                    toasts: &mut d.toasts,
                },
                id,
            );
        }
    });
}
