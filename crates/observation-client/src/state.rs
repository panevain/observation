//! App-wide resources and messages.

use bevy::prelude::*;
use observation_core::bundle::AreaBundle;
use observation_core::camera::{
    CameraKind, CameraModel, CameraPlacement, Catalog, fresh_custom_id,
};
use observation_core::coverage::{CoverageParams, Region, StaticLayers};
use observation_core::geo::{BBox, LatLon};
use observation_core::scene::{AreaRef, SceneFile};

/// Ground raster resolution, metres.
pub const GROUND_CELL_M: f32 = 0.5;
/// Façade patch size, metres.
pub const FACADE_CELL_M: f32 = 1.0;
/// Cameras are mounted this far in front of the surface they are placed on.
pub const MOUNT_OFFSET_M: f32 = 0.15;

/// Editing tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Place,
    Aim,
    Move,
    /// Draw the coverage region.
    Region,
    /// Show the street address under the pointer; click to select it.
    Address,
}

impl Tool {
    pub const ALL: [Self; 6] = [
        Self::Select,
        Self::Place,
        Self::Aim,
        Self::Move,
        Self::Region,
        Self::Address,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Place => "Place",
            Self::Aim => "Aim",
            Self::Move => "Move",
            Self::Region => "Region",
            Self::Address => "Address",
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Select => "V",
            Self::Place => "P",
            Self::Aim => "A",
            Self::Move => "M",
            Self::Region => "R",
            Self::Address => "L",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Select => {
                "Click a camera to select it · drag to orbit · right-drag to pan · scroll to zoom"
            }
            Self::Place => {
                "Click a wall to mount a camera there, or the ground to place it on a pole"
            }
            Self::Aim => "Click or drag on the scene to point the selected camera",
            Self::Move => "Click or drag on a wall or the ground to move the selected camera",
            Self::Region => {
                "Click to add corners · click the first corner or press Enter to finish · Backspace undoes a corner"
            }
            Self::Address => {
                "Point at a building to see its address · click to select it and the buildings at it"
            }
        }
    }

    /// Short instruction for the mode pill.
    pub fn banner(self) -> &'static str {
        match self {
            Self::Select => "Click a camera to select it",
            Self::Place => "Click a wall or the ground to place a camera",
            Self::Aim => "Click where the selected camera should look",
            Self::Move => "Click a wall or the ground to move the selected camera",
            Self::Region => "Click corners of the area to measure, then the first corner",
            Self::Address => "Point at a building to see its address",
        }
    }

    /// Keys and gestures for the status bar: (key, what it does).
    pub fn keymap(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Select => &[
                ("Click", "select"),
                ("Drag", "orbit"),
                ("Right-drag", "pan"),
                ("Scroll", "zoom"),
                ("F", "focus"),
                ("Del", "delete"),
            ],
            Self::Place => &[
                ("Click wall", "mount"),
                ("Click ground", "pole"),
                ("Esc", "done"),
            ],
            Self::Aim => &[
                ("Click/drag", "point camera"),
                ("Right-drag", "pan"),
                ("Esc", "done"),
            ],
            Self::Move => &[
                ("Click/drag", "move camera"),
                ("Right-drag", "pan"),
                ("Esc", "done"),
            ],
            Self::Region => &[
                ("Click", "add corner"),
                ("Enter", "finish"),
                ("Backspace", "remove corner"),
                ("Esc", "cancel"),
            ],
            Self::Address => &[
                ("Click", "select address"),
                ("Drag", "orbit"),
                ("Right-drag", "pan"),
                ("Del", "clear"),
                ("Esc", "done"),
            ],
        }
    }

    pub fn needs_selection(self) -> bool {
        matches!(self, Self::Aim | Self::Move)
    }
}

/// Whether a click with the Address tool also scrolls the camera list to that
/// address (a setting, remembered in this browser).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Resource)]
pub struct AddressJump(pub bool);

impl Default for AddressJump {
    fn default() -> Self {
        Self(true)
    }
}

/// How the coverage overlay is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayMode {
    /// Heat ramp by number of cameras that see a spot.
    #[default]
    Overlap,
    /// Each camera's own colour.
    PerCamera,
}

/// The inspector's tabs below the coverage summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InspectorTab {
    /// Model picker, Place, the camera list and custom models.
    #[default]
    Cameras,
    /// The selected camera's settings.
    Camera,
    /// What the coverage figures count (detail level, lighting, subject
    /// height) and how the overlay colours the ground.
    Analysis,
}

/// Transient UI state.
#[derive(Debug, Resource)]
pub struct UiState {
    pub tool: Tool,
    pub selected: Option<u32>,
    /// Model used by the Place tool.
    pub place_model: String,
    pub overlay: OverlayMode,
    pub show_facades: bool,
    pub show_frustums: bool,
    pub map_open: bool,
    pub help_open: bool,
    /// The local map-data regions dialog.
    pub regions_open: bool,
    pub custom_model_open: bool,
    /// Camera under the pointer in the camera list.
    pub hover_list: Option<u32>,
    /// Camera under the pointer in the 3D view.
    pub hover_scene: Option<u32>,
    /// Frame the selected camera in the 3D view (consumed by the input systems).
    pub focus_request: bool,
    /// Undo or redo asked for by the UI or a shortcut (consumed by `history`).
    pub history_request: Option<crate::history::HistoryOp>,
    /// Whether the last autosave reached browser storage.
    pub save: SaveState,
    /// Wall-clock seconds since start-up, for "saved just now" and the
    /// loading timer.
    pub now: f64,
    pub toasts: Vec<Toast>,
    /// Corners placed so far with the Region tool, world `[x, z]`.
    pub region_draft: Vec<[f32; 2]>,
    /// Ground point under the pointer while drawing a region.
    pub region_hover: Option<[f32; 2]>,
    /// How close to the first corner a click closes the region, metres.
    pub region_snap_m: f32,
    /// What the Address tool shows by the pointer: `None` off the scene,
    /// `Some(None)` where there is no address.
    pub hover_address: Option<Option<String>>,
    /// The address picked with the Address tool; its buildings are
    /// highlighted.
    pub selected_address: Option<observation_core::address::Address>,
    /// Scroll the camera list to the picked address's group once (consumed by
    /// the inspector).
    pub jump_to_address: bool,
    pub inspector_tab: InspectorTab,
    /// The selection the inspector last saw, so it can open the Camera tab
    /// when the selection changes some other way than [`UiState::select`].
    pub inspector_seen: Option<u32>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            tool: Tool::Select,
            selected: None,
            place_model: "generic-doorbell-160".to_owned(),
            overlay: OverlayMode::Overlap,
            show_facades: true,
            show_frustums: true,
            map_open: false,
            help_open: false,
            regions_open: false,
            custom_model_open: false,
            hover_list: None,
            hover_scene: None,
            focus_request: false,
            history_request: None,
            save: SaveState::Pending,
            now: 0.0,
            toasts: Vec::new(),
            region_draft: Vec::new(),
            region_hover: None,
            region_snap_m: 1.0,
            hover_address: None,
            selected_address: None,
            jump_to_address: false,
            inspector_tab: InspectorTab::Cameras,
            inspector_seen: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

/// A button on a toast.
#[derive(Debug, Clone)]
pub enum ToastAction {
    Undo,
    /// Send this area request again.
    Retry(LoadArea),
}

impl ToastAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Retry(_) => "Retry",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub action: Option<ToastAction>,
    /// Seconds remaining; errors stay until dismissed.
    pub ttl: f32,
}

/// Outcome of the autosave, shown in the status bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SaveState {
    /// Nothing written yet this session.
    Pending,
    /// Written at this many seconds since start-up.
    Saved(f64),
    /// The browser refused the write.
    Failed(SaveFailure),
}

/// Why the browser refused the autosave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFailure {
    /// The site's storage quota is used up (or is zero, as in some private modes).
    Full,
    /// The browser denies this page access to storage (site data or cookies blocked).
    Blocked,
    /// The browser offers no storage to this page at all.
    Unavailable,
}

impl SaveFailure {
    /// What went wrong, for the status bar's tooltip.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Full => "This browser's storage for the site is full.",
            Self::Blocked => {
                "This browser blocks the site from storing data (check its site data or cookie settings)."
            }
            Self::Unavailable => {
                "This browser offers no storage to the site (it may be in private mode)."
            }
        }
    }
}

impl UiState {
    /// Selects a camera and shows its settings in the inspector, even when
    /// it was already selected.
    pub fn select(&mut self, id: u32) {
        self.selected = Some(id);
        self.inspector_tab = InspectorTab::Camera;
    }

    /// The camera to highlight: hovered in the list or in the 3D view.
    pub fn hovered(&self) -> Option<u32> {
        self.hover_list.or(self.hover_scene)
    }

    /// Goes back to the default model for the Place tool if its model is
    /// gone (deleted, undone, or left out of a restored or imported scene).
    pub fn forget_missing_place_model(&mut self, models: &Models) {
        if models.get(&self.place_model).is_none() {
            self.place_model = Self::default().place_model;
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push_toast(ToastKind::Info, text.into(), None, 4.0);
    }

    /// An info toast with a button; it stays a little longer to be reached.
    pub fn info_with(&mut self, text: impl Into<String>, action: ToastAction) {
        self.push_toast(ToastKind::Info, text.into(), Some(action), 8.0);
    }

    /// A passing problem with what the user just did (a refused click, an
    /// invalid form): shown for a while, then gone.
    pub fn error(&mut self, text: impl Into<String>) {
        self.push_toast(ToastKind::Error, text.into(), None, 8.0);
    }

    /// A failure that loses or blocks work (a load, an import, a restore):
    /// stays until dismissed, so it is never missed.
    pub fn error_sticky(&mut self, text: impl Into<String>) {
        self.push_toast(ToastKind::Error, text.into(), None, f32::INFINITY);
    }

    /// A sticky error with a button (Retry).
    pub fn error_with(&mut self, text: impl Into<String>, action: ToastAction) {
        self.push_toast(ToastKind::Error, text.into(), Some(action), f32::INFINITY);
    }

    fn push_toast(&mut self, kind: ToastKind, text: String, action: Option<ToastAction>, ttl: f32) {
        // A newer Undo supersedes an older one: only the latest step is one
        // click away.
        if matches!(action, Some(ToastAction::Undo)) {
            self.toasts
                .retain(|t| !matches!(t.action, Some(ToastAction::Undo)));
        }
        // The same message again replaces the old one instead of piling up.
        self.toasts.retain(|t| t.kind != kind || t.text != text);
        self.toasts.push(Toast {
            kind,
            text,
            action,
            ttl,
        });
    }
}

/// Curated + user-defined camera models.
#[derive(Debug, Resource)]
pub struct Models {
    pub catalog: Catalog,
    pub custom: Vec<CameraModel>,
}

impl Models {
    pub fn get(&self, id: &str) -> Option<&CameraModel> {
        self.custom_model(id).or_else(|| self.catalog.get(id))
    }

    pub fn kind_of(&self, id: &str) -> CameraKind {
        self.get(id).map(|m| m.kind).unwrap_or_default()
    }

    /// A user-defined model (catalogue models cannot be edited).
    pub fn custom_model(&self, id: &str) -> Option<&CameraModel> {
        self.custom.iter().find(|m| m.id == id)
    }

    /// Replaces the custom model with `model`'s id; false if there is none.
    pub fn update_custom(&mut self, model: CameraModel) -> bool {
        match self.custom.iter_mut().find(|m| m.id == model.id) {
            Some(slot) => {
                *slot = model;
                true
            }
            None => false,
        }
    }

    /// Removes a custom model; false if there is none with this id.
    pub fn remove_custom(&mut self, id: &str) -> bool {
        let before = self.custom.len();
        self.custom.retain(|m| m.id != id);
        self.custom.len() != before
    }

    /// An id for a new custom model named `name` that no custom model has
    /// (models can be deleted, so the count alone can repeat one). Catalogue
    /// ids never start with
    /// [`observation_core::camera::CUSTOM_ID_PREFIX`], so they cannot clash.
    pub fn new_custom_id(&self, name: &str) -> String {
        fresh_custom_id(name, self.custom.len() + 1, |id| {
            self.custom_model(id).is_some()
        })
    }
}

/// Keeps the Place tool's model valid whenever the custom models change.
pub fn forget_missing_place_model(models: Res<Models>, mut ui: ResMut<UiState>) {
    ui.forget_missing_place_model(&models);
}

/// The user's document: what gets autosaved and exported.
#[derive(Debug, Resource)]
pub struct Scene {
    pub area: Option<AreaRef>,
    pub origin: LatLon,
    pub cameras: Vec<CameraPlacement>,
    pub params: CoverageParams,
    /// Area the coverage figures can be limited to.
    pub region: Option<Region>,
    next_id: u32,
    /// Bumped on every change; drives sync, coverage and autosave.
    pub revision: u64,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            area: None,
            origin: LatLon::new(0.0, 0.0),
            cameras: Vec::new(),
            params: CoverageParams::default(),
            region: None,
            next_id: 1,
            revision: 0,
        }
    }
}

impl Scene {
    pub fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn camera(&self, id: u32) -> Option<&CameraPlacement> {
        self.cameras.iter().find(|c| c.id == id)
    }

    pub fn camera_mut(&mut self, id: u32) -> Option<&mut CameraPlacement> {
        self.touch();
        self.cameras.iter_mut().find(|c| c.id == id)
    }

    pub fn add_camera(&mut self, mut c: CameraPlacement) -> u32 {
        c.id = self.next_id;
        self.next_id += 1;
        if c.label.is_empty() {
            c.label = format!("Camera {}", c.id);
        }
        let id = c.id;
        self.cameras.push(c);
        self.touch();
        id
    }

    pub fn remove_camera(&mut self, id: u32) {
        self.cameras.retain(|c| c.id != id);
        self.touch();
    }

    /// How many placed cameras use model `model_id`.
    pub fn cameras_using(&self, model_id: &str) -> usize {
        self.cameras
            .iter()
            .filter(|c| c.model_id == model_id)
            .count()
    }

    pub fn to_file(&self, models: &Models) -> Option<SceneFile> {
        let area = self.area?;
        let mut file = SceneFile::new(area, self.origin);
        file.cameras = self.cameras.clone();
        file.params = self.params;
        file.region = self.region.clone();
        file.custom_models = models.custom.clone();
        Some(file)
    }

    /// The undoable part of the document.
    pub fn snapshot(&self, models: &Models) -> Snapshot {
        Snapshot {
            area: self.area,
            origin: self.origin,
            cameras: self.cameras.clone(),
            params: self.params,
            region: self.region.clone(),
            custom_models: models.custom.clone(),
        }
    }

    /// Puts back a [`Self::snapshot`].
    pub fn restore(&mut self, s: &Snapshot, models: &mut Models) {
        self.area = s.area;
        self.origin = s.origin;
        self.cameras = s.cameras.clone();
        self.params = s.params;
        self.region = s.region.clone();
        // Never hand out an id the restored cameras already use (an import
        // resets `next_id`, so undoing past one can bring back higher ids).
        let max = self.cameras.iter().map(|c| c.id).max().unwrap_or(0);
        self.next_id = self.next_id.max(max + 1);
        models.custom = s.custom_models.clone();
        self.touch();
    }

    /// Replaces the document with a loaded file (custom models go to `models`).
    pub fn load_file(&mut self, file: SceneFile, models: &mut Models) {
        self.area = Some(file.area);
        self.origin = file.origin;
        self.next_id = file.cameras.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        self.cameras = file.cameras;
        self.params = file.params;
        self.region = file.region;
        models.custom = file.custom_models;
        self.touch();
    }
}

/// One undo step: everything [`Scene`] saves, plus the custom models.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub area: Option<AreaRef>,
    pub origin: LatLon,
    pub cameras: Vec<CameraPlacement>,
    pub params: CoverageParams,
    pub region: Option<Region>,
    pub custom_models: Vec<CameraModel>,
}

/// The currently loaded area and its analysis layers.
#[derive(Resource)]
pub struct Area {
    pub area: AreaRef,
    pub bundle: AreaBundle,
    pub layers: StaticLayers,
}

/// Request to (re)load an area.
#[derive(Debug, Clone, Message)]
pub struct LoadArea {
    pub area: AreaRef,
    /// Keep the current cameras (used when restoring a saved scene).
    pub keep_cameras: bool,
}

impl LoadArea {
    pub fn demo() -> Self {
        Self {
            area: AreaRef::Demo,
            keep_cameras: false,
        }
    }

    pub fn osm(bbox: BBox) -> Self {
        Self {
            area: AreaRef::Osm { bbox },
            keep_cameras: false,
        }
    }
}

/// Bundle fetch finished.
#[derive(Message)]
pub struct AreaLoaded {
    pub request: LoadArea,
    pub result: Result<AreaBundle, String>,
}

/// Where the docked panels left room for the 3D view (logical px), written by
/// the UI every frame. The panels are drawn into egui's background layer, which
/// egui never reports as "pointer over egui", so this is the one place that
/// knows which pointer positions belong to the UI.
#[derive(Debug, Default, Resource)]
pub struct UiLayout {
    /// `None` until the first frame is laid out; empty while the small-screen gate is up.
    pub viewport: Option<Rect>,
}

impl UiLayout {
    /// egui's panel resize handles reach this far into the viewport.
    const EDGE_PX: f32 = 6.0;

    /// The pointer at `p` is over a panel (or a panel's resize handle).
    pub fn is_ui(&self, p: Vec2) -> bool {
        self.viewport
            .is_some_and(|r| !r.inflate(-Self::EDGE_PX).contains(p))
    }
}

/// Network/area loading progress shown in the UI.
#[derive(Debug, Default, Resource)]
pub struct Loading {
    pub area: Option<AreaRef>,
    /// The request in flight, for Retry.
    pub request: Option<LoadArea>,
    /// When it was sent, seconds since start-up.
    pub started: f64,
}
