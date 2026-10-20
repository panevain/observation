//! Small reusable widgets built on the design tokens.

use crate::theme::{NO_FILL, Tokens, strong_family};
use bevy_egui::egui::{self, Color32, RichText, Sense, Stroke, Ui, vec2};

/// Section heading with consistent spacing.
pub fn section(ui: &mut Ui, t: &Tokens, title: &str) {
    ui.add_space(t.space.sm);
    ui.label(
        RichText::new(title.to_uppercase())
            .size(t.font.caption)
            .family(strong_family())
            .color(t.text_muted),
    );
    ui.add_space(t.space.xs);
}

/// Secondary explanatory text.
pub fn hint(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.label(RichText::new(text).size(t.font.caption).color(t.text_muted));
}

/// A large percentage figure with a caption underneath.
pub fn stat(ui: &mut Ui, t: &Tokens, value: f32, caption: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.label(
            RichText::new(format!("{:.0}%", value * 100.0))
                .size(t.font.heading + 4.0)
                .family(strong_family())
                .color(color),
        );
        ui.label(
            RichText::new(caption)
                .size(t.font.caption)
                .color(t.text_muted),
        );
    });
}

/// Horizontal progress bar for a fraction.
pub fn bar(ui: &mut Ui, t: &Tokens, frac: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), t.space.sm), Sense::hover());
    let r = egui::CornerRadius::same(t.radius.sm);
    ui.painter().rect_filled(rect, r, t.surface_sunken);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, r, color);
}

/// Small filled colour swatch.
pub fn swatch(ui: &mut Ui, t: &Tokens, color: Color32) -> egui::Response {
    paint_swatch(ui, t, color, t.space.md, Sense::hover(), false)
}

/// A larger clickable swatch, outlined when `selected` or hovered.
pub fn swatch_button(ui: &mut Ui, t: &Tokens, color: Color32, selected: bool) -> egui::Response {
    paint_swatch(ui, t, color, t.space.lg, Sense::click(), selected)
}

fn paint_swatch(
    ui: &mut Ui,
    t: &Tokens,
    color: Color32,
    side: f32,
    sense: Sense,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), sense);
    let stroke = if selected || (sense.senses_click() && resp.hovered()) {
        Stroke::new(t.stroke_width * 2.0, t.text)
    } else {
        Stroke::new(t.stroke_width, t.border)
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(t.radius.sm),
        color,
        stroke,
        egui::StrokeKind::Inside,
    );
    resp
}

/// A segmented control; returns true if the value changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    t: &Tokens,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.space.xs, t.space.xs);
        for (v, label) in options {
            let on = *value == *v;
            let text = RichText::new(*label).color(if on { t.on_accent } else { t.text });
            let btn = egui::Button::new(text)
                .fill(if on { t.accent } else { t.surface_sunken })
                .stroke(Stroke::new(
                    t.stroke_width,
                    if on { t.accent } else { t.border },
                ))
                .min_size(vec2(0.0, t.layout.hit_target));
            if ui.add(btn).clicked() && !on {
                *value = *v;
                changed = true;
            }
        }
    });
    changed
}

/// Primary (accent) button.
pub fn primary(t: &Tokens, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(t.on_accent).strong())
        .fill(t.accent)
        .stroke(Stroke::NONE)
        .min_size(vec2(0.0, t.layout.hit_target))
}

/// Neutral button with the standard hit target. No outline: the theme's fill
/// steps show hover and press.
pub fn secondary(t: &Tokens, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(t.text))
        .stroke(Stroke::NONE)
        .min_size(vec2(0.0, t.layout.hit_target))
}

/// Destructive button.
pub fn danger(t: &Tokens, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(t.danger))
        .fill(t.surface_sunken)
        .stroke(Stroke::new(t.stroke_width, t.danger))
        .min_size(vec2(0.0, t.layout.hit_target))
}

/// Label/value row in a two-column grid.
pub fn row(ui: &mut Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(RichText::new(label).color(t.text_muted));
    add(ui);
    ui.end_row();
}

/// How a tool-rail button behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailKind {
    /// One of the exclusive tools: accent fill when on.
    Tool,
    /// An independent on/off switch: a sunken fill and an accent bar when on,
    /// so it never looks like the active tool.
    Toggle,
}

/// A tool-rail button: icon over a caption, centred, with the keyboard
/// shortcut in the corner.
#[allow(clippy::too_many_arguments)]
pub fn rail_button(
    ui: &mut Ui,
    t: &Tokens,
    icon: RichText,
    label: &str,
    shortcut: Option<&str>,
    size: egui::Vec2,
    kind: RailKind,
    on: bool,
    enabled: bool,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = enabled && resp.hovered();
    let fill = match (kind, on) {
        (RailKind::Tool, true) => t.accent,
        (RailKind::Toggle, true) => t.surface_sunken,
        _ if hovered => t.hover,
        _ => NO_FILL,
    };
    let fg = match (kind, on, enabled) {
        (RailKind::Tool, true, _) => t.on_accent,
        (RailKind::Toggle, false, _) => t.text_muted,
        (_, _, true) => t.text,
        (_, _, false) => t.text_muted.gamma_multiply(0.6),
    };
    let painter = ui.painter();
    let r = egui::CornerRadius::same(t.radius.md);
    painter.rect_filled(rect, r, fill);
    if kind == RailKind::Toggle && on {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.min.x + t.space.md, rect.max.y - 3.0),
            egui::pos2(rect.max.x - t.space.md, rect.max.y - 1.0),
        );
        painter.rect_filled(bar, egui::CornerRadius::same(1), t.accent);
    }
    let icon = egui::WidgetText::from(icon.size(t.font.title + 2.0).color(fg)).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::FontSelection::Default,
    );
    let caption = egui::WidgetText::from(RichText::new(label).size(t.font.caption).color(fg))
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            rect.width() - 2.0,
            egui::FontSelection::Default,
        );
    let gap = 2.0;
    let total = icon.size().y + gap + caption.size().y;
    let top = rect.center().y - total / 2.0;
    let icon_pos = egui::pos2(rect.center().x - icon.size().x / 2.0, top);
    let caption_pos = egui::pos2(
        rect.center().x - caption.size().x / 2.0,
        top + icon.size().y + gap,
    );
    painter.galley(icon_pos, icon, fg);
    painter.galley(caption_pos, caption, fg);
    if let Some(key) = shortcut {
        let key_color = if kind == RailKind::Tool && on {
            t.on_accent
        } else {
            t.text_muted
        };
        painter.text(
            rect.right_top() + vec2(-4.0, 3.0),
            egui::Align2::RIGHT_TOP,
            key,
            egui::FontId::monospace(t.font.caption - 2.0),
            key_color,
        );
    }
    resp
}

/// A key name drawn as a keycap.
pub fn keycap(ui: &mut Ui, t: &Tokens, key: &str) {
    egui::Frame::new()
        .fill(t.surface_sunken)
        .stroke(Stroke::new(t.stroke_width, t.border))
        .corner_radius(egui::CornerRadius::same(t.radius.sm))
        .inner_margin(egui::Margin::symmetric(t.space.xs as i8, 0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(key)
                    .size(t.font.caption)
                    .monospace()
                    .color(t.text_muted),
            );
        });
}

/// Tooltip for every number field.
pub const NUMBER_TIP: &str = "Drag to change · Shift: finer · Ctrl: coarser · click to type";

/// Drag speed for a number field: ten times faster while Ctrl (or Cmd) is
/// held. egui already slows a drag tenfold with Shift.
pub fn drag_speed(ui: &Ui, speed: f64) -> f64 {
    if ui.input(|i| i.modifiers.command_only()) {
        speed * 10.0
    } else {
        speed
    }
}

/// Small caption heading inside a section.
pub fn subsection(ui: &mut Ui, t: &Tokens, title: &str) {
    ui.add_space(t.space.xs);
    ui.label(
        RichText::new(title)
            .size(t.font.caption)
            .family(strong_family())
            .color(t.text_muted),
    );
}

/// A compass for a heading (degrees clockwise from north). Click or drag to
/// point it; Shift snaps to 15°. The wedge shows the horizontal field of view.
pub fn compass_dial(
    ui: &mut Ui,
    t: &Tokens,
    heading: &mut f32,
    fov_deg: Option<f32>,
    color: Color32,
) -> egui::Response {
    let side = 64.0;
    let (rect, mut resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let c = rect.center();
    let r = side / 2.0 - 2.0;
    let dir = |deg: f32| {
        let a = deg.to_radians();
        vec2(a.sin(), -a.cos())
    };
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
        && p.distance(c) > 3.0
    {
        let v = p - c;
        let mut deg = v.x.atan2(-v.y).to_degrees().rem_euclid(360.0);
        deg = if ui.input(|i| i.modifiers.shift) {
            (deg / 15.0).round() * 15.0
        } else {
            deg.round()
        };
        let deg = deg.rem_euclid(360.0);
        if deg != *heading {
            *heading = deg;
            resp.mark_changed();
        }
    }
    let painter = ui.painter();
    let ring = if resp.hovered() || resp.dragged() {
        t.text_muted
    } else {
        t.border_strong
    };
    painter.circle(c, r, t.surface_sunken, Stroke::new(t.stroke_width, ring));
    for q in 0..4 {
        let d = dir(q as f32 * 90.0);
        painter.line_segment(
            [c + d * (r - 5.0), c + d * r],
            Stroke::new(t.stroke_width, ring),
        );
    }
    painter.text(
        c + dir(0.0) * (r - 11.0),
        egui::Align2::CENTER_CENTER,
        "N",
        egui::FontId::new(t.font.caption - 2.0, egui::FontFamily::Proportional),
        t.text_muted,
    );
    if let Some(fov) = fov_deg {
        let half = (fov / 2.0).min(179.0);
        let steps = 16;
        let mut pts = vec![c];
        for i in 0..=steps {
            let a = *heading - half + 2.0 * half * i as f32 / steps as f32;
            pts.push(c + dir(a) * (r - 4.0));
        }
        painter.add(egui::Shape::convex_polygon(
            pts,
            color.gamma_multiply(0.35),
            Stroke::new(t.stroke_width, color),
        ));
    }
    painter.line_segment(
        [c, c + dir(*heading) * (r - 2.0)],
        Stroke::new(t.stroke_width * 2.0, t.text),
    );
    painter.circle_filled(c, 2.5, t.text);
    resp
}
