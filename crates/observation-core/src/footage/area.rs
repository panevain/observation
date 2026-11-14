//! Labelled areas and targets: the places the footage figures are about.

use crate::coverage::{Region, RegionProblem, outline_area_m2, outline_problem};
use crate::util::planar::ring_bounds;
use serde::{Deserialize, Serialize};

/// Most labelled areas a scene may have.
pub const MAX_AREAS: usize = 64;
/// Most targets a scene may have.
pub const MAX_TARGETS: usize = 64;
/// Longest area or target name kept, in characters.
pub const MAX_NAME_CHARS: usize = 60;
/// Most corners a ground target may have.
pub const MAX_TARGET_POINTS: usize = 32;
/// Widest a ground target may be, metres, along `x` and along `z`. A target
/// is a door, a gate or a path, not a garden: an area answers for those.
pub const MAX_TARGET_SPAN_M: f32 = 30.0;
/// A wall target is a square patch of wall this wide and tall, metres,
/// centred on its point.
pub const WALL_TARGET_SIZE_M: f32 = 1.0;

/// What a drawn area is, as the user labelled it. The label is the user's
/// own statement about the ground ("as drawn"), never inferred from data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AreaLabel {
    /// The user's own land.
    Own,
    /// A neighbour's land.
    Neighbour,
    /// Public ground: a pavement, a verge, a park.
    Public,
    /// Anything else the user wants counted apart.
    Other,
}

impl AreaLabel {
    pub const ALL: [Self; 4] = [Self::Own, Self::Neighbour, Self::Public, Self::Other];

    pub fn label(self) -> &'static str {
        match self {
            Self::Own => "Own",
            Self::Neighbour => "Neighbour",
            Self::Public => "Public",
            Self::Other => "Other",
        }
    }
}

/// A ground polygon the user drew, with an optional label and name.
///
/// An enabled area with a usable outline takes part in the per-camera split;
/// a disabled one is kept but ignored. One area may also limit the coverage
/// figures (the scene's `limit_figures_to`), as the single region did before
/// areas had labels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelledArea {
    /// Unique within the scene.
    pub id: u32,
    /// `None` leaves the area unlabelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<AreaLabel>,
    /// Shown as plain text; trimmed and at most [`MAX_NAME_CHARS`] long.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Corners in world `[x, z]`, metres, in drawing order (not repeated at
    /// the end).
    pub points: Vec<[f32; 2]>,
    #[serde(default = "crate::util::default_true")]
    pub enabled: bool,
}

impl LabelledArea {
    /// An unlabelled, unnamed area with a region's outline and switch: what
    /// a scene's single `region` becomes.
    pub fn from_region(id: u32, region: Region) -> Self {
        Self {
            id,
            label: None,
            name: None,
            points: region.points,
            enabled: region.enabled,
        }
    }

    /// The outline and switch alone.
    pub fn to_region(&self) -> Region {
        Region {
            points: self.points.clone(),
            enabled: self.enabled,
        }
    }

    /// Why the outline cannot enclose an area, if it cannot.
    pub fn problem(&self) -> Option<RegionProblem> {
        outline_problem(&self.points)
    }

    /// Enabled, with an outline that encloses an area: the area counts.
    pub fn is_active(&self) -> bool {
        self.enabled && self.problem().is_none()
    }

    /// Enclosed area of the outline as drawn, m².
    pub fn area_m2(&self) -> f32 {
        outline_area_m2(&self.points)
    }
}

/// The area with id `id`, if any.
pub fn area_by_id(areas: &[LabelledArea], id: Option<u32>) -> Option<&LabelledArea> {
    let id = id?;
    areas.iter().find(|a| a.id == id)
}

/// The area with id `id`, if any, to change.
pub fn area_by_id_mut(areas: &mut [LabelledArea], id: Option<u32>) -> Option<&mut LabelledArea> {
    let id = id?;
    areas.iter_mut().find(|a| a.id == id)
}

/// An id one above the highest in `ids` (1 when there are none).
pub fn fresh_id(ids: impl IntoIterator<Item = u32>) -> u32 {
    ids.into_iter().max().map_or(1, |m| m.saturating_add(1))
}

/// Sets the area the figures are limited to (`limit`, an id in `areas`) to
/// `region`'s outline and switch, keeping that area's id, label and name; or
/// adds `region` as a new unlabelled area to limit the figures to; or, with
/// `None`, removes the area the figures are limited to. This is all the
/// single-region editor does with the list.
pub fn set_figures_area(
    areas: &mut Vec<LabelledArea>,
    limit: &mut Option<u32>,
    region: Option<Region>,
) {
    match region {
        Some(r) => {
            if let Some(a) = area_by_id_mut(areas, *limit) {
                a.points = r.points;
                a.enabled = r.enabled;
            } else {
                let id = fresh_id(areas.iter().map(|a| a.id));
                areas.push(LabelledArea::from_region(id, r));
                *limit = Some(id);
            }
        }
        None => {
            if let Some(id) = limit.take() {
                areas.retain(|a| a.id != id);
            }
        }
    }
}

/// A named place to ask about: "does this camera see the front door?".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    /// Unique within the scene.
    pub id: u32,
    /// Shown as plain text; trimmed and at most [`MAX_NAME_CHARS`] long
    /// (empty when the user gave none).
    #[serde(default)]
    pub name: String,
    pub at: TargetPlace,
}

/// Where a target is. Positions are world coordinates, never building
/// indices, which change when a bundle is rebuilt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetPlace {
    /// A small patch of ground (corners in world `[x, z]`, not repeated at
    /// the end), tested at the subject height above the terrain, as the
    /// coverage overlay tests the ground.
    Ground { points: Vec<[f32; 2]> },
    /// A [`WALL_TARGET_SIZE_M`] square of wall: its centre in world
    /// `[x, y, z]` (over the terrain, as camera positions are) and the wall's
    /// outward horizontal normal as `[x, z]` (any length but zero).
    Wall { point: [f32; 3], normal: [f32; 2] },
}

/// Why a target cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetProblem {
    TooManyCorners,
    TooLarge,
    Outline(RegionProblem),
    /// A wall target's normal has (almost) no length.
    NoDirection,
}

impl TargetProblem {
    pub fn message(self) -> &'static str {
        match self {
            Self::TooManyCorners => "A target has at most 32 corners",
            Self::TooLarge => "A target is at most 30 m across; draw an area for larger ground",
            Self::Outline(p) => p.message(),
            Self::NoDirection => "A wall target needs the direction the wall faces",
        }
    }
}

impl TargetPlace {
    /// Why the target cannot be used, if it cannot. (Coordinates are range
    /// checked where files are read.)
    pub fn problem(&self) -> Option<TargetProblem> {
        match self {
            Self::Ground { points } => {
                if points.len() > MAX_TARGET_POINTS {
                    return Some(TargetProblem::TooManyCorners);
                }
                if let Some(p) = outline_problem(points) {
                    return Some(TargetProblem::Outline(p));
                }
                let (lo, hi) = ring_bounds(points);
                (hi[0] - lo[0] > MAX_TARGET_SPAN_M || hi[1] - lo[1] > MAX_TARGET_SPAN_M)
                    .then_some(TargetProblem::TooLarge)
            }
            Self::Wall { normal, .. } => {
                let len = normal[0].hypot(normal[1]);
                (len.is_nan() || len < 1e-3).then_some(TargetProblem::NoDirection)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f32, size: f32) -> Vec<[f32; 2]> {
        vec![[x, 0.0], [x + size, 0.0], [x + size, size], [x, size]]
    }

    #[test]
    fn the_figures_area_is_set_kept_and_removed() {
        let (mut areas, mut limit) = (Vec::new(), None);
        let region = |x: f32, enabled| Region {
            points: square(x, 10.0),
            enabled,
        };
        set_figures_area(&mut areas, &mut limit, Some(region(0.0, true)));
        assert_eq!((areas.len(), limit), (1, Some(1)));
        // Labelled and named meanwhile: a new outline keeps both.
        areas[0].label = Some(AreaLabel::Own);
        areas[0].name = Some("Garden".into());
        areas.push(LabelledArea {
            id: 7,
            label: Some(AreaLabel::Neighbour),
            name: None,
            points: square(20.0, 5.0),
            enabled: true,
        });
        set_figures_area(&mut areas, &mut limit, Some(region(1.0, false)));
        let a = area_by_id(&areas, limit).expect("the figures area");
        assert_eq!(
            (a.id, a.label, a.name.as_deref()),
            (1, Some(AreaLabel::Own), Some("Garden"))
        );
        assert_eq!((a.points[0], a.enabled), ([1.0, 0.0], false));
        set_figures_area(&mut areas, &mut limit, None);
        assert_eq!((areas.len(), areas[0].id, limit), (1, 7, None));
        // A new one takes a fresh id.
        set_figures_area(&mut areas, &mut limit, Some(region(0.0, true)));
        assert_eq!(limit, Some(8));
        set_figures_area(&mut areas, &mut limit, None);
        set_figures_area(&mut areas, &mut limit, None);
        assert_eq!(areas.len(), 1, "nothing more to remove");
    }

    #[test]
    fn target_problems() {
        let ground = |points| TargetPlace::Ground { points };
        assert_eq!(ground(square(0.0, 2.0)).problem(), None);
        assert_eq!(ground(square(0.0, MAX_TARGET_SPAN_M)).problem(), None);
        assert_eq!(
            ground(square(0.0, MAX_TARGET_SPAN_M + 0.5)).problem(),
            Some(TargetProblem::TooLarge)
        );
        assert_eq!(
            ground(vec![[0.0, 0.0], [1.0, 0.0]]).problem(),
            Some(TargetProblem::Outline(RegionProblem::TooFewCorners))
        );
        let many: Vec<[f32; 2]> = (0..=MAX_TARGET_POINTS)
            .map(|i| {
                let a = i as f32 / 33.0 * std::f32::consts::TAU;
                [a.cos(), a.sin()]
            })
            .collect();
        assert_eq!(ground(many).problem(), Some(TargetProblem::TooManyCorners));
        let wall = |normal| TargetPlace::Wall {
            point: [0.0, 1.5, 0.0],
            normal,
        };
        assert_eq!(wall([0.0, 3.0]).problem(), None);
        assert_eq!(wall([0.0, 0.0]).problem(), Some(TargetProblem::NoDirection));
        assert_eq!(
            wall([f32::NAN, 1.0]).problem(),
            Some(TargetProblem::NoDirection)
        );
    }

    #[test]
    fn json_shape() {
        let t = Target {
            id: 2,
            name: "Front door".into(),
            at: TargetPlace::Wall {
                point: [1.0, 1.5, -2.0],
                normal: [0.0, 1.0],
            },
        };
        let json = serde_json::to_string(&t).unwrap_or_default();
        assert_eq!(
            json,
            r#"{"id":2,"name":"Front door","at":{"kind":"wall","point":[1.0,1.5,-2.0],"normal":[0.0,1.0]}}"#
        );
        let a: LabelledArea =
            serde_json::from_str(r#"{"id":1,"label":"neighbour","points":[[0,0],[1,0],[1,1]]}"#)
                .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a.label, Some(AreaLabel::Neighbour));
        assert!(a.enabled, "on unless the file says otherwise");
    }
}
