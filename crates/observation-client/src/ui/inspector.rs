//! Right-hand inspector: coverage summary, analysis settings, cameras.

use super::UiData;
use super::actions::{self, CameraAction};
use super::widgets::{
    NUMBER_TIP, bar, compass_dial, danger, drag_speed, hint, primary, row, secondary, section,
    segmented, stat, subsection, swatch, swatch_button,
};
use crate::cameras::camera_color;
use crate::icons;
use crate::state::{OverlayMode, Tool};
use crate::theme::{NO_FILL, Tokens, strong_family, to_rgb};
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
        empty_state(ui, d, t);
        return;
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

/// In place of three 0% figures: what to do first.
fn empty_state(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    egui::Frame::new()
        .fill(t.surface_sunken)
        .corner_radius(egui::CornerRadius::same(t.radius.lg))
        .inner_margin(egui::Margin::same(t.space.md as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(icons::SECURITY_CAMERA)
                        .size(t.font.heading)
                        .color(t.text_muted),
                );
                ui.label(
                    RichText::new("No cameras yet")
                        .size(t.font.title)
                        .family(strong_family()),
                );
            });
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Choose a model under Cameras, then click a wall to mount it or the ground to put it on a pole. The street and wall coverage appears here.",
                    )
                    .color(t.text_muted),
                )
                .wrap(),
            );
            ui.add_space(t.space.xs);
            if ui
                .add(primary(t, &format!("{}  Place camera…", icons::PLUS)))
                .on_hover_text("P")
                .clicked()
            {
                d.ui.tool = Tool::Place;
            }
        });
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
        .add(primary(t, &format!("{}  Place camera…", icons::PLUS)))
        .on_hover_text("Then click a wall or the ground (P)")
        .clicked()
    {
        d.ui.tool = Tool::Place;
    }
    let mut action = None;
    let mut hovered = None;
    ui.spacing_mut().item_spacing.y = 2.0;
    for c in &d.scene.cameras {
        let model = d
            .models
            .get(&c.model_id)
            .map_or_else(|| "Unknown model".to_owned(), CameraModel::display_name);
        let row = CameraRow {
            label: &c.label,
            model: &model,
            glyph: icons::kind(d.models.kind_of(&c.model_id)),
            color: camera_color(&d.scene, c, &d.models, t),
            enabled: c.enabled,
            selected: d.ui.selected == Some(c.id),
            highlighted: d.ui.hover_scene == Some(c.id),
        };
        let (resp, eye) = camera_row(ui, t, &row);
        if resp.hovered() || eye.hovered() {
            hovered = Some(c.id);
        }
        if eye.clicked() {
            action = Some((c.id, CameraAction::ToggleEnabled));
        } else if resp.clicked() {
            // Clicking the selected row deselects it.
            if row.selected {
                d.ui.selected = None;
            } else {
                action = Some((c.id, CameraAction::Select));
            }
        }
        resp.on_hover_text(&model).context_menu(|ui| {
            for a in [
                CameraAction::Select,
                CameraAction::Focus,
                CameraAction::Aim,
                CameraAction::Move,
                CameraAction::Duplicate,
                CameraAction::ToggleEnabled,
                CameraAction::Delete,
            ] {
                let mut button = egui::Button::new(format!("{}  {}", a.icon(), a.label()));
                if let Some(k) = a.shortcut() {
                    button = button.shortcut_text(k);
                }
                if a == CameraAction::ToggleEnabled {
                    button = button.selected(c.enabled);
                }
                if ui.add(button).clicked() {
                    action = Some((c.id, a));
                }
            }
        });
    }
    ui.spacing_mut().item_spacing.y = t.space.sm;
    if d.ui.hover_list != hovered {
        d.ui.hover_list = hovered;
    }
    if let Some((id, a)) = action {
        actions::apply(d, id, a);
    }
}

/// What one row of the camera list shows.
struct CameraRow<'a> {
    label: &'a str,
    model: &'a str,
    glyph: &'a str,
    color: egui::Color32,
    enabled: bool,
    selected: bool,
    /// Hovered in the 3D view.
    highlighted: bool,
}

/// A full-width camera row: an eye toggle, the colour, the type icon, the
/// name and the model. Returns the row's response and the eye's.
fn camera_row(ui: &mut egui::Ui, t: &Tokens, row: &CameraRow) -> (egui::Response, egui::Response) {
    let height = t.layout.hit_target - 4.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let eye_rect = egui::Rect::from_min_size(rect.min, egui::vec2(height, height));
    let eye = ui
        .interact(eye_rect, resp.id.with("eye"), egui::Sense::click())
        .on_hover_text(if row.enabled {
            "Included in coverage: click to leave it out"
        } else {
            "Left out of coverage: click to include it"
        });
    if !ui.is_rect_visible(rect) {
        return (resp, eye);
    }
    let painter = ui.painter();
    let fill = if row.selected {
        t.accent
    } else if resp.hovered() || eye.hovered() || row.highlighted {
        t.hover
    } else {
        NO_FILL
    };
    painter.rect_filled(rect, egui::CornerRadius::same(t.radius.md), fill);
    let (fg, muted) = if row.selected {
        (t.on_accent, t.on_accent)
    } else if row.enabled {
        (t.text, t.text_muted)
    } else {
        (t.text_muted, t.text_muted)
    };
    let font = egui::FontId::proportional(t.font.body);
    let eye_glyph = if row.enabled {
        icons::EYE
    } else {
        icons::EYE_SLASH
    };
    let eye_color = if eye.hovered() { fg } else { muted };
    painter.text(
        eye_rect.center(),
        egui::Align2::CENTER_CENTER,
        eye_glyph,
        font.clone(),
        eye_color,
    );
    let mut x = eye_rect.max.x + t.space.xs;
    let sw = egui::Rect::from_center_size(
        egui::pos2(x + t.space.md / 2.0, rect.center().y),
        egui::vec2(t.space.md, t.space.md),
    );
    let swatch_color = if row.enabled {
        row.color
    } else {
        row.color.gamma_multiply(0.35)
    };
    painter.rect(
        sw,
        egui::CornerRadius::same(t.radius.sm),
        swatch_color,
        egui::Stroke::new(t.stroke_width, t.border),
        egui::StrokeKind::Inside,
    );
    x = sw.max.x + t.space.sm;
    let glyph = painter.layout_no_wrap(row.glyph.to_owned(), font.clone(), muted);
    painter.galley(
        egui::pos2(x, rect.center().y - glyph.size().y / 2.0),
        glyph.clone(),
        muted,
    );
    x += glyph.size().x + t.space.sm;
    let avail = (rect.max.x - t.space.sm - x).max(0.0);
    let mut job = egui::text::LayoutJob::default();
    job.append(
        row.label,
        0.0,
        egui::TextFormat::simple(egui::FontId::new(t.font.body, strong_family()), fg),
    );
    job.append(row.model, t.space.sm, egui::TextFormat::simple(font, muted));
    job.wrap = egui::text::TextWrapping::truncate_at_width(avail);
    let text = ui.fonts_mut(|f| f.layout_job(job));
    painter.galley(
        egui::pos2(x, rect.center().y - text.size().y / 2.0),
        text,
        fg,
    );
    (resp, eye)
}

fn camera_details(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let Some(id) = d.ui.selected else { return };
    let Some(mut c) = d.scene.camera(id).cloned() else {
        d.ui.selected = None;
        return;
    };
    let color = camera_color(&d.scene, &c, &d.models, t);
    let model = d.models.get(&c.model_id).cloned();
    section(ui, t, "Selected camera");
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

    subsection(ui, t, "Position (m from the area centre)");
    ui.horizontal(|ui| {
        for (i, prefix, speed, range) in [
            (0, "E ", 0.1, None),
            (2, "S ", 0.1, None),
            (1, "H ", 0.05, Some(0.0..=60.0)),
        ] {
            let mut field = DragValue::new(&mut c.position[i])
                .speed(drag_speed(ui, speed))
                .prefix(prefix)
                .max_decimals(2);
            if let Some(r) = range {
                field = field.range(r);
            }
            ui.add(field).on_hover_text(NUMBER_TIP);
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
                    .on_hover_text("Negative looks down");
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
            let text = format!("{}  {}", a.icon(), a.label());
            let tip = a
                .shortcut()
                .map_or_else(|| a.label().to_owned(), |k| format!("{} ({k})", a.label()));
            if ui.add(secondary(t, &text)).on_hover_text(tip).clicked() {
                action = Some(a);
            }
        }
        let del = CameraAction::Delete;
        if ui
            .add(danger(t, &format!("{}  {}", del.icon(), del.label())))
            .on_hover_text("Delete (Del)")
            .clicked()
        {
            action = Some(del);
        }
    });

    if d.scene.camera(id).is_some_and(|old| *old != c)
        && let Some(slot) = d.scene.camera_mut(id)
    {
        *slot = c;
    }
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
