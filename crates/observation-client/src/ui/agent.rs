//! Connect agent: the top bar's button and connected indicator, and the
//! dialog with the code to give the agent, the status and Disconnect.

use super::widgets::{hint, keycap, primary, secondary, section};
use crate::agent::AgentLinkRes;
use crate::icons;
use crate::platform::Http;
use crate::theme::Tokens;
use bevy_egui::egui::{self, Align2, RichText, Vec2};
use observation_client_model::agent::LinkPhase;

/// The top bar's button: Connect agent, or the connected indicator.
pub fn top_bar_button(ui: &mut egui::Ui, t: &Tokens, link: &mut AgentLinkRes) {
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
                 change the scene. The connection lasts until you disconnect, close this tab or \
                 leave it unused for 30 minutes.",
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
