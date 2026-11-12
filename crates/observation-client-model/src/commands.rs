//! What the tools and keys do to the document and the editing state: a
//! click or a drag with each tool, drawing a region, and the keyboard
//! shortcuts.
//!
//! The client finds what the pointer is over and which keys went down, and
//! calls these with an [`EditCtx`] built from its resources.

use crate::document::{Document, Models};
use crate::editor::{Editor, OverlayMode, Panels, Tool, select};
use crate::toasts::Toasts;
use observation_core::camera::{CameraPlacement, yaw_pitch_from_forward};
use observation_core::coverage::{MAX_REGION_POINTS, Region};
use observation_core::glam::{Vec2, Vec3};

/// Cameras are mounted this far in front of the surface they are placed on.
pub const MOUNT_OFFSET_M: f32 = 0.15;

/// State whose write access is noticed. The client's resources record a
/// change on write access, and a change re-runs coverage, painting and
/// autosave, so a command takes [`Self::get_mut`] only where it writes.
/// A plain [`Document`] is its own `Tracked`, for callers that track
/// nothing or have already taken write access.
pub trait Tracked<T> {
    /// Read access; never marks anything changed.
    fn get(&self) -> &T;
    /// Write access. In the client, calling it marks the resource changed
    /// whether or not anything is then written, so call it only on a path
    /// that writes.
    fn get_mut(&mut self) -> &mut T;
}

impl Tracked<Document> for Document {
    fn get(&self) -> &Document {
        self
    }

    fn get_mut(&mut self) -> &mut Document {
        self
    }
}

/// What an edit may change.
pub struct EditCtx<'a> {
    pub doc: &'a mut dyn Tracked<Document>,
    pub editor: &'a mut Editor,
    pub panels: &'a mut Panels,
    pub toasts: &'a mut Toasts,
}

impl std::fmt::Debug for EditCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditCtx")
            .field("doc", self.doc.get())
            .field("editor", &self.editor)
            .field("panels", &self.panels)
            .field("toasts", &self.toasts)
            .finish()
    }
}

/// A ray-cast hit on something the tools care about.
#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub point: Vec3,
    pub normal: Vec3,
    pub target: HitTarget,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitTarget {
    Building,
    Ground,
    Camera(u32),
}

/// A keyboard shortcut. The client maps keys to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Switch to this tool.
    Tool(Tool),
    /// Back to the Select tool, closing the dialogs (Esc).
    Escape,
    /// Close the region being drawn (Enter).
    Finish,
    /// Undo a region corner, clear the picked address or delete the
    /// selected camera (Backspace, Delete).
    Erase,
    /// Show or hide the help (`/`, F1).
    Help,
    /// Move the view to the selected camera (F).
    Focus,
    /// Switch how the coverage overlay is coloured (N).
    Overlay,
}

/// Runs the shortcuts whose keys `pressed` says went down this frame.
/// Returns whether one asked to focus the view on the selected camera,
/// which the client does.
pub fn keys(ctx: &mut EditCtx, pressed: &dyn Fn(Key) -> bool) -> bool {
    for tool in Tool::ALL {
        if pressed(Key::Tool(tool)) {
            ctx.editor.tool = tool;
        }
    }
    if pressed(Key::Escape) {
        if ctx.editor.tool == Tool::Select {
            ctx.editor.selected = None;
        }
        ctx.editor.tool = Tool::Select;
        ctx.panels.map_open = false;
        ctx.panels.help_open = false;
        ctx.panels.regions_open = false;
    }
    if ctx.editor.tool == Tool::Region {
        // Drawing a region: Enter closes it, Backspace undoes a corner.
        if pressed(Key::Finish) {
            finish_region(ctx);
        }
        if pressed(Key::Erase) {
            ctx.editor.region_draft.pop();
        }
    } else if ctx.editor.tool == Tool::Address {
        // The picked address, not the camera selected before the tool.
        if pressed(Key::Erase) {
            ctx.editor.selected_address = None;
        }
    } else if pressed(Key::Erase)
        && let Some(id) = ctx.editor.selected.take()
    {
        ctx.doc.get_mut().remove_camera(id);
    }
    if pressed(Key::Help) {
        ctx.panels.help_open = !ctx.panels.help_open;
    }
    if pressed(Key::Overlay) {
        ctx.panels.overlay = match ctx.panels.overlay {
            OverlayMode::Overlap => OverlayMode::PerCamera,
            OverlayMode::PerCamera => OverlayMode::Overlap,
        };
    }
    pressed(Key::Focus)
}

/// Adds a region corner, or closes the region when the first corner is clicked.
pub fn region_click(ctx: &mut EditCtx, p: [f32; 2]) {
    let editor = &mut *ctx.editor;
    let closes = editor.region_draft.len() >= 3
        && editor.region_draft.first().is_some_and(|f| {
            Vec2::from_array(*f).distance(Vec2::from_array(p)) <= editor.region_snap_m
        });
    if closes {
        finish_region(ctx);
    } else if editor.region_draft.len() < MAX_REGION_POINTS {
        editor.region_draft.push(p);
    } else {
        ctx.toasts.error(format!(
            "A region has at most {MAX_REGION_POINTS} corners; click the first corner to close it"
        ));
    }
}

/// Turns the draft into the scene's region (nothing to do without corners).
/// Takes write access to the document even then, as it always has.
pub fn finish_region(ctx: &mut EditCtx) {
    let doc = ctx.doc.get_mut();
    let editor = &mut *ctx.editor;
    if editor.region_draft.is_empty() {
        return;
    }
    let region = Region {
        points: std::mem::take(&mut editor.region_draft),
        enabled: true,
    };
    match region.problem() {
        None => {
            ctx.toasts.info(format!(
                "Coverage now counts only the {:.0} m² region",
                region.area_m2()
            ));
            doc.region = Some(region);
            doc.touch();
            editor.tool = Tool::Select;
        }
        Some(problem) => {
            editor.region_draft = region.points;
            ctx.toasts.error(problem.message());
        }
    }
}

/// A click/tap (press + release without dragging).
pub fn click(ctx: &mut EditCtx, models: &Models, hit: Option<Hit>) {
    let editor = &mut *ctx.editor;
    let panels = &mut *ctx.panels;
    match editor.tool {
        Tool::Select => match hit {
            Some(Hit {
                target: HitTarget::Camera(id),
                ..
            }) => select(editor, panels, id),
            _ => editor.selected = None,
        },
        Tool::Place => {
            let Some(hit) = hit else { return };
            if let HitTarget::Camera(id) = hit.target {
                select(editor, panels, id);
                return;
            }
            let kind = models.kind_of(&editor.place_model);
            let (position, yaw) = mount(hit, kind.default_mount_height_m());
            let id = ctx.doc.get_mut().add_camera(CameraPlacement {
                id: 0,
                label: String::new(),
                model_id: editor.place_model.clone(),
                position: position.to_array(),
                yaw_deg: yaw.unwrap_or(0.0),
                pitch_deg: kind.default_pitch_deg(),
                roll_deg: 0.0,
                color: None,
                enabled: true,
                group: None,
            });
            select(editor, panels, id);
            if yaw.is_none() {
                // Placed on the ground: switch to Aim so the next click points it.
                editor.tool = Tool::Aim;
                ctx.toasts.info("Click where the new camera should point");
            }
        }
        // Handled by the client (by `region_click` for Region).
        Tool::Region | Tool::Address => {}
        Tool::Aim | Tool::Move => {
            if editor.selected.is_none()
                && let Some(Hit {
                    target: HitTarget::Camera(id),
                    ..
                }) = hit
            {
                select(editor, panels, id);
            }
        }
    }
}

/// A press or drag with the Aim or Move tool on the selected camera.
pub fn apply_tool(editor: &mut Editor, doc: &mut Document, models: &Models, hit: Hit) {
    let Some(id) = editor.selected else { return };
    let tool = editor.tool;
    let Some(cam) = doc.camera_mut(id) else {
        return;
    };
    match tool {
        Tool::Aim => {
            if !matches!(hit.target, HitTarget::Camera(c) if c == id) {
                cam.look_at(hit.point);
            }
        }
        Tool::Move => {
            if matches!(hit.target, HitTarget::Camera(_)) {
                return;
            }
            let kind = models.kind_of(&cam.model_id);
            let (position, yaw) = mount(hit, kind.default_mount_height_m());
            cam.position = position.to_array();
            if let Some(y) = yaw {
                cam.yaw_deg = y;
            }
        }
        _ => {}
    }
}

/// Mount position for a hit: on a wall, just in front of the surface facing
/// outward; on the ground or a roof, raised to the mount height.
pub fn mount(hit: Hit, mount_height: f32) -> (Vec3, Option<f32>) {
    let wall = hit.target == HitTarget::Building && hit.normal.y.abs() < 0.5;
    if wall {
        let n = hit.normal.with_y(0.0).normalize_or(Vec3::X);
        let yaw = yaw_pitch_from_forward(n).map(|(y, _)| y);
        (hit.point + n * MOUNT_OFFSET_M, yaw)
    } else {
        (hit.point + Vec3::Y * mount_height, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::InspectorTab;
    use crate::toasts::ToastKind;
    use observation_core::camera::{CameraKind, Catalog};

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

        /// Places a camera on a pole at the origin and selects it.
        fn with_camera(mut self) -> Self {
            let id = self.doc.add_camera(CameraPlacement {
                id: 0,
                label: String::new(),
                model_id: "m".to_owned(),
                position: [0.0, 3.0, 0.0],
                yaw_deg: 0.0,
                pitch_deg: 0.0,
                roll_deg: 0.0,
                color: None,
                enabled: true,
                group: None,
            });
            self.editor.selected = Some(id);
            self
        }

        fn toasts(&self) -> Vec<(ToastKind, &str)> {
            self.toasts
                .iter()
                .map(|t| (t.kind, t.text.as_str()))
                .collect()
        }

        fn press(&mut self, keys: &[Key]) -> bool {
            super::keys(&mut self.ctx(), &|k| keys.contains(&k))
        }
    }

    fn models() -> Models {
        Models::new(Catalog::default())
    }

    fn hit(target: HitTarget, point: [f32; 3], normal: [f32; 3]) -> Hit {
        Hit {
            point: Vec3::from_array(point),
            normal: Vec3::from_array(normal),
            target,
        }
    }

    fn ground(x: f32, z: f32) -> Hit {
        hit(HitTarget::Ground, [x, 0.0, z], [0.0, 1.0, 0.0])
    }

    /// An east-facing wall at `x = 5`.
    fn wall() -> Hit {
        hit(HitTarget::Building, [5.0, 2.0, 1.0], [1.0, 0.0, 0.0])
    }

    #[test]
    fn a_wall_mount_stands_off_the_wall_facing_out() {
        let (position, yaw) = mount(wall(), 2.6);
        assert_eq!(position, Vec3::new(5.0 + MOUNT_OFFSET_M, 2.0, 1.0));
        assert_eq!(yaw, Some(90.0));
    }

    #[test]
    fn a_ground_or_roof_mount_is_raised_and_keeps_its_heading() {
        assert_eq!(
            mount(ground(1.0, 2.0), 2.6),
            (Vec3::new(1.0, 2.6, 2.0), None)
        );
        let roof = hit(HitTarget::Building, [0.0, 8.0, 0.0], [0.0, 1.0, 0.0]);
        assert_eq!(mount(roof, 1.2), (Vec3::new(0.0, 9.2, 0.0), None));
    }

    #[test]
    fn placing_on_the_ground_selects_the_camera_and_switches_to_aim() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Place;
        f.panels.inspector_tab = InspectorTab::Analysis;
        click(&mut f.ctx(), &models(), Some(ground(4.0, -3.0)));

        let kind = CameraKind::default();
        let [c] = f.doc.cameras.as_slice() else {
            panic!("one camera placed")
        };
        assert_eq!(c.model_id, "generic-doorbell-160");
        assert_eq!(c.position, [4.0, kind.default_mount_height_m(), -3.0]);
        assert_eq!((c.yaw_deg, c.pitch_deg), (0.0, kind.default_pitch_deg()));
        assert_eq!(f.editor.selected, Some(c.id));
        assert_eq!(f.panels.inspector_tab, InspectorTab::Camera);
        assert_eq!(f.editor.tool, Tool::Aim);
        assert_eq!(
            f.toasts(),
            [(ToastKind::Info, "Click where the new camera should point")]
        );
    }

    #[test]
    fn placing_on_a_wall_faces_out_and_keeps_the_place_tool() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Place;
        click(&mut f.ctx(), &models(), Some(wall()));
        assert_eq!(f.doc.cameras.len(), 1);
        assert_eq!(f.doc.cameras[0].yaw_deg, 90.0);
        assert_eq!(f.editor.tool, Tool::Place);
        assert!(f.toasts.is_empty());
    }

    #[test]
    fn placing_on_a_camera_selects_it_and_on_nothing_does_nothing() {
        let mut f = Fixture::default().with_camera();
        f.editor.selected = None;
        f.editor.tool = Tool::Place;
        click(&mut f.ctx(), &models(), None);
        assert_eq!((f.doc.cameras.len(), f.editor.selected), (1, None));
        let on_camera = hit(HitTarget::Camera(1), [0.0, 3.0, 0.0], [0.0, 1.0, 0.0]);
        click(&mut f.ctx(), &models(), Some(on_camera));
        assert_eq!((f.doc.cameras.len(), f.editor.selected), (1, Some(1)));
        assert_eq!(f.editor.tool, Tool::Place);
    }

    #[test]
    fn select_picks_a_camera_and_clears_on_anything_else() {
        let mut f = Fixture::default().with_camera();
        f.editor.selected = None;
        let on_camera = hit(HitTarget::Camera(1), [0.0, 3.0, 0.0], [0.0, 1.0, 0.0]);
        click(&mut f.ctx(), &models(), Some(on_camera));
        assert_eq!(f.editor.selected, Some(1));
        assert_eq!(f.panels.inspector_tab, InspectorTab::Camera);
        click(&mut f.ctx(), &models(), Some(wall()));
        assert_eq!(f.editor.selected, None);
    }

    #[test]
    fn aim_and_move_pick_a_camera_only_when_none_is_selected() {
        let on_camera = |id| hit(HitTarget::Camera(id), [0.0; 3], [0.0, 1.0, 0.0]);
        let mut f = Fixture::default();
        f.editor.tool = Tool::Move;
        click(&mut f.ctx(), &models(), Some(on_camera(4)));
        assert_eq!(f.editor.selected, Some(4));
        click(&mut f.ctx(), &models(), Some(on_camera(5)));
        assert_eq!(f.editor.selected, Some(4));
        assert!(f.doc.cameras.is_empty());
    }

    #[test]
    fn aiming_points_the_camera_but_not_at_itself() {
        let mut f = Fixture::default().with_camera();
        f.editor.tool = Tool::Aim;
        apply_tool(
            &mut f.editor,
            &mut f.doc,
            &models(),
            hit(HitTarget::Camera(1), [0.0, 3.0, -1.0], [0.0, 1.0, 0.0]),
        );
        assert_eq!(
            (f.doc.cameras[0].yaw_deg, f.doc.cameras[0].pitch_deg),
            (0.0, 0.0)
        );
        apply_tool(&mut f.editor, &mut f.doc, &models(), ground(10.0, 0.0));
        let c = &f.doc.cameras[0];
        assert!((c.yaw_deg - 90.0).abs() < 1e-3, "{}", c.yaw_deg);
        assert!(c.pitch_deg < 0.0);
    }

    #[test]
    fn moving_onto_a_wall_turns_the_camera_and_onto_a_camera_does_nothing() {
        let mut f = Fixture::default().with_camera();
        f.editor.tool = Tool::Move;
        apply_tool(
            &mut f.editor,
            &mut f.doc,
            &models(),
            hit(HitTarget::Camera(2), [9.0; 3], [0.0, 1.0, 0.0]),
        );
        assert_eq!(f.doc.cameras[0].position, [0.0, 3.0, 0.0]);
        apply_tool(&mut f.editor, &mut f.doc, &models(), wall());
        let c = &f.doc.cameras[0];
        assert_eq!(c.position, [5.0 + MOUNT_OFFSET_M, 2.0, 1.0]);
        assert_eq!(c.yaw_deg, 90.0);
        apply_tool(&mut f.editor, &mut f.doc, &models(), ground(1.0, 1.0));
        let c = &f.doc.cameras[0];
        let height = CameraKind::default().default_mount_height_m();
        assert_eq!((c.position, c.yaw_deg), ([1.0, height, 1.0], 90.0));
    }

    #[test]
    fn a_click_near_the_first_corner_closes_a_region_of_three() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Region;
        for p in [[0.0, 0.0], [10.0, 0.0]] {
            region_click(&mut f.ctx(), p);
        }
        // Two corners: near the first one adds a corner rather than closing.
        region_click(&mut f.ctx(), [0.5, 0.0]);
        assert_eq!(f.editor.region_draft.len(), 3);
        f.editor.region_draft[2] = [10.0, 10.0];
        region_click(&mut f.ctx(), [0.5, 0.5]);
        assert_eq!(f.editor.region_draft, Vec::<[f32; 2]>::new());
        let region = f.doc.region.as_ref().expect("closed");
        assert_eq!(region.points, [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]);
        assert!(region.enabled);
        assert_eq!(f.editor.tool, Tool::Select);
        assert_eq!(
            f.toasts(),
            [(ToastKind::Info, "Coverage now counts only the 50 m² region")]
        );
    }

    #[test]
    fn a_region_stops_at_its_most_corners() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Region;
        f.editor.region_draft = (0..MAX_REGION_POINTS)
            .map(|i| [i as f32 * 100.0, (i % 2) as f32 * 100.0])
            .collect();
        region_click(&mut f.ctx(), [-50.0, -50.0]);
        assert_eq!(f.editor.region_draft.len(), MAX_REGION_POINTS);
        assert_eq!(
            f.toasts(),
            [(
                ToastKind::Error,
                format!(
                    "A region has at most {MAX_REGION_POINTS} corners; click the first corner to close it"
                )
                .as_str()
            )]
        );
    }

    #[test]
    fn finishing_a_bad_region_keeps_the_draft() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Region;
        f.editor.region_draft = vec![[0.0, 0.0], [10.0, 0.0]];
        let revision = f.doc.revision;
        finish_region(&mut f.ctx());
        assert_eq!(f.editor.region_draft, [[0.0, 0.0], [10.0, 0.0]]);
        assert_eq!((f.doc.region.is_none(), f.doc.revision), (true, revision));
        assert_eq!(f.editor.tool, Tool::Region);
        assert_eq!(
            f.toasts(),
            [(
                ToastKind::Error,
                "A region needs at least three corners; click to add more"
            )]
        );
    }

    #[test]
    fn tool_keys_switch_tools_and_escape_steps_back_to_select() {
        let mut f = Fixture::default().with_camera();
        f.panels.help_open = true;
        f.panels.map_open = true;
        f.panels.regions_open = true;
        assert!(!f.press(&[Key::Tool(Tool::Aim)]));
        assert_eq!(f.editor.tool, Tool::Aim);
        // From another tool, Esc keeps the selection and closes the dialogs.
        f.press(&[Key::Escape]);
        assert_eq!((f.editor.tool, f.editor.selected), (Tool::Select, Some(1)));
        assert!(!(f.panels.help_open || f.panels.map_open || f.panels.regions_open));
        // In Select, it clears the selection.
        f.press(&[Key::Escape]);
        assert_eq!(f.editor.selected, None);
    }

    #[test]
    fn erase_undoes_a_corner_clears_an_address_or_deletes_the_camera() {
        let mut f = Fixture::default().with_camera();
        f.editor.tool = Tool::Region;
        f.editor.region_draft = vec![[0.0, 0.0], [1.0, 0.0]];
        f.press(&[Key::Erase]);
        assert_eq!(f.editor.region_draft, [[0.0, 0.0]]);
        assert_eq!((f.doc.cameras.len(), f.editor.selected), (1, Some(1)));

        f.editor.tool = Tool::Address;
        f.editor.selected_address = Some(observation_core::address::Address {
            housenumber: Some("1".to_owned()),
            street: Some("High Street".to_owned()),
            housename: None,
            unit: None,
        });
        f.press(&[Key::Erase]);
        assert_eq!(f.editor.selected_address, None);
        assert_eq!(f.doc.cameras.len(), 1);

        // Today the tool stays on when the camera it works on is deleted.
        f.editor.tool = Tool::Aim;
        f.press(&[Key::Erase]);
        assert!(f.doc.cameras.is_empty());
        assert_eq!((f.editor.selected, f.editor.tool), (None, Tool::Aim));
    }

    #[test]
    fn enter_finishes_a_region_and_the_toggles_flip() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Region;
        f.editor.region_draft = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        f.press(&[Key::Finish]);
        assert!(f.doc.region.is_some());
        f.press(&[Key::Help, Key::Overlay]);
        assert!(f.panels.help_open);
        assert_eq!(f.panels.overlay, OverlayMode::PerCamera);
        f.press(&[Key::Overlay]);
        assert_eq!(f.panels.overlay, OverlayMode::Overlap);
        assert!(f.press(&[Key::Focus]));
    }

    /// Counts write access, which the client's resources record as a change.
    struct Counting {
        doc: Document,
        writes: usize,
    }

    impl Tracked<Document> for Counting {
        fn get(&self) -> &Document {
            &self.doc
        }

        fn get_mut(&mut self) -> &mut Document {
            self.writes += 1;
            &mut self.doc
        }
    }

    #[test]
    fn keys_take_write_access_only_to_delete_or_finish() {
        let mut f = Fixture::default().with_camera();
        let mut doc = Counting {
            doc: std::mem::take(&mut f.doc),
            writes: 0,
        };
        let press = |f: &mut Fixture, doc: &mut Counting, keys: &[Key]| {
            let mut ctx = EditCtx {
                doc,
                editor: &mut f.editor,
                panels: &mut f.panels,
                toasts: &mut f.toasts,
            };
            super::keys(&mut ctx, &|k| keys.contains(&k));
        };
        press(&mut f, &mut doc, &[]);
        press(&mut f, &mut doc, &[Key::Focus, Key::Help, Key::Overlay]);
        press(&mut f, &mut doc, &[Key::Tool(Tool::Region), Key::Erase]);
        assert_eq!(doc.writes, 0);
        // Enter takes it even with no corners to finish, as it always has.
        press(&mut f, &mut doc, &[Key::Finish]);
        assert_eq!(doc.writes, 1);
        press(&mut f, &mut doc, &[Key::Escape]);
        press(&mut f, &mut doc, &[Key::Erase]);
        assert_eq!((doc.writes, doc.doc.cameras.len()), (2, 0));
    }

    #[test]
    fn keys_write_nothing_without_a_selection_or_outside_the_region_tool() {
        let mut f = Fixture::default().with_camera();
        f.editor.selected = None;
        f.editor.region_draft = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        let mut doc = Counting {
            doc: std::mem::take(&mut f.doc),
            writes: 0,
        };
        for tool in [Tool::Select, Tool::Aim, Tool::Move] {
            f.editor.tool = tool;
            let mut ctx = EditCtx {
                doc: &mut doc,
                editor: &mut f.editor,
                panels: &mut f.panels,
                toasts: &mut f.toasts,
            };
            super::keys(&mut ctx, &|k| [Key::Erase, Key::Finish].contains(&k));
        }
        assert_eq!(doc.writes, 0);
        assert_eq!(doc.doc.cameras.len(), 1);
        assert_eq!(f.editor.region_draft.len(), 3);
        assert!(doc.doc.region.is_none() && f.toasts.is_empty());
    }

    #[test]
    fn a_click_exactly_the_snap_distance_away_closes_the_region() {
        let mut f = Fixture::default();
        f.editor.tool = Tool::Region;
        f.editor.region_snap_m = 1.0;
        f.editor.region_draft = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        region_click(&mut f.ctx(), [0.0, 1.01]);
        assert_eq!(f.editor.region_draft.len(), 4);
        f.editor.region_draft.pop();
        region_click(&mut f.ctx(), [0.0, 1.0]);
        assert!(f.doc.region.is_some());
        assert!(f.editor.region_draft.is_empty());
    }

    #[test]
    fn of_two_tool_keys_in_one_frame_the_later_in_the_rail_wins() {
        let mut f = Fixture::default();
        f.press(&[Key::Tool(Tool::Address), Key::Tool(Tool::Select)]);
        assert_eq!(f.editor.tool, Tool::Address);
        f.press(&[Key::Tool(Tool::Region), Key::Tool(Tool::Place)]);
        assert_eq!(f.editor.tool, Tool::Region);
    }
}
