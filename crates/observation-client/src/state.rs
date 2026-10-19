//! App-wide resources and messages.

use bevy::prelude::*;
use observation_core::bundle::AreaBundle;
use observation_core::camera::{CameraKind, CameraModel, CameraPlacement, Catalog};
use observation_core::coverage::{CoverageParams, StaticLayers};
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
}

impl Tool {
    pub const ALL: [Self; 4] = [Self::Select, Self::Place, Self::Aim, Self::Move];

    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Place => "Place",
            Self::Aim => "Aim",
            Self::Move => "Move",
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Select => "V",
            Self::Place => "P",
            Self::Aim => "A",
            Self::Move => "M",
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
        }
    }

    pub fn needs_selection(self) -> bool {
        matches!(self, Self::Aim | Self::Move)
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
    pub custom_model_open: bool,
    pub toasts: Vec<Toast>,
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
            custom_model_open: false,
            toasts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    /// Seconds remaining.
    pub ttl: f32,
}

impl UiState {
    pub fn info(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast {
            kind: ToastKind::Info,
            text: text.into(),
            ttl: 4.0,
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast {
            kind: ToastKind::Error,
            text: text.into(),
            ttl: 8.0,
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
        self.custom
            .iter()
            .find(|m| m.id == id)
            .or_else(|| self.catalog.get(id))
    }

    pub fn kind_of(&self, id: &str) -> CameraKind {
        self.get(id).map(|m| m.kind).unwrap_or_default()
    }
}

/// The user's document: what gets autosaved and exported.
#[derive(Debug, Resource)]
pub struct Scene {
    pub area: Option<AreaRef>,
    pub origin: LatLon,
    pub cameras: Vec<CameraPlacement>,
    pub params: CoverageParams,
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
        let used: Vec<u8> = self.cameras.iter().map(|c| c.color_index).collect();
        c.color_index = (0u8..8)
            .find(|i| !used.contains(i))
            .unwrap_or((c.id % 8) as u8);
        let id = c.id;
        self.cameras.push(c);
        self.touch();
        id
    }

    pub fn remove_camera(&mut self, id: u32) {
        self.cameras.retain(|c| c.id != id);
        self.touch();
    }

    pub fn to_file(&self, models: &Models) -> Option<SceneFile> {
        let area = self.area?;
        let mut file = SceneFile::new(area, self.origin);
        file.cameras = self.cameras.clone();
        file.params = self.params;
        file.custom_models = models.custom.clone();
        Some(file)
    }

    /// Replaces the document with a loaded file (custom models go to `models`).
    pub fn load_file(&mut self, file: SceneFile, models: &mut Models) {
        self.area = Some(file.area);
        self.origin = file.origin;
        self.next_id = file.cameras.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        self.cameras = file.cameras;
        self.params = file.params;
        models.custom = file.custom_models;
        self.touch();
    }
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
}
