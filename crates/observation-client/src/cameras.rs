//! Camera bodies in the scene and their field-of-view outlines.

use crate::coverage::cone_for;
use crate::state::{Area, Editor, Hover, Models, Panels, Scene, terrain_of};
use crate::theme::{Tokens, from_rgb, to_bevy, to_rgb};
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use bevy_egui::egui::Color32;
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
    /// Body material per colour, created on first use.
    materials: HashMap<[u8; 3], Handle<StandardMaterial>>,
}

impl BodyAssets {
    fn material(
        &mut self,
        color: Color32,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.materials
            .entry(to_rgb(color))
            .or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: to_bevy(color),
                    perceptual_roughness: 0.5,
                    ..default()
                })
            })
            .clone()
    }
}

/// A camera's colour: the user's choice, else a shade of its type's colour that
/// depends on how many cameras of that type (without their own colour) come
/// before it in `scene`.
pub fn camera_color(
    scene: &Scene,
    c: &CameraPlacement,
    models: &Models,
    tokens: &Tokens,
) -> Color32 {
    if let Some(rgb) = c.color {
        return from_rgb(rgb);
    }
    let kind = models.kind_of(&c.model_id);
    let nth = scene
        .cameras
        .iter()
        .take_while(|o| o.id != c.id)
        .filter(|o| o.color.is_none() && models.kind_of(&o.model_id) == kind)
        .count();
    tokens.kind_shade(kind, nth)
}

pub struct CamerasPlugin;

impl Plugin for CamerasPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_assets)
            // Bodies after the key and import edits and before the area load
            // and `record`; cones after `record` and before the pointer. On
            // the web this decides which frame bodies and cones show an edit in.
            .add_systems(
                Update,
                (
                    sync_bodies
                        .after(crate::input::keyboard)
                        .after(crate::persist::receive_imports)
                        .before(crate::world::AreaLoadSet),
                    draw_frustums
                        .after(crate::history::record)
                        .before(crate::input::PointerSet),
                )
                    .chain(),
            );
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
        materials: HashMap::default(),
    });
}

/// Camera body transform: −Z of the body points along the optical axis, and
/// its +X and +Y are the view cone's right and up, so a positive roll turns
/// the body as it turns the coverage (the image's right edge up).
fn body_transform(c: &CameraPlacement) -> Transform {
    let fwd = c.forward();
    let mut t = Transform::from_translation(Vec3::from_array(c.position));
    t.look_to(fwd, Vec3::Y);
    // Local +Z points back from the lens, so turning about it by +roll is
    // turning about the optical axis by −roll, as the cone does.
    t.rotate_local_z(c.roll_deg.to_radians());
    t
}

fn sync_bodies(
    mut commands: Commands,
    scene: Res<Scene>,
    models: Res<Models>,
    tokens: Res<Tokens>,
    mut assets: ResMut<BodyAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut bodies: Query<(
        Entity,
        &CameraBody,
        &mut Transform,
        &mut MeshMaterial3d<StandardMaterial>,
    )>,
) {
    if !scene.is_changed() && !models.is_changed() && !tokens.is_changed() {
        return;
    }
    // Type colours and the lens follow the theme.
    if tokens.is_changed()
        && let Some(mut lens) = materials.get_mut(&assets.lens_material)
    {
        lens.base_color = to_bevy(tokens.text);
    }
    let mut existing: HashMap<u32, Entity> = HashMap::default();
    for (e, body, mut t, mut mat) in &mut bodies {
        match scene.camera(body.0) {
            Some(c) => {
                let want = body_transform(c);
                if *t != want {
                    *t = want;
                }
                let m = assets.material(camera_color(&scene, c, &models, &tokens), &mut materials);
                if mat.0 != m {
                    mat.0 = m;
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
        let material = assets.material(camera_color(&scene, c, &models, &tokens), &mut materials);
        commands
            .spawn((
                CameraBody(c.id),
                Mesh3d(assets.mesh.clone()),
                MeshMaterial3d(material),
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
    // Drop materials no camera uses any more, such as the colours a drag
    // across the colour picker passes through.
    let used: HashSet<[u8; 3]> = scene
        .cameras
        .iter()
        .map(|c| to_rgb(camera_color(&scene, c, &models, &tokens)))
        .collect();
    assets.materials.retain(|rgb, _| used.contains(rgb));
}

fn draw_frustums(
    mut gizmos: Gizmos,
    scene: Res<Scene>,
    models: Res<Models>,
    panels: Res<Panels>,
    hover: Res<Hover>,
    editor: Res<Editor>,
    tokens: Res<Tokens>,
    area: Option<Res<Area>>,
) {
    let terrain = terrain_of(area.as_deref());
    for c in &scene.cameras {
        let selected = editor.selected == Some(c.id);
        let hovered = hover.hovered() == Some(c.id);
        if !(panels.show_frustums || selected || hovered) {
            continue;
        }
        let Some(m) = models.get(&c.model_id) else {
            continue;
        };
        let cone = cone_for(c, m, &scene.params);
        let mut color = to_bevy(camera_color(&scene, c, &models, &tokens));
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
        if hovered && !selected {
            gizmos.sphere(
                Isometry3d::from_translation(cone.eye),
                0.4,
                to_bevy(tokens.world.halo),
            );
        }
        if selected {
            // Neutral markers over a dark casing, so they read on any tile and
            // never look like a camera or coverage colour.
            let (mark, halo) = (to_bevy(tokens.world.selection), to_bevy(tokens.world.halo));
            gizmos.sphere(Isometry3d::from_translation(cone.eye), 0.46, halo);
            gizmos.sphere(Isometry3d::from_translation(cone.eye), 0.4, mark);
            // Where the optical axis meets the ground.
            if cone.forward.y < -1e-3
                && let Some(p) = terrain.ray_hit(cone.eye, cone.forward, r)
            {
                let aim = to_bevy(tokens.world.aim_target);
                gizmos.line(cone.eye, p, aim);
                let flat = |lift: f32| {
                    Isometry3d::new(
                        p + Vec3::Y * lift,
                        Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                    )
                };
                gizmos.circle(flat(0.04), 0.58, halo);
                gizmos.circle(flat(0.05), 0.5, aim);
                gizmos.circle(flat(0.05), 0.08, aim);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::frustum::ViewCone;

    /// The body turns with the view cone the coverage is computed from: its
    /// forward, right and up are the cone's, whatever the roll.
    #[test]
    fn the_body_rolls_the_same_way_as_the_view_cone() {
        for yaw in [0.0, 90.0, 215.0] {
            for pitch in [-20.0, 0.0, 35.0] {
                for roll in [-90.0, -30.0, 0.0, 30.0, 90.0, 170.0] {
                    let c = CameraPlacement {
                        id: 1,
                        label: String::new(),
                        model_id: "m".to_owned(),
                        position: [0.0, 3.0, 0.0],
                        yaw_deg: yaw,
                        pitch_deg: pitch,
                        roll_deg: roll,
                        color: None,
                        enabled: true,
                        group: None,
                    };
                    let cone = ViewCone::from_angles(c.eye(), yaw, pitch, roll, 90.0, 60.0, 20.0);
                    let r = body_transform(&c).rotation;
                    for (body, cone, axis) in [
                        (r * Vec3::NEG_Z, cone.forward, "forward"),
                        (r * Vec3::X, cone.right, "right"),
                        (r * Vec3::Y, cone.up, "up"),
                    ] {
                        assert!(
                            body.distance(cone) < 1e-4,
                            "{axis} at yaw {yaw}, pitch {pitch}, roll {roll}: {body} vs {cone}"
                        );
                    }
                }
            }
        }
    }

    /// Looking north with a 30° roll, the image's right edge goes up.
    #[test]
    fn a_positive_roll_lifts_the_bodys_right_side() {
        let c = CameraPlacement {
            id: 1,
            label: String::new(),
            model_id: "m".to_owned(),
            position: [0.0, 3.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 30.0,
            color: None,
            enabled: true,
            group: None,
        };
        let right = body_transform(&c).rotation * Vec3::X;
        assert!(
            (right - Vec3::new(0.866_025, 0.5, 0.0)).length() < 1e-4,
            "{right}"
        );
    }
}
