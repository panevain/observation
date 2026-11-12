//! Right-hand inspector: a coverage summary that stays in view, then tabs
//! for the camera list, the selected camera and the analysis settings.

mod camera_details;
mod camera_list;
mod custom_model;
mod summary;

use camera_details::{camera_details, model_combo};
use camera_list::{camera_list, empty_state};
use custom_model::custom_model;
use summary::summary;

use super::UiData;
use super::widgets::{
    NUMBER_TIP, drag_speed, hint, primary, secondary, segmented, subsection, swatch, tabs,
};
use crate::addresses::address_label;
use crate::icons;
use crate::state::{Editor, InspectorTab, OverlayMode, Panels, Tool, select};
use crate::theme::Tokens;
use bevy_egui::egui::{self, DragValue, RichText, ScrollArea};
use observation_core::camera::{
    CameraPlacement, DoriLevel, Lighting, MAX_CUSTOM_PPM, MIN_CUSTOM_PPM,
};

pub fn inspector(ui: &mut egui::Ui, d: &mut UiData) {
    let t = *d.tokens;
    follow_selection(&d.editor, &mut d.panels);
    if d.editor.jump_to_address {
        let groups = d.addresses.groups(&d.scene.cameras);
        if picked_group(d, &groups).is_some() {
            // Show the list it scrolls.
            d.panels.inspector_tab = InspectorTab::Cameras;
        } else {
            // No camera is listed there: nothing to scroll to.
            d.editor.jump_to_address = false;
        }
    }
    ui.spacing_mut().item_spacing.y = t.space.sm;
    summary(ui, d, &t);
    let camera = d
        .editor
        .selected
        .and_then(|id| d.scene.camera(id))
        .map(|c| c.label.trim())
        .filter(|l| !l.is_empty())
        .unwrap_or("Camera")
        .to_owned();
    tabs(
        ui,
        &t,
        &mut d.panels.inspector_tab,
        &[
            (InspectorTab::Cameras, "Cameras"),
            (InspectorTab::Camera, &camera),
            (InspectorTab::Analysis, "Analysis"),
        ],
    );
    ui.separator();
    let tab = d.panels.inspector_tab;
    // Only the list sets this; without it a row hovered last stays
    // highlighted in the view.
    if (tab != InspectorTab::Cameras || d.scene.cameras.is_empty()) && d.hover.hover_list.is_some()
    {
        d.hover.hover_list = None;
    }
    // One scroll area per tab, so each keeps its own position.
    ScrollArea::vertical()
        .id_salt(tab)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            match tab {
                InspectorTab::Cameras => cameras_tab(ui, d, &t),
                InspectorTab::Camera => camera_tab(ui, d, &t),
                InspectorTab::Analysis => analysis_tab(ui, d, &t),
            }
            ui.add_space(t.space.lg);
        });
}

/// Opens the Camera tab when the selection changes to another camera, and
/// goes back to the list when the selection is cleared there.
fn follow_selection(editor: &Editor, panels: &mut Panels) {
    if editor.selected != panels.inspector_seen {
        match editor.selected {
            Some(_) => panels.inspector_tab = InspectorTab::Camera,
            None if panels.inspector_tab == InspectorTab::Camera => {
                panels.inspector_tab = InspectorTab::Cameras;
            }
            None => {}
        }
        panels.inspector_seen = editor.selected;
    }
}

/// The group the address picked with the Address tool is listed under, if
/// any camera is listed there.
fn picked_group<'a>(
    d: &UiData,
    groups: &[(Option<&'a str>, Vec<&'a CameraPlacement>)],
) -> Option<&'a str> {
    d.editor
        .selected_address
        .as_ref()
        .map(address_label)
        .and_then(|label| d.addresses.group_for(groups, &label))
}

/// The model for new cameras, Place, the camera list and custom models.
fn cameras_tab(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    hint(ui, t, "New camera model");
    model_combo(
        ui,
        "place-model",
        &d.models.catalog.models,
        &d.models.custom,
        &mut d.editor.place_model,
    );
    if ui
        .add(primary(t, &format!("{}  Place camera…", icons::PLUS)))
        .on_hover_text("Then click a wall or the ground (P)")
        .clicked()
    {
        d.editor.tool = Tool::Place;
    }
    ui.add_space(t.space.xs);
    if d.scene.cameras.is_empty() {
        empty_state(ui, t);
    } else {
        camera_list(ui, d, t);
    }
    ui.separator();
    custom_model(ui, d, t);
}

/// The selected camera's settings, with a way back to the list and to the
/// cameras before and after it.
fn camera_tab(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let Some(id) = d.editor.selected else {
        hint(
            ui,
            t,
            "No camera selected. Click a camera in the view or in the Cameras list to change it here.",
        );
        return;
    };
    let order: Vec<u32> = d
        .addresses
        .groups(&d.scene.cameras)
        .iter()
        .flat_map(|(_, cameras)| cameras.iter().map(|c| c.id))
        .collect();
    ui.horizontal(|ui| {
        if ui
            .add(secondary(t, &format!("{}  Cameras", icons::ARROW_LEFT)))
            .on_hover_text("Back to the camera list")
            .clicked()
        {
            d.panels.inspector_tab = InspectorTab::Cameras;
        }
        let Some(i) = order.iter().position(|&c| c == id) else {
            return;
        };
        let n = order.len();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let next = ui
                .add_enabled(n > 1, secondary(t, icons::CARET_RIGHT))
                .on_hover_text("Next camera in the list");
            ui.label(
                RichText::new(format!("{} of {n}", i + 1))
                    .size(t.font.caption)
                    .color(t.text_muted),
            );
            let prev = ui
                .add_enabled(n > 1, secondary(t, icons::CARET_LEFT))
                .on_hover_text("Previous camera in the list");
            if next.clicked() {
                select(&mut d.editor, &mut d.panels, order[(i + 1) % n]);
            } else if prev.clicked() {
                select(&mut d.editor, &mut d.panels, order[(i + n - 1) % n]);
            }
        });
    });
    camera_details(ui, d, t);
}

/// What the figures count, and how the overlay colours the ground.
fn analysis_tab(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let mut p = d.scene.params;
    subsection(ui, t, "Detail level (pixels per metre on a person)");
    let opts: Vec<(DoriLevel, &str)> = DoriLevel::ALL.iter().map(|l| (*l, l.label())).collect();
    segmented(ui, t, &mut p.dori, &opts);
    if p.dori == DoriLevel::Custom {
        ui.horizontal_wrapped(|ui| {
            ui.label("Pixels per metre");
            ui.add(
                DragValue::new(&mut p.custom_ppm)
                    .range(MIN_CUSTOM_PPM..=MAX_CUSTOM_PPM)
                    .speed(drag_speed(ui, 1.0))
                    .suffix(" px/m"),
            )
            .on_hover_text(NUMBER_TIP);
        });
    }
    hint(
        ui,
        t,
        match p.dori {
            DoriLevel::Detect => "Detect (25 px/m): can tell someone is there.",
            DoriLevel::Observe => "Observe (62 px/m): clothing and activity.",
            DoriLevel::Recognise => "Recognise (125 px/m): a person you know.",
            DoriLevel::Identify => "Identify (250 px/m): a stranger, beyond doubt.",
            DoriLevel::Custom => "Custom: your own density, e.g. measured from real footage.",
        },
    );
    subsection(ui, t, "Lighting");
    segmented(
        ui,
        t,
        &mut p.lighting,
        &[(Lighting::Day, "Day"), (Lighting::Night, "Night (IR)")],
    );
    subsection(ui, t, "Subject height");
    ui.horizontal_wrapped(|ui| {
        ui.add(
            DragValue::new(&mut p.subject_height_m)
                .range(0.0..=2.5)
                .speed(drag_speed(ui, 0.05))
                .suffix(" m"),
        )
        .on_hover_text(NUMBER_TIP);
        for (label, h) in [("Ground", 0.0), ("Torso", 1.0), ("Face", 1.6)] {
            if ui.add(secondary(t, label)).clicked() {
                p.subject_height_m = h;
            }
        }
    });
    if p != d.scene.params {
        d.scene.params = p;
        d.scene.touch();
    }
    subsection(ui, t, "Overlay colours");
    segmented(
        ui,
        t,
        &mut d.panels.overlay,
        &[
            (OverlayMode::Overlap, "Overlap"),
            (OverlayMode::PerCamera, "Per camera"),
        ],
    );
    if d.panels.overlay == OverlayMode::Overlap {
        ui.horizontal_wrapped(|ui| {
            for (i, label) in ["1", "2", "3", "4+"].iter().enumerate() {
                swatch(ui, t, t.coverage_ramp[i]);
                ui.label(
                    RichText::new(*label)
                        .size(t.font.caption)
                        .color(t.text_muted),
                );
            }
            hint(ui, t, "cameras");
        });
    } else {
        hint(ui, t, "Each camera's coverage in its own colour.");
    }
}
