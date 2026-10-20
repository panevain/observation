//! Right-hand inspector: coverage summary, analysis settings, cameras.

use super::UiData;
use super::widgets::{
    bar, danger, hint, primary, row, secondary, section, segmented, stat, swatch, swatch_button,
};
use crate::cameras::camera_color;
use crate::icons;
use crate::state::{OverlayMode, Tool};
use crate::theme::{Tokens, to_rgb};
use bevy_egui::egui::{self, DragValue, RichText, ScrollArea, TextEdit};
use observation_core::camera::{
    ASSUMED_IR_RANGE_M, CameraKind, CameraModel, CameraPlacement, DoriLevel, Lighting,
    forward_from_yaw_pitch, is_https_url,
};

pub fn inspector(ui: &mut egui::Ui, d: &mut UiData) {
    let t = *d.tokens;
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = t.space.sm;
            coverage_card(ui, d, &t);
            ui.separator();
            camera_list(ui, d, &t);
            if d.ui.selected.is_some() {
                ui.separator();
                camera_details(ui, d, &t);
            }
            ui.separator();
            custom_model(ui, d, &t);
            ui.separator();
            analysis(ui, d, &t);
            ui.add_space(t.space.lg);
        });
}

fn coverage_card(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let in_region = d.scene.region.as_ref().is_some_and(|r| r.is_active());
    section(
        ui,
        t,
        if in_region {
            "Coverage inside region"
        } else {
            "Coverage"
        },
    );
    let s = d.coverage.stats;
    if d.scene.cameras.is_empty() {
        hint(
            ui,
            t,
            "Place a camera to see what it covers. Use the Place tool (P) and click a wall.",
        );
    }
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
    hint(
        ui,
        t,
        &format!(
            "{:.0} m² of {:.0} m² street surface · {:.0} m² open ground seen",
            s.street_seen_m2, s.street_m2, s.open_ground_seen_m2
        ),
    );
    region_controls(ui, d, t);
    ui.add_space(t.space.xs);
    segmented(
        ui,
        t,
        &mut d.ui.overlay,
        &[
            (OverlayMode::Overlap, "Overlap"),
            (OverlayMode::PerCamera, "Per camera"),
        ],
    );
    if d.ui.overlay == OverlayMode::Overlap {
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
    }
}

/// Draw, toggle, redraw or clear the region the figures are limited to.
fn region_controls(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    ui.horizontal_wrapped(|ui| {
        let Some(r) = &d.scene.region else {
            if ui
                .add(secondary(t, "Draw region…"))
                .on_hover_text("Limit the figures to an area you outline on the ground (R)")
                .clicked()
            {
                d.ui.tool = Tool::Region;
            }
            return;
        };
        let mut on = r.enabled;
        let label = format!("Only inside region ({:.0} m²)", r.area_m2());
        let toggled = ui
            .checkbox(&mut on, label)
            .on_hover_text("Count only the street and walls inside the outlined area")
            .changed();
        if ui
            .add(secondary(t, "Redraw"))
            .on_hover_text("Outline a new region (R)")
            .clicked()
        {
            d.ui.tool = Tool::Region;
        }
        if ui.add(secondary(t, "Clear")).clicked() {
            d.scene.region = None;
            d.scene.touch();
        } else if toggled && let Some(r) = &mut d.scene.region {
            r.enabled = on;
            d.scene.touch();
        }
    });
}

fn analysis(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    section(ui, t, "Analysis");
    let mut p = d.scene.params;
    hint(ui, t, "Detail level (pixels per metre on a person)");
    let opts: Vec<(DoriLevel, &str)> = DoriLevel::ALL.iter().map(|l| (*l, l.label())).collect();
    segmented(ui, t, &mut p.dori, &opts);
    hint(
        ui,
        t,
        match p.dori {
            DoriLevel::Detect => "Detect (25 px/m): can tell someone is there.",
            DoriLevel::Observe => "Observe (62 px/m): clothing and activity.",
            DoriLevel::Recognise => "Recognise (125 px/m): a person you know.",
            DoriLevel::Identify => "Identify (250 px/m): a stranger, beyond doubt.",
        },
    );
    segmented(
        ui,
        t,
        &mut p.lighting,
        &[(Lighting::Day, "Day"), (Lighting::Night, "Night (IR)")],
    );
    ui.horizontal_wrapped(|ui| {
        ui.label("Subject height");
        ui.add(
            DragValue::new(&mut p.subject_height_m)
                .range(0.0..=2.5)
                .speed(0.05)
                .suffix(" m"),
        );
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
}

fn camera_list(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    section(ui, t, "Cameras");
    ui.horizontal(|ui| {
        ui.label("New camera model");
    });
    model_combo(
        ui,
        "place-model",
        &d.models.catalog.models,
        &d.models.custom,
        &mut d.ui.place_model,
    );
    if ui
        .add(primary(t, "Place camera…"))
        .on_hover_text("Then click a wall or the ground (P)")
        .clicked()
    {
        d.ui.tool = Tool::Place;
    }
    let mut toggle = None;
    for c in &d.scene.cameras {
        let selected = d.ui.selected == Some(c.id);
        ui.horizontal(|ui| {
            let mut on = c.enabled;
            if ui
                .checkbox(&mut on, "")
                .on_hover_text("Include in coverage")
                .changed()
            {
                toggle = Some((c.id, on));
            }
            swatch(ui, t, camera_color(&d.scene, c, &d.models, t));
            let model = d
                .models
                .get(&c.model_id)
                .map_or_else(|| "Unknown model".to_owned(), CameraModel::display_name);
            let glyph = icons::kind(d.models.kind_of(&c.model_id));
            let label = RichText::new(format!("{glyph}  {}  ·  {}", c.label, model))
                .color(if c.enabled { t.text } else { t.text_muted });
            let row = egui::Button::selectable(selected, label).truncate();
            if ui.add(row).on_hover_text(&model).clicked() {
                d.ui.selected = if selected { None } else { Some(c.id) };
            }
        });
    }
    if let Some((id, on)) = toggle
        && let Some(c) = d.scene.camera_mut(id)
    {
        c.enabled = on;
    }
}

fn camera_details(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let Some(id) = d.ui.selected else { return };
    let Some(mut c) = d.scene.camera(id).cloned() else {
        d.ui.selected = None;
        return;
    };
    section(ui, t, "Selected camera");
    ui.add(
        TextEdit::singleline(&mut c.label)
            .desired_width(f32::INFINITY)
            .char_limit(48),
    );
    model_combo(
        ui,
        "sel-model",
        &d.models.catalog.models,
        &d.models.custom,
        &mut c.model_id,
    );
    color_row(ui, d, t, &mut c);

    egui::Grid::new("cam-grid")
        .num_columns(2)
        .spacing([t.space.md, t.space.sm])
        .show(ui, |ui| {
            row(ui, t, "Position (m)", |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        DragValue::new(&mut c.position[0])
                            .speed(0.1)
                            .prefix("E ")
                            .max_decimals(2),
                    );
                    ui.add(
                        DragValue::new(&mut c.position[2])
                            .speed(0.1)
                            .prefix("S ")
                            .max_decimals(2),
                    );
                    ui.add(
                        DragValue::new(&mut c.position[1])
                            .speed(0.05)
                            .range(0.0..=60.0)
                            .prefix("H ")
                            .max_decimals(2),
                    );
                });
            });
            row(ui, t, "Heading", |ui| {
                let mut yaw = c.yaw_deg;
                if ui
                    .add(
                        DragValue::new(&mut yaw)
                            .speed(1.0)
                            .suffix("°")
                            .max_decimals(1),
                    )
                    .changed()
                {
                    c.yaw_deg = yaw.rem_euclid(360.0);
                }
                ui.label(RichText::new(compass(c.yaw_deg)).color(t.text_muted));
            });
            row(ui, t, "Tilt", |ui| {
                ui.add(
                    DragValue::new(&mut c.pitch_deg)
                        .range(-90.0..=90.0)
                        .speed(0.5)
                        .suffix("°")
                        .max_decimals(1),
                );
            });
            row(ui, t, "Roll", |ui| {
                ui.add(
                    DragValue::new(&mut c.roll_deg)
                        .range(-180.0..=180.0)
                        .speed(0.5)
                        .suffix("°")
                        .max_decimals(1),
                );
            });
            row(ui, t, "Face vector", |ui| {
                let f = forward_from_yaw_pitch(c.yaw_deg, c.pitch_deg);
                ui.label(
                    RichText::new(format!("({:.2}, {:.2}, {:.2})", f.x, -f.z, f.y)).monospace(),
                )
                .on_hover_text("Unit vector (east, north, up)");
            });
            if let Some(m) = d.models.get(&c.model_id) {
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
            }
        });

    ui.horizontal_wrapped(|ui| {
        if ui.add(secondary(t, "Aim (A)")).clicked() {
            d.ui.tool = Tool::Aim;
        }
        if ui.add(secondary(t, "Move (M)")).clicked() {
            d.ui.tool = Tool::Move;
        }
        if ui.add(secondary(t, "Duplicate")).clicked() {
            let mut copy = c.clone();
            copy.label = String::new();
            copy.position[0] += 1.0;
            let new = d.scene.add_camera(copy);
            d.ui.selected = Some(new);
        }
        if ui.add(danger(t, "Delete")).clicked() {
            d.scene.remove_camera(id);
            d.ui.selected = None;
        }
    });

    if d.scene.camera(id).is_some_and(|old| *old != c)
        && let Some(slot) = d.scene.camera_mut(id)
    {
        *slot = c;
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

fn compass(yaw: f32) -> &'static str {
    const NAMES: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    NAMES[((yaw.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

fn model_combo(
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
    let resp = egui::ComboBox::from_id_salt(id)
        .width(ui.available_width())
        .height(360.0)
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

/// Draft state for the custom-model form.
#[derive(Clone)]
struct Draft(CameraModel);

impl Default for Draft {
    fn default() -> Self {
        Self(CameraModel {
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
        })
    }
}

fn custom_model(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let id = egui::Id::new("custom-model-draft");
    let mut draft: Draft = ui.data_mut(|m| m.get_temp(id)).unwrap_or_default();
    egui::CollapsingHeader::new(
        RichText::new("CUSTOM MODEL")
            .size(t.font.caption)
            .strong()
            .color(t.text_muted),
    )
    .open(d.ui.custom_model_open.then_some(true))
    .show(ui, |ui| {
        let m = &mut draft.0;
        egui::Grid::new("custom-grid")
            .num_columns(2)
            .spacing([t.space.md, t.space.sm])
            .show(ui, |ui| {
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
                row(ui, t, "Horizontal FOV", |ui| {
                    ui.add(
                        DragValue::new(&mut m.hfov_deg)
                            .range(10.0..=180.0)
                            .suffix("°"),
                    );
                });
                row(ui, t, "Vertical FOV", |ui| {
                    ui.add(
                        DragValue::new(&mut m.vfov_deg)
                            .range(10.0..=180.0)
                            .suffix("°"),
                    );
                });
                row(ui, t, "Resolution", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(DragValue::new(&mut m.resolution[0]).range(160..=8192));
                        ui.label("×");
                        ui.add(DragValue::new(&mut m.resolution[1]).range(120..=8192));
                    });
                });
                row(ui, t, "IR range", |ui| {
                    let mut r = m.ir_range_m.unwrap_or(0.0);
                    if ui
                        .add(DragValue::new(&mut r).range(0.0..=60.0).suffix(" m"))
                        .changed()
                    {
                        m.ir_range_m = Some(r);
                    }
                });
            });
        if ui.add(primary(t, "Add model")).clicked() {
            let mut model = m.clone();
            let slug: String = model
                .name
                .to_ascii_lowercase()
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect();
            model.id = format!(
                "custom-{}-{}",
                slug.trim_matches('-'),
                d.models.custom.len() + 1
            );
            match model.validate() {
                Ok(()) => {
                    d.ui.place_model = model.id.clone();
                    d.ui.info(format!(
                        "Added “{}” — it is now the model for new cameras",
                        model.name
                    ));
                    d.models.custom.push(model);
                    d.scene.touch();
                }
                Err(e) => d.ui.error(e.to_string()),
            }
        }
    });
    ui.data_mut(|m| m.insert_temp(id, draft));
}
