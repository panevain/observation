//! The custom camera model form.

use crate::icons;
use crate::state::{TrackModels, TrackScene};
use crate::theme::Tokens;
use crate::ui::UiData;
use crate::ui::widgets::{NUMBER_TIP, danger, drag_speed, primary, row};
use bevy_egui::egui::{self, DragValue, RichText, TextEdit};
use observation_client_model::commands::{self, EditCtx, Tracked};
use observation_client_model::document::Models;
use observation_core::camera::{CameraKind, CameraModel};

/// Draft state for the custom-model form.
#[derive(Clone)]
struct Draft {
    /// The form's values; the id is empty for a new model.
    model: CameraModel,
    /// The custom model as it was when editing began; `None` for a new model.
    base: Option<CameraModel>,
}

impl Draft {
    fn editing(model: CameraModel) -> Self {
        Self {
            model: model.clone(),
            base: Some(model),
        }
    }
}

impl Default for Draft {
    fn default() -> Self {
        let model = CameraModel {
            id: String::new(),
            vendor: String::new(),
            name: "My camera".to_owned(),
            kind: CameraKind::Bullet,
            hfov_deg: 110.0,
            vfov_deg: 60.0,
            resolution: [2560, 1440],
            ir_range_m: Some(10.0),
            source: None,
            notes: Some("Custom model".to_owned()),
        };
        Self { model, base: None }
    }
}

/// What the custom-model form asked for.
enum DraftAction {
    /// Edit this custom model, or start a new one if the id is empty.
    Pick(String),
    Add,
    Save,
    Delete,
}

pub(super) fn custom_model(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let id = egui::Id::new("custom-model-draft");
    let mut draft: Draft = ui.data_mut(|m| m.get_temp(id)).unwrap_or_default();
    // The model being edited changed under the form (saved, deleted, undone
    // or replaced by an import, perhaps by another model with the same id):
    // start again from what is there now.
    if let Some(base) = &draft.base {
        let current = d.models.custom_model(&base.id);
        if current != Some(base) {
            draft = current.cloned().map(Draft::editing).unwrap_or_default();
        }
    }
    let mut action = None;
    egui::CollapsingHeader::new(
        RichText::new("CUSTOM MODELS")
            .size(t.font.caption)
            .strong()
            .color(t.text_muted),
    )
    .open(d.panels.custom_model_open.then_some(true))
    .show(ui, |ui| {
        let m = &mut draft.model;
        egui::Grid::new("custom-grid")
            .num_columns(2)
            .spacing([t.space.md, t.space.sm])
            .show(ui, |ui| {
                if !d.models.custom.is_empty() {
                    row(ui, t, "Model", |ui| {
                        let mut editing = m.id.clone();
                        egui::ComboBox::from_id_salt("custom-edit")
                            .selected_text(
                                d.models.custom_model(&editing).map_or_else(
                                    || "New model".to_owned(),
                                    CameraModel::display_name,
                                ),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut editing, String::new(), "New model");
                                for c in &d.models.custom {
                                    ui.selectable_value(
                                        &mut editing,
                                        c.id.clone(),
                                        c.display_name(),
                                    );
                                }
                            })
                            .response
                            .on_hover_text("Pick one of your models to change or delete it");
                        if editing != m.id {
                            action = Some(DraftAction::Pick(editing));
                        }
                    });
                }
                row(ui, t, "Name", |ui| {
                    ui.add(TextEdit::singleline(&mut m.name).char_limit(40));
                });
                row(ui, t, "Type", |ui| {
                    egui::ComboBox::from_id_salt("custom-kind")
                        .selected_text(m.kind.label())
                        .show_ui(ui, |ui| {
                            for k in CameraKind::ALL {
                                ui.selectable_value(&mut m.kind, k, k.label());
                            }
                        });
                });
                // The ranges limit only what is typed or dragged: a picked
                // model may come from a file, where `CameraModel::validate`
                // allows wider values, and showing it must not change it.
                row(ui, t, "Horizontal FOV", |ui| {
                    ui.add(
                        DragValue::new(&mut m.hfov_deg)
                            .range(10.0..=180.0)
                            .clamp_existing_to_range(false)
                            .speed(drag_speed(ui, 1.0))
                            .suffix("°"),
                    )
                    .on_hover_text(NUMBER_TIP);
                });
                row(ui, t, "Vertical FOV", |ui| {
                    ui.add(
                        DragValue::new(&mut m.vfov_deg)
                            .range(10.0..=180.0)
                            .clamp_existing_to_range(false)
                            .speed(drag_speed(ui, 1.0))
                            .suffix("°"),
                    )
                    .on_hover_text(NUMBER_TIP);
                });
                row(ui, t, "Resolution", |ui| {
                    ui.horizontal(|ui| {
                        let speed = drag_speed(ui, 1.0);
                        ui.add(
                            DragValue::new(&mut m.resolution[0])
                                .range(160..=8192)
                                .clamp_existing_to_range(false)
                                .speed(speed),
                        )
                        .on_hover_text(NUMBER_TIP);
                        ui.label("×");
                        ui.add(
                            DragValue::new(&mut m.resolution[1])
                                .range(120..=8192)
                                .clamp_existing_to_range(false)
                                .speed(speed),
                        )
                        .on_hover_text(NUMBER_TIP);
                    });
                });
                row(ui, t, "IR range", |ui| {
                    let mut r = m.ir_range_m.unwrap_or(0.0);
                    if ui
                        .add(
                            DragValue::new(&mut r)
                                .range(0.0..=60.0)
                                .clamp_existing_to_range(false)
                                .speed(drag_speed(ui, 1.0))
                                .suffix(" m"),
                        )
                        .on_hover_text(NUMBER_TIP)
                        .changed()
                    {
                        m.ir_range_m = Some(r);
                    }
                });
            });
        if draft.base.is_none() {
            if ui.add(primary(t, "Add model")).clicked() {
                action = Some(DraftAction::Add);
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                let changed = draft.base.as_ref() != Some(&draft.model);
                if ui
                    .add_enabled(changed, primary(t, "Save changes"))
                    .on_hover_text("Cameras with this model use the new values")
                    .clicked()
                {
                    action = Some(DraftAction::Save);
                }
                if ui
                    .add(danger(t, &format!("{}  Delete", icons::TRASH)))
                    .on_hover_text("Only a model no camera uses can be deleted")
                    .clicked()
                {
                    action = Some(DraftAction::Delete);
                }
            });
        }
    });
    match action {
        Some(DraftAction::Pick(id)) => {
            draft = d
                .models
                .custom_model(&id)
                .cloned()
                .map(Draft::editing)
                .unwrap_or_default();
        }
        Some(DraftAction::Add) => edit_models(d, &mut |ctx, models| {
            // Go on editing the added model, so a second click cannot add a copy.
            if let Some(model) = commands::add_custom_model(ctx, models, draft.model.clone()) {
                draft = Draft::editing(model);
            }
        }),
        Some(DraftAction::Save) => edit_models(d, &mut |ctx, models| {
            commands::save_custom_model(ctx, models, draft.model.clone());
        }),
        Some(DraftAction::Delete) => edit_models(d, &mut |ctx, models| {
            commands::delete_custom_model(ctx, models, &draft.model.id);
        }),
        None => {}
    }
    ui.data_mut(|m| m.insert_temp(id, draft));
}

/// Runs a custom-model command; the scene and the models are marked changed
/// only if it writes to them.
fn edit_models(d: &mut UiData, f: &mut dyn FnMut(&mut EditCtx, &mut dyn Tracked<Models>)) {
    f(
        &mut EditCtx {
            doc: &mut TrackScene(&mut d.scene),
            editor: &mut d.editor,
            panels: &mut d.panels,
            toasts: &mut d.toasts,
        },
        &mut TrackModels(&mut d.models),
    );
}
