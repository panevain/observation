//! Camera bodies in the scene and their field-of-view outlines.

use crate::coverage::cone_for;
use crate::state::{Models, Scene, UiState};
use crate::theme::{Tokens, to_bevy};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use observation_core::camera::CameraPlacement;

/// Marks the mesh entity representing a placed camera (by scene id).
#[derive(Debug, Component)]
pub struct CameraBody(pub u32);

/// Shared meshes/materials for camera bodies.
#[derive(Resource)]
struct BodyAssets {
    mesh: Handle<Mesh>,
    lens: Handle<Mesh>,
    lens_material: Handle<StandardMaterial>,
    materials: Vec<Handle<StandardMaterial>>,
}

pub struct CamerasPlugin;

impl Plugin for CamerasPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_assets)
            .add_systems(Update, (sync_bodies, draw_frustums).chain());
    }
}

fn setup_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    tokens: Res<Tokens>,
) {
    commands.insert_resource(BodyAssets {
        mesh: meshes.add(Cuboid::new(0.22, 0.18, 0.34)),
        lens: meshes.add(Cylinder::new(0.06, 0.04)),
        lens_material: materials.add(StandardMaterial {
            base_color: to_bevy(tokens.text),
            perceptual_roughness: 0.3,
            ..default()
        }),
        materials: tokens
            .camera_palette
            .iter()
            .map(|c| {
                materials.add(StandardMaterial {
                    base_color: to_bevy(*c),
                    perceptual_roughness: 0.5,
                    ..default()
                })
            })
            .collect(),
    });
}

/// Camera body transform: −Z of the body points along the optical axis.
fn body_transform(c: &CameraPlacement) -> Transform {
    let fwd = c.forward();
    let mut t = Transform::from_translation(Vec3::from_array(c.position));
    t.look_to(fwd, Vec3::Y);
    t.rotate_local_z(-c.roll_deg.to_radians());
    t
}

fn sync_bodies(
    mut commands: Commands,
    scene: Res<Scene>,
    assets: Res<BodyAssets>,
    mut bodies: Query<(
        Entity,
        &CameraBody,
        &mut Transform,
        &mut MeshMaterial3d<StandardMaterial>,
    )>,
) {
    if !scene.is_changed() {
        return;
    }
    let mut existing: HashMap<u32, Entity> = HashMap::default();
    for (e, body, mut t, mut mat) in &mut bodies {
        match scene.camera(body.0) {
            Some(c) => {
                let want = body_transform(c);
                if *t != want {
                    *t = want;
                }
                let m = &assets.materials[c.color_index as usize % assets.materials.len()];
                if mat.0 != *m {
                    mat.0 = m.clone();
                }
                existing.insert(body.0, e);
            }
            None => commands.entity(e).despawn(),
        }
    }
    for c in &scene.cameras {
        if existing.contains_key(&c.id) {
            continue;
        }
        commands
            .spawn((
                CameraBody(c.id),
                Mesh3d(assets.mesh.clone()),
                MeshMaterial3d(
                    assets.materials[c.color_index as usize % assets.materials.len()].clone(),
                ),
                body_transform(c),
            ))
            .with_children(|p| {
                p.spawn((
                    Mesh3d(assets.lens.clone()),
                    MeshMaterial3d(assets.lens_material.clone()),
                    Transform::from_xyz(0.0, 0.0, -0.18)
                        .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                ));
            });
    }
}

fn draw_frustums(
    mut gizmos: Gizmos,
    scene: Res<Scene>,
    models: Res<Models>,
    ui: Res<UiState>,
    tokens: Res<Tokens>,
) {
    for c in &scene.cameras {
        let selected = ui.selected == Some(c.id);
        if !(ui.show_frustums || selected) {
            continue;
        }
        let Some(m) = models.get(&c.model_id) else {
            continue;
        };
        let cone = cone_for(c, m, &scene.params);
        let mut color = to_bevy(tokens.camera_color(c.color_index));
        if !c.enabled {
            color = color.with_alpha(0.25);
        }
        let r = cone.range;
        // Corner edges.
        for d in cone.corner_rays() {
            gizmos.line(cone.eye, cone.eye + d * r, color);
        }
        // Far boundary (a curved "fan" at the effective range).
        let outline = cone.outline_rays(12);
        for w in outline.windows(2) {
            gizmos.line(cone.eye + w[0] * r, cone.eye + w[1] * r, color);
        }
        if let (Some(a), Some(b)) = (outline.last(), outline.first()) {
            gizmos.line(cone.eye + *a * r, cone.eye + *b * r, color);
        }
        if selected {
            let sel = to_bevy(tokens.world.selection);
            gizmos.sphere(Isometry3d::from_translation(cone.eye), 0.4, sel);
            // Where the optical axis meets the ground.
            if cone.forward.y < -1e-3 {
                let t = -cone.eye.y / cone.forward.y;
                if t <= r {
                    let p = cone.eye + cone.forward * t;
                    gizmos.line(cone.eye, p, to_bevy(tokens.world.aim_target));
                    gizmos.circle(
                        Isometry3d::new(
                            p + Vec3::Y * 0.05,
                            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                        ),
                        0.5,
                        to_bevy(tokens.world.aim_target),
                    );
                }
            }
        }
    }
}
