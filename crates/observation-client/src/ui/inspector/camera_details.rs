//! The selected camera's settings, and the camera model picker.

use crate::addresses::NO_ADDRESS;
use crate::cameras::camera_color;
use crate::icons;
use crate::state::{TrackScene, terrain_of};
use crate::theme::{Tokens, strong_family, to_rgb};
use crate::ui::UiData;
use crate::ui::actions::{self, CameraAction};
use crate::ui::widgets::{
    NUMBER_TIP, compass_dial, danger, drag_speed, row, secondary, subsection, swatch, swatch_button,
};
use bevy_egui::egui::{self, DragValue, RichText, TextEdit};
use observation_client_model::commands;
use observation_core::camera::{
    ASSUMED_IR_RANGE_M, CameraKind, CameraModel, CameraPlacement, MAX_GROUP_CHARS,
    forward_from_yaw_pitch, is_https_url,
};

pub(super) fn camera_details(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let Some(id) = d.editor.selected else { return };
    let Some(mut c) = d.scene.camera(id).cloned() else {
        d.editor.selected = None;
        return;
    };
    let color = camera_color(&d.scene, &c, &d.models, t);
    let model = d.models.get(&c.model_id).cloned();
    ui.horizontal(|ui| {
        swatch(ui, t, color);
        ui.label(
            RichText::new(icons::kind(d.models.kind_of(&c.model_id)))
                .size(t.font.title)
                .color(t.text_muted),
        );
        ui.add(
            TextEdit::singleline(&mut c.label)
                .font(egui::FontId::new(t.font.title, strong_family()))
                .desired_width(f32::INFINITY)
                .char_limit(48),
        )
        .on_hover_text("Camera name");
    });
    model_combo(
        ui,
        "sel-model",
        &d.models.catalog.models,
        &d.models.custom,
        &mut c.model_id,
    );
    color_row(ui, d, t, &mut c);
    group_row(ui, d, t, &mut c);

    subsection(
        ui,
        t,
        "Position (m from the area centre, height above the ground)",
    );
    ui.horizontal(|ui| {
        // Height is above the ground under the camera, so moving it east or
        // south keeps it at the same height over a slope.
        let terrain = terrain_of(d.area.as_deref());
        let [mut east, _, mut south] = c.position;
        let mut height = terrain.height_above(c.position);
        let mut changed = false;
        for (value, prefix, speed, range) in [
            (&mut east, "E ", 0.1, None),
            (&mut south, "S ", 0.1, None),
            (&mut height, "H ", 0.05, Some(0.0..=60.0)),
        ] {
            let mut field = DragValue::new(value)
                .speed(drag_speed(ui, speed))
                .prefix(prefix)
                .max_decimals(2);
            if let Some(r) = range {
                // Showing a camera must not move it.
                field = field.range(r).clamp_existing_to_range(false);
            }
            changed |= ui.add(field).on_hover_text(NUMBER_TIP).changed();
        }
        if changed {
            let ground = terrain.height_at(observation_core::glam::Vec2::new(east, south));
            c.position = [east, ground + height, south];
        }
    });

    subsection(ui, t, "Orientation");
    ui.horizontal(|ui| {
        let mut yaw = c.yaw_deg;
        let fov = model.as_ref().map(|m| m.hfov_deg);
        if compass_dial(ui, t, &mut yaw, fov, color)
            .on_hover_text("Heading: click or drag to point the camera (Shift snaps to 15°)")
            .changed()
        {
            c.yaw_deg = yaw;
        }
        egui::Grid::new("orientation")
            .num_columns(2)
            .spacing([t.space.sm, t.space.xs])
            .show(ui, |ui| {
                row(ui, t, "Heading", |ui| {
                    ui.horizontal(|ui| {
                        let mut yaw = c.yaw_deg;
                        if ui
                            .add(
                                DragValue::new(&mut yaw)
                                    .speed(drag_speed(ui, 1.0))
                                    .suffix("°")
                                    .max_decimals(1),
                            )
                            .on_hover_text(NUMBER_TIP)
                            .changed()
                        {
                            c.yaw_deg = yaw.rem_euclid(360.0);
                        }
                        let f = forward_from_yaw_pitch(c.yaw_deg, c.pitch_deg);
                        ui.label(RichText::new(compass(c.yaw_deg)).color(t.text_muted))
                            .on_hover_text(format!(
                                "Faces ({:.2}, {:.2}, {:.2}) as (east, north, up)",
                                f.x, -f.z, f.y
                            ));
                    });
                });
                row(ui, t, "Tilt", |ui| {
                    ui.add(
                        DragValue::new(&mut c.pitch_deg)
                            .range(-90.0..=90.0)
                            .speed(drag_speed(ui, 0.5))
                            .suffix("°")
                            .max_decimals(1),
                    )
                    .on_hover_text(format!("Negative looks down. {NUMBER_TIP}"));
                });
                row(ui, t, "Roll", |ui| {
                    ui.add(
                        DragValue::new(&mut c.roll_deg)
                            .range(-180.0..=180.0)
                            .speed(drag_speed(ui, 0.5))
                            .suffix("°")
                            .max_decimals(1),
                    )
                    .on_hover_text(NUMBER_TIP);
                });
            });
    });

    if let Some(m) = &model {
        egui::CollapsingHeader::new(
            RichText::new("Optics")
                .size(t.font.caption)
                .family(strong_family())
                .color(t.text_muted),
        )
        .id_salt("optics")
        .show(ui, |ui| {
            egui::Grid::new("optics-grid")
                .num_columns(2)
                .spacing([t.space.md, t.space.sm])
                .show(ui, |ui| {
                    row(ui, t, "Field of view", |ui| {
                        ui.label(format!("{:.0}° × {:.0}°", m.hfov_deg, m.vfov_deg));
                    });
                    row(ui, t, "Resolution", |ui| {
                        ui.label(format!("{} × {}", m.resolution[0], m.resolution[1]));
                    });
                    row(ui, t, "Night vision", |ui| match m.ir_range_m {
                        Some(r) => {
                            ui.label(format!("{r:.1} m"));
                        }
                        None => {
                            ui.label(
                                RichText::new(format!("{ASSUMED_IR_RANGE_M:.1} m (assumed)"))
                                    .color(t.warning),
                            )
                            .on_hover_text("The manufacturer does not publish an IR range");
                        }
                    });
                    if let Some(r) = d.coverage.ranges.get(&id) {
                        row(ui, t, "Modelled range", |ui| {
                            ui.label(format!("{r:.1} m"));
                        });
                    }
                    // Validated on load too; never hand a non-https link to the browser.
                    if let Some(src) = m.source.as_deref().filter(|s| is_https_url(s)) {
                        row(ui, t, "Source", |ui| {
                            ui.hyperlink_to("spec sheet", src);
                        });
                    }
                });
        });
    }

    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        for a in [
            CameraAction::Aim,
            CameraAction::Move,
            CameraAction::Focus,
            CameraAction::Duplicate,
        ] {
            let text = format!("{}  {}", actions::icon(a), a.label());
            let tip = a
                .shortcut()
                .map_or_else(|| a.label().to_owned(), |k| format!("{} ({k})", a.label()));
            if ui.add(secondary(t, &text)).on_hover_text(tip).clicked() {
                action = Some(a);
            }
        }
        let del = CameraAction::Delete;
        if ui
            .add(danger(
                t,
                &format!("{}  {}", actions::icon(del), del.label()),
            ))
            .on_hover_text("Delete (Del)")
            .clicked()
        {
            action = Some(del);
        }
    });

    commands::update_camera(&mut TrackScene(&mut d.scene), id, c);
    if let Some(a) = action {
        actions::apply(d, id, a);
    }
}

/// Cone colour: a picker, quick swatches and a way back to the type default.
fn color_row(ui: &mut egui::Ui, d: &UiData, t: &Tokens, c: &mut CameraPlacement) {
    let kind = d.models.kind_of(&c.model_id);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Colour").color(t.text_muted));
        let mut rgb = to_rgb(camera_color(&d.scene, c, &d.models, t));
        if ui
            .color_edit_button_srgb(&mut rgb)
            .on_hover_text("Colour of the camera, its view cone and its per-camera overlay")
            .changed()
        {
            c.color = Some(rgb);
        }
        for &p in &t.camera_palette {
            if swatch_button(ui, t, p, c.color == Some(to_rgb(p))).clicked() {
                c.color = Some(to_rgb(p));
            }
        }
        if c.color.is_some()
            && ui
                .add(secondary(t, "Default"))
                .on_hover_text(format!("Use the {} colour", kind.label().to_lowercase()))
                .clicked()
        {
            c.color = None;
        }
    });
}

/// The group the camera is listed under: its address from OpenStreetMap
/// unless a name is typed or another nearby address is picked.
fn group_row(ui: &mut egui::Ui, d: &UiData, t: &Tokens, c: &mut CameraPlacement) {
    let automatic = d.addresses.automatic(c.id);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Group").color(t.text_muted));
        // Right to left, so the field takes only the width the picker
        // leaves and never widens the panel.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            group_picker(ui, d, c);
            group_field(ui, automatic, c);
        });
    });
}

/// The group name: empty means by address.
fn group_field(ui: &mut egui::Ui, automatic: Option<&str>, c: &mut CameraPlacement) {
    let mut name = c.group.clone().unwrap_or_default();
    let resp = ui
        .add(
            TextEdit::singleline(&mut name)
                .hint_text(automatic.unwrap_or(NO_ADDRESS))
                .desired_width(ui.available_width())
                .char_limit(MAX_GROUP_CHARS),
        )
        .on_hover_text(match (name.trim().is_empty(), automatic) {
            (true, Some(a)) => format!(
                "Listed under {a}, the nearest address in OpenStreetMap. Type a name to list it elsewhere"
            ),
            (true, None) => {
                "No address in OpenStreetMap is near this camera. Type a name to group it"
                    .to_owned()
            }
            (false, Some(a)) => format!(
                "Listed under this name. Clear it to list the camera under {a}, the nearest address in OpenStreetMap"
            ),
            (false, None) => format!(
                "Listed under this name. Clear it to list the camera under {NO_ADDRESS}: no address in OpenStreetMap is near it"
            ),
        });
    if resp.changed() {
        c.group = (!name.trim().is_empty()).then_some(name);
    }
}

/// Pick a nearby address as the group, or go back to grouping by address.
fn group_picker(ui: &mut egui::Ui, d: &UiData, c: &mut CameraPlacement) {
    ui.menu_button(icons::MAP_PIN, |ui| {
        if ui
            .add(egui::Button::new("By address").selected(c.group.is_none()))
            .on_hover_text("Group by the nearest address in OpenStreetMap")
            .clicked()
        {
            c.group = None;
            ui.close();
        }
        let known = d.addresses.known();
        if !known.is_empty() {
            ui.separator();
        }
        for a in known {
            if ui
                .add(egui::Button::new(a).selected(c.group.as_deref() == Some(a)))
                .clicked()
            {
                c.group = Some(a.to_owned());
                ui.close();
            }
        }
    })
    .response
    .on_hover_text("Pick an address nearby, or go back to grouping by address");
}

fn compass(yaw: f32) -> &'static str {
    const NAMES: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    NAMES[((yaw.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

pub(super) fn model_combo(
    ui: &mut egui::Ui,
    id: &str,
    catalog: &[CameraModel],
    custom: &[CameraModel],
    value: &mut String,
) {
    let current = catalog
        .iter()
        .chain(custom)
        .find(|m| m.id == *value)
        .map_or_else(|| value.clone(), CameraModel::display_name);
    let query_id = egui::Id::new((id, "model-search"));
    let mut query: String = ui.data_mut(|m| m.get_temp(query_id)).unwrap_or_default();
    // Cut a long name short rather than widen the panel.
    let resp = egui::ComboBox::from_id_salt(id)
        .width(ui.available_width())
        .height(360.0)
        .truncate()
        .selected_text(current)
        .show_ui(ui, |ui| {
            let search = ui.add(
                TextEdit::singleline(&mut query)
                    .hint_text("Search models…")
                    .desired_width(f32::INFINITY),
            );
            // Typing always goes to the search box while the list is open, even
            // after Enter took its focus, so keys never reach the shortcuts.
            let entered = search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !search.has_focus() {
                search.request_focus();
            }
            let mut first = None;
            let mut any = false;
            for kind in CameraKind::ALL {
                let items: Vec<&CameraModel> = catalog
                    .iter()
                    .chain(custom)
                    .filter(|m| m.kind == kind && matches_query(m, &query))
                    .collect();
                if items.is_empty() {
                    continue;
                }
                any = true;
                ui.label(
                    RichText::new(format!("{}  {}", icons::kind(kind), kind.label()))
                        .small()
                        .strong(),
                );
                for m in items {
                    first.get_or_insert(m.id.clone());
                    ui.selectable_value(
                        value,
                        m.id.clone(),
                        format!("{}  ({:.0}°)", m.display_name(), m.hfov_deg),
                    );
                }
            }
            if !any {
                ui.label(RichText::new("No matching models").weak());
            }
            // Enter picks the first match of a search (an empty one matches
            // everything, so Enter alone changes nothing).
            if entered
                && !query.trim().is_empty()
                && let Some(m) = first
            {
                *value = m;
                ui.close();
            }
        });
    if resp.inner.is_none() {
        // Closed: start the next search afresh.
        query.clear();
    }
    ui.data_mut(|m| m.insert_temp(query_id, query));
}

/// Every whitespace-separated word of `query` appears in the model's vendor,
/// name, type or id (case-insensitive).
fn matches_query(m: &CameraModel, query: &str) -> bool {
    let hay = format!("{} {} {} {}", m.vendor, m.name, m.kind.label(), m.id).to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|w| hay.contains(w))
}
