//! "Local map data" dialog: imported regions with their age, live import
//! progress and refresh schedule, and adding, updating, cancelling, deleting
//! and turning scheduled refreshes on or off.

use super::widgets::{bar, danger, hint, primary, secondary, section};
use crate::icons;
use crate::regions::{CatalogState, RegionRequest, Regions, now_unix};
use crate::theme::{Tokens, strong_family};
use bevy_egui::egui::{self, Align, Align2, Layout, RichText, Vec2};
use observation_core::regions::{ActionKind, JobPhase, MAX_SIZE_IDS, RegionAction, RegionView};

/// Matches shown at once in the add list (their sizes are asked for in one request).
const MAX_MATCHES: usize = MAX_SIZE_IDS;

/// `12.3 MB`.
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / f64::from(1 << 20))
}

/// `1.2 GB` from a gigabyte up, `12.3 MB` below.
fn byte_size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / f64::from(1 << 30))
    } else {
        megabytes(bytes)
    }
}

/// A rough length of time: `5 min`, `3 h`, `1 day`, `4 days`.
fn duration(secs: u64) -> String {
    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;
    match secs {
        s if s < HOUR => format!("{} min", (s / 60).max(1)),
        s if s < DAY => format!("{} h", s / HOUR),
        s if s < 2 * DAY => "1 day".to_owned(),
        s => format!("{} days", s / DAY),
    }
}

/// `3 days ago`, or `just now` within a minute (or a clock running behind the agent's).
fn ago(then: u64, now: u64) -> String {
    if now < then + 60 {
        "just now".to_owned()
    } else {
        format!("{} ago", duration(now - then))
    }
}

/// When the next scheduled refresh starts. The agent looks for due regions
/// hourly, so one due within the hour (or overdue) starts within the hour.
fn next_refresh(at: u64, now: u64) -> String {
    if at <= now + 3600 {
        "next within the hour".to_owned()
    } else {
        format!("next in {}", duration(at - now))
    }
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
    let now = now_unix();
    let (refresh_days, refresh_range) = r.status.as_ref().map_or((0, 1..=1), |s| {
        let s = &s.snapshot;
        (s.refresh_days, s.min_refresh_days..=s.max_refresh_days)
    });
    let disk: u64 = regions.iter().filter_map(|x| x.disk_bytes).sum();
    let mut summary = format!(
        "{} {} · {} on disk",
        regions.len(),
        if regions.len() == 1 {
            "region"
        } else {
            "regions"
        },
        byte_size(disk)
    );
    if refresh_days == 0 {
        summary.push_str(" · scheduled refreshes are off on this server");
    }
    hint(ui, t, &summary);
    ui.add_space(t.space.xs);
    let not_allowed = if r.manageable() {
        "Enter the admin token first"
    } else {
        "Region management is off on this server"
    };
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
            if let Some(at) = region.imported_unix {
                facts.push(format!("downloaded {}", ago(at, now)));
            }
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
        if region.ready && refresh_days > 0 {
            ui.horizontal(|ui| {
                let enabled = can_act && !busy;
                let mut on = region.auto_refresh;
                if ui
                    .add_enabled(enabled, egui::Checkbox::new(&mut on, "Refresh every"))
                    .on_hover_text("Download and import the latest data on a schedule")
                    .on_disabled_hover_text(not_allowed)
                    .changed()
                {
                    r.request(RegionRequest::Settings(RegionAction {
                        id: region.id.clone(),
                        auto_refresh: Some(on),
                        ..RegionAction::default()
                    }));
                }
                let current = region.refresh_days.unwrap_or(refresh_days);
                let mut days = match &r.refresh_edit {
                    Some((id, d)) if *id == region.id => *d,
                    _ => current,
                };
                let hover = match region.refresh_days {
                    Some(_) => format!(
                        "Days between refreshes of this region, {} to {} (drag, or click and type)",
                        refresh_range.start(),
                        refresh_range.end()
                    ),
                    None => format!(
                        "Days between refreshes of this region, {} to {} (drag, or click and type). Until set, the server's default of {refresh_days} days",
                        refresh_range.start(),
                        refresh_range.end()
                    ),
                };
                let unit = if days == 1 { " day" } else { " days" };
                let field = ui
                    .add_enabled(
                        enabled,
                        egui::DragValue::new(&mut days)
                            .range(refresh_range.clone())
                            // The server's default may lie outside the range:
                            // show it as it is (clamping it would count as an
                            // edit every frame). Edits are still clamped.
                            .clamp_existing_to_range(false)
                            .speed(0.5)
                            .suffix(unit),
                    )
                    .on_hover_text(hover)
                    .on_disabled_hover_text(not_allowed);
                if field.changed() {
                    r.refresh_edit = Some((region.id.clone(), days));
                }
                // Escape drops the edit (the field has already taken the typed value).
                if (field.drag_stopped() || field.lost_focus())
                    && let Some((id, d)) = r.refresh_edit.take()
                    && id == region.id
                    && region.refresh_days != Some(d)
                    && !ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    r.request(RegionRequest::Settings(RegionAction {
                        id,
                        refresh_days: Some(d),
                        ..RegionAction::default()
                    }));
                }
                if let Some(at) = region.next_refresh_unix {
                    hint(ui, t, &next_refresh(at, now));
                }
            });
        }
        if let Some(e) = &region.last_error {
            let when = region
                .last_attempt_unix
                .map(|at| format!(" {}", ago(at, now)))
                .unwrap_or_default();
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "{} Last import failed{when}: {e}",
                        icons::WARNING_CIRCLE
                    ))
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
    r.want_sizes(matches.iter().map(|e| e.id.as_str()), now_unix());
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
                        let size = r.sizes.get(&e.id).copied();
                        let too_large = limit > 0 && size.is_some_and(|b| b > limit);
                        if have.contains(&e.id) {
                            hint(ui, t, "Added");
                        } else if too_large {
                            ui.label(
                                RichText::new("Too large")
                                    .size(t.font.caption)
                                    .color(t.warning),
                            )
                            .on_hover_text(format!(
                                "Over this server's {} download limit (OBS_REGIONS_MAX_DOWNLOAD_MB)",
                                byte_size(limit)
                            ));
                        } else if ui
                            .add_enabled(can_act && r.busy.is_none(), primary(t, "Import"))
                            .on_disabled_hover_text("Enter the admin token first")
                            .clicked()
                        {
                            r.request(RegionRequest::Action(ActionKind::Import, e.id.clone()));
                        }
                        if let Some(b) = size {
                            hint(ui, t, &byte_size(b));
                        } else if r.sizes_pending.contains(&e.id) {
                            hint(ui, t, "…");
                        }
                    });
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rough_durations() {
        assert_eq!(duration(0), "1 min");
        assert_eq!(duration(59 * 60), "59 min");
        assert_eq!(duration(3600), "1 h");
        assert_eq!(duration(86_399), "23 h");
        assert_eq!(duration(86_400), "1 day");
        assert_eq!(duration(3 * 86_400 + 5), "3 days");
        assert_eq!(ago(100, 120), "just now");
        assert_eq!(ago(100, 50), "just now", "agent clock ahead");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(next_refresh(100, 4000), "next within the hour", "overdue");
        assert_eq!(next_refresh(2 * 86_400, 0), "next in 2 days");
        assert_eq!(byte_size(3 << 29), "1.5 GB");
        assert_eq!(byte_size(1 << 20), "1.0 MB");
    }
}
