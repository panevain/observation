//! Floating overlays: viewport gate, toasts, loading card, help, chips.

use super::UiData;
use super::widgets::{hint, primary, swatch};
use crate::cameras::camera_color;
use crate::icons;
use crate::state::{ToastKind, Tool, UiState};
use crate::theme::{MIN_VIEWPORT, Tokens, strong_family};
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

/// A compact pill at the top left of the viewport: the active tool, what a
/// click will do, and which camera is selected. Neutral, so it never competes
/// with the coverage and camera colours; the rail and the cursor carry the
/// mode too.
pub fn mode_pill(ctx: &egui::Context, t: &Tokens, vp: egui::Rect, d: &UiData) {
    let tool = d.ui.tool;
    let selected = d.ui.selected.and_then(|id| d.scene.camera(id));
    let max_width = vp.width() - 2.0 * t.space.md;
    egui::Area::new(Id::new("mode-pill"))
        .order(Order::Middle)
        .fixed_pos(vp.min + vec2(t.space.md, t.space.md))
        .interactable(false)
        .show(ctx, |ui| {
            t.card()
                .corner_radius(egui::CornerRadius::same(t.radius.lg))
                .inner_margin(egui::Margin::symmetric(t.space.md as i8, t.space.xs as i8))
                .show(ui, |ui| {
                    ui.set_max_width(max_width - 2.0 * t.space.md);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = t.space.sm;
                        ui.label(
                            icons::fill::rich(icons::tool(tool))
                                .size(t.font.title)
                                .color(t.accent),
                        );
                        ui.label(
                            RichText::new(tool.label())
                                .family(strong_family())
                                .color(t.text),
                        );
                        ui.label(RichText::new(tool.banner()).color(t.text_muted));
                        selection_chip(ui, t, d, selected);
                        if tool != Tool::Select {
                            keycap(ui, t, "Esc");
                        }
                    });
                });
        });
}

/// A key name drawn as a keycap.
pub fn keycap(ui: &mut egui::Ui, t: &Tokens, key: &str) {
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

/// Cursor over the 3D view for each tool, so the mode shows where the click
/// lands.
pub fn tool_cursor(ctx: &egui::Context, vp: egui::Rect, tool: Tool) {
    let over_view = ctx
        .pointer_hover_pos()
        .is_some_and(|p| vp.contains(p) && !ctx.is_pointer_over_egui());
    if !over_view {
        return;
    }
    ctx.set_cursor_icon(match tool {
        Tool::Select => egui::CursorIcon::Default,
        Tool::Place => egui::CursorIcon::Copy,
        Tool::Aim => egui::CursorIcon::Crosshair,
        Tool::Move => egui::CursorIcon::Move,
        Tool::Region => egui::CursorIcon::Cell,
    });
}

/// What the pill says about the selection (or, for Place, the model).
fn selection_chip(
    ui: &mut egui::Ui,
    t: &Tokens,
    d: &UiData,
    selected: Option<&observation_core::camera::CameraPlacement>,
) {
    egui::Frame::new()
        .fill(t.surface_sunken)
        .corner_radius(egui::CornerRadius::same(t.radius.md))
        .inner_margin(egui::Margin::symmetric(t.space.sm as i8, 0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if d.ui.tool == Tool::Place {
                    let model = d
                        .models
                        .get(&d.ui.place_model)
                        .map_or_else(|| d.ui.place_model.clone(), |m| m.display_name());
                    ui.label(RichText::new(format!("New: {model}")).strong());
                } else if d.ui.tool == Tool::Region {
                    let n = d.ui.region_draft.len();
                    let text = match n {
                        0 => "No corners yet".to_owned(),
                        1 => "1 corner".to_owned(),
                        n => format!("{n} corners"),
                    };
                    ui.label(RichText::new(text).strong());
                } else if let Some(c) = selected {
                    swatch(ui, t, camera_color(&d.scene, c, &d.models, t));
                    ui.label(RichText::new(&c.label).strong());
                } else {
                    ui.label(RichText::new("No camera selected").color(t.text_muted));
                }
            });
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
                    ("R", "Draw a region to limit the coverage figures to"),
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
