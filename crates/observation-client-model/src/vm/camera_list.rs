//! The camera list on the inspector's Cameras tab.

use crate::document::Models;
use observation_core::camera::{CameraModel, CameraPlacement};

/// The camera list, grouped by address.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraListVm<'a> {
    pub groups: Vec<GroupVm<'a>>,
}

/// A group of cameras: an address, a hand-set group, or (`None`) neither.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupVm<'a> {
    pub name: Option<&'a str>,
    /// The address picked with the Address tool: shaded, and scrolled to.
    pub picked: bool,
    /// Shown with a heading (its name and count).
    pub heading: bool,
    /// Without a heading in a list that has headings: each row names the
    /// group instead.
    pub subtitled: bool,
    pub rows: Vec<RowVm<'a>>,
}

/// One camera's row.
#[derive(Debug, Clone, PartialEq)]
pub struct RowVm<'a> {
    pub camera: &'a CameraPlacement,
    /// Its model's name, also the row's tooltip.
    pub model: String,
    pub selected: bool,
    /// Hovered in the 3D view.
    pub highlighted: bool,
}

impl<'a> CameraListVm<'a> {
    /// The list for `groups` (as the client's address cache groups the
    /// cameras), with `picked` the group of the picked address, `selected`
    /// the selected camera and `hovered` the one under the pointer in the
    /// view.
    pub fn new(
        groups: &[(Option<&'a str>, Vec<&'a CameraPlacement>)],
        picked: Option<&str>,
        models: &Models,
        selected: Option<u32>,
        hovered: Option<u32>,
    ) -> Self {
        // Headings only help once something has an address or a group.
        let headed = !matches!(groups, [(None, _)] | []);
        let groups = groups
            .iter()
            .map(|(name, cameras)| {
                let picked = name.is_some() && *name == picked;
                // The picked address keeps its heading, which is what gets
                // shaded.
                let heading = headed && (cameras.len() > 1 || picked);
                GroupVm {
                    name: *name,
                    picked,
                    heading,
                    subtitled: headed && !heading,
                    rows: cameras
                        .iter()
                        .map(|&c| RowVm {
                            camera: c,
                            model: models.get(&c.model_id).map_or_else(
                                || "Unknown model".to_owned(),
                                CameraModel::display_name,
                            ),
                            selected: selected == Some(c.id),
                            highlighted: hovered == Some(c.id),
                        })
                        .collect(),
                }
            })
            .collect();
        Self { groups }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::camera::Catalog;

    fn camera(id: u32, model_id: &str) -> CameraPlacement {
        CameraPlacement {
            id,
            label: format!("Camera {id}"),
            model_id: model_id.to_owned(),
            position: [0.0, 3.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        }
    }

    fn models() -> Models {
        let mut models = Models::new(Catalog::default());
        models.custom.push(CameraModel {
            id: "custom-1-porch".to_owned(),
            vendor: "Acme".to_owned(),
            name: "Porch".to_owned(),
            kind: observation_core::camera::CameraKind::Doorbell,
            hfov_deg: 90.0,
            vfov_deg: 55.0,
            resolution: [1920, 1080],
            ir_range_m: None,
            source: None,
            notes: None,
        });
        models
    }

    /// Name, picked, heading, subtitled and row ids of a group.
    type Shape<'a> = (Option<&'a str>, bool, bool, bool, Vec<u32>);

    fn shape<'a>(vm: &CameraListVm<'a>) -> Vec<Shape<'a>> {
        vm.groups
            .iter()
            .map(|g| {
                let ids = g.rows.iter().map(|r| r.camera.id).collect();
                (g.name, g.picked, g.heading, g.subtitled, ids)
            })
            .collect()
    }

    #[test]
    fn cameras_with_no_address_get_no_headings() {
        let (a, b) = (camera(1, "x"), camera(2, "x"));
        let groups = [(None, vec![&a, &b])];
        let vm = CameraListVm::new(&groups, None, &models(), None, None);
        assert_eq!(shape(&vm), [(None, false, false, false, vec![1, 2])]);
    }

    #[test]
    fn a_group_of_one_is_a_subtitle_unless_it_is_picked() {
        let (a, b, c, d) = (
            camera(1, "x"),
            camera(2, "x"),
            camera(3, "x"),
            camera(4, "x"),
        );
        let groups = [
            (Some("1 High St"), vec![&a, &b]),
            (Some("2 High St"), vec![&c]),
            (None, vec![&d]),
        ];
        let vm = CameraListVm::new(&groups, None, &models(), None, None);
        assert_eq!(
            shape(&vm),
            [
                (Some("1 High St"), false, true, false, vec![1, 2]),
                (Some("2 High St"), false, false, true, vec![3]),
                (None, false, false, true, vec![4]),
            ]
        );
        let vm = CameraListVm::new(&groups, Some("2 High St"), &models(), None, None);
        assert_eq!(
            shape(&vm)[1],
            (Some("2 High St"), true, true, false, vec![3]),
            "the picked address keeps its heading"
        );
    }

    #[test]
    fn the_no_address_group_is_never_the_picked_one() {
        let (a, b) = (camera(1, "x"), camera(2, "x"));
        let groups = [(Some("1 High St"), vec![&a]), (None, vec![&b])];
        let vm = CameraListVm::new(&groups, None, &models(), None, None);
        assert!(vm.groups.iter().all(|g| !g.picked));
    }

    #[test]
    fn rows_name_their_model_and_show_selection_and_hover() {
        let (a, b) = (camera(1, "custom-1-porch"), camera(2, "gone"));
        let groups = [(None, vec![&a, &b])];
        let vm = CameraListVm::new(&groups, None, &models(), Some(2), Some(1));
        let rows: Vec<_> = vm.groups[0]
            .rows
            .iter()
            .map(|r| (r.model.as_str(), r.selected, r.highlighted))
            .collect();
        let porch = models()
            .get("custom-1-porch")
            .expect("a model")
            .display_name();
        assert_eq!(
            rows,
            [
                (porch.as_str(), false, true),
                ("Unknown model", true, false)
            ]
        );
    }
}
