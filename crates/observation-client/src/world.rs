//! 3D environment: buildings, ground, map tiles, lights and coverage overlays.

use crate::coverage::CoverageState;
use crate::input::OrbitCamera;
use crate::net::Tiles;
use crate::state::{Area, AreaLoaded, FACADE_CELL_M, GROUND_CELL_M, Models, Scene, UiState};
use crate::theme::{Tokens, rgba, texel_to_linear, to_bevy};
use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
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

/// Handles of the coverage overlay assets.
#[derive(Resource)]
pub struct Overlay {
    pub ground_image: Handle<Image>,
    pub facade_mesh: Handle<Mesh>,
}

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_lights).add_systems(
            Update,
            (
                on_area_loaded,
                apply_ground_tiles,
                follow_theme.run_if(resource_changed::<Tokens>),
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
    commands.spawn((
        AreaEntity,
        BuildingMesh,
        Mesh3d(meshes.add(building_mesh)),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: to_bevy(tokens.world.building),
            perceptual_roughness: 0.9,
            ..default()
        })),
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
    for t in ground_tiles {
        let b = t.bounds();
        let nw = world2(&proj, LatLon::new(b.north, b.west));
        let se = world2(&proj, LatLon::new(b.south, b.east));
        commands.spawn((
            AreaEntity,
            GroundTile(t),
            Mesh3d(meshes.add(ground_quad(nw, se, 0.0))),
            MeshMaterial3d(plain.clone()),
            NotShadowCaster,
            Pickable::IGNORE,
        ));
    }

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

/// Recolours the sky, the buildings and the plain ground in place after a
/// theme change.
fn follow_theme(
    tokens: Res<Tokens>,
    mut clear: ResMut<ClearColor>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    buildings: Query<&MeshMaterial3d<StandardMaterial>, With<BuildingMesh>>,
    plain: Query<&MeshMaterial3d<StandardMaterial>, (With<GroundTile>, Without<TileTextured>)>,
) {
    clear.0 = to_bevy(tokens.world.sky);
    // Tiles without their map image share the plain ground material.
    let recolor = buildings
        .iter()
        .map(|m| (m, tokens.world.building))
        .chain(plain.iter().map(|m| (m, tokens.world.ground)));
    for (m, color) in recolor {
        if let Some(mut mat) = materials.get_mut(&m.0) {
            mat.base_color = to_bevy(color);
        }
    }
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
