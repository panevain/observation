//! Floating overlays: viewport gate, toasts, loading card, help, chips.

use super::UiData;
use super::actions::CameraAction;
use super::widgets::{hint, keycap, primary, secondary, swatch};
use crate::cameras::camera_color;
use crate::icons;
use crate::state::{ToastAction, ToastKind, Tool, UiState};
use crate::theme::{MIN_VIEWPORT, Tokens, strong_family};
use bevy_egui::egui::{self, Align2, Id, Order, RichText, Stroke, vec2};
use observation_core::scene::AreaRef;

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

/// Icon buttons floating just above the selected camera in the 3D view.
/// Returns the action clicked, if any.
pub fn camera_toolbar(
    ctx: &egui::Context,
    t: &Tokens,
    vp: egui::Rect,
    at: egui::Pos2,
) -> Option<CameraAction> {
    let mut action = None;
    let gap = t.space.xl;
    let resp = egui::Area::new(Id::new("camera-toolbar"))
        .order(Order::Middle)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(at - vec2(0.0, gap))
        .constrain_to(vp.shrink(t.space.sm))
        .show(ctx, |ui| {
            t.card()
                .inner_margin(egui::Margin::same(t.space.xs as i8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for a in [
                            CameraAction::Aim,
                            CameraAction::Move,
                            CameraAction::Focus,
                            CameraAction::Duplicate,
                            CameraAction::Delete,
                        ] {
                            let color = if a == CameraAction::Delete {
                                t.danger
                            } else {
                                t.text
                            };
                            let tip = a.shortcut().map_or_else(
                                || a.label().to_owned(),
                                |k| format!("{} ({k})", a.label()),
                            );
                            let button = egui::Button::new(
                                RichText::new(a.icon()).size(t.font.title).color(color),
                            )
                            .frame_when_inactive(false)
                            .min_size(vec2(t.layout.hit_target, t.layout.hit_target));
                            if ui.add(button).on_hover_text(tip).clicked() {
                                action = Some(a);
                            }
                        }
                    });
                });
        });
    // A short stem from the toolbar down towards the camera.
    let stem_top = egui::pos2(
        at.x.clamp(resp.response.rect.min.x, resp.response.rect.max.x),
        resp.response.rect.max.y,
    );
    if stem_top.y < at.y - 4.0 {
        ctx.layer_painter(egui::LayerId::new(
            Order::Middle,
            Id::new("camera-toolbar-stem"),
        ))
        .line_segment(
            [stem_top, at - vec2(0.0, 8.0)],
            Stroke::new(t.stroke_width, t.border_strong),
        );
    }
    action
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
        Tool::Address => egui::CursorIcon::Help,
    });
}

/// The Address tool's card by the pointer: the address under it, or that
/// there is none.
pub fn address_card(ctx: &egui::Context, t: &Tokens, vp: egui::Rect, ui_state: &UiState) {
    let (Tool::Address, Some(address)) = (ui_state.tool, &ui_state.hover_address) else {
        return;
    };
    let Some(at) = ctx
        .pointer_hover_pos()
        .filter(|p| vp.contains(*p) && !ctx.is_pointer_over_egui())
    else {
        return;
    };
    egui::Area::new(Id::new("address-card"))
        .order(Order::Tooltip)
        .fixed_pos(at + vec2(t.space.lg, t.space.lg))
        .constrain_to(vp)
        .interactable(false)
        .show(ctx, |ui| {
            t.card()
                .inner_margin(egui::Margin::symmetric(t.space.md as i8, t.space.sm as i8))
                .show(ui, |ui| {
                    ui.set_max_width(vp.width() / 2.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = t.space.sm;
                        ui.label(RichText::new(icons::SIGNPOST).color(t.text_muted));
                        match address {
                            Some(a) => {
                                ui.label(RichText::new(a).family(strong_family()).color(t.text))
                            }
                            None => ui.label(RichText::new("No address here").color(t.text_muted)),
                        };
                    });
                });
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
                } else if d.ui.tool == Tool::Address {
                    match &d.ui.selected_address {
                        Some(a) => ui.label(RichText::new(a).strong()),
                        None => ui.label(RichText::new("No address selected").color(t.text_muted)),
                    };
                } else if let Some(c) = selected {
                    swatch(ui, t, camera_color(&d.scene, c, &d.models, t));
                    ui.label(RichText::new(&c.label).strong());
                } else {
                    ui.label(RichText::new("No camera selected").color(t.text_muted));
                }
            });
        });
}

/// What the user did with a toast this frame.
pub enum ToastEvent {
    Action(ToastAction),
    Dismiss(usize),
}

/// The latest toasts at the bottom of the viewport. Errors stay until
/// dismissed; a toast may carry one button (Undo, Retry).
pub fn toasts(
    ctx: &egui::Context,
    t: &Tokens,
    vp: egui::Rect,
    ui_state: &UiState,
) -> Option<ToastEvent> {
    if ui_state.toasts.is_empty() {
        return None;
    }
    let mut event = None;
    egui::Area::new(Id::new("toasts"))
        .order(Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(vp.center().x, vp.max.y - t.space.lg))
        .show(ctx, |ui| {
            let n = ui_state.toasts.len();
            for (i, toast) in ui_state.toasts.iter().enumerate().rev().take(3) {
                let error = toast.kind == ToastKind::Error;
                let (icon, accent) = if error {
                    (icons::WARNING_CIRCLE, t.danger)
                } else {
                    (icons::INFO, t.text_muted)
                };
                t.card()
                    .stroke(Stroke::new(
                        t.stroke_width * if error { 2.0 } else { 1.0 },
                        if error { t.danger } else { t.border },
                    ))
                    .inner_margin(egui::Margin::symmetric(t.space.md as i8, t.space.sm as i8))
                    .show(ui, |ui| {
                        ui.set_max_width((vp.width() - 2.0 * t.space.xl).min(560.0));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icon).size(t.font.title).color(accent));
                            ui.add(egui::Label::new(&toast.text).wrap());
                            if let Some(a) = &toast.action
                                && ui.add(secondary(t, a.label())).clicked()
                            {
                                event = Some(ToastEvent::Action(a.clone()));
                            }
                            // Errors never time out, so they need a way out.
                            if (error || toast.action.is_some())
                                && ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(icons::X).color(t.text_muted),
                                        )
                                        .frame_when_inactive(false),
                                    )
                                    .on_hover_text("Dismiss")
                                    .clicked()
                            {
                                event = Some(ToastEvent::Dismiss(i));
                            }
                        });
                    });
                if i + 3 > n {
                    ui.add_space(t.space.xs);
                }
            }
        });
    event
}

/// What the user chose on the loading card.
pub enum LoadingChoice {
    Cancel,
    Demo,
}

/// The loading card: what is being fetched, for how long, and a way out.
/// A first request for a new place waits on the public Overpass service,
/// which can take most of a minute, so after a few seconds it says so.
/// `cancellable` is false for reloads that keep the document's cameras (a
/// restore, an import, an undo across areas): the document already belongs to
/// the incoming area, so stopping would leave it on the wrong map.
pub fn loading(
    ctx: &egui::Context,
    t: &Tokens,
    vp: egui::Rect,
    area: AreaRef,
    elapsed: f64,
    cancellable: bool,
) -> Option<LoadingChoice> {
    let mut choice = None;
    egui::Area::new(Id::new("loading"))
        .order(Order::Foreground)
        .pivot(Align2::CENTER_CENTER)
        .fixed_pos(vp.center())
        .show(ctx, |ui| {
            t.card().show(ui, |ui| {
                ui.set_max_width(380.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    let what = match area {
                        AreaRef::Demo => "Loading the demo street…".to_owned(),
                        AreaRef::Osm { bbox } => format!(
                            "Loading buildings for {:.0} × {:.0} m…",
                            bbox.width_m(),
                            bbox.height_m()
                        ),
                    };
                    ui.label(RichText::new(what).family(strong_family()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{elapsed:.0} s"))
                                .size(t.font.caption)
                                .color(t.text_muted),
                        );
                    });
                });
                let stage = match (area, elapsed) {
                    (AreaRef::Demo, _) => "Built into the server; this should take a moment.",
                    (_, e) if e < 5.0 => "Fetching map data and preparing the 3D scene on the server.",
                    (_, e) if e < 25.0 => {
                        "Still working: a new place waits on OpenStreetMap's public Overpass service, which can take up to a minute. Places loaded before come from the server's cache."
                    }
                    _ => "The map-data service is slow right now. You can keep waiting, cancel and try a smaller area, or explore the demo street meanwhile.",
                };
                ui.add_space(t.space.xs);
                ui.add(egui::Label::new(
                    RichText::new(stage).size(t.font.caption).color(t.text_muted),
                ).wrap());
                ui.add_space(t.space.sm);
                ui.horizontal(|ui| {
                    if cancellable && ui.add(secondary(t, "Cancel")).clicked() {
                        choice = Some(LoadingChoice::Cancel);
                    }
                    if area != AreaRef::Demo
                        && elapsed >= 5.0
                        && ui.add(secondary(t, "Use demo street instead")).clicked()
                    {
                        choice = Some(LoadingChoice::Demo);
                    }
                });
            });
        });
    choice
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
                    ("L", "Address tool: point at a building to see its address"),
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
