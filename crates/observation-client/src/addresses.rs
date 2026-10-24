//! Which street address each camera is at, from the loaded area's OSM data,
//! and the group it is listed under.

use crate::state::{Area, Scene};
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use bevy_egui::EguiPostUpdateSet;
use observation_core::camera::{CameraPlacement, MAX_GROUP_CHARS};

/// Heading for cameras with neither a hand-set group nor an address nearby.
pub const NO_ADDRESS: &str = "No address";

pub struct AddressesPlugin;

impl Plugin for AddressesPlugin {
    fn build(&self, app: &mut App) {
        // After every `Update` edit (placing, moving, undo, a new area) and
        // before the UI pass, so the list never shows a frame-old grouping.
        app.init_resource::<CameraAddresses>().add_systems(
            PostUpdate,
            resolve_addresses.before(EguiPostUpdateSet::EndPass),
        );
    }
}

/// Each camera's address, worked out again only when it moves or the area
/// changes.
#[derive(Debug, Default, Resource)]
pub struct CameraAddresses {
    /// Per camera id: the `[x, z]` it was resolved at, and the address label.
    by_camera: HashMap<u32, ([f32; 2], Option<String>)>,
    revision: Option<u64>,
}

impl CameraAddresses {
    /// The address OSM gives for the camera's position.
    pub fn automatic(&self, id: u32) -> Option<&str> {
        self.by_camera.get(&id).and_then(|(_, a)| a.as_deref())
    }

    /// The group a camera is listed under: its own, else its address; `None`
    /// when it has neither.
    pub fn group_of<'a>(&'a self, c: &'a CameraPlacement) -> Option<&'a str> {
        c.group
            .as_deref()
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .or_else(|| self.automatic(c.id))
    }

    /// Cameras by group, in the order each group first appears in the scene,
    /// with the cameras that have no group (`None`) last.
    pub fn groups<'a>(
        &'a self,
        cameras: &'a [CameraPlacement],
    ) -> Vec<(Option<&'a str>, Vec<&'a CameraPlacement>)> {
        let mut out: Vec<(Option<&str>, Vec<&CameraPlacement>)> = Vec::new();
        for c in cameras {
            let g = self.group_of(c);
            match out.iter_mut().find(|(name, _)| *name == g) {
                Some((_, list)) => list.push(c),
                None => out.push((g, vec![c])),
            }
        }
        out.sort_by_key(|(name, _)| name.is_none());
        out
    }

    /// Address labels near the cameras, for suggestions when naming a group.
    pub fn known(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .by_camera
            .values()
            .filter_map(|(_, a)| a.as_deref())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

fn resolve_addresses(
    scene: Res<Scene>,
    area: Option<Res<Area>>,
    mut cache: ResMut<CameraAddresses>,
) {
    // No area, or the cameras belong to another one still loading (an undo
    // across an area change, an imported scene): nothing to resolve against.
    let Some(area) = area.filter(|a| scene.area == Some(a.area)) else {
        if !cache.by_camera.is_empty() {
            cache.by_camera.clear();
        }
        cache.revision = None;
        return;
    };
    if area.is_changed() {
        cache.by_camera.clear();
        cache.revision = None;
    }
    if cache.revision == Some(scene.revision) {
        return;
    }
    cache.revision = Some(scene.revision);
    let ids: HashSet<u32> = scene.cameras.iter().map(|c| c.id).collect();
    cache.by_camera.retain(|id, _| ids.contains(id));
    for c in &scene.cameras {
        let at = [c.position[0], c.position[2]];
        if cache.by_camera.get(&c.id).is_some_and(|(p, _)| *p == at) {
            continue;
        }
        // Cut to a group name's length, so picking it as a group matches.
        let address = area
            .bundle
            .address_at(at)
            .map(|a| a.label().chars().take(MAX_GROUP_CHARS).collect());
        cache.by_camera.insert(c.id, (at, address));
    }
}
