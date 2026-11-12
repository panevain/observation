//! The Cameras tab's list, and what it shows when there is none.

use super::picked_group;
use crate::addresses::NO_ADDRESS;
use crate::cameras::camera_color;
use crate::icons;
use crate::theme::{NO_FILL, Tokens, strong_family};
use crate::ui::UiData;
use crate::ui::actions::{self, CameraAction};
use bevy_egui::egui::{self, RichText};
use observation_client_model::vm::CameraListVm;

/// In place of an empty list: what to do first.
pub(super) fn empty_state(ui: &mut egui::Ui, t: &Tokens) {
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
                        "Choose a model above, press Place camera, then click a wall to mount it or the ground to put it on a pole. The street and wall coverage appears at the top.",
                    )
                    .color(t.text_muted),
                )
                .wrap(),
            );
        });
}

/// The camera list, grouped by address. A group of one camera has no
/// heading: its address goes under the camera's name instead.
pub(super) fn camera_list(ui: &mut egui::Ui, d: &mut UiData, t: &Tokens) {
    let mut action = None;
    let mut hovered = None;
    ui.spacing_mut().item_spacing.y = 2.0;
    let groups = d.addresses.groups(&d.scene.cameras);
    // The address picked with the Address tool: its group is shaded, and
    // scrolled into view once after a click.
    let picked = picked_group(d, &groups);
    let jump = d.editor.jump_to_address;
    if jump {
        d.editor.jump_to_address = false;
    }
    let vm = CameraListVm::new(
        &groups,
        picked,
        &d.models,
        d.editor.selected,
        d.hover.hover_scene,
    );
    for group in &vm.groups {
        if group.heading {
            let heading = group_heading(ui, t, group.name, group.rows.len(), group.picked);
            if group.picked && jump {
                heading.scroll_to_me(Some(egui::Align::TOP));
            }
        }
        let subtitle = group.subtitled.then(|| group.name.unwrap_or(NO_ADDRESS));
        for r in &group.rows {
            let c = r.camera;
            let model = &r.model;
            let row = CameraRow {
                label: &c.label,
                model,
                subtitle,
                glyph: icons::kind(d.models.kind_of(&c.model_id)),
                color: camera_color(&d.scene, c, &d.models, t),
                enabled: c.enabled,
                selected: r.selected,
                highlighted: r.highlighted,
            };
            let (resp, eye) = camera_row(ui, t, &row);
            if resp.hovered() || eye.hovered() {
                hovered = Some(c.id);
            }
            if eye.clicked() {
                action = Some((c.id, CameraAction::ToggleEnabled));
            } else if resp.clicked() {
                // Also for the selected row: back to its settings.
                action = Some((c.id, CameraAction::Select));
            }
            resp.on_hover_text(model).context_menu(|ui| {
                for a in [
                    CameraAction::Select,
                    CameraAction::Focus,
                    CameraAction::Aim,
                    CameraAction::Move,
                    CameraAction::Duplicate,
                    CameraAction::ToggleEnabled,
                    CameraAction::Delete,
                ] {
                    let mut button =
                        egui::Button::new(format!("{}  {}", actions::icon(a), a.label()));
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
    }
    ui.spacing_mut().item_spacing.y = t.space.sm;
    if d.hover.hover_list != hovered {
        d.hover.hover_list = hovered;
    }
    if let Some((id, a)) = action {
        actions::apply(d, id, a);
    }
}

/// The heading above a group of cameras: its address (or name) and count;
/// `None` for the cameras with neither. `picked` marks the address selected
/// with the Address tool.
fn group_heading(
    ui: &mut egui::Ui,
    t: &Tokens,
    group: Option<&str>,
    count: usize,
    picked: bool,
) -> egui::Response {
    ui.add_space(t.space.xs);
    let (fill, name_color) = if picked {
        (t.hover, t.text)
    } else {
        (NO_FILL, t.text_muted)
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(t.radius.md))
        .show(ui, |ui| group_heading_row(ui, t, group, count, name_color))
        .response
}

fn group_heading_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    group: Option<&str>,
    count: usize,
    name_color: egui::Color32,
) {
    ui.horizontal(|ui| {
        let (glyph, group) = match group {
            Some(g) => (icons::MAP_PIN, g),
            None => (icons::WARNING_CIRCLE, NO_ADDRESS),
        };
        ui.label(RichText::new(glyph).color(name_color));
        let count = RichText::new(count.to_string())
            .size(t.font.caption)
            .color(t.text_muted);
        // Leave room for the count after a long address.
        let count_w = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                count.text().to_owned(),
                egui::FontId::proportional(t.font.caption),
                t.text_muted,
            )
            .size()
            .x
        });
        let name_w = (ui.available_width() - count_w - ui.spacing().item_spacing.x).max(0.0);
        ui.allocate_ui(egui::vec2(name_w, ui.spacing().interact_size.y), |ui| {
            // Fill the width so the counts line up on the right.
            ui.set_min_width(name_w);
            ui.add(
                egui::Label::new(
                    RichText::new(group)
                        .size(t.font.caption)
                        .family(strong_family())
                        .color(name_color),
                )
                .truncate(),
            );
        });
        ui.label(count);
    });
}

/// What one row of the camera list shows.
struct CameraRow<'a> {
    label: &'a str,
    model: &'a str,
    /// The address of a camera listed without a group heading.
    subtitle: Option<&'a str>,
    glyph: &'a str,
    color: egui::Color32,
    enabled: bool,
    selected: bool,
    /// Hovered in the 3D view.
    highlighted: bool,
}

/// A full-width camera row: an eye toggle, the colour, the type icon, the
/// name and the model, and the address under them when there is no group
/// heading. Returns the row's response and the eye's.
fn camera_row(ui: &mut egui::Ui, t: &Tokens, row: &CameraRow) -> (egui::Response, egui::Response) {
    let line = t.layout.hit_target - 4.0;
    let height = line + row.subtitle.map_or(0.0, |_| t.font.caption);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let eye_rect = egui::Rect::from_center_size(
        egui::pos2(rect.min.x + line / 2.0, rect.center().y),
        egui::vec2(line, line),
    );
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
    let sub = row.subtitle.map(|s| {
        let mut job = egui::text::LayoutJob::single_section(
            s.to_owned(),
            egui::TextFormat::simple(egui::FontId::proportional(t.font.caption), muted),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(avail);
        ui.fonts_mut(|f| f.layout_job(job))
    });
    // The name line and the address under it, centred together.
    let top = rect.center().y - (text.size().y + sub.as_ref().map_or(0.0, |g| g.size().y)) / 2.0;
    let below = top + text.size().y;
    painter.galley(egui::pos2(x, top), text, fg);
    if let Some(sub) = sub {
        painter.galley(egui::pos2(x, below), sub, muted);
    }
    (resp, eye)
}
