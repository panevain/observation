//! Scene edits, as plain functions on a [`SceneFile`].
//!
//! Each edit ends with [`finish`], which reads the edited scene back through
//! [`SceneFile::from_json`], the app's single validation point for scene
//! files: an edit that would leave a scene the app refuses fails instead.

use crate::ToolError;
use crate::area::{bundle_for, check_inside, ground_y};
use crate::catalog::{is_custom, model, new_custom_id};
use observation_core::camera::{
    CameraKind, CameraModel, CameraPlacement, Catalog, DoriLevel, Lighting, MAX_GROUP_CHARS,
};
use observation_core::example::example_cameras;
use observation_core::glam::Vec3;
use observation_core::scene::{
    AreaRef, MAX_CAMERAS, MAX_CUSTOM_MODELS, SCENE_VERSION, SceneFile, lift_onto_terrain,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// Reads a scene passed to a tool: a scene file as a JSON object, or as a
/// string holding one. A demo scene saved before terrain (version 1) is
/// raised onto the demo street's ground, as the app does when it loads one.
pub fn parse(scene: &Value) -> Result<SceneFile, ToolError> {
    let mut file = match scene {
        Value::String(s) => SceneFile::from_json(s)?,
        other => SceneFile::from_json(&other.to_string())?,
    };
    if file.on_flat_ground() && file.area == AreaRef::Demo {
        let terrain = &bundle_for(&file)?.terrain;
        lift_onto_terrain(&mut file.cameras, terrain);
        file.version = SCENE_VERSION;
    }
    Ok(file)
}

/// Checks an edited scene the way the app checks one it imports, and
/// returns it as a JSON value.
pub fn finish(scene: &SceneFile) -> Result<(SceneFile, Value), ToolError> {
    let text = scene.to_json();
    let checked = SceneFile::from_json(&text)?;
    let value = serde_json::from_str(&text).map_err(|e| ToolError::invalid(e.to_string()))?;
    Ok((checked, value))
}

/// A new, empty scene on the demo street.
pub fn new_demo_scene() -> Result<SceneFile, ToolError> {
    let bundle = crate::area::demo()?;
    Ok(SceneFile::new(AreaRef::Demo, bundle.origin))
}

/// A mount archetype, as the tools accept it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum Kind {
    Doorbell,
    Floodlight,
    Spotlight,
    Bullet,
    Turret,
    Dome,
    Indoor,
}

impl From<Kind> for CameraKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Doorbell => Self::Doorbell,
            Kind::Floodlight => Self::Floodlight,
            Kind::Spotlight => Self::Spotlight,
            Kind::Bullet => Self::Bullet,
            Kind::Turret => Self::Turret,
            Kind::Dome => Self::Dome,
            Kind::Indoor => Self::Indoor,
        }
    }
}

/// A detail level (DORI, EN 62676-4), as the tools accept it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum Dori {
    /// 25 px/m: can tell a person is present.
    Detect,
    /// 62 px/m: general activity, clothing.
    Observe,
    /// 125 px/m: recognise a known person.
    Recognise,
    /// 250 px/m: identify a stranger.
    Identify,
    /// The scene's `custom_ppm`.
    Custom,
}

impl From<Dori> for DoriLevel {
    fn from(d: Dori) -> Self {
        match d {
            Dori::Detect => Self::Detect,
            Dori::Observe => Self::Observe,
            Dori::Recognise => Self::Recognise,
            Dori::Identify => Self::Identify,
            Dori::Custom => Self::Custom,
        }
    }
}

/// Lighting, as the tools accept it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum Light {
    Day,
    /// Limited to each camera's infrared range.
    Night,
}

impl From<Light> for Lighting {
    fn from(l: Light) -> Self {
        match l {
            Light::Day => Self::Day,
            Light::Night => Self::Night,
        }
    }
}

/// A point on the area, `height_m` above the ground.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub struct Target {
    /// Metres east of the area's origin.
    pub x: f32,
    /// Metres south of the area's origin (north is negative z).
    pub z: f32,
    /// Metres above the ground there. Defaults to the scene's subject height.
    pub height_m: Option<f32>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Place {
    /// Catalogue or custom model id (see `list_camera_models`).
    pub model_id: String,
    /// Metres east of the area's origin.
    pub x: f32,
    /// Metres south of the area's origin (north is negative z).
    pub z: f32,
    /// Mount height above the ground, metres. Defaults to the model kind's
    /// typical height (doorbell 1.2, floodlight 2.7, bullet 2.6, ...).
    pub height_m: Option<f32>,
    /// Compass bearing of the view, degrees clockwise from north. Ignored
    /// when `look_at` is given; defaults to 0 (north).
    pub yaw_deg: Option<f32>,
    /// Tilt, degrees (negative looks down). Ignored when `look_at` is
    /// given; defaults to the kind's typical tilt.
    pub pitch_deg: Option<f32>,
    /// Rotation about the view axis, degrees. Defaults to 0.
    pub roll_deg: Option<f32>,
    /// Point the camera at this spot instead of using yaw and pitch.
    pub look_at: Option<Target>,
    /// Name shown in the app. Defaults to "Camera <id>".
    pub label: Option<String>,
}

fn next_camera_id(scene: &SceneFile) -> u32 {
    scene
        .cameras
        .iter()
        .map(|c| c.id)
        .max()
        .map_or(1, |m| m.saturating_add(1))
}

fn camera_mut(scene: &mut SceneFile, id: u32) -> Result<&mut CameraPlacement, ToolError> {
    scene
        .cameras
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or(ToolError::NoCamera(id))
}

fn check_height(h: f32) -> Result<f32, ToolError> {
    if h.is_finite() && (0.0..=100.0).contains(&h) {
        Ok(h)
    } else {
        Err(ToolError::invalid(format!(
            "height {h} m is out of range: use 0 to 100 m above the ground"
        )))
    }
}

fn check_pitch(p: f32) -> Result<f32, ToolError> {
    if p.is_finite() && (-90.0..=90.0).contains(&p) {
        Ok(p)
    } else {
        Err(ToolError::invalid(format!(
            "pitch {p}° is out of range: use -90° (straight down) to 90°"
        )))
    }
}

fn check_angle(what: &str, a: f32) -> Result<f32, ToolError> {
    if a.is_finite() {
        Ok(a)
    } else {
        Err(ToolError::invalid(format!(
            "{what} must be a number of degrees"
        )))
    }
}

/// The world position of `t`.
fn target_point(scene: &SceneFile, t: Target) -> Result<Vec3, ToolError> {
    let bundle = bundle_for(scene)?;
    check_inside(bundle, t.x, t.z)?;
    let h = check_height(t.height_m.unwrap_or(scene.params.subject_height_m))?;
    Ok(Vec3::new(t.x, ground_y(bundle, t.x, t.z) + h, t.z))
}

/// Places a camera; returns its id.
pub fn place_camera(scene: &mut SceneFile, catalog: &Catalog, p: Place) -> Result<u32, ToolError> {
    if scene.cameras.len() >= MAX_CAMERAS {
        return Err(ToolError::invalid(format!(
            "a scene holds at most {MAX_CAMERAS} cameras"
        )));
    }
    let kind = model(catalog, Some(scene), &p.model_id)
        .ok_or_else(|| ToolError::NoModel(p.model_id.clone()))?
        .kind;
    let bundle = bundle_for(scene)?;
    check_inside(bundle, p.x, p.z)?;
    let height = check_height(p.height_m.unwrap_or(kind.default_mount_height_m()))?;
    let id = next_camera_id(scene);
    let mut cam = CameraPlacement {
        id,
        label: p
            .label
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| format!("Camera {id}")),
        model_id: p.model_id,
        position: [p.x, ground_y(bundle, p.x, p.z) + height, p.z],
        yaw_deg: check_angle("yaw", p.yaw_deg.unwrap_or(0.0))?.rem_euclid(360.0),
        pitch_deg: check_pitch(p.pitch_deg.unwrap_or(kind.default_pitch_deg()))?,
        roll_deg: check_angle("roll", p.roll_deg.unwrap_or(0.0))?,
        color: None,
        enabled: true,
        group: None,
    };
    if let Some(t) = p.look_at {
        cam.look_at(target_point(scene, t)?);
    }
    scene.cameras.push(cam);
    Ok(id)
}

/// Moves a camera to `[x, z]` (either may be left as it is), at `height_m`
/// above the ground there, or at the height above the ground it had.
/// Where it points is unchanged.
pub fn move_camera(
    scene: &mut SceneFile,
    id: u32,
    x: Option<f32>,
    z: Option<f32>,
    height_m: Option<f32>,
) -> Result<(), ToolError> {
    let bundle = bundle_for(scene)?;
    let cam = camera_mut(scene, id)?;
    let [ox, oy, oz] = cam.position;
    let (nx, nz) = (x.unwrap_or(ox), z.unwrap_or(oz));
    check_inside(bundle, nx, nz)?;
    let height = match height_m {
        Some(h) => check_height(h)?,
        None => (oy - ground_y(bundle, ox, oz)).max(0.0),
    };
    cam.position = [nx, ground_y(bundle, nx, nz) + height, nz];
    Ok(())
}

/// Sets any of a camera's yaw, pitch and roll.
pub fn aim_camera(
    scene: &mut SceneFile,
    id: u32,
    yaw_deg: Option<f32>,
    pitch_deg: Option<f32>,
    roll_deg: Option<f32>,
) -> Result<(), ToolError> {
    let cam = camera_mut(scene, id)?;
    if let Some(y) = yaw_deg {
        cam.yaw_deg = check_angle("yaw", y)?.rem_euclid(360.0);
    }
    if let Some(p) = pitch_deg {
        cam.pitch_deg = check_pitch(p)?;
    }
    if let Some(r) = roll_deg {
        cam.roll_deg = check_angle("roll", r)?;
    }
    Ok(())
}

/// Points a camera at a spot on the area (keeps its roll).
pub fn point_camera_at(scene: &mut SceneFile, id: u32, t: Target) -> Result<(), ToolError> {
    let p = target_point(scene, t)?;
    let cam = camera_mut(scene, id)?;
    if (p - cam.eye()).length() < 0.01 {
        return Err(ToolError::invalid("the target is where the camera is"));
    }
    cam.look_at(p);
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct CameraUpdate {
    /// New name shown in the app.
    pub label: Option<String>,
    /// New catalogue or custom model id.
    pub model_id: Option<String>,
    /// Whether the camera counts towards coverage.
    pub enabled: Option<bool>,
    /// Group the camera is listed under in the app; "" groups it by address.
    pub group: Option<String>,
}

/// Changes a camera's label, model, enabled state or group.
pub fn update_camera(
    scene: &mut SceneFile,
    catalog: &Catalog,
    id: u32,
    u: CameraUpdate,
) -> Result<(), ToolError> {
    if let Some(m) = &u.model_id
        && model(catalog, Some(scene), m).is_none()
    {
        return Err(ToolError::NoModel(m.clone()));
    }
    let cam = camera_mut(scene, id)?;
    if let Some(l) = u.label {
        let l = l.trim();
        if l.is_empty() {
            return Err(ToolError::invalid("a label cannot be blank"));
        }
        cam.label = l.to_owned();
    }
    if let Some(m) = u.model_id {
        cam.model_id = m;
    }
    if let Some(e) = u.enabled {
        cam.enabled = e;
    }
    if let Some(g) = u.group {
        let g: String = g.trim().chars().take(MAX_GROUP_CHARS).collect();
        cam.group = Some(g).filter(|g| !g.is_empty());
    }
    Ok(())
}

pub fn remove_camera(scene: &mut SceneFile, id: u32) -> Result<(), ToolError> {
    let before = scene.cameras.len();
    scene.cameras.retain(|c| c.id != id);
    if scene.cameras.len() == before {
        return Err(ToolError::NoCamera(id));
    }
    Ok(())
}

/// A custom camera model's specifications, for creating one.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct NewModel {
    /// Product name, e.g. "Porch Cam 2K".
    pub name: String,
    /// Manufacturer; may be empty.
    #[serde(default)]
    pub vendor: String,
    pub kind: Kind,
    /// Advertised horizontal field of view, degrees (1 to 360).
    pub hfov_deg: f32,
    /// Advertised vertical field of view, degrees (1 to 360).
    pub vfov_deg: f32,
    /// Stream resolution `[width, height]`, pixels (each at least 16).
    pub resolution: [u32; 2],
    /// Advertised night-vision range, metres (0 to 200). Unknown: omit it.
    pub ir_range_m: Option<f32>,
    /// The specifications' source: an https:// URL.
    pub source: Option<String>,
    pub notes: Option<String>,
}

/// Changes to a custom model; omitted fields stay as they are.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ModelUpdate {
    pub name: Option<String>,
    pub vendor: Option<String>,
    pub kind: Option<Kind>,
    pub hfov_deg: Option<f32>,
    pub vfov_deg: Option<f32>,
    pub resolution: Option<[u32; 2]>,
    /// Night-vision range, metres; a negative value clears it (unknown).
    pub ir_range_m: Option<f32>,
    /// An https:// URL; "" clears it.
    pub source: Option<String>,
    /// "" clears them.
    pub notes: Option<String>,
}

fn non_empty(s: String) -> Option<String> {
    Some(s).filter(|s| !s.trim().is_empty())
}

fn check_model(m: &CameraModel) -> Result<(), ToolError> {
    if m.name.trim().is_empty() {
        return Err(ToolError::invalid("a model needs a name"));
    }
    m.validate()
        .map_err(|e| ToolError::invalid(format!("invalid model: {e}")))
}

/// Adds a custom model to the scene; returns its new id.
pub fn create_custom_model(scene: &mut SceneFile, n: NewModel) -> Result<String, ToolError> {
    if scene.custom_models.len() >= MAX_CUSTOM_MODELS {
        return Err(ToolError::invalid(format!(
            "a scene holds at most {MAX_CUSTOM_MODELS} custom models; delete one first"
        )));
    }
    let m = CameraModel {
        id: new_custom_id(scene, &n.name),
        vendor: n.vendor.trim().to_owned(),
        name: n.name.trim().to_owned(),
        kind: n.kind.into(),
        hfov_deg: n.hfov_deg,
        vfov_deg: n.vfov_deg,
        resolution: n.resolution,
        ir_range_m: n.ir_range_m,
        source: n.source.and_then(non_empty),
        notes: n.notes.and_then(non_empty),
    };
    check_model(&m)?;
    let id = m.id.clone();
    scene.custom_models.push(m);
    Ok(id)
}

/// Edits a custom model in place, keeping its id (cameras refer to it).
pub fn update_custom_model(
    scene: &mut SceneFile,
    id: &str,
    u: ModelUpdate,
) -> Result<(), ToolError> {
    let slot = scene
        .custom_models
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| ToolError::NoCustomModel(id.to_owned()))?;
    let mut m = slot.clone();
    if let Some(v) = u.name {
        m.name = v.trim().to_owned();
    }
    if let Some(v) = u.vendor {
        m.vendor = v.trim().to_owned();
    }
    if let Some(v) = u.kind {
        m.kind = v.into();
    }
    if let Some(v) = u.hfov_deg {
        m.hfov_deg = v;
    }
    if let Some(v) = u.vfov_deg {
        m.vfov_deg = v;
    }
    if let Some(v) = u.resolution {
        m.resolution = v;
    }
    if let Some(v) = u.ir_range_m {
        m.ir_range_m = (v >= 0.0).then_some(v);
    }
    if let Some(v) = u.source {
        m.source = non_empty(v);
    }
    if let Some(v) = u.notes {
        m.notes = non_empty(v);
    }
    check_model(&m)?;
    *slot = m;
    Ok(())
}

/// Deletes an unused custom model. One that cameras still use is refused,
/// as the app refuses it, so no camera is left without a model.
pub fn delete_custom_model(scene: &mut SceneFile, id: &str) -> Result<(), ToolError> {
    if !is_custom(id) || !scene.custom_models.iter().any(|m| m.id == id) {
        return Err(ToolError::NoCustomModel(id.to_owned()));
    }
    let users: Vec<u32> = scene
        .cameras
        .iter()
        .filter(|c| c.model_id == id)
        .map(|c| c.id)
        .collect();
    if !users.is_empty() {
        return Err(ToolError::invalid(format!(
            "cameras {users:?} use model `{id}`: give them another model or remove them first"
        )));
    }
    scene.custom_models.retain(|m| m.id != id);
    Ok(())
}

/// Adds up to `count` example cameras on house fronts facing the street,
/// cycling through `model_ids` (the catalogue's first model of each kind
/// when empty); returns their ids.
pub fn add_example_cameras(
    scene: &mut SceneFile,
    catalog: &Catalog,
    count: usize,
    model_ids: &[String],
) -> Result<Vec<u32>, ToolError> {
    let room = MAX_CAMERAS.saturating_sub(scene.cameras.len());
    let count = count.min(room);
    let models: Vec<CameraModel> = if model_ids.is_empty() {
        let mut seen = Vec::new();
        catalog
            .models
            .iter()
            .filter(|m| {
                m.kind != CameraKind::Indoor && !seen.contains(&m.kind) && {
                    seen.push(m.kind);
                    true
                }
            })
            .cloned()
            .collect()
    } else {
        model_ids
            .iter()
            .map(|id| {
                model(catalog, Some(scene), id)
                    .cloned()
                    .ok_or_else(|| ToolError::NoModel(id.clone()))
            })
            .collect::<Result<_, _>>()?
    };
    let refs: Vec<&CameraModel> = models.iter().collect();
    let placed = example_cameras(bundle_for(scene)?, &refs, count);
    let mut ids = Vec::with_capacity(placed.len());
    for mut c in placed {
        c.id = next_camera_id(scene);
        c.label = format!("Camera {}", c.id);
        ids.push(c.id);
        scene.cameras.push(c);
    }
    Ok(ids)
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
pub struct ParamsUpdate {
    /// The detail level the app shows.
    pub dori: Option<Dori>,
    pub lighting: Option<Light>,
    /// Height above the ground at which people are tested, metres (0 = feet,
    /// about 1.0 = torso, about 1.6 = face).
    pub subject_height_m: Option<f32>,
    /// Pixel density for the Custom level, px/m (1 to 2000).
    pub custom_ppm: Option<f32>,
}

/// Changes the scene's analysis settings.
pub fn set_coverage_params(scene: &mut SceneFile, u: ParamsUpdate) {
    let p = &mut scene.params;
    if let Some(d) = u.dori {
        p.dori = d.into();
    }
    if let Some(l) = u.lighting {
        p.lighting = l.into();
    }
    if let Some(h) = u.subject_height_m {
        p.subject_height_m = h;
    }
    if let Some(c) = u.custom_ppm {
        p.custom_ppm = c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;

    fn place_at(scene: &mut SceneFile, x: f32, z: f32) -> u32 {
        let cat = catalog().unwrap();
        let model_id = cat.models[0].id.clone();
        place_camera(
            scene,
            cat,
            Place {
                model_id,
                x,
                z,
                height_m: Some(2.0),
                yaw_deg: Some(90.0),
                pitch_deg: None,
                roll_deg: None,
                look_at: None,
                label: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn placing_moving_aiming_and_removing() {
        let mut s = new_demo_scene().unwrap();
        let a = place_at(&mut s, 0.0, 0.0);
        let b = place_at(&mut s, 5.0, 5.0);
        assert_eq!((a, b), (1, 2));
        let bundle = bundle_for(&s).unwrap();
        let ground = ground_y(bundle, 0.0, 0.0);
        assert!((s.cameras[0].position[1] - ground - 2.0).abs() < 1e-3);
        assert_eq!(s.cameras[0].label, "Camera 1");

        // Moving keeps the height above the new ground and the aim.
        move_camera(&mut s, a, Some(10.0), None, None).unwrap();
        let c = &s.cameras[0];
        assert!((c.position[1] - ground_y(bundle, 10.0, 0.0) - 2.0).abs() < 1e-3);
        assert_eq!(c.yaw_deg, 90.0);

        aim_camera(&mut s, a, Some(-90.0), Some(-30.0), None).unwrap();
        assert_eq!(
            (s.cameras[0].yaw_deg, s.cameras[0].pitch_deg),
            (270.0, -30.0)
        );
        assert!(aim_camera(&mut s, a, None, Some(120.0), None).is_err());

        // Pointing east from x = 10 at the ground.
        point_camera_at(
            &mut s,
            a,
            Target {
                x: 30.0,
                z: 0.0,
                height_m: Some(0.0),
            },
        )
        .unwrap();
        assert!((s.cameras[0].yaw_deg - 90.0).abs() < 0.5);
        assert!(s.cameras[0].pitch_deg < 0.0);

        remove_camera(&mut s, a).unwrap();
        assert!(matches!(
            remove_camera(&mut s, a),
            Err(ToolError::NoCamera(1))
        ));
        // Ids are not reused while a higher one exists.
        assert_eq!(place_at(&mut s, 0.0, 0.0), 3);
        finish(&s).unwrap();
    }

    #[test]
    fn placing_outside_the_area_or_with_an_unknown_model_fails() {
        let mut s = new_demo_scene().unwrap();
        let cat = catalog().unwrap();
        let p = |model_id: &str, x: f32| Place {
            model_id: model_id.to_owned(),
            x,
            z: 0.0,
            height_m: None,
            yaw_deg: None,
            pitch_deg: None,
            roll_deg: None,
            look_at: None,
            label: None,
        };
        assert!(matches!(
            place_camera(&mut s, cat, p("no-such-model", 0.0)),
            Err(ToolError::NoModel(_))
        ));
        let id = cat.models[0].id.clone();
        assert!(place_camera(&mut s, cat, p(&id, 10_000.0)).is_err());
        assert!(s.cameras.is_empty());
    }

    #[test]
    fn custom_models_round_trip_and_are_protected_while_used() {
        let mut s = new_demo_scene().unwrap();
        let cat = catalog().unwrap();
        let spec = NewModel {
            name: "Porch Cam".into(),
            vendor: String::new(),
            kind: Kind::Doorbell,
            hfov_deg: 150.0,
            vfov_deg: 90.0,
            resolution: [1920, 1080],
            ir_range_m: None,
            source: None,
            notes: None,
        };
        let id = create_custom_model(&mut s, spec.clone()).unwrap();
        assert_eq!(id, "custom-porch-cam-1");
        assert_eq!(
            create_custom_model(&mut s, spec.clone()).unwrap(),
            "custom-porch-cam-2"
        );
        let bad = NewModel {
            source: Some("javascript:alert(1)".into()),
            ..spec
        };
        assert!(create_custom_model(&mut s, bad).is_err());

        update_custom_model(
            &mut s,
            &id,
            ModelUpdate {
                ir_range_m: Some(9.0),
                ..ModelUpdate::default()
            },
        )
        .unwrap();
        assert_eq!(s.custom_models[0].ir_range_m, Some(9.0));
        assert!(
            update_custom_model(
                &mut s,
                &id,
                ModelUpdate {
                    hfov_deg: Some(0.0),
                    ..ModelUpdate::default()
                }
            )
            .is_err()
        );

        let cam = place_camera(
            &mut s,
            cat,
            Place {
                model_id: id.clone(),
                x: 0.0,
                z: 0.0,
                height_m: None,
                yaw_deg: None,
                pitch_deg: None,
                roll_deg: None,
                look_at: None,
                label: Some("Porch".into()),
            },
        )
        .unwrap();
        assert!(delete_custom_model(&mut s, &id).is_err());
        remove_camera(&mut s, cam).unwrap();
        delete_custom_model(&mut s, &id).unwrap();
        assert!(delete_custom_model(&mut s, &cat.models[0].id).is_err());
        finish(&s).unwrap();
    }

    #[test]
    fn example_cameras_are_added_with_fresh_ids() {
        let mut s = new_demo_scene().unwrap();
        let cat = catalog().unwrap();
        place_at(&mut s, 0.0, 0.0);
        let ids = add_example_cameras(&mut s, cat, 4, &[]).unwrap();
        assert!(!ids.is_empty());
        assert_eq!(ids[0], 2);
        finish(&s).unwrap();
    }

    #[test]
    fn edits_the_app_would_refuse_fail_at_finish() {
        let mut s = new_demo_scene().unwrap();
        set_coverage_params(
            &mut s,
            ParamsUpdate {
                subject_height_m: Some(20.0),
                ..ParamsUpdate::default()
            },
        );
        assert!(finish(&s).is_err());
    }

    #[test]
    fn scenes_parse_from_objects_and_strings() {
        let s = new_demo_scene().unwrap();
        let text = s.to_json();
        let obj: Value = serde_json::from_str(&text).unwrap();
        let (from_obj, from_str) = (parse(&obj).unwrap(), parse(&Value::String(text)).unwrap());
        assert_eq!(from_obj, from_str);
        // JSON float parsing can move the origin by an ulp, as with any saved scene.
        assert!((from_obj.origin.lon - s.origin.lon).abs() < 1e-9);
        assert_eq!(from_obj.cameras, s.cameras);
        assert!(parse(&serde_json::json!({"format": "nope"})).is_err());
    }
}
