//! Small reusable widgets built on the design tokens.

use crate::theme::{Tokens, strong_family};
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
