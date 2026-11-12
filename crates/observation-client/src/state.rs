//! App-wide resources and messages.

use bevy::prelude::*;
use observation_core::bundle::AreaBundle;
use observation_core::camera::Catalog;
use observation_core::coverage::{GroundGrid, StaticLayers};
use observation_core::geo::BBox;
use observation_core::provenance::DataChange;
use observation_core::scene::AreaRef;
use observation_core::terrain::{FLAT, Terrain};

use crate::platform::SaveFailure;
pub use observation_client_model::document::Snapshot;
use observation_client_model::document::{self, Document};
use observation_client_model::editor;
pub use observation_client_model::editor::Tool;
use observation_client_model::toasts;
pub use observation_client_model::toasts::{ToastAction, ToastKind};

/// Ground raster resolution, metres.
pub const GROUND_CELL_M: f32 = 0.5;
/// Façade patch size, metres.
pub const FACADE_CELL_M: f32 = 1.0;
/// Cameras are mounted this far in front of the surface they are placed on.
pub const MOUNT_OFFSET_M: f32 = 0.15;

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
    /// What the Address tool shows by the pointer: `None` off the scene,
    /// `Some(None)` where there is no address.
    pub hover_address: Option<Option<String>>,
    /// Scroll the camera list to the picked address's group once (consumed by
    /// the inspector).
    pub jump_to_address: bool,
    pub inspector_tab: InspectorTab,
    /// The selection the inspector last saw, so it can open the Camera tab
    /// when the selection changes some other way than [`select`].
    pub inspector_seen: Option<u32>,
    /// A restored or imported scene's area came back with different map
    /// data: shown in the Coverage card until dismissed or another area
    /// loads.
    pub data_change: Option<DataChange>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
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
            hover_address: None,
            jump_to_address: false,
            inspector_tab: InspectorTab::Cameras,
            inspector_seen: None,
            data_change: None,
        }
    }
}

/// The toasts on screen, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Toasts(toasts::Toasts);

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

/// Selects a camera and shows its settings in the inspector, even when it
/// was already selected.
pub fn select(editor: &mut Editor, ui: &mut UiState, id: u32) {
    editor.selected = Some(id);
    ui.inspector_tab = InspectorTab::Camera;
}

/// The editing state (tool, selection, Place model, region draft, picked
/// address), as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Editor(editor::Editor);

impl UiState {
    /// The camera to highlight: hovered in the list or in the 3D view.
    pub fn hovered(&self) -> Option<u32> {
        self.hover_list.or(self.hover_scene)
    }
}

/// Curated + user-defined camera models, as a resource: its change detection
/// keeps the Place tool's model valid and drives autosave.
#[derive(Debug, Resource, Deref, DerefMut)]
pub struct Models(document::Models);

impl Models {
    /// The shipped catalogue, and no custom models yet.
    pub fn new(catalog: Catalog) -> Self {
        Self(document::Models::new(catalog))
    }
}

/// Keeps the Place tool's model valid whenever the custom models change.
pub fn forget_missing_place_model(models: Res<Models>, mut editor: ResMut<Editor>) {
    editor.forget_missing_place_model(&models);
}

/// The user's document, as a resource: its change detection (and
/// [`Document::revision`]) drives sync, coverage and autosave.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Scene(Document);

/// The currently loaded area and its analysis layers.
#[derive(Resource)]
pub struct Area {
    pub area: AreaRef,
    pub bundle: AreaBundle,
    pub layers: StaticLayers,
}

/// Flat ground, for before an area has loaded.
static FLAT_GROUND: Terrain = FLAT;

/// The loaded area's ground, or flat ground before one has loaded.
pub fn terrain_of(area: Option<&Area>) -> &Terrain {
    area.map_or(&FLAT_GROUND, |a| &a.bundle.terrain)
}

impl Area {
    /// The loaded area, world `[x, z]`: see [`grid_bounds`].
    pub fn bounds(&self) -> Rect {
        grid_bounds(&self.layers.grid)
    }
}

/// The ground a coverage grid spans, world `[x, z]`. Coverage is computed
/// only here, so region corners are kept inside it, and the Region tool marks
/// its edge.
pub fn grid_bounds(grid: &GroundGrid) -> Rect {
    Rect::from_corners(grid.min, grid.max())
}

/// Request to (re)load an area, as a message.
#[derive(Debug, Clone, Message, Deref)]
pub struct LoadArea(document::LoadArea);

impl From<document::LoadArea> for LoadArea {
    fn from(request: document::LoadArea) -> Self {
        Self(request)
    }
}

impl LoadArea {
    pub fn new(area: AreaRef, keep_cameras: bool) -> Self {
        Self(document::LoadArea { area, keep_cameras })
    }

    pub fn demo() -> Self {
        Self(document::LoadArea::demo())
    }

    pub fn osm(bbox: BBox) -> Self {
        Self(document::LoadArea::osm(bbox))
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
