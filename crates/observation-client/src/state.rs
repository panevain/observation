//! App-wide resources and messages.

use bevy::prelude::*;
use observation_core::bundle::AreaBundle;
use observation_core::camera::Catalog;
use observation_core::coverage::{GroundGrid, StaticLayers};
use observation_core::geo::BBox;
use observation_core::provenance::DataChange;
use observation_core::scene::AreaRef;
use observation_core::terrain::{FLAT, Terrain};

use observation_client_model::commands::Tracked;
use observation_client_model::document::{self, Document};
use observation_client_model::editor;
pub use observation_client_model::editor::{InspectorTab, OverlayMode, Tool, select};
use observation_client_model::net::AreaRequests;
use observation_client_model::toasts;
pub use observation_client_model::toasts::{ToastAction, ToastKind};

/// Ground raster resolution, metres.
pub const GROUND_CELL_M: f32 = 0.5;
/// Façade patch size, metres.
pub const FACADE_CELL_M: f32 = 1.0;
/// Whether a click with the Address tool also scrolls the camera list to that
/// address (a setting, remembered in this browser).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Resource)]
pub struct AddressJump(pub bool);

impl Default for AddressJump {
    fn default() -> Self {
        Self(true)
    }
}

/// A restored or imported scene's area came back with different map data:
/// shown in the Coverage card until dismissed or another area loads, and
/// dropped once undo or redo moves away from that step.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct DataChangeNotice(Option<DataChange>);

/// The toasts on screen, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Toasts(toasts::Toasts);

/// The editing state (tool, selection, Place model, region draft, picked
/// address), as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Editor(editor::Editor);

/// The panels, dialogs and overlay settings, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Panels(editor::Panels);

/// What the pointer is over, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Hover(editor::Hover);

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

/// The scene for an
/// [`EditCtx`](observation_client_model::commands::EditCtx), marked changed
/// only when a command writes to it.
#[derive(Debug)]
pub struct TrackScene<'a, 'w>(pub &'a mut ResMut<'w, Scene>);

impl Tracked<Document> for TrackScene<'_, '_> {
    fn get(&self) -> &Document {
        self.0
    }

    fn get_mut(&mut self) -> &mut Document {
        self.0
    }
}

/// The camera models for a custom-model command, marked changed only when
/// the command writes to them.
#[derive(Debug)]
pub struct TrackModels<'a, 'w>(pub &'a mut ResMut<'w, Models>);

impl Tracked<document::Models> for TrackModels<'_, '_> {
    fn get(&self) -> &document::Models {
        self.0
    }

    fn get_mut(&mut self) -> &mut document::Models {
        self.0
    }
}

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

/// The area request in flight, shown in the UI, as a resource.
#[derive(Debug, Default, Resource, Deref, DerefMut)]
pub struct Loading(AreaRequests);
