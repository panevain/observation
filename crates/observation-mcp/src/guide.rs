//! `get_started`: what Observation is, the workflow, the conventions and the
//! wording rules, in about 200 tokens.
//!
//! Some MCP clients, llama.cpp's chat page among them, never put the
//! server's instructions into the model's prompt, so the model learns the
//! workflow only from tool names, descriptions and results. This tool says
//! it all in one cheap call, and names only the tools the client is offered
//! ([`crate::toolsets`]).

use crate::ObservationMcp;
use rmcp::{tool, tool_router};

/// A line of the guide, and the tools it needs: it is left out unless the
/// client is offered the first of them.
const STEPS: [(&str, &str); 7] = [
    (
        "open_scene",
        "1. open_scene (from \"new\": demo street; \"app\": the user's scene) gives a \
         scene_id such as \"s1\" for every scene tool.",
    ),
    ("scene_summary", "2. Look: scene_summary, render_map."),
    (
        "suggest_mount_points",
        "3. Choose: suggest_mount_points or suggest_placement.",
    ),
    (
        "place_camera",
        "4. place_camera with x, z from their at, height_m and yaw_deg.",
    ),
    (
        "compute_coverage",
        "5. Check: compute_coverage, find_gaps, check_point.",
    ),
    ("export_scene", "6. export_scene gives the file to import."),
    (
        "propose_changes",
        "With the app connected: propose_changes, then wait_for_decision; nothing changes \
         until the user accepts.",
    ),
];

const ABOUT: &str = "Observation estimates what home security cameras could see of a street \
    from map data, not footage.";

const CONVENTIONS: &str = "Positions: [x, z] metres from the area's centre, x east, z south; \
    heights above the ground. yaw_deg: 0 faces north (-z), 90 east (+x), 180 south (+z), \
    270 west. Negative pitch_deg tilts down.";

const WORDING: &str = "Say \"not ruled out\" or \"ruled out: <reason>\", never that a camera \
    sees. Name buildings by index and distance, never by address. Always pass on the data date \
    and caveats_summary: estimates leave out trees, fences and vehicles.";

/// The guide for a client offered the tools `offered` says yes to.
pub fn guide(offered: impl Fn(&str) -> bool) -> String {
    let steps: Vec<&str> = STEPS
        .iter()
        .filter(|(tool, _)| offered(tool))
        .map(|(_, line)| *line)
        .collect();
    format!(
        "{ABOUT}\nWorkflow:\n{}\n{CONVENTIONS}\n{WORDING}",
        steps.join("\n")
    )
}

#[tool_router(router = guide_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Call this first: what Observation is, the workflow, positions and bearings, and how to word results.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn get_started(&self) -> String {
        guide(|tool| self.tools.offers(tool))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolsets::{Profile, ToolSelection};

    #[test]
    fn the_guide_is_short_and_names_only_offered_tools() {
        let full = ObservationMcp::new().get_started();
        assert!(full.contains("propose_changes") && full.contains("yaw_deg"));
        // About 200 tokens: Qwen's tokenizer reads this text at about 4 bytes a token.
        assert!(full.len() / 4 <= 250, "{} bytes:\n{full}", full.len());
        let small = ObservationMcp::new()
            .with_tools(ToolSelection::profile(Profile::Small))
            .get_started();
        assert!(!small.contains("propose_changes"), "{small}");
        assert!(small.contains("export_scene") && small.contains("never by address"));
    }
}
