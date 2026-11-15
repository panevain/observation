//! Change sets: what an agent proposes to do to a scene (issue #259), item
//! by item, so that accepting one never reverts the user's unrelated edits.
//!
//! A [`ChangeSet`] is made by comparing two scenes ([`ChangeSet::between`]):
//! the scene the agent started from (the app's shared scene, the **base**)
//! and the one it proposes. Each [`Change`] names one item (a camera, a
//! custom model, a drawn area or a target, by id; or the analysis settings,
//! or the area the figures are limited to) and records both the item's value
//! in the base, or that it was absent, and its proposed value, or that it is
//! removed.
//!
//! The base values decide whether a proposal still fits the scene the user
//! has now. It is **stale** as soon as any item it touches differs from its
//! base value ([`ChangeSet::conflicts`]), absence included: an id the user
//! has since taken for a camera of their own is a conflict too. A stale
//! proposal is never rebased; the agent proposes again. Applying
//! ([`ChangeSet::apply`]) puts each proposed value in place in the current
//! scene and checks the result as an imported file is checked.
//!
//! Everything here comes from an agent, so [`ChangeSet::check`] refuses a
//! malformed set before anything else reads it, and the lines that describe
//! it ([`ChangeSet::lines`]) are plain text with names quoted and cut short.

use super::{MAX_ITEM_ID, SceneError, SceneFile};
use crate::camera::{CameraModel, CameraPlacement};
use crate::coverage::CoverageParams;
use crate::footage::{LabelledArea, Target, TargetPlace};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::Display;
use thiserror::Error;

/// Most changes one set may hold: every camera, custom model, area and
/// target of a full scene removed and as many added, with room to spare.
pub const MAX_CHANGES: usize = 1024;
/// Longest name quoted in a change line, characters.
const MAX_QUOTED_CHARS: usize = 60;

/// One item's change: its value in the base (`None`: absent) and its
/// proposed value (`None`: removed). At least one is set, and they differ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemChange<K, T> {
    pub id: K,
    // A missing `Option` reads as `None` without `default`, which here would
    // ask for `T: Default`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<T>,
}

impl<K, T> ItemChange<K, T> {
    pub fn is_add(&self) -> bool {
        self.base.is_none() && self.new.is_some()
    }

    pub fn is_remove(&self) -> bool {
        self.base.is_some() && self.new.is_none()
    }
}

/// One change, by the kind of item it touches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum Change {
    Camera(ItemChange<u32, CameraPlacement>),
    CustomModel(ItemChange<String, CameraModel>),
    Area(ItemChange<u32, LabelledArea>),
    Target(ItemChange<u32, Target>),
    /// The analysis settings (detail level, lighting, subject height,
    /// custom pixel density).
    Params {
        base: CoverageParams,
        new: CoverageParams,
    },
    /// The id of the area the coverage figures are limited to.
    LimitFiguresTo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new: Option<u32>,
    },
}

/// A whole proposal's changes, accepted or rejected as one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub changes: Vec<Change>,
}

/// Why a change set cannot be made, read or applied.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ChangeError {
    #[error("the proposed scene is on another area; a proposal cannot change the area")]
    OtherArea,
    #[error("the proposal changes nothing")]
    Empty,
    #[error("the proposal has {0} changes, but at most {MAX_CHANGES} are allowed")]
    TooMany(usize),
    #[error("a change is malformed: {0}")]
    Malformed(String),
    #[error("{0} has changed since the proposal was made")]
    Stale(String),
    #[error("the scene the proposal would leave is invalid: {0}")]
    Invalid(String),
}

/// What an item is, for the change logic: something with an id, compared
/// whole.
trait Item: Clone + PartialEq {
    type Id: Clone + PartialEq + Eq + std::hash::Hash + Display;
    /// "camera", "custom model", ...
    const NOUN: &'static str;
    fn item_id(&self) -> Self::Id;
}

impl Item for CameraPlacement {
    type Id = u32;
    const NOUN: &'static str = "camera";
    fn item_id(&self) -> u32 {
        self.id
    }
}

impl Item for CameraModel {
    type Id = String;
    const NOUN: &'static str = "custom model";
    fn item_id(&self) -> String {
        self.id.clone()
    }
}

impl Item for LabelledArea {
    type Id = u32;
    const NOUN: &'static str = "area";
    fn item_id(&self) -> u32 {
        self.id
    }
}

impl Item for Target {
    type Id = u32;
    const NOUN: &'static str = "target";
    fn item_id(&self) -> u32 {
        self.id
    }
}

/// The changes that turn the items `base` into `new`: updates and removals
/// in the base's order, then additions in the new order.
fn diff_items<T: Item>(base: &[T], new: &[T]) -> Vec<ItemChange<T::Id, T>> {
    let find = |list: &[T], id: &T::Id| list.iter().find(|x| x.item_id() == *id).cloned();
    let mut out = Vec::new();
    for b in base {
        let id = b.item_id();
        match find(new, &id) {
            Some(n) if n == *b => {}
            n => out.push(ItemChange {
                id,
                base: Some(b.clone()),
                new: n,
            }),
        }
    }
    for n in new {
        let id = n.item_id();
        if find(base, &id).is_none() {
            out.push(ItemChange {
                id,
                base: None,
                new: Some(n.clone()),
            });
        }
    }
    out
}

/// The item with `c`'s id differs from `c`'s base value, as "camera 3".
fn conflict<T: Item>(list: &[T], c: &ItemChange<T::Id, T>) -> Option<String> {
    let current = list.iter().find(|x| x.item_id() == c.id);
    (current != c.base.as_ref()).then(|| format!("{} {}", T::NOUN, c.id))
}

/// Puts `c`'s proposed value in place: replaced where the item is, added at
/// the end, or removed. The current value must be the base value.
fn apply_item<T: Item>(list: &mut Vec<T>, c: &ItemChange<T::Id, T>) -> Result<(), ChangeError> {
    if let Some(what) = conflict(list, c) {
        return Err(ChangeError::Stale(what));
    }
    let at = list.iter().position(|x| x.item_id() == c.id);
    match (at, &c.new) {
        (Some(i), Some(new)) => list[i] = new.clone(),
        (Some(i), None) => {
            list.remove(i);
        }
        (None, Some(new)) => list.push(new.clone()),
        (None, None) => {}
    }
    Ok(())
}

/// Whether an item change is well formed: something changes, and both
/// values carry the change's id.
fn check_item<T: Item>(c: &ItemChange<T::Id, T>) -> Result<(), ChangeError> {
    let bad = |why: &str| {
        Err(ChangeError::Malformed(format!(
            "{} {}: {why}",
            T::NOUN,
            c.id
        )))
    };
    if c.base.is_none() && c.new.is_none() {
        return bad("neither a base nor a new value");
    }
    if c.base == c.new {
        return bad("the new value is the base value");
    }
    if [&c.base, &c.new]
        .into_iter()
        .flatten()
        .any(|v| v.item_id() != c.id)
    {
        return bad("a value has another id");
    }
    Ok(())
}

/// [`check_item`] for an item numbered by a `u32` id, which must also be at
/// most [`MAX_ITEM_ID`], as in a scene file.
fn check_numbered<T: Item<Id = u32>>(c: &ItemChange<u32, T>) -> Result<(), ChangeError> {
    if c.id > MAX_ITEM_ID {
        return Err(ChangeError::Malformed(format!(
            "{} {}: the id is above the largest, {MAX_ITEM_ID}",
            T::NOUN,
            c.id
        )));
    }
    check_item(c)
}

impl Change {
    /// What the change touches, unique within a set: kind and id.
    fn key(&self) -> String {
        match self {
            Self::Camera(c) => format!("camera {}", c.id),
            Self::CustomModel(c) => format!("custom model {}", c.id),
            Self::Area(c) => format!("area {}", c.id),
            Self::Target(c) => format!("target {}", c.id),
            Self::Params { .. } => "analysis settings".to_owned(),
            Self::LimitFiguresTo { .. } => "the area the figures are limited to".to_owned(),
        }
    }

    fn check(&self) -> Result<(), ChangeError> {
        match self {
            Self::Camera(c) => check_numbered(c),
            Self::CustomModel(c) => check_item(c),
            Self::Area(c) => check_numbered(c),
            Self::Target(c) => check_numbered(c),
            Self::Params { base, new } if base == new => Err(ChangeError::Malformed(
                "analysis settings: the new value is the base value".into(),
            )),
            Self::LimitFiguresTo { base, new } if base == new => Err(ChangeError::Malformed(
                "figures limit: the new value is the base value".into(),
            )),
            Self::LimitFiguresTo { base, new } => match [base, new]
                .into_iter()
                .flatten()
                .find(|&&id| id > MAX_ITEM_ID)
            {
                Some(id) => Err(ChangeError::Malformed(format!(
                    "figures limit: area {id} is above the largest id, {MAX_ITEM_ID}"
                ))),
                None => Ok(()),
            },
            _ => Ok(()),
        }
    }

    /// The item as `scene` has it now differs from the change's base value:
    /// what it is, as "camera 3".
    fn conflict(&self, scene: &SceneFile) -> Option<String> {
        match self {
            Self::Camera(c) => conflict(&scene.cameras, c),
            Self::CustomModel(c) => conflict(&scene.custom_models, c),
            Self::Area(c) => conflict(&scene.areas, c),
            Self::Target(c) => conflict(&scene.targets, c),
            Self::Params { base, .. } => {
                (scene.params != *base).then(|| "the analysis settings".to_owned())
            }
            Self::LimitFiguresTo { base, .. } => (scene.limit_figures_to != *base)
                .then(|| "the area the figures are limited to".to_owned()),
        }
    }

    fn apply(&self, scene: &mut SceneFile) -> Result<(), ChangeError> {
        if let Some(what) = self.conflict(scene) {
            return Err(ChangeError::Stale(what));
        }
        match self {
            Self::Camera(c) => apply_item(&mut scene.cameras, c),
            Self::CustomModel(c) => apply_item(&mut scene.custom_models, c),
            Self::Area(c) => apply_item(&mut scene.areas, c),
            Self::Target(c) => apply_item(&mut scene.targets, c),
            Self::Params { new, .. } => {
                scene.params = *new;
                Ok(())
            }
            Self::LimitFiguresTo { new, .. } => {
                scene.limit_figures_to = *new;
                Ok(())
            }
        }
    }
}

impl ChangeSet {
    /// The changes that turn `base` into `proposed`, both on the same area:
    /// the analysis settings, then custom models, cameras, areas and
    /// targets, then the figures' limit. The provenance and the change log
    /// are the app's own and are not compared.
    pub fn between(base: &SceneFile, proposed: &SceneFile) -> Result<Self, ChangeError> {
        if base.area != proposed.area || base.origin != proposed.origin {
            return Err(ChangeError::OtherArea);
        }
        let mut changes = Vec::new();
        if base.params != proposed.params {
            changes.push(Change::Params {
                base: base.params,
                new: proposed.params,
            });
        }
        changes.extend(
            diff_items(&base.custom_models, &proposed.custom_models)
                .into_iter()
                .map(Change::CustomModel),
        );
        changes.extend(
            diff_items(&base.cameras, &proposed.cameras)
                .into_iter()
                .map(Change::Camera),
        );
        changes.extend(
            diff_items(&base.areas, &proposed.areas)
                .into_iter()
                .map(Change::Area),
        );
        changes.extend(
            diff_items(&base.targets, &proposed.targets)
                .into_iter()
                .map(Change::Target),
        );
        if base.limit_figures_to != proposed.limit_figures_to {
            changes.push(Change::LimitFiguresTo {
                base: base.limit_figures_to,
                new: proposed.limit_figures_to,
            });
        }
        Ok(Self { changes })
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Refuses a set that is empty, too long or malformed: a change that
    /// changes nothing, a value whose id is not the change's, an id above
    /// [`MAX_ITEM_ID`], or two changes to the same item. Run on every set read from elsewhere.
    pub fn check(&self) -> Result<(), ChangeError> {
        if self.changes.is_empty() {
            return Err(ChangeError::Empty);
        }
        if self.changes.len() > MAX_CHANGES {
            return Err(ChangeError::TooMany(self.changes.len()));
        }
        let mut seen = HashSet::new();
        for c in &self.changes {
            c.check()?;
            let key = c.key();
            if !seen.insert(key.clone()) {
                return Err(ChangeError::Malformed(format!("{key} is changed twice")));
            }
        }
        Ok(())
    }

    /// The items the set touches whose value in `scene` is no longer their
    /// base value, as "camera 3": empty while the proposal still fits.
    pub fn conflicts(&self, scene: &SceneFile) -> Vec<String> {
        self.changes
            .iter()
            .filter_map(|c| c.conflict(scene))
            .collect()
    }

    /// `scene` with every change applied, checked as an imported file is
    /// ([`SceneFile::from_json`]) and returned as it would load. Refused if
    /// the set is malformed, stale against `scene`, or leaves a scene the app
    /// would refuse.
    pub fn apply(&self, scene: &SceneFile) -> Result<SceneFile, ChangeError> {
        self.check()?;
        let mut out = scene.clone();
        for c in &self.changes {
            c.apply(&mut out)?;
        }
        SceneFile::from_json(&out.to_json()).map_err(|e: SceneError| match e {
            SceneError::Invalid(why) => ChangeError::Invalid(why),
            other => ChangeError::Invalid(other.to_string()),
        })
    }

    /// One line per change, in plain words, for the proposal card and the
    /// agent. `model_name` gives a camera model's name by id (the base and
    /// proposed scenes' custom models, then the catalogue), if known.
    pub fn lines(&self, model_name: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
        self.changes
            .iter()
            .map(|c| describe(c, model_name))
            .collect()
    }
}

/// A number as the lines show it, with a real minus sign.
fn num(v: f32, decimals: usize) -> String {
    let text = format!("{v:.decimals$}");
    // "-0.0" reads as zero.
    if text
        .trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.')
    {
        return text.trim_start_matches('-').to_owned();
    }
    text.replacen('-', "\u{2212}", 1)
}

/// A compass bearing in `[0, 360)`.
fn bearing(yaw: f32) -> f32 {
    yaw.rem_euclid(360.0)
}

/// `text` quoted, cut to [`MAX_QUOTED_CHARS`] (agent text is untrusted and
/// is only ever drawn as text).
fn quoted(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut: String = flat.chars().take(MAX_QUOTED_CHARS).collect();
    if flat.chars().count() > MAX_QUOTED_CHARS {
        cut.push('…');
    }
    format!("\u{2018}{cut}\u{2019}")
}

fn camera_name(c: &CameraPlacement) -> String {
    if c.label.trim().is_empty() {
        format!("Camera {}", c.id)
    } else {
        format!("Camera {} {}", c.id, quoted(&c.label))
    }
}

fn model_of(id: &str, model_name: &dyn Fn(&str) -> Option<String>) -> String {
    model_name(id).map_or_else(|| quoted(id), |n| quoted(&n))
}

fn describe(c: &Change, model_name: &dyn Fn(&str) -> Option<String>) -> String {
    match c {
        Change::Camera(c) => describe_camera(c, model_name),
        Change::CustomModel(c) => describe_model(c),
        Change::Area(c) => describe_area(c),
        Change::Target(c) => describe_target(c),
        Change::Params { base, new } => describe_params(base, new),
        Change::LimitFiguresTo { new, .. } => match new {
            Some(id) => format!("Limit the figures to area {id}"),
            None => "Stop limiting the figures to an area".to_owned(),
        },
    }
}

fn describe_camera(
    c: &ItemChange<u32, CameraPlacement>,
    model_name: &dyn Fn(&str) -> Option<String>,
) -> String {
    match (&c.base, &c.new) {
        (None, Some(n)) => format!(
            "Add {} ({}) at x {}, z {}, aimed {}°{}",
            camera_name(n),
            model_of(&n.model_id, model_name),
            num(n.position[0], 1),
            num(n.position[2], 1),
            num(bearing(n.yaw_deg), 0),
            if n.enabled { "" } else { ", turned off" }
        ),
        (Some(b), None) => format!("Remove {}", camera_name(b)),
        (Some(b), Some(n)) => {
            let mut parts = Vec::new();
            let (dx, dz) = (n.position[0] - b.position[0], n.position[2] - b.position[2]);
            let moved = dx.hypot(dz);
            if moved >= 0.05 {
                parts.push(format!(
                    "move by {} m to x {}, z {}",
                    num(moved, 1),
                    num(n.position[0], 1),
                    num(n.position[2], 1)
                ));
            }
            let dy = n.position[1] - b.position[1];
            if dy.abs() >= 0.05 {
                let verb = if dy > 0.0 { "raise" } else { "lower" };
                parts.push(format!("{verb} by {} m", num(dy.abs(), 1)));
            }
            let turn = (bearing(n.yaw_deg) - bearing(b.yaw_deg)).abs();
            if turn.min(360.0 - turn) >= 0.5 {
                parts.push(format!(
                    "aim at {}° (was {}°)",
                    num(bearing(n.yaw_deg), 0),
                    num(bearing(b.yaw_deg), 0)
                ));
            }
            if (n.pitch_deg - b.pitch_deg).abs() >= 0.5 {
                parts.push(format!(
                    "tilt to {}° (was {}°)",
                    num(n.pitch_deg, 0),
                    num(b.pitch_deg, 0)
                ));
            }
            if (n.roll_deg - b.roll_deg).abs() >= 0.5 {
                parts.push(format!("roll to {}°", num(n.roll_deg, 0)));
            }
            if n.model_id != b.model_id {
                parts.push(format!(
                    "change model to {} (was {})",
                    model_of(&n.model_id, model_name),
                    model_of(&b.model_id, model_name)
                ));
            }
            if n.label != b.label {
                parts.push(format!("rename to {}", quoted(&n.label)));
            }
            if n.enabled != b.enabled {
                parts.push(if n.enabled { "turn on" } else { "turn off" }.to_owned());
            }
            if n.group != b.group {
                parts.push(match &n.group {
                    Some(g) => format!("list under {}", quoted(g)),
                    None => "list by address".to_owned(),
                });
            }
            if n.color != b.color {
                parts.push("change colour".to_owned());
            }
            if parts.is_empty() {
                parts.push("adjust slightly".to_owned());
            }
            format!("{}: {}", camera_name(b), parts.join("; "))
        }
        (None, None) => String::new(),
    }
}

fn describe_model(c: &ItemChange<String, CameraModel>) -> String {
    let name = |m: &CameraModel| quoted(&m.display_name());
    match (&c.base, &c.new) {
        (None, Some(n)) => format!(
            "Add custom model {} ({}° by {}°, {}×{})",
            name(n),
            num(n.hfov_deg, 0),
            num(n.vfov_deg, 0),
            n.resolution[0],
            n.resolution[1]
        ),
        (Some(b), None) => format!("Remove custom model {}", name(b)),
        (Some(b), Some(n)) => {
            let mut parts = Vec::new();
            if n.display_name() != b.display_name() {
                parts.push(format!("rename to {}", name(n)));
            }
            if n.kind != b.kind {
                parts.push(format!("make it a {}", n.kind.label().to_lowercase()));
            }
            if n.hfov_deg != b.hfov_deg || n.vfov_deg != b.vfov_deg {
                parts.push(format!(
                    "field of view {}° by {}°",
                    num(n.hfov_deg, 0),
                    num(n.vfov_deg, 0)
                ));
            }
            if n.resolution != b.resolution {
                parts.push(format!(
                    "resolution {}×{}",
                    n.resolution[0], n.resolution[1]
                ));
            }
            if n.ir_range_m != b.ir_range_m {
                parts.push(match n.ir_range_m {
                    Some(r) => format!("night range {} m", num(r, 1)),
                    None => "no published night range".to_owned(),
                });
            }
            if parts.is_empty() {
                parts.push("change its notes or source".to_owned());
            }
            format!("Custom model {}: {}", name(b), parts.join("; "))
        }
        (None, None) => String::new(),
    }
}

fn area_name(a: &LabelledArea) -> String {
    match &a.name {
        Some(n) => format!("area {} {}", a.id, quoted(n)),
        None => format!("area {}", a.id),
    }
}

fn describe_area(c: &ItemChange<u32, LabelledArea>) -> String {
    let label = |a: &LabelledArea| a.label.map_or("unlabelled", |l| l.label());
    match (&c.base, &c.new) {
        (None, Some(n)) => format!(
            "Draw {} ({}, {} m²{})",
            area_name(n),
            label(n),
            num(n.area_m2(), 0),
            if n.enabled { "" } else { ", turned off" }
        ),
        (Some(b), None) => format!("Remove {}", area_name(b)),
        (Some(b), Some(n)) => {
            let mut parts = Vec::new();
            if n.points != b.points {
                parts.push(format!("redraw its outline ({} m²)", num(n.area_m2(), 0)));
            }
            if n.label != b.label {
                parts.push(format!("label it {}", label(n)));
            }
            if n.name != b.name {
                parts.push(match &n.name {
                    Some(name) => format!("rename to {}", quoted(name)),
                    None => "remove its name".to_owned(),
                });
            }
            if n.enabled != b.enabled {
                parts.push(if n.enabled { "turn on" } else { "turn off" }.to_owned());
            }
            let mut name = area_name(b);
            name[..1].make_ascii_uppercase();
            format!("{name}: {}", parts.join("; "))
        }
        (None, None) => String::new(),
    }
}

/// Where a target is, as "a patch of ground at x 1.0, z 2.0".
fn target_place(at: &TargetPlace) -> String {
    match at {
        TargetPlace::Ground { points } => {
            let n = points.len().max(1) as f32;
            let (x, z) = points
                .iter()
                .fold((0.0, 0.0), |(x, z), p| (x + p[0], z + p[1]));
            format!(
                "a patch of ground at x {}, z {}",
                num(x / n, 1),
                num(z / n, 1)
            )
        }
        TargetPlace::Wall { point, .. } => {
            format!("a wall at x {}, z {}", num(point[0], 1), num(point[2], 1))
        }
    }
}

fn target_name(t: &Target) -> String {
    if t.name.is_empty() {
        format!("target {}", t.id)
    } else {
        format!("target {}", quoted(&t.name))
    }
}

fn describe_target(c: &ItemChange<u32, Target>) -> String {
    match (&c.base, &c.new) {
        (None, Some(n)) => format!("Add {}: {}", target_name(n), target_place(&n.at)),
        (Some(b), None) => format!("Remove {}", target_name(b)),
        (Some(b), Some(n)) => {
            let mut parts = Vec::new();
            if n.name != b.name {
                parts.push(format!("rename to {}", quoted(&n.name)));
            }
            if n.at != b.at {
                parts.push(format!("move to {}", target_place(&n.at)));
            }
            let mut name = target_name(b);
            name[..1].make_ascii_uppercase();
            format!("{name}: {}", parts.join("; "))
        }
        (None, None) => String::new(),
    }
}

fn describe_params(b: &CoverageParams, n: &CoverageParams) -> String {
    let mut parts = Vec::new();
    if n.dori != b.dori {
        parts.push(format!(
            "detail level {} (was {})",
            n.dori.label(),
            b.dori.label()
        ));
    }
    if n.lighting != b.lighting {
        parts.push(format!("{:?} (was {:?})", n.lighting, b.lighting).to_lowercase());
    }
    if n.subject_height_m != b.subject_height_m {
        parts.push(format!(
            "subject height {} m (was {} m)",
            num(n.subject_height_m, 1),
            num(b.subject_height_m, 1)
        ));
    }
    if n.custom_ppm != b.custom_ppm {
        parts.push(format!(
            "custom density {} px/m (was {} px/m)",
            num(n.custom_ppm, 0),
            num(b.custom_ppm, 0)
        ));
    }
    format!("Analysis settings: {}", parts.join("; "))
}

#[cfg(test)]
mod tests;
