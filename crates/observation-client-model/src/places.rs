//! The labelled areas and the targets: drawing them with the Areas and
//! Target tools, and what the Places tab and the Coverage card's menu do to
//! them (label, name, switch, redraw, delete, and which area limits the
//! figures).
//!
//! Labels and outlines are the user's own statement ("as drawn"); nothing
//! here infers them from map data.

use crate::commands::{EditCtx, Hit, HitTarget, Tracked};
use crate::document::Document;
use crate::editor::{AreaDraw, Editor, InspectorTab, Tool};
use observation_core::coverage::{MAX_REGION_POINTS, Region};
use observation_core::footage::{
    AreaLabel, LabelledArea, MAX_AREAS, MAX_NAME_CHARS, MAX_TARGET_POINTS, MAX_TARGETS, Target,
    TargetPlace, area_by_id, area_by_id_mut, fresh_id,
};

/// Side of the square a click on the ground marks as a target, metres: a
/// doorstep or a gate.
pub const GROUND_TARGET_M: f32 = 1.0;

/// How an area is named in lists and messages: its name, or "Area 3".
pub fn area_title(area: &LabelledArea) -> String {
    area.name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| format!("Area {}", area.id), str::to_owned)
}

/// How a target is named in lists and messages: its name, or "Target 2".
pub fn target_title(target: &Target) -> String {
    let name = target.name.trim();
    if name.is_empty() {
        format!("Target {}", target.id)
    } else {
        name.to_owned()
    }
}

/// "Own", "Neighbour", …, or "Unlabelled".
pub fn label_text(label: Option<AreaLabel>) -> &'static str {
    label.map_or("Unlabelled", AreaLabel::label)
}

/// The most corners the outline being drawn may have.
pub fn max_draft_points(editor: &Editor) -> usize {
    if editor.tool == Tool::Target {
        MAX_TARGET_POINTS
    } else {
        MAX_REGION_POINTS
    }
}

/// Adds a corner to the outline being drawn, or closes it when the first
/// corner is clicked.
pub fn outline_click(ctx: &mut EditCtx, p: [f32; 2]) {
    let editor = &mut *ctx.editor;
    let closes = editor.region_draft.len() >= 3
        && editor.region_draft.first().is_some_and(|f| {
            let (dx, dz) = (f[0] - p[0], f[1] - p[1]);
            dx.hypot(dz) <= editor.region_snap_m
        });
    let max = max_draft_points(editor);
    if closes {
        finish_outline(ctx);
    } else if editor.region_draft.len() < max {
        editor.region_draft.push(p);
    } else {
        let what = if editor.tool == Tool::Target {
            "A target"
        } else {
            "An area"
        };
        ctx.toasts.error(format!(
            "{what} has at most {max} corners; click the first corner to close it"
        ));
    }
}

/// Turns the outline being drawn into an area (with the Areas tool) or a
/// ground target (with the Target tool). Nothing happens without corners;
/// an outline that cannot be used stays to be finished, with the reason.
pub fn finish_outline(ctx: &mut EditCtx) {
    if ctx.editor.region_draft.is_empty() {
        return;
    }
    match ctx.editor.tool {
        Tool::Areas => finish_area(ctx),
        Tool::Target => {
            let points = std::mem::take(&mut ctx.editor.region_draft);
            if let Err(points) = add_target(ctx, TargetPlace::Ground { points }) {
                ctx.editor.region_draft = points;
            }
        }
        _ => {}
    }
}

fn finish_area(ctx: &mut EditCtx) {
    let region = Region {
        points: std::mem::take(&mut ctx.editor.region_draft),
        enabled: true,
    };
    if let Some(problem) = region.problem() {
        ctx.editor.region_draft = region.points;
        ctx.toasts.error(problem.message());
        return;
    }
    let area_m2 = region.area_m2();
    let redraw = match ctx.editor.area_draw {
        AreaDraw::Redraw(id) if area_by_id(&ctx.doc.get().areas, Some(id)).is_some() => Some(id),
        _ => None,
    };
    // Redrawing an area keeps the count; any other drawing may add one.
    let adds = match ctx.editor.area_draw {
        AreaDraw::Limit => ctx.doc.get().region().is_none(),
        AreaDraw::Redraw(_) => redraw.is_none(),
        AreaDraw::New => true,
    };
    if adds && ctx.doc.get().areas.len() >= MAX_AREAS {
        ctx.editor.region_draft = region.points;
        ctx.toasts.error(format!(
            "A scene holds at most {MAX_AREAS} areas; delete one you no longer need first"
        ));
        return;
    }
    match (ctx.editor.area_draw, redraw) {
        (AreaDraw::Limit, _) => {
            let doc = ctx.doc.get_mut();
            doc.set_region(Some(region));
            doc.touch();
            ctx.editor.selected_area = doc.limit_figures_to;
            ctx.toasts
                .info(format!("Coverage now counts only the {area_m2:.0} m² area"));
            ctx.editor.tool = Tool::Select;
        }
        (_, Some(id)) => {
            let doc = ctx.doc.get_mut();
            if let Some(a) = area_by_id_mut(&mut doc.areas, Some(id)) {
                a.points = region.points;
                ctx.toasts
                    .info(format!("Redrew {} ({area_m2:.0} m²)", area_title(a)));
            }
            doc.touch();
            ctx.editor.selected_area = Some(id);
            ctx.editor.tool = Tool::Select;
        }
        _ => {
            let doc = ctx.doc.get_mut();
            let id = fresh_id(doc.areas.iter().map(|a| a.id));
            doc.areas.push(LabelledArea {
                id,
                label: None,
                name: None,
                points: region.points,
                enabled: true,
            });
            doc.touch();
            ctx.editor.selected_area = Some(id);
            ctx.panels.inspector_tab = InspectorTab::Places;
            ctx.toasts.info(format!(
                "Added a {area_m2:.0} m² area: label it under Places, or draw the next one"
            ));
            // The tool stays on, so several areas can be drawn in a row.
        }
    }
    ctx.editor.area_draw = AreaDraw::New;
}

/// A click with the Target tool's point shape: a wall marks a 1 m patch of
/// it, the ground a [`GROUND_TARGET_M`] square centred on the click (the
/// client keeps the click within the loaded area). A roof or a camera body
/// marks nothing.
pub fn target_click(ctx: &mut EditCtx, hit: Option<Hit>) {
    let Some(hit) = hit else { return };
    let at = match hit.target {
        HitTarget::Camera(_) => return,
        HitTarget::Building if hit.normal.y.abs() < 0.5 => TargetPlace::Wall {
            point: hit.point.to_array(),
            normal: [hit.normal.x, hit.normal.z],
        },
        HitTarget::Building => {
            ctx.toasts
                .error("Click a wall or open ground: a roof cannot be a target");
            return;
        }
        HitTarget::Ground => {
            let h = GROUND_TARGET_M / 2.0;
            let (x, z) = (hit.point.x, hit.point.z);
            TargetPlace::Ground {
                points: vec![
                    [x - h, z - h],
                    [x + h, z - h],
                    [x + h, z + h],
                    [x - h, z + h],
                ],
            }
        }
    };
    // A wall or a square is always usable, so nothing is handed back.
    let _ = add_target(ctx, at);
}

/// Adds a target at `at`, named "Target N", and picks it in the Places tab.
/// Hands a ground outline's corners back if the target cannot be added.
fn add_target(ctx: &mut EditCtx, at: TargetPlace) -> Result<u32, Vec<[f32; 2]>> {
    let give_back = |at: TargetPlace| match at {
        TargetPlace::Ground { points } => points,
        TargetPlace::Wall { .. } => Vec::new(),
    };
    if let Some(problem) = at.problem() {
        ctx.toasts.error(problem.message());
        return Err(give_back(at));
    }
    if ctx.doc.get().targets.len() >= MAX_TARGETS {
        ctx.toasts.error(format!(
            "A scene holds at most {MAX_TARGETS} targets; delete one you no longer need first"
        ));
        return Err(give_back(at));
    }
    let doc = ctx.doc.get_mut();
    let id = fresh_id(doc.targets.iter().map(|t| t.id));
    let target = Target {
        id,
        name: format!("Target {id}"),
        at,
    };
    let kind = match target.at {
        TargetPlace::Wall { .. } => "on the wall",
        TargetPlace::Ground { .. } => "on the ground",
    };
    ctx.toasts.info(format!(
        "Added {} {kind}: its verdicts are under Places",
        target.name
    ));
    doc.targets.push(target);
    doc.touch();
    ctx.editor.selected_target = Some(id);
    ctx.panels.inspector_tab = InspectorTab::Places;
    Ok(id)
}

/// Names area `id` (as typed, cut to [`MAX_NAME_CHARS`]; blank means none).
/// Writes only when the name differs.
pub fn rename_area(doc: &mut dyn Tracked<Document>, id: u32, name: &str) {
    let name = cut_name(name).filter(|n| !n.trim().is_empty());
    if area_by_id(&doc.get().areas, Some(id)).is_some_and(|a| a.name != name) {
        let doc = doc.get_mut();
        if let Some(a) = area_by_id_mut(&mut doc.areas, Some(id)) {
            a.name = name;
        }
        doc.touch();
    }
}

/// Labels area `id` (`None`: unlabelled). Writes only when it differs.
pub fn set_area_label(doc: &mut dyn Tracked<Document>, id: u32, label: Option<AreaLabel>) {
    if area_by_id(&doc.get().areas, Some(id)).is_some_and(|a| a.label != label) {
        let doc = doc.get_mut();
        if let Some(a) = area_by_id_mut(&mut doc.areas, Some(id)) {
            a.label = label;
        }
        doc.touch();
    }
}

/// Turns area `id` on or off: an area that is off is kept but takes no part
/// in the split, nor limits the figures. Writes only when it differs.
pub fn set_area_enabled(doc: &mut dyn Tracked<Document>, id: u32, enabled: bool) {
    if area_by_id(&doc.get().areas, Some(id)).is_some_and(|a| a.enabled != enabled) {
        let doc = doc.get_mut();
        if let Some(a) = area_by_id_mut(&mut doc.areas, Some(id)) {
            a.enabled = enabled;
        }
        doc.touch();
    }
}

/// Limits the figures to area `id`, or with `None` to no area (the area is
/// kept). Writes only when it differs, and only for an area that exists.
pub fn set_figures_limit(doc: &mut dyn Tracked<Document>, id: Option<u32>) {
    let exists = id.is_none_or(|id| doc.get().areas.iter().any(|a| a.id == id));
    if exists && doc.get().limit_figures_to != id {
        let doc = doc.get_mut();
        doc.limit_figures_to = id;
        doc.touch();
    }
}

/// Deletes area `id`; if the figures were limited to it, they no longer are.
pub fn delete_area(ctx: &mut EditCtx, id: u32) {
    if area_by_id(&ctx.doc.get().areas, Some(id)).is_none() {
        return;
    }
    let doc = ctx.doc.get_mut();
    doc.areas.retain(|a| a.id != id);
    if doc.limit_figures_to == Some(id) {
        doc.limit_figures_to = None;
    }
    doc.touch();
    ctx.editor.forget_missing_places(doc);
}

/// Starts drawing a new outline for area `id` with the Areas tool.
pub fn redraw_area(editor: &mut Editor, id: u32) {
    editor.set_tool(Tool::Areas);
    editor.region_draft.clear();
    editor.area_draw = AreaDraw::Redraw(id);
    editor.selected_area = Some(id);
}

/// Starts drawing a new area with the Areas tool.
pub fn draw_area(editor: &mut Editor) {
    editor.set_tool(Tool::Areas);
    editor.region_draft.clear();
    editor.area_draw = AreaDraw::New;
}

/// Starts drawing the area the figures are limited to (redrawing it if
/// there is one), as the Coverage card's menu does.
pub fn draw_limit_area(editor: &mut Editor) {
    editor.set_tool(Tool::Areas);
    editor.region_draft.clear();
    editor.area_draw = AreaDraw::Limit;
}

/// Names target `id` (as typed, cut to [`MAX_NAME_CHARS`]). Writes only when
/// the name differs.
pub fn rename_target(doc: &mut dyn Tracked<Document>, id: u32, name: &str) {
    let name = cut_name(name).unwrap_or_default();
    if doc
        .get()
        .targets
        .iter()
        .any(|t| t.id == id && t.name != name)
    {
        let doc = doc.get_mut();
        if let Some(t) = doc.targets.iter_mut().find(|t| t.id == id) {
            t.name = name;
        }
        doc.touch();
    }
}

/// Deletes target `id`.
pub fn delete_target(ctx: &mut EditCtx, id: u32) {
    if !ctx.doc.get().targets.iter().any(|t| t.id == id) {
        return;
    }
    let doc = ctx.doc.get_mut();
    doc.targets.retain(|t| t.id != id);
    doc.touch();
    ctx.editor.forget_missing_places(doc);
}

/// `name` cut to [`MAX_NAME_CHARS`] characters; `None` when empty.
fn cut_name(name: &str) -> Option<String> {
    let cut: String = name.chars().take(MAX_NAME_CHARS).collect();
    (!cut.is_empty()).then_some(cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Panels, TargetShape};
    use crate::testing::Counting;
    use crate::toasts::{ToastKind, Toasts};
    use observation_core::glam::Vec3;

    #[derive(Default)]
    struct Fixture {
        doc: Document,
        editor: Editor,
        panels: Panels,
        toasts: Toasts,
    }

    impl Fixture {
        fn ctx(&mut self) -> EditCtx<'_> {
            EditCtx {
                doc: &mut self.doc,
                editor: &mut self.editor,
                panels: &mut self.panels,
                toasts: &mut self.toasts,
            }
        }

        fn toasts(&self) -> Vec<(ToastKind, &str)> {
            self.toasts
                .iter()
                .map(|t| (t.kind, t.text.as_str()))
                .collect()
        }

        /// Draws `points` with the active tool and closes the outline.
        fn draw(&mut self, points: &[[f32; 2]]) {
            for &p in points {
                outline_click(&mut self.ctx(), p);
            }
            finish_outline(&mut self.ctx());
        }
    }

    fn square(x: f32, size: f32) -> Vec<[f32; 2]> {
        vec![[x, 0.0], [x + size, 0.0], [x + size, size], [x, size]]
    }

    fn hit(target: HitTarget, point: [f32; 3], normal: [f32; 3]) -> Hit {
        Hit {
            point: Vec3::from_array(point),
            normal: Vec3::from_array(normal),
            target,
        }
    }

    #[test]
    fn the_areas_tool_adds_one_area_after_another() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Areas;
        f.draw(&square(0.0, 10.0));
        f.draw(&square(20.0, 5.0));
        let ids: Vec<u32> = f.doc.areas.iter().map(|a| a.id).collect();
        assert_eq!(ids, [1, 2]);
        assert!(f.doc.areas.iter().all(|a| a.label.is_none() && a.enabled));
        assert_eq!(f.doc.limit_figures_to, None, "new areas do not limit");
        assert_eq!(f.editor.selected_area, Some(2));
        assert_eq!(f.editor.tool, Tool::Areas, "ready for the next one");
        assert_eq!(f.panels.inspector_tab, InspectorTab::Places);
        assert_eq!(
            f.toasts().last(),
            Some(&(
                ToastKind::Info,
                "Added a 25 m² area: label it under Places, or draw the next one"
            ))
        );
    }

    #[test]
    fn redrawing_keeps_the_label_name_and_switch() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Areas;
        f.draw(&square(0.0, 10.0));
        set_area_label(&mut f.doc, 1, Some(AreaLabel::Neighbour));
        rename_area(&mut f.doc, 1, "Next door");
        set_area_enabled(&mut f.doc, 1, false);
        redraw_area(&mut f.editor, 1);
        assert_eq!(f.editor.area_draw, AreaDraw::Redraw(1));
        f.draw(&square(5.0, 4.0));
        let [a] = f.doc.areas.as_slice() else {
            panic!("still one area")
        };
        assert_eq!(a.points, square(5.0, 4.0));
        assert_eq!(
            (a.label, a.name.as_deref(), a.enabled),
            (Some(AreaLabel::Neighbour), Some("Next door"), false)
        );
        assert_eq!(f.editor.area_draw, AreaDraw::New);
        assert_eq!(f.editor.tool, Tool::Select);
        assert_eq!(
            f.toasts().last(),
            Some(&(ToastKind::Info, "Redrew Next door (16 m²)"))
        );
    }

    #[test]
    fn redrawing_an_area_deleted_meanwhile_adds_one() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Areas;
        f.editor.area_draw = AreaDraw::Redraw(9);
        f.draw(&square(0.0, 10.0));
        assert_eq!(f.doc.areas.len(), 1);
        assert_eq!(f.doc.areas[0].id, 1);
    }

    #[test]
    fn the_limit_area_is_drawn_as_the_single_region_was() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Areas;
        f.draw(&square(0.0, 10.0));
        set_area_label(&mut f.doc, 1, Some(AreaLabel::Own));
        draw_limit_area(&mut f.editor);
        f.draw(&square(20.0, 10.0));
        assert_eq!(f.doc.areas.len(), 2);
        assert_eq!(f.doc.limit_figures_to, Some(2));
        assert_eq!(f.doc.region().map(|r| r.points[0]), Some([20.0, 0.0]));
        assert_eq!(f.editor.tool, Tool::Select);
        assert_eq!(
            f.toasts().last(),
            Some(&(ToastKind::Info, "Coverage now counts only the 100 m² area"))
        );
        // Drawing it again replaces its outline.
        draw_limit_area(&mut f.editor);
        f.draw(&square(30.0, 2.0));
        assert_eq!(f.doc.areas.len(), 2);
        assert_eq!(f.doc.region().map(|r| r.points[0]), Some([30.0, 0.0]));
    }

    #[test]
    fn a_bad_outline_stays_to_be_finished() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Areas;
        f.editor.region_draft = vec![[0.0, 0.0], [10.0, 0.0], [0.0, 10.0], [10.0, 10.0]];
        let revision = f.doc.revision;
        finish_outline(&mut f.ctx());
        assert_eq!(f.editor.region_draft.len(), 4);
        assert_eq!((f.doc.areas.len(), f.doc.revision), (0, revision));
        assert_eq!(f.toasts()[0].0, ToastKind::Error);
    }

    #[test]
    fn no_more_than_the_most_areas() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Areas;
        f.doc.areas = (1..=MAX_AREAS as u32)
            .map(|id| LabelledArea {
                id,
                label: None,
                name: None,
                points: square(0.0, 1.0),
                enabled: true,
            })
            .collect();
        f.draw(&square(0.0, 10.0));
        assert_eq!(f.doc.areas.len(), MAX_AREAS);
        assert_eq!(f.editor.region_draft.len(), 4, "kept");
        assert_eq!(f.toasts()[0].0, ToastKind::Error);

        // Nor does drawing the limiting area add a 65th, or redrawing an
        // area deleted meanwhile.
        let full =
            format!("A scene holds at most {MAX_AREAS} areas; delete one you no longer need first");
        for draw in [AreaDraw::Limit, AreaDraw::Redraw(999)] {
            f.editor.region_draft.clear();
            f.editor.area_draw = draw;
            let revision = f.doc.revision;
            f.draw(&square(0.0, 10.0));
            assert_eq!((f.doc.areas.len(), f.doc.revision), (MAX_AREAS, revision));
            assert_eq!(f.doc.limit_figures_to, None);
            assert_eq!(f.editor.region_draft.len(), 4, "kept");
            assert_eq!(f.editor.area_draw, draw, "still drawing it");
            assert_eq!(f.toasts().last(), Some(&(ToastKind::Error, full.as_str())));
        }

        // Redrawing the limiting area when there is one changes no count.
        set_figures_limit(&mut f.doc, Some(3));
        f.editor.region_draft.clear();
        f.editor.area_draw = AreaDraw::Limit;
        f.draw(&square(40.0, 10.0));
        assert_eq!(f.doc.areas.len(), MAX_AREAS);
        assert_eq!(f.doc.region().map(|r| r.points[0]), Some([40.0, 0.0]));
    }

    #[test]
    fn deleting_the_limit_area_lifts_the_limit_and_the_selection() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Areas;
        f.draw(&square(0.0, 10.0));
        f.draw(&square(20.0, 10.0));
        set_figures_limit(&mut f.doc, Some(1));
        f.editor.selected_area = Some(1);
        delete_area(&mut f.ctx(), 1);
        assert_eq!(f.doc.areas.len(), 1);
        assert_eq!(
            (f.doc.limit_figures_to, f.editor.selected_area),
            (None, None)
        );
        // A limit on another area stays.
        set_figures_limit(&mut f.doc, Some(2));
        f.editor.tool = Tool::Areas;
        f.draw(&square(40.0, 10.0));
        delete_area(&mut f.ctx(), 3);
        assert_eq!(f.doc.limit_figures_to, Some(2));
    }

    #[test]
    fn panel_edits_write_only_what_changes() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Areas;
        f.draw(&square(0.0, 10.0));
        f.editor.tool = Tool::Target;
        f.editor.target_shape = TargetShape::Outline;
        f.draw(&square(0.0, 2.0));
        let mut doc = Counting::new(std::mem::take(&mut f.doc));
        let r = doc.value.revision;
        rename_area(&mut doc, 1, "");
        set_area_label(&mut doc, 1, None);
        set_area_enabled(&mut doc, 1, true);
        set_figures_limit(&mut doc, None);
        set_figures_limit(&mut doc, Some(7));
        rename_target(&mut doc, 1, "Target 1");
        rename_area(&mut doc, 99, "Nowhere");
        assert_eq!((doc.writes, doc.value.revision), (0, r), "nothing changed");
        rename_area(&mut doc, 1, "Garden ");
        set_area_label(&mut doc, 1, Some(AreaLabel::Own));
        set_figures_limit(&mut doc, Some(1));
        rename_target(&mut doc, 1, "Front door");
        assert_eq!((doc.writes, doc.value.revision), (4, r + 4));
        let a = &doc.value.areas[0];
        assert_eq!(
            (a.name.as_deref(), a.label),
            (Some("Garden "), Some(AreaLabel::Own)),
            "kept as typed; files trim it"
        );
        assert_eq!(doc.value.targets[0].name, "Front door");
        // Blank is no name; a long one is cut.
        rename_area(&mut doc, 1, "   ");
        assert_eq!(doc.value.areas[0].name, None);
        rename_target(&mut doc, 1, &"x".repeat(100));
        assert_eq!(doc.value.targets[0].name.chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn a_wall_click_marks_the_wall_and_a_ground_click_a_square() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Target;
        let wall = hit(HitTarget::Building, [5.0, 1.5, 1.0], [1.0, 0.0, 0.0]);
        target_click(&mut f.ctx(), Some(wall));
        target_click(
            &mut f.ctx(),
            Some(hit(HitTarget::Ground, [2.0, 0.3, 4.0], [0.0, 1.0, 0.0])),
        );
        assert_eq!(
            f.doc.targets,
            [
                Target {
                    id: 1,
                    name: "Target 1".into(),
                    at: TargetPlace::Wall {
                        point: [5.0, 1.5, 1.0],
                        normal: [1.0, 0.0],
                    },
                },
                Target {
                    id: 2,
                    name: "Target 2".into(),
                    at: TargetPlace::Ground {
                        points: vec![[1.5, 3.5], [2.5, 3.5], [2.5, 4.5], [1.5, 4.5]],
                    },
                },
            ]
        );
        assert_eq!(f.editor.selected_target, Some(2));
        assert_eq!(f.panels.inspector_tab, InspectorTab::Places);
        assert_eq!(
            f.toasts()[0],
            (
                ToastKind::Info,
                "Added Target 1 on the wall: its verdicts are under Places"
            )
        );
    }

    #[test]
    fn a_roof_a_camera_or_nothing_marks_no_target() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Target;
        let mut doc = Counting::new(std::mem::take(&mut f.doc));
        let mut ctx = EditCtx {
            doc: &mut doc,
            editor: &mut f.editor,
            panels: &mut f.panels,
            toasts: &mut f.toasts,
        };
        target_click(&mut ctx, None);
        target_click(
            &mut ctx,
            Some(hit(HitTarget::Camera(1), [0.0; 3], [0.0, 1.0, 0.0])),
        );
        target_click(
            &mut ctx,
            Some(hit(HitTarget::Building, [0.0, 8.0, 0.0], [0.0, 1.0, 0.0])),
        );
        assert_eq!(doc.writes, 0);
        assert!(doc.value.targets.is_empty());
        assert_eq!(
            f.toasts().as_slice(),
            [(
                ToastKind::Error,
                "Click a wall or open ground: a roof cannot be a target"
            )]
        );
    }

    #[test]
    fn a_target_outline_is_checked_like_a_target() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Target;
        f.editor.target_shape = TargetShape::Outline;
        // Too wide: kept to be finished, with the reason.
        f.draw(&square(0.0, 40.0));
        assert!(f.doc.targets.is_empty());
        assert_eq!(f.editor.region_draft.len(), 4);
        assert_eq!(
            f.toasts().as_slice(),
            [(
                ToastKind::Error,
                "A target is at most 30 m across; draw an area for larger ground"
            )]
        );
        f.editor.region_draft.clear();
        f.draw(&square(0.0, 3.0));
        assert_eq!(
            f.doc.targets[0].at,
            TargetPlace::Ground {
                points: square(0.0, 3.0)
            }
        );
        assert!(f.editor.region_draft.is_empty());
        assert_eq!(f.editor.tool, Tool::Target, "ready for the next one");
        // At most 32 corners while drawing.
        f.editor.region_draft = (0..MAX_TARGET_POINTS)
            .map(|i| [i as f32, (i % 2) as f32])
            .collect();
        outline_click(&mut f.ctx(), [-1.0, -1.0]);
        assert_eq!(f.editor.region_draft.len(), MAX_TARGET_POINTS);
        assert!(f.toasts().last().is_some_and(
            |t| t.1 == "A target has at most 32 corners; click the first corner to close it"
        ));
    }

    #[test]
    fn no_more_than_the_most_targets() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Target;
        let ground = hit(HitTarget::Ground, [0.0; 3], [0.0, 1.0, 0.0]);
        for _ in 0..=MAX_TARGETS {
            target_click(&mut f.ctx(), Some(ground));
        }
        assert_eq!(f.doc.targets.len(), MAX_TARGETS);
        assert_eq!(f.toasts().last().map(|t| t.0), Some(ToastKind::Error));
    }

    #[test]
    fn deleting_a_target_drops_its_selection() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Target;
        let ground = hit(HitTarget::Ground, [0.0; 3], [0.0, 1.0, 0.0]);
        target_click(&mut f.ctx(), Some(ground));
        target_click(&mut f.ctx(), Some(ground));
        f.editor.selected_target = Some(1);
        delete_target(&mut f.ctx(), 1);
        assert_eq!(f.doc.targets.len(), 1);
        assert_eq!(f.editor.selected_target, None);
        let r = f.doc.revision;
        delete_target(&mut f.ctx(), 1);
        assert_eq!(f.doc.revision, r, "already gone");
    }

    #[test]
    fn titles_fall_back_to_the_id() {
        let mut a = LabelledArea {
            id: 3,
            label: None,
            name: Some("  ".into()),
            points: Vec::new(),
            enabled: true,
        };
        assert_eq!(area_title(&a), "Area 3");
        a.name = Some(" Garden ".into());
        assert_eq!(area_title(&a), "Garden");
        let t = Target {
            id: 2,
            name: String::new(),
            at: TargetPlace::Wall {
                point: [0.0; 3],
                normal: [1.0, 0.0],
            },
        };
        assert_eq!(target_title(&t), "Target 2");
        assert_eq!(label_text(None), "Unlabelled");
        assert_eq!(label_text(Some(AreaLabel::Public)), "Public");
    }
}
