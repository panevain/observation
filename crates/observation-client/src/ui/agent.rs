//! Connect agent: the top bar's button and connected indicator, the dialog
//! with the code to give the agent, the status and Disconnect, and the card
//! that shows the agent's proposal for the user to accept or reject.

use super::UiData;
use super::widgets::{hint, keycap, primary, secondary, section};
use crate::agent::AgentLinkRes;
use crate::icons;
use crate::platform::{Clock, Http, unix_secs};
use crate::theme::Tokens;
use bevy_egui::egui::{self, Align2, RichText, Vec2};
use observation_agent_api::MAX_REASON_CHARS;
use observation_client_model::agent::LinkPhase;
use observation_client_model::proposal::{FiguresVm, ProposalCardVm};

/// The top bar's button: Connect agent, or the connected indicator; while
/// a proposal waits with its card closed, a button that reopens the card.
pub fn top_bar_button(ui: &mut egui::Ui, t: &Tokens, link: &mut AgentLinkRes) {
    let waiting = link.proposal().is_some_and(|p| p.standing.is_open());
    if waiting && !link.card_open {
        let text = RichText::new(format!("{} Proposal waiting", icons::ROBOT)).color(t.accent);
        let button = egui::Button::new(text)
            .stroke(egui::Stroke::NONE)
            .min_size(egui::vec2(0.0, t.layout.hit_target));
        if ui
            .add(button)
            .on_hover_text("The agent proposed a change: review it")
            .clicked()
        {
            link.card_open = true;
        }
        return;
    }
    let (text, tip) = match link.phase() {
        LinkPhase::Connected { agent } => (
            RichText::new(format!("{} Agent connected", icons::PLUGS_CONNECTED)).color(t.accent),
            format!(
                "Connected to {}: it can read the open scene, the selection and the point you share (S over the ground)",
                agent.as_deref().unwrap_or("an agent")
            ),
        ),
        LinkPhase::Starting | LinkPhase::Waiting { .. } => (
            RichText::new(format!("{} Waiting for agent", icons::ROBOT)).color(t.text),
            "Waiting for the agent to use the code".to_owned(),
        ),
        LinkPhase::Idle | LinkPhase::Ended { .. } => (
            RichText::new(format!("{} Connect agent", icons::ROBOT)).color(t.text),
            "Let an AI agent on this computer see the open scene".to_owned(),
        ),
    };
    let button = egui::Button::new(text)
        .stroke(egui::Stroke::NONE)
        .min_size(egui::vec2(0.0, t.layout.hit_target));
    if ui.add(button).on_hover_text(tip).clicked() {
        link.dialog_open = !link.dialog_open;
    }
}

/// The Connect agent dialog.
pub fn window(ctx: &egui::Context, t: &Tokens, link: &mut AgentLinkRes, http: &dyn Http) {
    let mut open = link.dialog_open;
    egui::Window::new("Connect agent")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(420.0)
        .frame(t.card())
        .open(&mut open)
        .show(ctx, |ui| {
            hint(
                ui,
                t,
                "An AI agent running on this computer (through the Observation MCP server) can read \
                 the open scene, its area, the selected camera and a point you share. It cannot \
                 change the scene itself: it can propose a change, which you accept or reject. The \
                 connection lasts until you disconnect, close this tab or leave it unused for 30 \
                 minutes.",
            );
            ui.add_space(t.space.md);
            match link.phase().clone() {
                LinkPhase::Idle | LinkPhase::Ended { .. } => {
                    if let LinkPhase::Ended { why } = link.phase() {
                        ui.label(RichText::new(why).color(t.text_muted));
                        ui.add_space(t.space.sm);
                    }
                    if ui.add(primary(t, "Connect")).clicked() {
                        link.connect(http);
                    }
                }
                LinkPhase::Starting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Starting…");
                    });
                }
                LinkPhase::Waiting { .. } => {
                    section(ui, t, "Code for the agent");
                    if let Some(code) = link.code() {
                        ui.label(
                            RichText::new(code)
                                .monospace()
                                .size(t.font.heading)
                                .color(t.text),
                        );
                    }
                    hint(
                        ui,
                        t,
                        "Ask your agent to connect to the Observation app with this code and the \
                         address this page is open at. The code works once and expires in 10 \
                         minutes.",
                    );
                    ui.add_space(t.space.md);
                    if ui.add(secondary(t, "Cancel")).clicked() {
                        link.disconnect(http);
                    }
                }
                LinkPhase::Connected { agent } => {
                    ui.label(
                        RichText::new(format!(
                            "{} Connected to {}",
                            icons::PLUGS_CONNECTED,
                            agent.as_deref().unwrap_or("an agent")
                        ))
                        .color(t.accent),
                    );
                    ui.add_space(t.space.sm);
                    ui.horizontal(|ui| {
                        ui.label("Share the point under the pointer:");
                        keycap(ui, t, "S");
                    });
                    match link.pinned() {
                        Some(p) => hint(
                            ui,
                            t,
                            &format!("Shared: {:.1} m east, {:.1} m south of the centre", p.x, p.z),
                        ),
                        None => hint(ui, t, "No point shared yet."),
                    }
                    if let Some(e) = &link.post_error {
                        ui.add_space(t.space.sm);
                        ui.label(
                            RichText::new(format!(
                                "{} The scene could not be shared: {e}",
                                icons::WARNING
                            ))
                            .color(t.warning),
                        );
                    }
                    ui.add_space(t.space.md);
                    if ui.add(secondary(t, "Disconnect")).clicked() {
                        link.disconnect(http);
                    }
                }
            }
        });
    link.dialog_open = open;
}

/// The proposal card, floating at the top left of the 3D view: the
/// summary, the changes, the street seen before and after, and Accept or
/// Reject. Everything the agent wrote is drawn as plain text.
pub fn proposal_card(
    ctx: &egui::Context,
    t: &Tokens,
    d: &mut UiData,
    viewport: egui::Rect,
    http: &dyn Http,
    clock: &dyn Clock,
) {
    let Some(p) = d.agent.proposal() else {
        d.agent.card_open = false;
        return;
    };
    let vm = ProposalCardVm::new(p, d.agent.agent_name());
    let figures = d.coverage.counted().then(|| {
        let params = &d.scene.params;
        FiguresVm::new(
            ((params.dori, params.lighting), &d.coverage.stats),
            d.proposal_coverage.after.as_ref().map(|(s, a)| (*s, a)),
        )
    });
    let computing = p.proposed().is_some() && d.proposal_coverage.after.is_none();
    let mut open = true;
    let mut close = false;
    let mut action = None;
    egui::Window::new(RichText::new(format!("{} {}", icons::ROBOT, vm.title)))
        .id(egui::Id::new("proposal-card"))
        .collapsible(true)
        .resizable(false)
        .default_pos(viewport.left_top() + egui::vec2(t.space.md, t.space.md))
        .constrain_to(viewport)
        .default_width(380.0)
        .frame(t.card())
        .open(&mut open)
        .show(ctx, |ui| {
            ui.add(egui::Label::new(RichText::new(&vm.summary).color(t.text).strong()).wrap());
            section(ui, t, "Changes");
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for line in &vm.lines {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("•  {line}"))
                                    .size(t.font.caption)
                                    .color(t.text),
                            )
                            .wrap(),
                        );
                    }
                });
            if let Some(f) = &figures {
                section(ui, t, &f.lead);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("Now {}", f.before)).color(t.text));
                    ui.label(RichText::new("→").color(t.text_muted));
                    match (&f.after, &f.change) {
                        (Some(after), Some(change)) => {
                            ui.label(RichText::new(format!("after {after}")).color(t.text));
                            ui.label(RichText::new(format!("({change})")).color(t.text_muted));
                        }
                        _ if computing => {
                            ui.spinner();
                            hint(ui, t, "computing…");
                        }
                        _ => hint(ui, t, "not available"),
                    }
                });
                if let Some(a) = &d.area {
                    crate::ui::provenance::card_data_date(ui, t, &a.bundle);
                }
                crate::ui::provenance::caveats(ui, t);
            }
            if let Some(status) = &vm.status {
                ui.add_space(t.space.sm);
                let (icon, color) = if vm.warning {
                    (icons::WARNING, t.warning)
                } else {
                    (icons::INFO, t.text_muted)
                };
                ui.add(
                    egui::Label::new(RichText::new(format!("{icon} {status}")).color(color)).wrap(),
                );
            }
            if let Some(e) = &vm.decision_error {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{} The agent could not be told: {e}",
                            icons::WARNING
                        ))
                        .color(t.warning),
                    )
                    .wrap(),
                );
            }
            ui.add_space(t.space.md);
            if vm.can_reject {
                if let Some(p) = d.agent.proposal_mut() {
                    ui.add(
                        egui::TextEdit::singleline(&mut p.reason)
                            .hint_text("Reason, if you reject it (optional)")
                            .char_limit(MAX_REASON_CHARS)
                            .desired_width(f32::INFINITY),
                    );
                }
                ui.add_space(t.space.sm);
                ui.horizontal(|ui| {
                    let accept = ui
                        .add_enabled(
                            vm.can_accept,
                            primary(t, &format!("{} Accept", icons::CHECK)),
                        )
                        .on_hover_text(&vm.accept_tip)
                        .on_disabled_hover_text(&vm.accept_tip);
                    if accept.clicked() {
                        action = Some(true);
                    }
                    if ui
                        .add(secondary(t, &format!("{} Reject", icons::X)))
                        .on_hover_text("Leave the scene as it is and tell the agent")
                        .clicked()
                    {
                        action = Some(false);
                    }
                });
                hint(
                    ui,
                    t,
                    "The whole proposal is applied or none of it. Accepting adds it to the \
                     scene's change log.",
                );
            } else if ui.add(secondary(t, "Close")).clicked() {
                close = true;
            }
        });
    match action {
        Some(true) => {
            let now = unix_secs(clock);
            if let Err(why) = d.agent.accept(http, &mut d.scene, &mut d.models, now) {
                d.toasts.error(why);
            }
        }
        Some(false) => {
            d.agent.reject(http);
            d.toasts.info("Rejected the agent's proposal");
        }
        None => {}
    }
    if !open || close {
        d.agent.close_card();
    }
}
