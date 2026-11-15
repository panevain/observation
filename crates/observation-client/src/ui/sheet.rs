//! "Evidence sheet" dialog: the tier, what it leaves out, and the SVG, PNG
//! and print buttons (see [`observation_client_model::evidence`]).

use super::UiData;
use super::widgets::{hint, primary, secondary, section, segmented};
use crate::platform::{Platform, unix_secs};
use crate::state::FACADE_CELL_M;
use crate::theme::Tokens;
use bevy::prelude::Resource;
use bevy_egui::egui::{self, Align2, RichText, Vec2};
use crossbeam_channel::{Receiver, Sender, unbounded};
use observation_client_model::evidence::{
    SheetFormat, TIER_NOTE, build_sheet, export_sheet, failure_text, not_ready, sheet_file_stem,
};
use observation_core::evidence::Tier;

/// Exports that failed after the dialog handed them over (a PNG is drawn
/// later), waiting to be shown as toasts.
#[derive(Resource)]
pub struct SheetResults {
    tx: Sender<String>,
    rx: Receiver<String>,
}

impl Default for SheetResults {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx }
    }
}

/// Shows the failures that came back since the last frame.
pub fn drain(results: &SheetResults, d: &mut UiData) {
    for why in results.rx.try_iter() {
        d.toasts.error(why);
    }
}

/// Shows the dialog while it is open.
pub fn window(
    ctx: &egui::Context,
    t: &Tokens,
    d: &mut UiData,
    platform: &Platform,
    results: &SheetResults,
) {
    let mut open = d.panels.sheet_open;
    let mut asked = None;
    egui::Window::new("Evidence sheet")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(420.0)
        .frame(t.card())
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            hint(
                ui,
                t,
                "A dated sheet of the figures, with the settings, the data date, the \
                 methodology and what the figures leave out, for someone without the app.",
            );
            section(ui, t, "What it shows");
            let options: Vec<(Tier, &str)> = Tier::ALL.iter().map(|t| (*t, t.label())).collect();
            segmented(ui, t, &mut d.panels.sheet_tier, &options);
            ui.label(RichText::new(d.panels.sheet_tier.summary()).size(t.font.body));
            hint(ui, t, TIER_NOTE);
            section(ui, t, "Save");
            let ready = not_ready(&d.scene, &d.models, &d.coverage, d.area.is_some());
            if let Some(why) = &ready {
                ui.label(RichText::new(why).color(t.text_muted));
            }
            ui.horizontal_wrapped(|ui| {
                for format in SheetFormat::ALL {
                    let button = if format == SheetFormat::Svg {
                        primary(t, format.label())
                    } else {
                        secondary(t, format.label())
                    };
                    if ui
                        .add_enabled(ready.is_none(), button)
                        .on_hover_text(format.tip())
                        .on_disabled_hover_text(ready.as_deref().unwrap_or_default())
                        .clicked()
                    {
                        asked = Some(format);
                    }
                }
            });
        });
    d.panels.sheet_open = open;
    let (Some(format), Some(area)) = (asked, d.area.as_ref()) else {
        return;
    };
    let tier = d.panels.sheet_tier;
    let now = unix_secs(&*platform.clock);
    let scene = d.slots.index.active_name().to_owned();
    let sheet = match build_sheet(
        &d.scene,
        &d.models,
        &d.coverage,
        &area.bundle,
        &area.layers,
        FACADE_CELL_M,
        &scene,
        now,
        tier,
    ) {
        Ok(sheet) => sheet,
        Err(why) => {
            d.toasts.error(why);
            return;
        }
    };
    let tx = results.tx.clone();
    export_sheet(
        format,
        &sheet,
        &sheet_file_stem(&scene, now, tier),
        &*platform.files,
        &*platform.sheet,
        Box::new(move |r| {
            if let Err(why) = r {
                // The receiver lives as long as the app; a send error means shutdown.
                let _ = tx.send(failure_text(format, &why));
            }
        }),
    );
}
