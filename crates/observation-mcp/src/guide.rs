//! `get_started` and the server's instructions: what Observation is, the
//! workflow, the conventions and the wording rules, naming only the tools
//! the client is offered ([`crate::toolsets`]).
//!
//! Some MCP clients, llama.cpp's chat page among them, never put the
//! server's instructions into the model's prompt, so the model learns the
//! workflow only from tool names, descriptions and results: `get_started`
//! says it all in one cheap call (about 250 tokens). Both texts are built
//! from [`Line`]s, once per selection ([`crate::toolsets::Toolbox`]), so
//! neither points to a tool the selection refuses.

use crate::ObservationMcp;
use rmcp::{tool, tool_router};

/// A line of text that names tools. It is left out unless every tool in
/// `needs` is offered; then `items` whose tool is offered are listed after
/// `lead`, joined by `sep` and followed by `end`. A line with items is left
/// out when none of them is offered.
pub(crate) struct Line {
    pub needs: &'static [&'static str],
    pub lead: &'static str,
    /// `(tool, words)`: the words are listed when the tool is offered.
    pub items: &'static [(&'static str, &'static str)],
    pub sep: &'static str,
    pub end: &'static str,
}

impl Line {
    /// A line kept when every tool in `needs` is offered.
    const fn whole(needs: &'static [&'static str], text: &'static str) -> Self {
        Self {
            needs,
            lead: text,
            items: &[],
            sep: "",
            end: "",
        }
    }

    /// A line listing those of `tools` that are offered, after `lead`.
    const fn list(
        lead: &'static str,
        items: &'static [(&'static str, &'static str)],
        sep: &'static str,
        end: &'static str,
    ) -> Self {
        Self {
            needs: &[],
            lead,
            items,
            sep,
            end,
        }
    }

    /// The line for a client offered the tools `offered` says yes to.
    pub fn render(&self, offered: &impl Fn(&str) -> bool) -> Option<String> {
        if !self.needs.iter().all(|t| offered(t)) {
            return None;
        }
        if self.items.is_empty() {
            return Some(self.lead.to_owned());
        }
        let listed: Vec<&str> = self
            .items
            .iter()
            .filter(|(tool, _)| offered(tool))
            .map(|(_, words)| *words)
            .collect();
        (!listed.is_empty()).then(|| format!("{}{}{}", self.lead, listed.join(self.sep), self.end))
    }
}

/// The lines kept for `offered`.
pub(crate) fn render(lines: &[Line], offered: &impl Fn(&str) -> bool) -> Vec<String> {
    lines.iter().filter_map(|l| l.render(offered)).collect()
}

/// The workflow's steps, numbered as they are kept.
const STEPS: [Line; 6] = [
    Line::whole(
        &["open_scene"],
        "open_scene (from \"new\": demo street; \"app\": the user's scene) gives a scene_id \
         such as \"s1\" for every scene tool.",
    ),
    Line::list(
        "Look: ",
        &[
            ("scene_summary", "scene_summary"),
            ("render_map", "render_map"),
        ],
        ", ",
        ".",
    ),
    Line::list(
        "Choose: ",
        &[
            ("suggest_mount_points", "suggest_mount_points"),
            ("suggest_placement", "suggest_placement"),
        ],
        " or ",
        ".",
    ),
    Line::whole(
        &["place_camera"],
        "place_camera with x, z (a suggestion's at), height_m and yaw_deg.",
    ),
    Line::list(
        "Check: ",
        &[
            ("compute_coverage", "compute_coverage"),
            ("find_gaps", "find_gaps"),
            ("check_point", "check_point"),
        ],
        ", ",
        ".",
    ),
    Line::whole(&["export_scene"], "export_scene gives the file to import."),
];

/// After the numbered steps.
const CONNECTED: Line = Line::whole(
    &["propose_changes", "wait_for_decision"],
    "With the app connected: propose_changes, then wait_for_decision; nothing changes until \
     the user accepts.",
);

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
    let mut steps: Vec<String> = render(&STEPS, &offered)
        .into_iter()
        .enumerate()
        .map(|(i, line)| format!("{}. {line}", i + 1))
        .collect();
    steps.extend(CONNECTED.render(&offered));
    let workflow = if steps.is_empty() {
        String::new()
    } else {
        format!("Workflow:\n{}\n", steps.join("\n"))
    };
    format!("{ABOUT}\n{workflow}{CONVENTIONS}\n{WORDING}")
}

/// The server's instructions, sentence by sentence.
const INSTRUCTIONS: [Line; 15] = [
    Line::whole(
        &[],
        "Observation estimates which parts of a street home security cameras could see, from \
         map data.",
    ),
    Line::whole(
        &["get_started"],
        "Call get_started first: it gives the workflow, the conventions and the wording rules \
         in a few lines.",
    ),
    Line::whole(
        &["open_scene"],
        "open_scene returns a scene_id such as \"s1\": pass it as scene_id to every scene tool.",
    ),
    Line::list(
        "The server keeps each version (",
        &[
            ("undo", "undo"),
            ("diff_scenes", "diff_scenes"),
            ("apply_edits", "apply_edits runs several edits as one"),
            ("export_scene", "export_scene gives the file"),
        ],
        "; ",
        ").",
    ),
    Line::whole(
        &[],
        "Positions are [x, z] metres from the area's centre, x east and z south; yaw_deg is a \
         compass bearing (0 north, 90 east, 180 south).",
    ),
    Line::list(
        "To look, use ",
        &[
            ("scene_summary", "scene_summary"),
            ("describe_surroundings", "describe_surroundings"),
            ("render_map", "render_map"),
        ],
        ", ",
        ".",
    ),
    Line::list(
        "To choose mounts and aims: ",
        &[
            ("suggest_mount_points", "suggest_mount_points"),
            ("suggest_placement", "suggest_placement"),
            ("optimise_aim", "optimise_aim"),
            ("cover_target", "cover_target"),
        ],
        ", ",
        ".",
    ),
    Line::whole(&[], "Offline, only the demo street is available."),
    Line::whole(
        &["connect_app", "open_scene"],
        "When the user connects their open app (connect_app, with its address and the code \
         from its Connect agent dialog), connect_app opens their scene and returns its \
         scene_id; open_scene with from \"app\" opens it again after they change it.",
    ),
    Line::whole(
        &["propose_changes", "wait_for_decision"],
        "propose_changes with that scene_id and a one-sentence summary asks them to accept \
         your edits: nothing changes until wait_for_decision says accepted.",
    ),
    Line::whole(
        &["wait_for_app_change"],
        "wait_for_app_change hears what they change.",
    ),
    Line::list(
        "To show them a place, cameras or layers, without changing the scene: ",
        &[
            ("focus_view", "focus_view"),
            ("select_cameras", "select_cameras"),
            ("highlight", "highlight"),
            ("set_view_options", "set_view_options"),
        ],
        ", ",
        ".",
    ),
    Line::list(
        "To find an area and ask the user to load it: ",
        &[
            ("search_places", "search_places"),
            ("request_load_area", "request_load_area"),
        ],
        ", then ",
        " (it loads only when they click Load).",
    ),
    Line::whole(
        &[],
        "Verdicts are one-sided: say \"not ruled out\" or \"ruled out: <reason>\", never that \
         a camera sees something. Name buildings by index and distance, never by address.",
    ),
    Line::whole(
        &[],
        "Always pass on the data date and caveats_summary: the figures are estimates that \
         leave out trees, fences and vehicles.",
    ),
];

/// The server's instructions for a client offered the tools `offered` says
/// yes to.
pub fn instructions(offered: impl Fn(&str) -> bool) -> String {
    render(&INSTRUCTIONS, &offered).join(" ")
}

#[tool_router(router = guide_tool_router, vis = "pub(crate)")]
impl ObservationMcp {
    #[tool(
        description = "Call this first: what Observation is, the workflow, positions and bearings, and how to word results.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub(crate) fn get_started(&self) -> String {
        self.tools.guide().to_owned()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::toolsets::{Group, Profile, ToolSelection, VIEW_TOOLS};

    /// Every word of `text` that is the name of a tool (one this build has,
    /// or a view tool) and that the server does not offer.
    pub(crate) fn unoffered_tools(server: &ObservationMcp, text: &str) -> Vec<String> {
        let mut found: Vec<String> = text
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| ObservationMcp::all_tools().has_route(w) || VIEW_TOOLS.contains(w))
            .filter(|w| !server.toolbox().offers(w))
            .map(str::to_owned)
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Selections to check the texts with.
    pub(crate) fn selections() -> Vec<ToolSelection> {
        let mut all = vec![
            ToolSelection::default(),
            ToolSelection::profile(Profile::Small),
            ToolSelection::profile(Profile::SmallApp),
            ToolSelection {
                profile: Profile::Small,
                groups: Some(vec![
                    Group::Scenes,
                    Group::Cameras,
                    Group::Describe,
                    Group::Analysis,
                ]),
            },
        ];
        all.extend(Group::ALL.into_iter().map(|g| ToolSelection {
            profile: Profile::Full,
            groups: Some(vec![g]),
        }));
        all
    }

    #[test]
    fn the_guide_is_short_and_names_only_offered_tools() {
        let full = ObservationMcp::new().get_started();
        assert!(full.contains("propose_changes") && full.contains("yaw_deg"));
        // About 250 tokens: Qwen's tokenizer reads this text at about 4 bytes a token.
        assert!(full.len() / 4 <= 250, "{} bytes:\n{full}", full.len());
        let small = ObservationMcp::new()
            .with_tools(ToolSelection::profile(Profile::Small))
            .get_started();
        assert!(!small.contains("propose_changes"), "{small}");
        assert!(small.contains("export_scene") && small.contains("never by address"));
        for selection in selections() {
            let server = ObservationMcp::new().with_tools(selection.clone());
            let text = server.get_started();
            assert_eq!(
                unoffered_tools(&server, &text),
                Vec::<String>::new(),
                "{selection:?}:\n{text}"
            );
        }
    }

    #[test]
    fn a_step_names_only_the_tools_offered_and_the_steps_stay_numbered() {
        let server = ObservationMcp::new().with_tools(ToolSelection {
            profile: Profile::Small,
            groups: Some(vec![
                Group::Scenes,
                Group::Cameras,
                Group::Describe,
                Group::Analysis,
            ]),
        });
        let text = server.get_started();
        // render_map (map), suggest_placement (goals) and the proposals are
        // not offered; the steps naming them keep their other tools.
        for absent in ["render_map", "suggest_placement", "propose_changes"] {
            assert!(!text.contains(absent), "{absent}:\n{text}");
        }
        assert!(text.contains("2. Look: scene_summary."), "{text}");
        assert!(text.contains("3. Choose: suggest_mount_points."), "{text}");
        assert!(text.contains("6. export_scene"), "{text}");

        // With only the guide, no workflow is left to give.
        let guide_only = ObservationMcp::new().with_tools(ToolSelection {
            profile: Profile::Full,
            groups: Some(vec![Group::Guide]),
        });
        let text = guide_only.get_started();
        assert!(!text.contains("Workflow"), "{text}");
        assert!(text.contains("yaw_deg"), "{text}");
    }
}
