//! 3D environment: buildings, ground, map tiles, lights and coverage overlays.

use crate::coverage::CoverageState;
use crate::input::OrbitCamera;
use crate::net::Tiles;
use crate::state::{Area, AreaLoaded, FACADE_CELL_M, GROUND_CELL_M, Models, Scene, UiState};
use crate::theme::{Tokens, rgba, texel_to_linear, to_bevy};
use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use observation_core::address::Address;
use observation_core::camera::CameraModel;
use observation_core::coverage::StaticLayers;
use observation_core::example::example_cameras;
use observation_core::geo::{LatLon, Projection};
use observation_core::scene::AreaRef;
use observation_core::tiles::{TileId, tiles_covering, zoom_for_bbox};

/// Extra ground drawn around the area so it doesn't float in the void, metres.
const GROUND_MARGIN_M: f32 = 150.0;
/// Upper bound on ground tiles per area.
const MAX_GROUND_TILES: usize = 64;
/// Models used, in turn, for the example cameras on the demo street: a mix of
/// types and brands.
const EXAMPLE_MODELS: [&str; 10] = [
    "ring-wired-doorbell-pro",
    "ring-floodlight-cam-wired-plus",
    "google-nest-doorbell-battery",
    "reolink-rlc-810a",
    "arlo-pro-4",
    "eufy-eufycam-s3-pro",
    "ubiquiti-unifi-g5-dome",
    "wyze-cam-floodlight-pro",
    "tp-link-tapo-c320ws",
    "blink-outdoor-4",
];

/// Height of the coverage overlay above the map tiles, metres. Layers closer
/// than the depth buffer can resolve at range fight and flicker.
const OVERLAY_Y_M: f32 = 0.08;

/// Everything spawned for the current area (despawned on reload).
#[derive(Component)]
pub struct AreaEntity;

#[derive(Component)]
pub struct BuildingMesh;

#[derive(Component)]
pub struct GroundMesh;

#[derive(Component)]
struct GroundTile(TileId);

/// The tile's map image has been applied.
#[derive(Component)]
struct TileTextured;

#[derive(Component)]
pub struct FacadeOverlay;

/// Tint over the ground beyond the loaded area, where nothing is analysed.
/// Shown while the Region tool is on, with hatching and an edge drawn over it.
#[derive(Component)]
pub struct OutsideArea {
    /// The loaded area (the coverage grid), world `[x, z]`.
    pub inner: Rect,
    /// The drawn ground around it: the map tiles' extent.
    pub outer: Rect,
}

/// Opacity of the [`OutsideArea`] tint.
const OUTSIDE_TINT_ALPHA: f32 = 0.15;

/// The buildings at the address picked with the Address tool, drawn over the
/// building mesh.
#[derive(Component)]
struct AddressHighlight;

/// Handles of the coverage overlay assets.
#[derive(Resource)]
pub struct Overlay {
    pub ground_image: Handle<Image>,
    pub facade_mesh: Handle<Mesh>,
}

/// Materials of the current area that take a theme colour.
#[derive(Resource)]
struct WorldMaterials {
    building: Handle<StandardMaterial>,
    /// Ground tiles without their map image.
    plain: Handle<StandardMaterial>,
    /// The [`OutsideArea`] tint.
    outside: Handle<StandardMaterial>,
}

pub struct WorldPlugin;

/// Builds a newly loaded area and inserts [`Area`]; systems that need the new
/// area in the same frame run after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct AreaLoadSet;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_lights).add_systems(
            Update,
            (
                on_area_loaded.in_set(AreaLoadSet),
                apply_ground_tiles,
                follow_theme.run_if(resource_changed::<Tokens>),
                highlight_address,
            )
                .chain(),
        );
    }
}

fn setup_lights(mut commands: Commands, tokens: Res<Tokens>) {
    commands.insert_resource(ClearColor(to_bevy(tokens.world.sky)));
    commands.insert_resource(GlobalAmbientLight {
        brightness: 380.0,
        ..default()
    });
    commands.spawn((
        DirectionalLight {
            illuminance: 5_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        // Afternoon sun from the south-west.
        Transform::from_xyz(-60.0, 120.0, 80.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

/// Flat quad on the ground plane with explicit UVs: `(min.x, min.z)` → uv(0,0).
pub fn ground_quad(min: Vec2, max: Vec2, y: f32) -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [min.x, y, min.y],
            [max.x, y, min.y],
            [max.x, y, max.y],
            [min.x, y, max.y],
        ],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4])
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    )
    .with_inserted_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]))
}

/// Flat ring on the ground plane: `outer` with `inner` cut out.
fn ground_frame(inner: Rect, outer: Rect, y: f32) -> Mesh {
    let corners = |r: Rect| {
        [
            [r.min.x, y, r.min.y],
            [r.max.x, y, r.min.y],
            [r.max.x, y, r.max.y],
            [r.min.x, y, r.max.y],
        ]
    };
    // Outer corners 0-3, inner 4-7; one quad per side, wound like `ground_quad`.
    let mut idx = Vec::with_capacity(24);
    for i in 0..4 {
        let j = (i + 1) % 4;
        idx.extend([i, 4 + j, j, i, 4 + i, 4 + j]);
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        [corners(outer), corners(inner)].concat(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 8])
    .with_inserted_indices(Indices::U32(idx))
}

/// Asset stores an area load writes to.
#[derive(bevy::ecs::system::SystemParam)]
struct AreaAssets<'w> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    tiles: ResMut<'w, Tiles>,
}

fn on_area_loaded(
    mut loaded: MessageReader<AreaLoaded>,
    mut commands: Commands,
    assets: AreaAssets,
    mut scene: ResMut<Scene>,
    models: Res<Models>,
    mut ui: ResMut<UiState>,
    mut coverage: ResMut<CoverageState>,
    mut orbit: Query<&mut OrbitCamera>,
    old: Query<Entity, With<AreaEntity>>,
    tokens: Res<Tokens>,
) {
    let Some(msg) = loaded.read().last() else {
        return;
    };
    let Ok(bundle) = &msg.result else { return };
    let bundle = bundle.clone();
    let AreaAssets {
        mut meshes,
        mut materials,
        mut images,
        mut tiles,
    } = assets;

    for e in &old {
        commands.entity(e).despawn();
    }

    let layers = StaticLayers::new(&bundle, GROUND_CELL_M, FACADE_CELL_M);
    let proj = bundle.projection();
    let grid = layers.grid;

    // Buildings.
    let m = &bundle.mesh;
    let building_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.clone())
    .with_inserted_indices(Indices::U32(m.indices.clone()));
    let building = materials.add(StandardMaterial {
        base_color: to_bevy(tokens.world.building),
        perceptual_roughness: 0.9,
        ..default()
    });
    commands.spawn((
        AreaEntity,
        BuildingMesh,
        Mesh3d(meshes.add(building_mesh)),
        MeshMaterial3d(building.clone()),
    ));

    // Pick target for ground clicks. It is not drawn: the tiles below cover
    // it, and two coplanar surfaces would fight in the depth buffer.
    let (gmin, gmax) = (grid.min - GROUND_MARGIN_M, grid.max() + GROUND_MARGIN_M);
    commands.spawn((
        AreaEntity,
        GroundMesh,
        Mesh3d(meshes.add(ground_quad(gmin, gmax, 0.0))),
        Visibility::Hidden,
    ));

    // Map tiles draped on the ground: plain ground colour until the image
    // arrives (or for good, if it fails).
    let plain = materials.add(StandardMaterial {
        base_color: to_bevy(tokens.world.ground),
        perceptual_roughness: 1.0,
        ..default()
    });
    let tile_bbox = bbox_of(&proj, gmin, gmax);
    let z = zoom_for_bbox(&tile_bbox, MAX_GROUND_TILES, 19);
    let ground_tiles = tiles_covering(&tile_bbox, z);
    tiles.pin(ground_tiles.iter().copied());
    let inner = Rect::from_corners(grid.min, grid.max());
    let mut outer = Rect::from_corners(gmin, gmax);
    for t in ground_tiles {
        let b = t.bounds();
        let nw = world2(&proj, LatLon::new(b.north, b.west));
        let se = world2(&proj, LatLon::new(b.south, b.east));
        outer = outer.union(Rect::from_corners(nw, se));
        commands.spawn((
            AreaEntity,
            GroundTile(t),
            Mesh3d(meshes.add(ground_quad(nw, se, 0.0))),
            MeshMaterial3d(plain.clone()),
            NotShadowCaster,
            Pickable::IGNORE,
        ));
    }
    let outside = materials.add(StandardMaterial {
        base_color: to_bevy(tokens.world.outside_area).with_alpha(OUTSIDE_TINT_ALPHA),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        depth_bias: 10.0,
        ..default()
    });
    commands.spawn((
        AreaEntity,
        OutsideArea { inner, outer },
        Mesh3d(meshes.add(ground_frame(inner, outer, OVERLAY_Y_M))),
        MeshMaterial3d(outside.clone()),
        Visibility::Hidden,
        NotShadowCaster,
        Pickable::IGNORE,
    ));
    commands.insert_resource(WorldMaterials {
        building,
        plain,
        outside,
    });

    // Ground coverage overlay texture (one texel per ground cell).
    let ground_image = images.add(Image::new_fill(
        Extent3d {
            width: grid.w,
            height: grid.h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ));
    commands.spawn((
        AreaEntity,
        Mesh3d(meshes.add(ground_quad(grid.min, grid.max(), OVERLAY_Y_M))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color_texture: Some(ground_image.clone()),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            depth_bias: 10.0,
            ..default()
        })),
        NotShadowCaster,
        Pickable::IGNORE,
    ));

    // Façade overlay: one quad per façade cell, coloured per vertex.
    let mut pos = Vec::with_capacity(layers.facades.len() * 4);
    let mut nrm = Vec::with_capacity(layers.facades.len() * 4);
    let mut idx = Vec::with_capacity(layers.facades.len() * 6);
    for (i, f) in layers.facades.iter().enumerate() {
        let off = f.normal * 0.03;
        for c in f.corners {
            pos.push((c + off).to_array());
            nrm.push(f.normal.to_array());
        }
        let b = i as u32 * 4;
        idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    let n = pos.len();
    let facade_mesh = meshes.add(
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; n])
        .with_inserted_indices(Indices::U32(idx)),
    );
    commands.spawn((
        AreaEntity,
        FacadeOverlay,
        Mesh3d(facade_mesh.clone()),
        MeshMaterial3d(materials.add(StandardMaterial {
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            depth_bias: 10.0,
            ..default()
        })),
        NotShadowCaster,
        Pickable::IGNORE,
    ));
    commands.insert_resource(Overlay {
        ground_image,
        facade_mesh,
    });

    // An address picked in the old area means nothing in the new one.
    ui.selected_address = None;
    ui.hover_address = None;
    ui.jump_to_address = false;

    // Document state.
    let area = msg.request.area;
    if !msg.request.keep_cameras || scene.area != Some(area) {
        scene.cameras.clear();
        // A region belongs to the area it was drawn on.
        scene.region = None;
        ui.selected = None;
        if area == AreaRef::Demo {
            // A fresh demo street comes with example cameras to explore.
            let picks: Vec<&CameraModel> = EXAMPLE_MODELS
                .iter()
                .filter_map(|id| models.get(id))
                .collect();
            for c in example_cameras(&bundle, &picks, EXAMPLE_MODELS.len()) {
                scene.add_camera(c);
            }
        }
    }
    scene.area = Some(area);
    scene.origin = bundle.origin;
    scene.touch();
    *coverage = CoverageState::default();

    if let Ok(mut o) = orbit.single_mut() {
        let size = grid.max() - grid.min;
        o.focus = Vec3::ZERO;
        o.distance = size.max_element() * 0.9;
        o.pitch = -0.75;
        o.yaw = 0.35;
    }

    ui.info(format!(
        "Loaded {} buildings and {} street segments",
        bundle.buildings.len(),
        bundle.roads.len()
    ));
    commands.insert_resource(Area {
        area,
        bundle,
        layers,
    });
}

fn apply_ground_tiles(
    q: Query<(Entity, &GroundTile), Without<TileTextured>>,
    mut tiles: ResMut<Tiles>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for (e, tile) in &q {
        if let Some(img) = tiles.get(tile.0) {
            commands.entity(e).insert((
                TileTextured,
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color_texture: Some(img),
                    perceptual_roughness: 1.0,
                    ..default()
                })),
            ));
        }
    }
}

/// Recolours the sky, the buildings, the plain ground and the outside-area
/// tint in place after a theme change.
fn follow_theme(
    tokens: Res<Tokens>,
    mut clear: ResMut<ClearColor>,
    world: Option<Res<WorldMaterials>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    clear.0 = to_bevy(tokens.world.sky);
    let Some(world) = world else { return };
    for (handle, color) in [
        (&world.building, to_bevy(tokens.world.building)),
        (&world.plain, to_bevy(tokens.world.ground)),
        (
            &world.outside,
            to_bevy(tokens.world.outside_area).with_alpha(OUTSIDE_TINT_ALPHA),
        ),
    ] {
        if let Some(mut mat) = materials.get_mut(handle) {
            mat.base_color = color;
        }
    }
}

/// The highlight as drawn: for which address, and its material.
#[derive(Default)]
struct HighlightShown {
    address: Option<Address>,
    material: Option<Handle<StandardMaterial>>,
}

/// Redraws the [`AddressHighlight`] when the picked address or the area
/// changes: a copy of just those buildings' triangles in the highlight colour,
/// drawn in front of the same triangles of the building mesh. A theme change
/// only recolours it.
#[allow(clippy::too_many_arguments)]
fn highlight_address(
    mut commands: Commands,
    ui: Res<UiState>,
    area: Option<Res<Area>>,
    tokens: Res<Tokens>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    old: Query<Entity, With<AddressHighlight>>,
    mut shown: Local<HighlightShown>,
) {
    let area_changed = area.as_ref().is_some_and(|a| a.is_changed());
    if shown.address == ui.selected_address && !area_changed {
        if tokens.is_changed()
            && let Some(mut mat) = shown.material.as_ref().and_then(|h| materials.get_mut(h))
        {
            mat.base_color = to_bevy(tokens.world.address);
        }
        return;
    }
    shown.address.clone_from(&ui.selected_address);
    shown.material = None;
    for e in &old {
        commands.entity(e).despawn();
    }
    let (Some(address), Some(area)) = (&ui.selected_address, area) else {
        return;
    };
    let b = &area.bundle;
    let m = &b.mesh;
    // Only the vertices those buildings use, renumbered. A triangle with an
    // index out of range (a damaged bundle) is left out whole.
    let mut remap: HashMap<u32, u32> = HashMap::default();
    let (mut pos, mut nrm, mut idx) = (Vec::new(), Vec::new(), Vec::new());
    for i in b.buildings_at(|a| a == address) {
        let [first, count] = b.buildings[i].index_range;
        let range = first as usize..(first as usize).saturating_add(count as usize);
        for tri in m.indices.get(range).unwrap_or_default().as_chunks::<3>().0 {
            if tri.iter().any(|&v| {
                m.positions.get(v as usize).is_none() || m.normals.get(v as usize).is_none()
            }) {
                continue;
            }
            for &v in tri {
                let next = pos.len() as u32;
                let at = *remap.entry(v).or_insert_with(|| {
                    pos.push(m.positions[v as usize]);
                    nrm.push(m.normals[v as usize]);
                    next
                });
                idx.push(at);
            }
        }
    }
    if idx.is_empty() {
        return;
    }
    let mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
    .with_inserted_indices(Indices::U32(idx));
    let material = materials.add(StandardMaterial {
        base_color: to_bevy(tokens.world.address),
        perceptual_roughness: 0.9,
        // In front of the building mesh, behind the coverage on its walls.
        depth_bias: 5.0,
        ..default()
    });
    shown.material = Some(material.clone());
    commands.spawn((
        AddressHighlight,
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(material),
        NotShadowCaster,
        Pickable::IGNORE,
    ));
}

fn world2(proj: &Projection, p: LatLon) -> Vec2 {
    let w = proj.to_world(p);
    Vec2::new(w[0] as f32, w[1] as f32)
}

fn bbox_of(proj: &Projection, min: Vec2, max: Vec2) -> observation_core::geo::BBox {
    let nw = proj.to_latlon([f64::from(min.x), f64::from(min.y)]);
    let se = proj.to_latlon([f64::from(max.x), f64::from(max.y)]);
    observation_core::geo::BBox {
        south: se.lat,
        west: nw.lon,
        north: nw.lat,
        east: se.lon,
    }
}

/// Writes coverage colours into the overlay texture and façade vertex colours.
pub fn write_overlay(
    overlay: &Overlay,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    ground: &[[u8; 4]],
    facade: &[[u8; 4]],
) {
    if let Some(mut img) = images.get_mut(&overlay.ground_image) {
        img.data = Some(ground.iter().flatten().copied().collect());
    }
    if let Some(mut mesh) = meshes.get_mut(&overlay.facade_mesh) {
        let colors: Vec<[f32; 4]> = facade
            .iter()
            .flat_map(|c| {
                let lin = texel_to_linear(*c);
                [lin; 4]
            })
            .collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    }
}

/// Transparent texel.
pub const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// Token colour as texel bytes (re-exported for the coverage painter).
pub fn texel(c: bevy_egui::egui::Color32) -> [u8; 4] {
    rgba(c)
}
