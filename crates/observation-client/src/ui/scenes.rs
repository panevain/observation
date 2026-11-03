//! "Scenes" dialog: the scenes saved in this browser, with opening, starting,
//! copying, renaming and deleting them, and how much of the browser's storage
//! they take.

use super::widgets::{bar, danger, hint, primary, secondary, section};
use crate::icons;
use crate::persist::PersistAction;
use crate::slots::{MAX_NAME_CHARS, STORAGE_BUDGET, Slots};
use crate::theme::{Tokens, strong_family};
use bevy::prelude::MessageWriter;
use bevy_egui::egui::{self, Align, Align2, Layout, RichText, Vec2};

/// Share of the storage guide past which the usage bar turns to a warning.
const NEARLY_FULL: f32 = 0.8;

/// `12 KB`, or `1.2 MB` from a megabyte up. Scene JSON is almost all ASCII,
/// so its UTF-16 length reads as bytes.
fn size(units: usize) -> String {
    if units >= 1 << 20 {
        format!("{:.1} MB", units as f64 / f64::from(1 << 20))
    } else {
        format!("{} KB", units.div_ceil(1 << 10))
    }
}

/// The open scene's name for the top bar, cut to `max` characters.
pub fn short_name(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_owned();
    }
    let mut cut: String = name.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Shows the dialog while `open`. `busy` (an area is loading) holds back
/// everything that would switch scenes; `has_area` is whether there is
/// anything to copy.
pub fn window(
    ctx: &egui::Context,
    t: &Tokens,
    slots: &mut Slots,
    busy: bool,
    has_area: bool,
    open: &mut bool,
    persist: &mut MessageWriter<PersistAction>,
) {
    egui::Window::new("Scenes")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(440.0)
        .min_width(t.space.xl * 14.0)
        .max_height(ctx.viewport_rect().height() * 0.8)
        .frame(t.card())
        .open(open)
        .show(ctx, |ui| {
            hint(
                ui,
                t,
                "Each scene is saved in this browser as you work. Export one to keep a copy elsewhere or to open it in another browser.",
            );
            if busy {
                ui.add_space(t.space.sm);
                hint(ui, t, "Scenes can be switched once the area has loaded.");
            }
            section(ui, t, "Saved scenes");
            egui::ScrollArea::vertical()
                .id_salt("scenes-list")
                .max_height(280.0)
                .auto_shrink([false, true])
                .show(ui, |ui| list(ui, t, slots, busy, persist));
            ui.add_space(t.space.sm);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!busy, primary(t, &format!("{} New scene", icons::PLUS)))
                    .on_hover_text("Start an empty scene on this area; the open one stays saved")
                    .clicked()
                {
                    persist.write(PersistAction::NewScene);
                }
                let tip = if has_area {
                    "Copy the open scene and carry on in the copy"
                } else {
                    "Nothing to copy until an area has loaded"
                };
                if ui
                    .add_enabled(
                        !busy && has_area,
                        secondary(t, &format!("{} Duplicate", icons::COPY)),
                    )
                    .on_hover_text(tip)
                    .on_disabled_hover_text(tip)
                    .clicked()
                {
                    persist.write(PersistAction::DuplicateScene);
                }
            });

            section(ui, t, "Storage");
            let used = slots.used();
            let frac = used as f32 / STORAGE_BUDGET as f32;
            let color = if frac > NEARLY_FULL {
                t.warning
            } else {
                t.accent
            };
            bar(ui, t, frac.min(1.0), color);
            hint(
                ui,
                t,
                &format!(
                    "{} of the {} or so most browsers keep for a site. When it is full, scenes stop saving: delete one or export it.",
                    size(used),
                    size(STORAGE_BUDGET)
                ),
            );
        });
}

fn list(
    ui: &mut egui::Ui,
    t: &Tokens,
    slots: &mut Slots,
    busy: bool,
    persist: &mut MessageWriter<PersistAction>,
) {
    let only_one = slots.index.slots.len() <= 1;
    for slot in slots.index.slots.clone() {
        let open = slot.id == slots.index.active;
        ui.horizontal(|ui| {
            if let Some((id, text)) = &mut slots.renaming
                && *id == slot.id
            {
                let field = ui.add(
                    egui::TextEdit::singleline(text)
                        .char_limit(MAX_NAME_CHARS)
                        .hint_text("Scene name")
                        .desired_width(240.0),
                );
                if std::mem::take(&mut slots.rename_focus) {
                    field.request_focus();
                }
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let save = ui
                    .add(secondary(t, icons::CHECK))
                    .on_hover_text("Save the name")
                    .clicked();
                let cancel = ui
                    .add(secondary(t, icons::X))
                    .on_hover_text("Keep the old name")
                    .clicked();
                if save || enter {
                    persist.write(PersistAction::RenameScene(slot.id, text.clone()));
                    slots.renaming = None;
                } else if cancel {
                    slots.renaming = None;
                }
                return;
            }
            if open {
                ui.label(RichText::new(&slot.name).family(strong_family()));
                ui.label(
                    RichText::new("open")
                        .size(t.font.caption)
                        .color(t.text_muted),
                );
            } else if ui
                .add_enabled(!busy, egui::Button::selectable(false, slot.name.as_str()))
                .on_hover_text("Open this scene; the open one stays saved")
                .clicked()
            {
                persist.write(PersistAction::OpenScene(slot.id));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if slots.confirm_delete == Some(slot.id) {
                    if ui
                        .add_enabled(!busy, danger(t, "Delete for good"))
                        .clicked()
                    {
                        slots.confirm_delete = None;
                        persist.write(PersistAction::DeleteScene(slot.id));
                    }
                    if ui.add(secondary(t, "Keep")).clicked() {
                        slots.confirm_delete = None;
                    }
                    return;
                }
                let tip = if only_one {
                    "Your only scene: start a new one before deleting this one"
                } else if open {
                    "Delete this scene from this browser; the next one opens"
                } else {
                    "Delete this scene from this browser"
                };
                if ui
                    .add_enabled(!busy && !only_one, secondary(t, icons::TRASH))
                    .on_hover_text(tip)
                    .on_disabled_hover_text(tip)
                    .clicked()
                {
                    slots.confirm_delete = Some(slot.id);
                }
                if ui
                    .add(secondary(t, icons::PENCIL_SIMPLE))
                    .on_hover_text("Rename")
                    .clicked()
                {
                    slots.renaming = Some((slot.id, slot.name.clone()));
                    slots.rename_focus = true;
                }
                let stored = slots.sizes.get(&slot.id).copied().unwrap_or(0);
                let text = if stored == 0 {
                    "empty".to_owned()
                } else {
                    size(stored)
                };
                ui.label(RichText::new(text).size(t.font.caption).color(t.text_muted));
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_short_names() {
        assert_eq!(size(0), "0 KB");
        assert_eq!(size(1), "1 KB");
        assert_eq!(size(2048), "2 KB");
        assert_eq!(size(5 << 20), "5.0 MB");
        assert_eq!(short_name("Front", 8), "Front");
        assert_eq!(short_name("Front of house", 8), "Front o…");
    }
}
