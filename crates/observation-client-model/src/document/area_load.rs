//! What loading an area does to the document: a fresh area starts empty (the
//! demo street with example cameras), a restored scene's own area keeps its
//! cameras, and either way the provenance is brought up to date.

use observation_core::bundle::AreaBundle;
use observation_core::camera::CameraModel;
use observation_core::example::example_cameras;
use observation_core::provenance::{DataChange, Provenance};
use observation_core::scene::{AreaRef, lift_onto_terrain};

use super::{Document, LoadArea, Models};
use crate::editor::{Editor, Hover};
use crate::toasts::Toasts;

/// Models used, in turn, for the example cameras on the demo street: a mix of
/// types and brands.
pub const EXAMPLE_MODELS: [&str; 10] = [
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

/// Takes in the area `request` asked for, now that its `bundle` has arrived;
/// returns how a restored or imported scene's map data changed since it was
/// saved, if it did (also told as a toast).
pub fn apply(
    scene: &mut Document,
    editor: &mut Editor,
    hover: &mut Hover,
    toasts: &mut Toasts,
    models: &Models,
    request: &LoadArea,
    bundle: &AreaBundle,
) -> Option<DataChange> {
    // An address picked in the old area means nothing in the new one.
    editor.selected_address = None;
    hover.hover_address = None;

    // Document state.
    let area = request.area;
    let keep = request.keep_cameras && scene.area == Some(area);
    if keep && std::mem::take(&mut scene.on_flat_ground) {
        // A scene saved before terrain: its cameras stood on flat ground.
        lift_onto_terrain(&mut scene.cameras, &bundle.terrain);
    }
    if !keep {
        scene.on_flat_ground = false;
        scene.cameras.clear();
        // A region belongs to the area it was drawn on, as does one being
        // drawn.
        scene.region = None;
        editor.region_draft.clear();
        editor.selected = None;
        if area == AreaRef::Demo {
            // A fresh demo street comes with example cameras to explore.
            let picks: Vec<&CameraModel> = EXAMPLE_MODELS
                .iter()
                .filter_map(|id| models.get(id))
                .collect();
            for c in example_cameras(bundle, &picks, EXAMPLE_MODELS.len()) {
                scene.add_camera(c);
            }
        }
    }
    scene.area = Some(area);
    scene.origin = bundle.origin;
    // A restored or imported scene learns whether its map data changed
    // since it was saved; any older notice was about the area replaced.
    let data_change = scene.area_loaded(Provenance::of(bundle, &models.catalog_version), keep);
    if let Some(message) = data_change.as_ref().map(DataChange::message) {
        toasts.info(message);
    }
    scene.touch();

    toasts.info(format!(
        "Loaded {} buildings and {} street segments",
        bundle.buildings.len(),
        bundle.roads.len()
    ));
    data_change
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::address::Address;
    use observation_core::bundle::{OsmBuilding, OsmRoad, build_bundle};
    use observation_core::camera::{CameraKind, CameraPlacement, Catalog};
    use observation_core::coverage::Region;
    use observation_core::geo::{BBox, LatLon};
    use observation_core::terrain::Terrain;

    fn osm() -> AreaRef {
        AreaRef::Osm {
            bbox: BBox::new(51.5, -0.001, 51.5009, 0.001).expect("valid box"),
        }
    }

    /// A street running east-west with four houses on each side, on `terrain`.
    fn street_on(terrain: Terrain) -> AreaBundle {
        let bbox = BBox::around(LatLon::new(51.5, 0.0), 200.0);
        let o = bbox.center();
        let m_lat = 1.0 / 111_195.0;
        let m_lon = m_lat / o.lat.to_radians().cos();
        let at = |x: f64, y: f64| LatLon::new(o.lat + y * m_lat, o.lon + x * m_lon);
        let mut buildings = Vec::new();
        for i in 0..4 {
            for side in [-1.0, 1.0] {
                let (x0, y0, y1) = (-60.0 + f64::from(i) * 25.0, side * 12.0, side * 24.0);
                buildings.push(OsmBuilding {
                    osm_id: i64::from(i) * 2 + i64::from(side > 0.0),
                    outer: vec![
                        at(x0, y0),
                        at(x0 + 12.0, y0),
                        at(x0 + 12.0, y1),
                        at(x0, y1),
                        at(x0, y0),
                    ],
                    holes: vec![],
                    min_height_m: 0.0,
                    height_m: 6.5,
                    address: None,
                });
            }
        }
        let roads = vec![OsmRoad {
            osm_id: 1,
            highway: "residential".into(),
            name: None,
            width_m: 6.0,
            points: vec![at(-90.0, 0.0), at(90.0, 0.0)],
        }];
        build_bundle(bbox, &buildings, &roads, &[], terrain, "test", 7, Some(5))
    }

    fn street() -> AreaBundle {
        street_on(Terrain::default())
    }

    /// Ground 5 m up everywhere.
    fn raised() -> Terrain {
        Terrain {
            min: [-400.0, -400.0],
            cell_m: 100.0,
            cols: 9,
            rows: 9,
            heights_dm: vec![50; 81],
            datum_m: 0.0,
            attribution: "test".into(),
        }
    }

    /// The example models, as custom models.
    fn models() -> Models {
        let mut models = Models::new(Catalog::default());
        for (i, id) in EXAMPLE_MODELS.iter().enumerate() {
            models.custom.push(CameraModel {
                id: (*id).to_owned(),
                vendor: String::new(),
                name: (*id).to_owned(),
                kind: if i % 2 == 0 {
                    CameraKind::Doorbell
                } else {
                    CameraKind::Floodlight
                },
                hfov_deg: 90.0,
                vfov_deg: 55.0,
                resolution: [1920, 1080],
                ir_range_m: None,
                source: None,
                notes: None,
            });
        }
        models
    }

    fn camera(y: f32) -> CameraPlacement {
        CameraPlacement {
            id: 0,
            label: String::new(),
            model_id: EXAMPLE_MODELS[0].into(),
            position: [3.0, y, 4.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        }
    }

    /// A document on `area` with one camera, a region and flat-ground
    /// cameras, and an editor and hover with something picked in it.
    fn before(area: AreaRef) -> (Document, Editor, Hover) {
        let mut doc = Document {
            area: Some(area),
            region: Some(Region {
                points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
                enabled: true,
            }),
            on_flat_ground: true,
            ..Document::default()
        };
        let id = doc.add_camera(camera(3.0));
        let editor = Editor {
            selected: Some(id),
            region_draft: vec![[1.0, 1.0]],
            selected_address: Some(Address::default()),
            ..Editor::default()
        };
        let hover = Hover {
            hover_address: Some(Some("1 Test Street".into())),
            ..Hover::default()
        };
        (doc, editor, hover)
    }

    struct Applied {
        doc: Document,
        editor: Editor,
        hover: Hover,
        toasts: Vec<String>,
        change: Option<DataChange>,
    }

    fn load(
        (mut doc, mut editor, mut hover): (Document, Editor, Hover),
        area: AreaRef,
        keep_cameras: bool,
        bundle: &AreaBundle,
    ) -> Applied {
        let mut toasts = Toasts::default();
        let request = LoadArea { area, keep_cameras };
        let change = apply(
            &mut doc,
            &mut editor,
            &mut hover,
            &mut toasts,
            &models(),
            &request,
            bundle,
        );
        let toasts = toasts.iter().map(|t| t.text.clone()).collect();
        Applied {
            doc,
            editor,
            hover,
            toasts,
            change,
        }
    }

    const LOADED: &str = "Loaded 8 buildings and 1 street segments";

    #[test]
    fn a_fresh_demo_street_replaces_the_old_area_with_example_cameras() {
        let bundle = street();
        let revision = before(osm()).0.revision;
        let a = load(before(osm()), AreaRef::Demo, false, &bundle);
        let ids: Vec<_> = a.doc.cameras.iter().map(|c| c.model_id.as_str()).collect();
        assert_eq!(ids, EXAMPLE_MODELS[..8], "one per house, models in turn");
        assert_eq!(a.doc.cameras[0].label, "Camera 2", "numbered on");
        assert_eq!(a.doc.region, None);
        assert!(!a.doc.on_flat_ground);
        assert_eq!(a.doc.area, Some(AreaRef::Demo));
        assert_eq!(a.doc.origin, bundle.origin);
        assert_eq!(
            a.doc.provenance,
            Some(Provenance::of(&bundle, &models().catalog_version))
        );
        assert!(a.doc.revision > revision);
        assert!(a.editor.region_draft.is_empty());
        assert_eq!(a.editor.selected, None);
        assert_eq!(a.editor.selected_address, None);
        assert_eq!(a.hover.hover_address, None);
        assert_eq!(a.toasts, [LOADED]);
        assert_eq!(a.change, None);
    }

    #[test]
    fn a_fresh_osm_area_starts_empty() {
        let old = before(AreaRef::Demo);
        let revision = old.0.revision;
        let a = load(old, osm(), false, &street());
        assert!(a.doc.cameras.is_empty());
        assert!(
            a.doc.revision > revision,
            "a new revision for undo and the addresses"
        );
        assert_eq!(a.doc.region, None);
        assert_eq!(a.doc.area, Some(osm()));
        assert_eq!(a.toasts, [LOADED]);
    }

    #[test]
    fn a_restored_scene_keeps_its_cameras_on_its_own_area_and_lifts_flat_ones() {
        let bundle = street_on(raised());
        let old = before(osm());
        let (cameras, region) = (old.0.cameras.clone(), old.0.region.clone());
        let revision = old.0.revision;
        let a = load(old, osm(), true, &bundle);
        assert!(a.doc.revision > revision);
        let mut lifted = cameras.clone();
        lift_onto_terrain(&mut lifted, &bundle.terrain);
        assert_ne!(lifted, cameras, "the ground is raised");
        assert_eq!(a.doc.cameras, lifted);
        assert!(!a.doc.on_flat_ground, "lifted once");
        assert_eq!(a.doc.region, region);
        assert_eq!(a.editor.region_draft, [[1.0, 1.0]], "kept with the region");
        assert!(a.editor.selected.is_some());
        assert_eq!(a.editor.selected_address, None, "addresses always go");
        assert_eq!(a.hover.hover_address, None);

        // Cameras already on the terrain stay where they are.
        let mut old = before(osm());
        old.0.on_flat_ground = false;
        let revision = old.0.revision;
        let a = load(old, osm(), true, &bundle);
        assert_eq!(a.doc.cameras, cameras);
        assert!(
            a.doc.revision > revision,
            "a new revision though nothing moved"
        );
    }

    #[test]
    fn a_restored_demo_scene_keeps_its_cameras_and_gets_no_examples() {
        let mut old = before(AreaRef::Demo);
        old.0.on_flat_ground = false;
        let (cameras, region) = (old.0.cameras.clone(), old.0.region.clone());
        let revision = old.0.revision;
        let a = load(old, AreaRef::Demo, true, &street());
        assert_eq!(a.doc.cameras, cameras, "no example cameras added");
        assert_eq!(a.doc.region, region);
        assert_eq!(a.editor.region_draft, [[1.0, 1.0]]);
        assert!(a.editor.selected.is_some());
        assert!(a.doc.revision > revision);
        assert_eq!(a.toasts, [LOADED]);
    }

    #[test]
    fn keeping_cameras_needs_the_same_area() {
        let a = load(before(AreaRef::Demo), osm(), true, &street_on(raised()));
        assert!(a.doc.cameras.is_empty());
        assert_eq!(a.doc.region, None);
        assert!(!a.doc.on_flat_ground);
    }

    #[test]
    fn a_kept_scene_is_told_how_its_map_data_changed() {
        let bundle = street();
        let mut saved = Provenance::of(&bundle, &models().catalog_version);
        saved.osm_fingerprint = Some("0000000000000000".into());
        let expected = saved.data_change(&Provenance::of(&bundle, &models().catalog_version));
        assert!(expected.is_some());

        let mut old = before(osm());
        old.0.saved_provenance = Some(saved.clone());
        let a = load(old, osm(), true, &bundle);
        assert_eq!(a.change, expected);
        assert_eq!(a.doc.saved_provenance, None);
        let message = expected
            .as_ref()
            .map(DataChange::message)
            .expect("a change");
        assert_eq!(a.toasts, [message, LOADED.to_owned()]);

        // A scene replaced by a fresh area has nothing to be told.
        let mut old = before(osm());
        old.0.saved_provenance = Some(saved);
        let a = load(old, osm(), false, &bundle);
        assert_eq!(a.change, None);
        assert_eq!(a.doc.saved_provenance, None);
        assert_eq!(a.toasts, [LOADED]);
    }
}
