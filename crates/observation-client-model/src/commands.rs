//! What the tools, keys and buttons do to the document and the editing
//! state: a click or a drag with each tool, drawing a region, the keyboard
//! shortcuts, the camera commands and the custom models.
//!
//! The client finds what the pointer is over, which keys went down and
//! which button was clicked, and calls these with an [`EditCtx`] built from
//! its resources.

use crate::document::{Document, Models};
use crate::editor::{Editor, OverlayMode, Panels, Tool, select};
use crate::toasts::Toasts;
use observation_core::camera::{
    CameraModel, CameraPlacement, CatalogError, yaw_pitch_from_forward,
};
use observation_core::coverage::{CoverageParams, MAX_REGION_POINTS, Region};
use observation_core::glam::{Vec2, Vec3};
use observation_core::scene::MAX_CUSTOM_MODELS;
use observation_core::terrain::Terrain;

/// Cameras are mounted this far in front of the surface they are placed on.
pub const MOUNT_OFFSET_M: f32 = 0.15;

/// State whose write access is noticed. The client's resources record a
/// change on write access, and a change re-runs coverage, painting and
/// autosave, so a command takes [`Self::get_mut`] only where it writes.
/// A plain [`Document`] or [`Models`] is its own `Tracked`, for callers
/// that track nothing or have already taken write access.
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

impl Tracked<Models> for Models {
    fn get(&self) -> &Models {
        self
    }

    fn get_mut(&mut self) -> &mut Models {
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

impl Key {
    /// The tool and panel shortcuts, plain keys that the browser also uses
    /// with Ctrl, Cmd or Alt (Ctrl+R reloads, Ctrl+P prints, Ctrl+F finds):
    /// they do nothing while one of those is held, and the page leaves those
    /// keys to the browser (web/index.html). Esc, Enter, Delete and
    /// Backspace work as without the modifier.
    pub fn yields_to_modifiers(self) -> bool {
        matches!(
            self,
            Self::Tool(_) | Self::Help | Self::Focus | Self::Overlay
        )
    }
}

/// Runs the shortcuts whose keys `pressed` says went down this frame;
/// `modifier_held` is whether Ctrl, Cmd (Meta) or Alt is held, but not
/// Shift (see [`Key::yields_to_modifiers`]). Returns whether one asked to
/// focus the view on the selected camera, which the client does.
pub fn keys(ctx: &mut EditCtx, pressed: &dyn Fn(Key) -> bool, modifier_held: bool) -> bool {
    let pressed = |key: Key| !(modifier_held && key.yields_to_modifiers()) && pressed(key);
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
        // As the camera list's Delete: Aim and Move have nothing left to work on.
        if ctx.editor.tool.needs_selection() {
            ctx.editor.tool = Tool::Select;
        }
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

/// A command on one camera, from the inspector, the camera list's context
/// menu or the floating toolbar next to the selected camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraAction {
    Select,
    Focus,
    Aim,
    Move,
    Duplicate,
    ToggleEnabled,
    Delete,
}

impl CameraAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Focus => "Focus",
            Self::Aim => "Aim",
            Self::Move => "Move",
            Self::Duplicate => "Duplicate",
            Self::ToggleEnabled => "Include in coverage",
            Self::Delete => "Delete",
        }
    }

    pub fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::Focus => Some("F"),
            Self::Aim => Some("A"),
            Self::Move => Some("M"),
            Self::Delete => Some("Del"),
            _ => None,
        }
    }
}

/// Runs `action` on camera `id`. Actions other than Delete and
/// ToggleEnabled also select it. Returns whether it asked to focus the view
/// on the camera, which the client does.
pub fn camera_action(ctx: &mut EditCtx, terrain: &Terrain, id: u32, action: CameraAction) -> bool {
    let Some(c) = ctx.doc.get().camera(id).cloned() else {
        return false;
    };
    let editor = &mut *ctx.editor;
    let panels = &mut *ctx.panels;
    match action {
        CameraAction::Select => select(editor, panels, id),
        CameraAction::Focus => {
            select(editor, panels, id);
            return true;
        }
        CameraAction::Aim => {
            select(editor, panels, id);
            editor.tool = Tool::Aim;
        }
        CameraAction::Move => {
            select(editor, panels, id);
            editor.tool = Tool::Move;
        }
        CameraAction::Duplicate => {
            let mut copy = c;
            copy.label = String::new();
            // A metre east, at the same height above the ground.
            let [x, _, z] = copy.position;
            copy.position = terrain.moved_to(copy.position, x + 1.0, z);
            let new = ctx.doc.get_mut().add_camera(copy);
            select(editor, panels, new);
        }
        CameraAction::ToggleEnabled => {
            if let Some(c) = ctx.doc.get_mut().camera_mut(id) {
                c.enabled = !c.enabled;
            }
        }
        CameraAction::Delete => {
            ctx.doc.get_mut().remove_camera(id);
            if editor.selected == Some(id) {
                editor.selected = None;
                if editor.tool.needs_selection() {
                    editor.tool = Tool::Select;
                }
            }
        }
    }
    false
}

/// Adds `model` as a new custom model under a fresh id and makes it the
/// model for new cameras. Returns the added model, to go on editing it so
/// a second click cannot add a copy.
pub fn add_custom_model(
    ctx: &mut EditCtx,
    models: &mut dyn Tracked<Models>,
    mut model: CameraModel,
) -> Option<CameraModel> {
    // A scene with more could not be restored or imported.
    if models.get().custom.len() >= MAX_CUSTOM_MODELS {
        ctx.toasts.error(format!(
            "Cannot add this model: a scene holds at most {MAX_CUSTOM_MODELS} custom models. Delete one you no longer use first"
        ));
        return None;
    }
    model.id = models.get().new_custom_id(&model.name);
    match model.validate() {
        Ok(()) => {
            ctx.editor.place_model = model.id.clone();
            ctx.toasts.info(format!(
                "Added “{}” — it is now the model for new cameras",
                model.name
            ));
            models.get_mut().custom.push(model.clone());
            ctx.doc.get_mut().touch();
            Some(model)
        }
        Err(e) => {
            ctx.toasts
                .error(format!("Cannot add this model: {}", invalid_reason(e)));
            None
        }
    }
}

/// Updates the custom model in place, keeping its id: placed cameras refer
/// to it by id.
pub fn save_custom_model(ctx: &mut EditCtx, models: &mut dyn Tracked<Models>, model: CameraModel) {
    if let Err(e) = model.validate() {
        ctx.toasts
            .error(format!("Cannot save this model: {}", invalid_reason(e)));
        return;
    }
    let name = model.display_name();
    if models.get_mut().update_custom(model) {
        ctx.toasts.info(format!("Saved “{name}”"));
        ctx.doc.get_mut().touch();
    }
}

/// Deletes an unused custom model; refuses one that cameras still use, so
/// no camera is left without a model.
pub fn delete_custom_model(ctx: &mut EditCtx, models: &mut dyn Tracked<Models>, id: &str) {
    let Some(name) = models.get().custom_model(id).map(CameraModel::display_name) else {
        return;
    };
    let users = ctx.doc.get().cameras_using(id);
    if users > 0 {
        let users = if users == 1 {
            "1 camera uses it".to_owned()
        } else {
            format!("{users} cameras use it")
        };
        ctx.toasts.error(format!(
            "Cannot delete “{name}”: {users}. Give them another model or delete them first"
        ));
        return;
    }
    models.get_mut().remove_custom(id);
    ctx.doc.get_mut().touch();
}

/// Sets what the figures count (detail level, lighting, subject height),
/// as the Analysis tab and the Coverage card's ladder do. Writes, and starts
/// a new revision, only when the parameters differ.
pub fn set_params(doc: &mut dyn Tracked<Document>, params: CoverageParams) {
    if doc.get().params != params {
        let doc = doc.get_mut();
        doc.params = params;
        doc.touch();
    }
}

/// Removes the region the figures are limited to, if there is one.
pub fn clear_region(doc: &mut dyn Tracked<Document>) {
    if doc.get().region.is_some() {
        let doc = doc.get_mut();
        doc.region = None;
        doc.touch();
    }
}

/// Limits the figures to the region, or stops doing so, if there is one.
/// The Coverage card calls it only when its checkbox changed.
pub fn set_region_enabled(doc: &mut dyn Tracked<Document>, enabled: bool) {
    if doc.get().region.is_some() {
        let doc = doc.get_mut();
        if let Some(r) = &mut doc.region {
            r.enabled = enabled;
        }
        doc.touch();
    }
}

/// Writes back the inspector's edited copy of camera `id`, only when it
/// differs from the camera (`camera_mut` starts a new revision).
pub fn update_camera(doc: &mut dyn Tracked<Document>, id: u32, edited: CameraPlacement) {
    if doc.get().camera(id).is_some_and(|old| *old != edited)
        && let Some(slot) = doc.get_mut().camera_mut(id)
    {
        *slot = edited;
    }
}

/// Why a model failed validation, in the user's terms.
fn invalid_reason(e: CatalogError) -> String {
    match e {
        // The generated id means nothing to the user; the reason does.
        CatalogError::Invalid { why, .. } => why,
        e => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::InspectorTab;
    use crate::testing::Counting;
    use crate::toasts::ToastKind;
    use observation_core::camera::{CUSTOM_ID_PREFIX, CameraKind, Catalog, DoriLevel};
    use observation_core::terrain::FLAT;

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
            super::keys(&mut self.ctx(), &|k| keys.contains(&k), false)
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

        // Aim or Move goes back to Select with its camera, as from the list.
        f.editor.tool = Tool::Aim;
        f.press(&[Key::Erase]);
        assert!(f.doc.cameras.is_empty());
        assert_eq!((f.editor.selected, f.editor.tool), (None, Tool::Select));
    }

    #[test]
    fn deleting_with_the_key_leaves_aim_and_move_like_the_list_does() {
        for tool in [Tool::Aim, Tool::Move] {
            let mut by_key = Fixture::default().with_camera();
            by_key.editor.tool = tool;
            by_key.press(&[Key::Erase]);
            let mut by_list = Fixture::default().with_camera();
            by_list.editor.tool = tool;
            act(&mut by_list, 1, CameraAction::Delete);
            assert_eq!(by_key.editor, by_list.editor, "{tool:?}");
            assert_eq!(by_key.editor.tool, Tool::Select);
        }
        // Select and Place stay as they are.
        let mut f = Fixture::default().with_camera();
        f.editor.tool = Tool::Place;
        f.press(&[Key::Erase]);
        assert_eq!((f.editor.selected, f.editor.tool), (None, Tool::Place));
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

    #[test]
    fn keys_take_write_access_only_to_delete_or_finish() {
        let mut f = Fixture::default().with_camera();
        let mut doc = Counting::new(std::mem::take(&mut f.doc));
        let press = |f: &mut Fixture, doc: &mut Counting<Document>, keys: &[Key]| {
            let mut ctx = EditCtx {
                doc,
                editor: &mut f.editor,
                panels: &mut f.panels,
                toasts: &mut f.toasts,
            };
            super::keys(&mut ctx, &|k| keys.contains(&k), false);
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
        assert_eq!((doc.writes, doc.value.cameras.len()), (2, 0));
    }

    /// Runs a camera action on a counted copy of the fixture's document.
    /// Returns (focus asked, write accesses).
    fn act(f: &mut Fixture, id: u32, action: CameraAction) -> (bool, usize) {
        let mut doc = Counting::new(std::mem::take(&mut f.doc));
        let mut ctx = EditCtx {
            doc: &mut doc,
            editor: &mut f.editor,
            panels: &mut f.panels,
            toasts: &mut f.toasts,
        };
        let focus = camera_action(&mut ctx, &FLAT, id, action);
        f.doc = doc.value;
        (focus, doc.writes)
    }

    #[test]
    fn selecting_actions_select_and_only_focus_asks_for_the_view() {
        let mut f = Fixture::default().with_camera();
        f.editor.selected = None;
        assert_eq!(act(&mut f, 1, CameraAction::Select), (false, 0));
        assert_eq!(
            (f.editor.selected, f.panels.inspector_tab),
            (Some(1), InspectorTab::Camera)
        );
        f.panels.inspector_tab = InspectorTab::Cameras;
        assert_eq!(act(&mut f, 1, CameraAction::Focus), (true, 0));
        assert_eq!(f.panels.inspector_tab, InspectorTab::Camera);
        assert_eq!(act(&mut f, 1, CameraAction::Aim), (false, 0));
        assert_eq!(f.editor.tool, Tool::Aim);
        assert_eq!(act(&mut f, 1, CameraAction::Move), (false, 0));
        assert_eq!(f.editor.tool, Tool::Move);
        // A camera that is gone: nothing at all.
        f.editor.tool = Tool::Select;
        assert_eq!(act(&mut f, 9, CameraAction::Delete), (false, 0));
        assert_eq!((f.doc.cameras.len(), f.editor.selected), (1, Some(1)));
    }

    #[test]
    fn duplicating_puts_an_unlabelled_copy_a_metre_east_and_selects_it() {
        let mut f = Fixture::default().with_camera();
        f.doc.cameras[0].label = "Porch".to_owned();
        f.doc.cameras[0].yaw_deg = 30.0;
        assert_eq!(act(&mut f, 1, CameraAction::Duplicate), (false, 1));
        let [a, b] = f.doc.cameras.as_slice() else {
            panic!("two cameras")
        };
        assert_eq!((b.id, b.label.as_str()), (2, "Camera 2"));
        assert_eq!(b.position, [1.0, 3.0, 0.0]);
        assert_eq!((b.yaw_deg, b.model_id.as_str()), (30.0, "m"));
        assert_eq!(a.label, "Porch");
        assert_eq!(f.editor.selected, Some(2));
    }

    #[test]
    fn toggling_includes_or_leaves_out_without_selecting() {
        let mut f = Fixture::default().with_camera();
        f.editor.selected = None;
        assert_eq!(act(&mut f, 1, CameraAction::ToggleEnabled), (false, 1));
        assert!(!f.doc.cameras[0].enabled);
        act(&mut f, 1, CameraAction::ToggleEnabled);
        assert!(f.doc.cameras[0].enabled);
        assert_eq!(f.editor.selected, None);
    }

    #[test]
    fn deleting_the_selected_camera_from_the_list_goes_back_to_select() {
        let mut f = Fixture::default().with_camera().with_camera();
        f.editor.selected = Some(1);
        f.editor.tool = Tool::Aim;
        // Another camera: the selection and the tool stay.
        assert_eq!(act(&mut f, 2, CameraAction::Delete), (false, 1));
        assert_eq!((f.editor.selected, f.editor.tool), (Some(1), Tool::Aim));
        act(&mut f, 1, CameraAction::Delete);
        assert!(f.doc.cameras.is_empty());
        assert_eq!((f.editor.selected, f.editor.tool), (None, Tool::Select));
        // Tools that need no selection stay on.
        let mut f = Fixture::default().with_camera();
        f.editor.tool = Tool::Region;
        act(&mut f, 1, CameraAction::Delete);
        assert_eq!((f.editor.selected, f.editor.tool), (None, Tool::Region));
    }

    fn draft() -> CameraModel {
        CameraModel {
            id: String::new(),
            vendor: String::new(),
            name: "My camera".to_owned(),
            kind: CameraKind::Bullet,
            hfov_deg: 110.0,
            vfov_deg: 60.0,
            resolution: [2560, 1440],
            ir_range_m: Some(10.0),
            source: None,
            notes: None,
        }
    }

    /// Runs a custom-model command on counted copies of the document and
    /// `models`. Returns its result and the (document, models) write
    /// accesses.
    fn with_models<R>(
        f: &mut Fixture,
        models: &mut Models,
        run: impl FnOnce(&mut EditCtx, &mut dyn Tracked<Models>) -> R,
    ) -> (R, (usize, usize)) {
        let mut doc = Counting::new(std::mem::take(&mut f.doc));
        let mut counted = Counting::new(std::mem::replace(models, super::tests::models()));
        let mut ctx = EditCtx {
            doc: &mut doc,
            editor: &mut f.editor,
            panels: &mut f.panels,
            toasts: &mut f.toasts,
        };
        let out = run(&mut ctx, &mut counted);
        f.doc = doc.value;
        *models = counted.value;
        (out, (doc.writes, counted.writes))
    }

    #[test]
    fn adding_a_model_gives_it_a_fresh_id_and_places_it_next() {
        let mut f = Fixture::default();
        let mut m = models();
        let (added, writes) =
            with_models(&mut f, &mut m, |ctx, m| add_custom_model(ctx, m, draft()));
        let id = format!("{CUSTOM_ID_PREFIX}my-camera-1");
        assert_eq!(added.map(|m| m.id), Some(id.clone()));
        assert_eq!(writes, (1, 1));
        assert_eq!(m.custom.len(), 1);
        assert_eq!(m.custom[0].id, id);
        assert_eq!(f.editor.place_model, id);
        assert_eq!(
            f.toasts(),
            [(
                ToastKind::Info,
                "Added “My camera” — it is now the model for new cameras"
            )]
        );
        // The same name again gets the next free id.
        let (added, _) = with_models(&mut f, &mut m, |ctx, m| add_custom_model(ctx, m, draft()));
        assert_eq!(
            added.map(|m| m.id),
            Some(format!("{CUSTOM_ID_PREFIX}my-camera-2"))
        );
    }

    #[test]
    fn adding_refuses_a_bad_model_and_one_past_the_limit() {
        let mut f = Fixture::default();
        let mut m = models();
        let bad = CameraModel {
            hfov_deg: 0.5,
            ..draft()
        };
        let (added, writes) = with_models(&mut f, &mut m, |ctx, m| add_custom_model(ctx, m, bad));
        assert_eq!((added, writes), (None, (0, 0)));
        assert_eq!(
            f.toasts(),
            [(
                ToastKind::Error,
                "Cannot add this model: horizontal field of view is 0.5°, but must be 1° to 360°"
            )]
        );

        let mut f = Fixture::default();
        for n in 0..MAX_CUSTOM_MODELS {
            m.custom.push(CameraModel {
                id: format!("{CUSTOM_ID_PREFIX}m-{n}"),
                ..draft()
            });
        }
        let (added, writes) =
            with_models(&mut f, &mut m, |ctx, m| add_custom_model(ctx, m, draft()));
        assert_eq!((added, writes), (None, (0, 0)));
        assert_eq!(m.custom.len(), MAX_CUSTOM_MODELS);
        assert_eq!(f.editor.place_model, Editor::default().place_model);
        assert_eq!(
            f.toasts(),
            [(
                ToastKind::Error,
                format!(
                    "Cannot add this model: a scene holds at most {MAX_CUSTOM_MODELS} custom models. Delete one you no longer use first"
                )
                .as_str()
            )]
        );
    }

    #[test]
    fn saving_updates_the_model_in_place() {
        let mut f = Fixture::default();
        let mut m = models();
        let id = format!("{CUSTOM_ID_PREFIX}my-camera-1");
        m.custom.push(CameraModel {
            id: id.clone(),
            ..draft()
        });
        let edited = CameraModel {
            id: id.clone(),
            vendor: "Acme".to_owned(),
            hfov_deg: 90.0,
            ..draft()
        };
        let ((), writes) = with_models(&mut f, &mut m, |ctx, m| save_custom_model(ctx, m, edited));
        assert_eq!(writes, (1, 1));
        assert_eq!(
            (m.custom[0].id.as_str(), m.custom[0].hfov_deg),
            (id.as_str(), 90.0)
        );
        assert_eq!(f.toasts(), [(ToastKind::Info, "Saved “Acme My camera”")]);

        // Invalid: refused before any write.
        let mut f = Fixture::default();
        let bad = CameraModel {
            id: id.clone(),
            resolution: [8, 8],
            ..draft()
        };
        let ((), writes) = with_models(&mut f, &mut m, |ctx, m| save_custom_model(ctx, m, bad));
        assert_eq!(writes, (0, 0));
        assert_eq!(m.custom[0].hfov_deg, 90.0);
        assert_eq!(
            f.toasts(),
            [(
                ToastKind::Error,
                "Cannot save this model: resolution 8 × 8 is too small; each side needs at least 16 px"
            )]
        );

        // A model that is gone: the models are written to, nothing else.
        let mut f = Fixture::default();
        let gone = CameraModel {
            id: format!("{CUSTOM_ID_PREFIX}gone-1"),
            ..draft()
        };
        let ((), writes) = with_models(&mut f, &mut m, |ctx, m| save_custom_model(ctx, m, gone));
        assert_eq!(writes, (0, 1));
        assert!(f.toasts.is_empty());
    }

    #[test]
    fn deleting_a_model_refuses_while_cameras_use_it() {
        let mut f = Fixture::default().with_camera().with_camera();
        let mut m = models();
        m.custom.push(CameraModel {
            id: "m".to_owned(),
            ..draft()
        });
        let ((), writes) = with_models(&mut f, &mut m, |ctx, m| delete_custom_model(ctx, m, "m"));
        assert_eq!((writes, m.custom.len()), ((0, 0), 1));
        f.doc.remove_camera(2);
        with_models(&mut f, &mut m, |ctx, m| delete_custom_model(ctx, m, "m"));
        assert_eq!(
            f.toasts(),
            [
                (
                    ToastKind::Error,
                    "Cannot delete “My camera”: 2 cameras use it. Give them another model or delete them first"
                ),
                (
                    ToastKind::Error,
                    "Cannot delete “My camera”: 1 camera uses it. Give them another model or delete them first"
                ),
            ]
        );
        f.doc.remove_camera(1);
        let ((), writes) = with_models(&mut f, &mut m, |ctx, m| delete_custom_model(ctx, m, "m"));
        assert_eq!((writes, m.custom.len()), ((1, 1), 0));
        // Unknown: nothing.
        let ((), writes) = with_models(&mut f, &mut m, |ctx, m| delete_custom_model(ctx, m, "m"));
        assert_eq!(writes, (0, 0));
    }

    #[test]
    fn keys_write_nothing_without_a_selection_or_outside_the_region_tool() {
        let mut f = Fixture::default().with_camera();
        f.editor.selected = None;
        f.editor.region_draft = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        let mut doc = Counting::new(std::mem::take(&mut f.doc));
        for tool in [Tool::Select, Tool::Aim, Tool::Move] {
            f.editor.tool = tool;
            let mut ctx = EditCtx {
                doc: &mut doc,
                editor: &mut f.editor,
                panels: &mut f.panels,
                toasts: &mut f.toasts,
            };
            super::keys(&mut ctx, &|k| [Key::Erase, Key::Finish].contains(&k), false);
        }
        assert_eq!(doc.writes, 0);
        assert_eq!(doc.value.cameras.len(), 1);
        assert_eq!(f.editor.region_draft.len(), 3);
        assert!(doc.value.region.is_none() && f.toasts.is_empty());
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

    #[test]
    fn tool_and_panel_keys_do_nothing_while_a_modifier_is_held() {
        let mut f = Fixture::default().with_camera();
        let all = [Key::Tool(Tool::Region), Key::Help, Key::Overlay, Key::Focus];
        let focus = super::keys(&mut f.ctx(), &|k| all.contains(&k), true);
        assert!(!focus);
        assert_eq!(f.editor.tool, Tool::Select);
        assert!(!f.panels.help_open);
        assert_eq!(f.panels.overlay, OverlayMode::Overlap);
        for tool in Tool::ALL {
            super::keys(&mut f.ctx(), &|k| k == Key::Tool(tool), true);
            assert_eq!(f.editor.tool, Tool::Select, "{tool:?}");
        }
        // Without the modifier they work as before.
        assert!(super::keys(&mut f.ctx(), &|k| all.contains(&k), false));
        assert_eq!(f.editor.tool, Tool::Region);
    }

    #[test]
    fn esc_enter_and_delete_work_with_a_modifier_held() {
        let mut f = Fixture::default().with_camera();
        f.editor.tool = Tool::Region;
        f.editor.region_draft = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        super::keys(&mut f.ctx(), &|k| k == Key::Finish, true);
        assert!(f.doc.region.is_some());
        super::keys(&mut f.ctx(), &|k| k == Key::Erase, true);
        assert!(f.doc.cameras.is_empty());
        f.panels.help_open = true;
        super::keys(&mut f.ctx(), &|k| k == Key::Escape, true);
        assert!(!f.panels.help_open);
    }

    #[test]
    fn only_the_tool_and_panel_keys_yield_to_modifiers() {
        let yielding: Vec<Key> = Tool::ALL
            .into_iter()
            .map(Key::Tool)
            .chain([
                Key::Escape,
                Key::Finish,
                Key::Erase,
                Key::Help,
                Key::Focus,
                Key::Overlay,
            ])
            .filter(|k| k.yields_to_modifiers())
            .collect();
        let mut expected: Vec<Key> = Tool::ALL.into_iter().map(Key::Tool).collect();
        expected.extend([Key::Help, Key::Focus, Key::Overlay]);
        assert_eq!(yielding, expected);
    }

    /// A document behind a write counter, with one camera and an enabled
    /// region, at revision `r` once set up.
    fn counted() -> (Counting<Document>, u64) {
        let mut f = Fixture::default().with_camera();
        f.doc.region = Some(Region {
            points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
            enabled: true,
        });
        let r = f.doc.revision;
        (Counting::new(f.doc), r)
    }

    #[test]
    fn set_params_writes_only_new_parameters_and_starts_a_revision() {
        let (mut doc, r) = counted();
        let same = doc.value.params;
        set_params(&mut doc, same);
        assert_eq!(
            (doc.writes, doc.value.revision),
            (0, r),
            "unchanged: no write"
        );

        let mut params = same;
        params.dori = DoriLevel::Identify;
        params.subject_height_m = 1.6;
        set_params(&mut doc, params);
        assert_eq!(doc.value.params, params);
        assert_eq!((doc.writes, doc.value.revision), (1, r + 1));
    }

    #[test]
    fn clear_region_removes_it_once() {
        let (mut doc, r) = counted();
        clear_region(&mut doc);
        assert_eq!(doc.value.region, None);
        assert_eq!((doc.writes, doc.value.revision), (1, r + 1));

        clear_region(&mut doc);
        assert_eq!((doc.writes, doc.value.revision), (1, r + 1), "nothing left");
    }

    #[test]
    fn set_region_enabled_turns_the_limit_off_and_on() {
        let (mut doc, r) = counted();
        set_region_enabled(&mut doc, false);
        assert_eq!(doc.value.region.as_ref().map(|g| g.enabled), Some(false));
        assert_eq!((doc.writes, doc.value.revision), (1, r + 1));
        set_region_enabled(&mut doc, true);
        assert_eq!(doc.value.region.as_ref().map(|g| g.enabled), Some(true));
        assert_eq!((doc.writes, doc.value.revision), (2, r + 2));

        doc.value.region = None;
        set_region_enabled(&mut doc, true);
        assert_eq!(doc.value.region, None, "no region to enable");
        assert_eq!((doc.writes, doc.value.revision), (2, r + 2));
    }

    #[test]
    fn update_camera_writes_back_only_an_edited_copy() {
        let (mut doc, r) = counted();
        let id = doc.value.cameras[0].id;
        let copy = doc.value.cameras[0].clone();
        update_camera(&mut doc, id, copy.clone());
        assert_eq!(
            (doc.writes, doc.value.revision),
            (0, r),
            "unchanged: no write"
        );

        let edited = CameraPlacement {
            label: "Porch".to_owned(),
            yaw_deg: 90.0,
            ..copy
        };
        update_camera(&mut doc, id, edited.clone());
        assert_eq!(doc.value.camera(id), Some(&edited));
        assert_eq!((doc.writes, doc.value.revision), (1, r + 1));

        update_camera(&mut doc, id + 1, edited);
        assert_eq!(
            (doc.writes, doc.value.revision),
            (1, r + 1),
            "no such camera"
        );
    }
}
