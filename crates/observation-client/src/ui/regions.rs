//! "Local map data" dialog: imported regions with live import progress, and
//! adding, updating, cancelling and deleting them.

use super::widgets::{bar, danger, hint, primary, secondary, section};
use crate::icons;
use crate::regions::{CatalogState, RegionRequest, Regions};
use crate::theme::{Tokens, strong_family};
use bevy_egui::egui::{self, Align, Align2, Layout, RichText, Vec2};
use observation_core::regions::{ActionKind, JobPhase, RegionView};

/// Matches shown at once in the add list.
const MAX_MATCHES: usize = 40;

/// `12.3 MB`.
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / f64::from(1 << 20))
}

/// One line for the top bar while an import runs, e.g. `Monaco 42%`.
pub fn progress_label(r: &RegionView) -> Option<String> {
    let job = r.job.as_ref()?;
    Some(match job.phase {
        JobPhase::Queued => format!("{} queued", r.name),
        _ => format!("{} {:.0}%", r.name, job.overall() * 100.0),
    })
}

/// What the running phase has done so far, e.g. `3.1 MB of 45.0 MB`.
fn job_detail(r: &RegionView) -> String {
    let Some(job) = &r.job else {
        return String::new();
    };
    match (job.phase, job.total) {
        (JobPhase::Queued, _) => "Waiting for another import to finish".to_owned(),
        (JobPhase::Downloading, Some(total)) => {
            format!(
                "Downloading {} of {}",
                megabytes(job.done),
                megabytes(total)
            )
        }
        (JobPhase::Downloading, None) => format!("Downloading {}", megabytes(job.done)),
        (JobPhase::Filtering, _) => "Keeping only buildings and roads".to_owned(),
        (JobPhase::Importing, Some(total)) => {
            format!("Importing {} of {} map objects", job.done.min(total), total)
        }
        (JobPhase::Importing, None) => "Importing map objects".to_owned(),
        (JobPhase::Finishing, _) => "Switching to the new data".to_owned(),
    }
}

/// Shows the dialog while `open`.
pub fn window(ctx: &egui::Context, t: &Tokens, r: &mut Regions, open: &mut bool) {
    let manageable = r.manageable();
    let can_act = manageable && !r.token.trim().is_empty();
    egui::Window::new("Local map data")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(520.0)
        .min_width(t.space.xl * 18.0)
        .max_height(ctx.viewport_rect().height() * 0.8)
        .frame(t.card())
        .open(open)
        .show(ctx, |ui| {
            hint(
                ui,
                t,
                "Areas inside a ready region load from this server's own copy of OpenStreetMap: fast, and independent of the public Overpass service. Other areas still use Overpass.",
            );
            match &r.status {
                None => {
                    ui.add_space(t.space.sm);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        hint(ui, t, "Connecting…");
                    });
                    return;
                }
                Some(s) if !s.agent_online => {
                    ui.add_space(t.space.sm);
                    ui.label(
                        RichText::new(format!(
                            "{} The region service is not reachable; areas load from public Overpass meanwhile.",
                            icons::WARNING
                        ))
                        .color(t.warning),
                    );
                }
                Some(_) if !r.live => {
                    ui.add_space(t.space.sm);
                    hint(ui, t, "Live updates paused; reconnecting…");
                }
                Some(_) => {}
            }

            section(ui, t, "Regions");
            egui::ScrollArea::vertical()
                .id_salt("regions-list")
                .max_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| region_list(ui, t, r, can_act));

            if manageable {
                section(ui, t, "Add a region");
                add_region(ui, t, r, can_act);
                section(ui, t, "Admin token");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut r.token)
                            .password(true)
                            .hint_text("OBS_ADMIN_TOKEN")
                            .desired_width(240.0),
                    );
                });
                hint(
                    ui,
                    t,
                    "Needed to add, update or delete regions. Kept for this browser session only.",
                );
            } else {
                ui.add_space(t.space.sm);
                hint(
                    ui,
                    t,
                    "Regions are managed by the server's administrator (region management is off on this server).",
                );
            }
        });
}

fn region_list(ui: &mut egui::Ui, t: &Tokens, r: &mut Regions, can_act: bool) {
    let regions = r.regions().to_vec();
    if regions.is_empty() {
        hint(
            ui,
            t,
            "No regions yet: every area loads from public Overpass.",
        );
        return;
    }
    for region in &regions {
        let busy = r.busy.as_deref() == Some(region.id.as_str());
        ui.horizontal(|ui| {
            ui.label(RichText::new(&region.name).family(strong_family()));
            ui.label(
                RichText::new(&region.id)
                    .size(t.font.caption)
                    .color(t.text_muted),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !can_act {
                    return;
                }
                ui.add_enabled_ui(!busy, |ui| {
                    if region.job.is_some() {
                        if ui.add(secondary(t, "Cancel")).clicked() {
                            r.request(RegionRequest::Action(ActionKind::Cancel, region.id.clone()));
                        }
                        return;
                    }
                    if r.confirm_delete.as_deref() == Some(region.id.as_str()) {
                        if ui.add(danger(t, "Delete for good")).clicked() {
                            r.confirm_delete = None;
                            r.request(RegionRequest::Action(ActionKind::Delete, region.id.clone()));
                        }
                        if ui.add(secondary(t, "Keep")).clicked() {
                            r.confirm_delete = None;
                        }
                        return;
                    }
                    if ui
                        .add(secondary(t, icons::TRASH))
                        .on_hover_text("Delete this region's data")
                        .clicked()
                    {
                        r.confirm_delete = Some(region.id.clone());
                    }
                    if ui
                        .add(secondary(t, icons::ARROW_CLOCKWISE))
                        .on_hover_text("Download and import the latest data")
                        .clicked()
                    {
                        r.request(RegionRequest::Action(ActionKind::Import, region.id.clone()));
                    }
                });
            });
        });
        if region.job.is_some() {
            let frac = region.job.as_ref().map_or(0.0, |j| j.overall());
            bar(ui, t, frac, t.accent);
            hint(ui, t, &job_detail(region));
        }
        let mut facts = Vec::new();
        if region.ready {
            facts.push("Ready".to_owned());
            if let Some(ts) = &region.data_timestamp {
                facts.push(format!("data from {}", ts.get(..10).unwrap_or(ts)));
            }
            if let Some(b) = region.disk_bytes {
                facts.push(megabytes(b));
            }
        } else if region.job.is_none() {
            facts.push("No data".to_owned());
        }
        if !facts.is_empty() {
            hint(ui, t, &facts.join(" · "));
        }
        if let Some(e) = &region.last_error {
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{} Last import failed: {e}", icons::WARNING_CIRCLE))
                        .size(t.font.caption)
                        .color(t.danger),
                )
                .wrap(),
            );
        }
        ui.add_space(t.space.xs);
        ui.separator();
    }
}

fn add_region(ui: &mut egui::Ui, t: &Tokens, r: &mut Regions, can_act: bool) {
    if matches!(r.catalog, CatalogState::NotLoaded) {
        r.request(RegionRequest::Catalog);
    }
    let limit = r
        .status
        .as_ref()
        .map_or(0, |s| s.snapshot.max_download_bytes);
    ui.horizontal(|ui| {
        ui.label(icons::MAGNIFYING_GLASS);
        ui.add(
            egui::TextEdit::singleline(&mut r.search)
                .hint_text("Country, state or city")
                .desired_width(f32::INFINITY),
        );
    });
    if limit > 0 {
        hint(
            ui,
            t,
            &format!(
                "Extracts up to {:.0} MB. Countries take minutes to hours to import; the progress shows here and in the top bar.",
                limit as f64 / f64::from(1 << 20)
            ),
        );
    }
    let entries = match &r.catalog {
        CatalogState::NotLoaded | CatalogState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                hint(ui, t, "Loading the list of extracts…");
            });
            return;
        }
        CatalogState::Failed(e) => {
            ui.label(RichText::new(e).color(t.danger));
            if ui.add(secondary(t, "Try again")).clicked() {
                r.request(RegionRequest::Catalog);
            }
            return;
        }
        CatalogState::Loaded(list) => list,
    };
    let q = r.search.trim().to_lowercase();
    if q.is_empty() {
        return;
    }
    let matches: Vec<_> = entries
        .iter()
        .filter(|e| e.name.to_lowercase().contains(&q) || e.id.contains(&q))
        .take(MAX_MATCHES)
        .cloned()
        .collect();
    if matches.is_empty() {
        hint(ui, t, "No extract matches.");
        return;
    }
    let have: Vec<String> = r.regions().iter().map(|x| x.id.clone()).collect();
    egui::ScrollArea::vertical()
        .id_salt("regions-catalog")
        .max_height(200.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for e in matches {
                ui.horizontal(|ui| {
                    ui.label(&e.name);
                    if let Some(p) = &e.parent {
                        ui.label(RichText::new(p).size(t.font.caption).color(t.text_muted));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if have.contains(&e.id) {
                            hint(ui, t, "Added");
                        } else if ui
                            .add_enabled(can_act && r.busy.is_none(), primary(t, "Import"))
                            .on_disabled_hover_text("Enter the admin token first")
                            .clicked()
                        {
                            r.request(RegionRequest::Action(ActionKind::Import, e.id.clone()));
                        }
                    });
                });
            }
        });
}
