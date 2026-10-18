//! Floating overlays: viewport gate, toasts, loading card, help, chips.

use super::UiData;
use super::widgets::{hint, primary};
use crate::state::{ToastKind, UiState};
use crate::theme::{MIN_VIEWPORT, Tokens};
use bevy_egui::egui::{self, Align2, Id, Order, RichText, Stroke, vec2};

/// Full-screen block for viewports below the supported minimum.
pub fn viewport_gate(ctx: &egui::Context, t: &Tokens, w: f32, h: f32) {
    egui::Area::new(Id::new("gate"))
        .order(Order::Foreground)
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            let rect = ctx.viewport_rect();
            ui.painter().rect_filled(rect, 0.0, t.bg);
            ui.allocate_rect(rect, egui::Sense::click_and_drag());
        });
    egui::Area::new(Id::new("gate-card"))
        .order(Order::Tooltip)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            t.card().show(ui, |ui| {
                ui.set_max_width(320.0);
                ui.label(RichText::new("Larger screen needed").size(t.font.heading).strong());
                ui.add_space(t.space.sm);
                ui.label(format!(
                    "Observation is designed for tablets and desktops. Please use a window of at least {:.0} × {:.0} pixels.",
                    MIN_VIEWPORT.x, MIN_VIEWPORT.y
                ));
                hint(ui, t, &format!("Current size: {w:.0} × {h:.0}"));
            });
        });
}

/// Small chips in the viewport corner: active tool and compass.
pub fn viewport_chips(ctx: &egui::Context, t: &Tokens, vp: egui::Rect, d: &UiData) {
    egui::Area::new(Id::new("chips"))
        .order(Order::Middle)
        .fixed_pos(vp.min + vec2(t.space.md, t.space.md))
        .interactable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                chip(ui, t, &format!("Tool: {}", d.ui.tool.label()));
                if let Some(c) = d.ui.selected.and_then(|id| d.scene.camera(id)) {
                    chip(ui, t, &format!("Selected: {}", c.label));
                }
            });
        });
}

fn chip(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    egui::Frame::new()
        .fill(t.surface_raised)
        .stroke(Stroke::new(t.stroke_width, t.border))
        .corner_radius(egui::CornerRadius::same(t.radius.lg))
        .inner_margin(egui::Margin::symmetric(t.space.md as i8, t.space.xs as i8))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(t.font.caption));
        });
}

pub fn toasts(ctx: &egui::Context, t: &Tokens, vp: egui::Rect, ui_state: &UiState) {
    if ui_state.toasts.is_empty() {
        return;
    }
    egui::Area::new(Id::new("toasts"))
        .order(Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(vp.center().x, vp.max.y - t.space.lg))
        .interactable(false)
        .show(ctx, |ui| {
            for toast in ui_state.toasts.iter().rev().take(3) {
                let accent = match toast.kind {
                    ToastKind::Info => t.accent,
                    ToastKind::Error => t.danger,
                };
                egui::Frame::new()
                    .fill(t.surface_raised)
                    .stroke(Stroke::new(t.stroke_width * 2.0, accent))
                    .corner_radius(egui::CornerRadius::same(t.radius.md))
                    .inner_margin(egui::Margin::symmetric(t.space.lg as i8, t.space.sm as i8))
                    .show(ui, |ui| {
                        ui.label(&toast.text);
                    });
                ui.add_space(t.space.xs);
            }
        });
}

pub fn loading(ctx: &egui::Context, t: &Tokens, vp: egui::Rect) {
    egui::Area::new(Id::new("loading"))
        .order(Order::Foreground)
        .pivot(Align2::CENTER_CENTER)
        .fixed_pos(vp.center())
        .show(ctx, |ui| {
            t.card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading buildings…");
                });
                hint(ui, t, "Fetching and preparing map data on the server.");
            });
        });
}

pub fn help(ctx: &egui::Context, t: &Tokens, ui_state: &mut UiState) {
    let mut open = true;
    egui::Window::new("Getting started")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .default_width(460.0)
        .min_width(t.space.xl * 16.0)
        .frame(t.card())
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("Observation shows how much of a street ordinary home security cameras can see. Buildings block the view, so the shaded areas are what each camera could actually record.");
            ui.add_space(t.space.sm);
            for (n, step) in [
                "Pick a street with Change area… (or explore the demo street).",
                "Choose a camera model, press Place (P) and click a wall to mount it.",
                "Aim (A) by clicking where it should look; fine-tune in the inspector.",
                "Read the street coverage figures; switch Day/Night and detail level.",
            ]
            .iter()
            .enumerate()
            {
                ui.label(format!("{}. {step}", n + 1));
            }
            ui.add_space(t.space.md);
            egui::Grid::new("keys").num_columns(2).spacing([t.space.lg, t.space.xs]).show(ui, |ui| {
                for (k, v) in [
                    ("V / P / A / M", "Select / Place / Aim / Move tool"),
                    ("Drag", "Orbit (or aim/move with those tools)"),
                    ("Right-drag · Shift-drag", "Pan"),
                    ("Scroll · pinch", "Zoom"),
                    ("Two-finger drag", "Pan (touch)"),
                    ("F", "Focus selected camera"),
                    ("N", "Toggle overlap / per-camera colours"),
                    ("Delete", "Remove selected camera"),
                    ("Esc", "Back to Select / close dialogs"),
                    ("?", "This help"),
                ] {
                    ui.label(RichText::new(k).monospace().color(t.text));
                    ui.label(RichText::new(v).color(t.text_muted));
                    ui.end_row();
                }
            });
            ui.add_space(t.space.md);
            hint(ui, t, "Coverage is modelled from published field of view and resolution (DORI pixel density); real cameras vary with lens distortion, lighting and compression.");
            ui.add_space(t.space.sm);
            if ui.add(primary(t, "Got it")).clicked() {
                ui_state.help_open = false;
            }
        });
    if !open {
        ui_state.help_open = false;
    }
}
