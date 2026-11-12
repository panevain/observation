//! Undo and redo for the whole document: cameras, analysis settings, the
//! region, custom models and the loaded area.
//!
//! Edits are recorded as snapshots once they settle: the client says when an
//! edit is in progress (a drag, a held key, a focused text field, an area in
//! flight), and a step counts when it ends, not once per frame.

use crate::commands::Tracked;
use crate::document::{Document, Models, Snapshot};
use crate::editor::{Editor, Tool};
use crate::toasts::{ToastAction, Toasts};
use observation_core::camera::CameraModel;
use observation_core::scene::AreaRef;

/// Steps kept in each direction.
pub const MAX_STEPS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOp {
    Undo,
    Redo,
}

/// What a step did that the client carries out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepOutcome {
    /// The area to load again: the step went back or forward across an area
    /// change, and keeps the restored cameras.
    pub reload: Option<AreaRef>,
}

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The document as of the last recorded step; `None` until the first area
    /// has loaded, so the start-up restore is not an undoable step.
    last: Option<Snapshot>,
    last_revision: u64,
}

impl History {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// What undoing would revert, for tooltips.
    pub fn undo_label(&self) -> Option<String> {
        Some(describe(self.undo.last()?, self.last.as_ref()?))
    }

    /// What redoing would reapply, for tooltips.
    pub fn redo_label(&self) -> Option<String> {
        Some(describe(self.last.as_ref()?, self.redo.last()?))
    }

    /// Records a settled edit, once a frame. `editing` is whether an edit is
    /// in progress or an area is in flight; cameras from a flat-ground scene
    /// still waiting for their area count too. Returns whether the history
    /// may step now; while busy it neither records nor steps, or the
    /// half-done edit would be lost, and a requested step is dropped.
    pub fn observe(
        &mut self,
        doc: &Document,
        models: &Models,
        editing: bool,
        toasts: &mut Toasts,
    ) -> bool {
        // Cameras from a flat-ground scene, still waiting for their area to
        // lift them onto the terrain: recording them would let an undo sink them.
        let busy = editing || doc.on_flat_ground;
        if busy {
            // The first snapshot can be taken mid-load: the start-up restore must
            // be undoable to if the user replaces it before its area arrives.
            if self.last.is_none() && doc.area.is_some() && !doc.on_flat_ground {
                self.last = Some(doc.snapshot(models));
                self.last_revision = doc.revision;
            }
            return false;
        }

        // Record a settled edit before any jump, so it is part of the history.
        if doc.revision != self.last_revision && doc.area.is_some() {
            let now = doc.snapshot(models);
            self.last_revision = doc.revision;
            match self.last.take() {
                None => self.last = Some(now),
                Some(last) if last == now => self.last = Some(last),
                Some(last) => {
                    // An offered Undo belongs to the step before this one.
                    toasts.forget_undo();
                    if let Some(text) = destructive(&last, &now) {
                        toasts.info_with(text, ToastAction::Undo);
                    }
                    self.undo.push(last);
                    if self.undo.len() > MAX_STEPS {
                        self.undo.remove(0);
                    }
                    self.redo.clear();
                    self.last = Some(now);
                }
            }
        }
        true
    }

    /// Undoes or redoes one step, if there is one: puts the document back,
    /// and drops a selection the restored document lacks. `None` when there
    /// was nothing to do; the document is then not written to.
    pub fn step(
        &mut self,
        op: HistoryOp,
        doc: &mut dyn Tracked<Document>,
        models: &mut dyn Tracked<Models>,
        editor: &mut Editor,
        toasts: &mut Toasts,
    ) -> Option<StepOutcome> {
        let (from, to) = match op {
            HistoryOp::Undo => (&mut self.undo, &mut self.redo),
            HistoryOp::Redo => (&mut self.redo, &mut self.undo),
        };
        // Check both before taking either: an empty stack must not drop `last`.
        if from.is_empty() || self.last.is_none() {
            return None;
        }
        let (Some(target), Some(current)) = (from.pop(), self.last.take()) else {
            return None;
        };
        let (verb, what) = match op {
            HistoryOp::Undo => ("Undid", describe(&target, &current)),
            HistoryOp::Redo => ("Redid", describe(&current, &target)),
        };
        // Any offered Undo now points at a different step.
        toasts.forget_undo();
        toasts.info(format!("{verb}: {what}"));
        let reload = target.area.filter(|_| target.area != current.area);
        to.push(current);
        doc.get_mut().restore(&target, models.get_mut());
        let doc = doc.get();
        // UI state that pointed into the old document.
        if editor.selected.is_some_and(|id| doc.camera(id).is_none()) {
            editor.selected = None;
        }
        if editor.selected.is_none() && editor.tool.needs_selection() {
            editor.tool = Tool::Select;
        }
        self.last = Some(target);
        self.last_revision = doc.revision;
        Some(StepOutcome { reload })
    }
}

/// A step that removes work worth offering an Undo for, described.
fn destructive(before: &Snapshot, after: &Snapshot) -> Option<String> {
    let lost = before
        .cameras
        .iter()
        .filter(|c| !after.cameras.iter().any(|a| a.id == c.id))
        .count();
    if before.area != after.area && lost > 0 {
        return Some(format!("New area loaded; {lost} camera(s) removed"));
    }
    match lost {
        0 => deleted_model(before, after).map(|m| format!("Deleted model {}", m.display_name())),
        1 => before
            .cameras
            .iter()
            .find(|c| !after.cameras.iter().any(|a| a.id == c.id))
            .map(|c| format!("Deleted {}", c.label)),
        n => Some(format!("{n} cameras removed")),
    }
}

/// The custom model a step deleted, if deleting it is all the step did (as
/// the inspector does; an import that drops a model changes more).
fn deleted_model<'a>(before: &'a Snapshot, after: &Snapshot) -> Option<&'a CameraModel> {
    // Exhaustive, so a new field cannot be left out of the comparison.
    let Snapshot {
        area,
        origin,
        cameras,
        params,
        region,
        custom_models: b,
    } = before;
    let a = &after.custom_models;
    let rest_equal = *area == after.area
        && *origin == after.origin
        && *cameras == after.cameras
        && *params == after.params
        && *region == after.region;
    if !rest_equal || b.len() != a.len() + 1 {
        return None;
    }
    // The first difference is the deleted model; the rest must follow in order.
    let i = b.iter().zip(a).take_while(|(x, y)| x == y).count();
    b.get(i).filter(|_| b.get(i + 1..) == a.get(i..))
}

/// A short name for the change from `before` to `after`.
fn describe(before: &Snapshot, after: &Snapshot) -> String {
    if before.area != after.area {
        return "area change".to_owned();
    }
    let added: Vec<_> = after
        .cameras
        .iter()
        .filter(|c| !before.cameras.iter().any(|b| b.id == c.id))
        .collect();
    let removed: Vec<_> = before
        .cameras
        .iter()
        .filter(|c| !after.cameras.iter().any(|a| a.id == c.id))
        .collect();
    match (added.as_slice(), removed.as_slice()) {
        ([c], []) => return format!("add {}", c.label),
        ([], [c]) => return format!("delete {}", c.label),
        ([], []) => {}
        _ => return "camera changes".to_owned(),
    }
    let changed: Vec<_> = after
        .cameras
        .iter()
        .filter(|c| before.cameras.iter().any(|b| b.id == c.id && b != *c))
        .collect();
    match changed.as_slice() {
        [c] => format!("edit {}", c.label),
        [] if before.region != after.region => "region change".to_owned(),
        [] if before.params != after.params => "analysis settings".to_owned(),
        [] if before.custom_models != after.custom_models => "custom model".to_owned(),
        [] => "change".to_owned(),
        _ => "camera changes".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toasts::ToastKind;
    use observation_core::camera::{CameraKind, CameraPlacement, Catalog};
    use observation_core::coverage::Region;
    use observation_core::geo::BBox;

    struct Fixture {
        history: History,
        doc: Document,
        models: Models,
        editor: Editor,
        toasts: Toasts,
    }

    impl Fixture {
        /// The demo area loaded, and its first state observed.
        fn new() -> Self {
            let mut doc = Document::default();
            doc.area = Some(AreaRef::Demo);
            let mut f = Self {
                history: History::default(),
                doc,
                models: Models::new(Catalog::default()),
                editor: Editor::default(),
                toasts: Toasts::default(),
            };
            f.doc.touch();
            assert!(f.observe(false));
            f
        }

        fn observe(&mut self, editing: bool) -> bool {
            self.history
                .observe(&self.doc, &self.models, editing, &mut self.toasts)
        }

        fn step(&mut self, op: HistoryOp) -> Option<StepOutcome> {
            self.history.step(
                op,
                &mut self.doc,
                &mut self.models,
                &mut self.editor,
                &mut self.toasts,
            )
        }

        fn add(&mut self, label: &str) -> u32 {
            let id = self.doc.add_camera(CameraPlacement {
                id: 0,
                label: label.to_owned(),
                model_id: "m".to_owned(),
                position: [0.0, 3.0, 0.0],
                yaw_deg: 0.0,
                pitch_deg: 0.0,
                roll_deg: 0.0,
                color: None,
                enabled: true,
                group: None,
            });
            assert!(self.observe(false));
            id
        }

        fn last_toast(&self) -> Option<(ToastKind, &str, Option<&ToastAction>)> {
            self.toasts
                .iter()
                .last()
                .map(|t| (t.kind, t.text.as_str(), t.action.as_ref()))
        }
    }

    #[test]
    fn nothing_is_recorded_before_an_area_loads() {
        let mut h = History::default();
        let mut doc = Document::default();
        let models = Models::new(Catalog::default());
        let mut toasts = Toasts::default();
        doc.touch();
        assert!(h.observe(&doc, &models, false, &mut toasts));
        doc.area = Some(AreaRef::Demo);
        doc.touch();
        h.observe(&doc, &models, false, &mut toasts);
        // The first state with an area is the base, not an undoable step.
        assert!(!h.can_undo() && !h.can_redo());
        assert_eq!(h.undo_label(), None);
    }

    #[test]
    fn a_settled_edit_is_one_step_and_clears_redo() {
        let mut f = Fixture::new();
        f.add("Porch");
        assert!(f.history.can_undo());
        assert_eq!(f.history.undo_label().as_deref(), Some("add Porch"));
        assert!(f.step(HistoryOp::Undo).is_some());
        assert!(f.history.can_redo());
        f.add("Gate");
        assert!(!f.history.can_redo());
    }

    #[test]
    fn a_new_revision_with_the_same_content_is_no_step() {
        let mut f = Fixture::new();
        f.doc.touch();
        assert!(f.observe(false));
        assert!(!f.history.can_undo());
    }

    #[test]
    fn nothing_is_recorded_or_stepped_while_editing_or_on_flat_ground() {
        let mut f = Fixture::new();
        f.add("Porch");
        f.doc.remove_camera(1);
        assert!(!f.observe(true));
        assert_eq!(f.history.undo_label().as_deref(), Some("add Porch"));
        f.doc.on_flat_ground = true;
        assert!(!f.observe(false));
        f.doc.on_flat_ground = false;
        assert!(f.observe(false));
        assert_eq!(f.history.undo_label().as_deref(), Some("delete Porch"));
    }

    #[test]
    fn the_first_state_is_taken_mid_load_but_not_on_flat_ground() {
        let models = Models::new(Catalog::default());
        let mut toasts = Toasts::default();
        let mut h = History::default();
        let mut doc = Document::default();
        doc.area = Some(AreaRef::Demo);
        doc.on_flat_ground = true;
        h.observe(&doc, &models, true, &mut toasts);
        assert_eq!(h.last, None);
        doc.on_flat_ground = false;
        doc.touch();
        h.observe(&doc, &models, true, &mut toasts);
        assert_eq!(h.last, Some(doc.snapshot(&models)));
        assert_eq!(h.last_revision, doc.revision);
    }

    #[test]
    fn at_most_max_steps_are_kept() {
        let mut f = Fixture::new();
        for i in 0..=MAX_STEPS {
            f.add(&format!("C{i}"));
        }
        assert_eq!(f.history.undo.len(), MAX_STEPS);
        // The oldest step went.
        assert!(f.history.undo[0].cameras.len() == 1);
    }

    #[test]
    fn a_deletion_offers_undo_and_a_later_step_withdraws_it() {
        let mut f = Fixture::new();
        let id = f.add("Porch");
        f.doc.remove_camera(id);
        f.observe(false);
        assert_eq!(
            f.last_toast(),
            Some((ToastKind::Info, "Deleted Porch", Some(&ToastAction::Undo)))
        );
        f.add("Gate");
        assert!(f.toasts.iter().all(|t| t.action.is_none()));
    }

    #[test]
    fn undo_and_redo_restore_announce_and_fix_the_selection() {
        let mut f = Fixture::new();
        let id = f.add("Porch");
        f.editor.selected = Some(id);
        f.editor.tool = Tool::Aim;
        assert_eq!(f.step(HistoryOp::Undo), Some(StepOutcome { reload: None }));
        assert!(f.doc.cameras.is_empty());
        assert_eq!((f.editor.selected, f.editor.tool), (None, Tool::Select));
        assert_eq!(
            f.last_toast(),
            Some((ToastKind::Info, "Undid: add Porch", None))
        );
        // Nothing more to undo: `last` stays, so redo still works.
        assert_eq!(f.step(HistoryOp::Undo), None);
        assert_eq!(f.step(HistoryOp::Redo), Some(StepOutcome { reload: None }));
        assert_eq!(f.doc.cameras.len(), 1);
        assert_eq!(
            f.last_toast(),
            Some((ToastKind::Info, "Redid: add Porch", None))
        );
        // The restore is not recorded again as an edit.
        assert!(f.observe(false));
        assert!(!f.history.can_redo() && f.history.can_undo());
    }

    #[test]
    fn stepping_across_an_area_change_reloads_that_area() {
        let mut f = Fixture::new();
        f.add("Porch");
        let osm = AreaRef::Osm {
            bbox: BBox::new(51.0, -0.1, 51.001, -0.099).expect("valid box"),
        };
        f.doc.area = Some(osm);
        f.doc.cameras.clear();
        f.doc.touch();
        f.observe(false);
        assert_eq!(
            f.last_toast(),
            Some((
                ToastKind::Info,
                "New area loaded; 1 camera(s) removed",
                Some(&ToastAction::Undo)
            ))
        );
        let outcome = f.step(HistoryOp::Undo).expect("a step");
        assert_eq!(outcome.reload, Some(AreaRef::Demo));
        assert_eq!(
            f.last_toast(),
            Some((ToastKind::Info, "Undid: area change", None))
        );
        assert_eq!(f.doc.cameras.len(), 1);
    }

    #[test]
    fn labels_name_what_changed() {
        let mut f = Fixture::new();
        let id = f.add("Porch");
        f.doc.camera_mut(id).expect("camera").yaw_deg = 90.0;
        f.observe(false);
        assert_eq!(f.history.undo_label().as_deref(), Some("edit Porch"));
        f.doc.region = Some(Region {
            points: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
            enabled: true,
        });
        f.doc.touch();
        f.observe(false);
        assert_eq!(f.history.undo_label().as_deref(), Some("region change"));
        f.doc.params.subject_height_m += 0.5;
        f.doc.touch();
        f.observe(false);
        assert_eq!(f.history.undo_label().as_deref(), Some("analysis settings"));
        f.step(HistoryOp::Undo);
        assert_eq!(f.history.redo_label().as_deref(), Some("analysis settings"));
    }

    #[test]
    fn deleting_only_a_custom_model_offers_undo_by_name() {
        let mut f = Fixture::new();
        let model = |id: &str| CameraModel {
            id: id.to_owned(),
            vendor: "Acme".to_owned(),
            name: id.to_owned(),
            kind: CameraKind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 50.0,
            resolution: [1920, 1080],
            ir_range_m: None,
            source: None,
            notes: None,
        };
        f.models.custom = vec![model("custom-a-1"), model("custom-b-2")];
        f.doc.touch();
        f.observe(false);
        assert_eq!(f.history.undo_label().as_deref(), Some("custom model"));
        f.models.remove_custom("custom-a-1");
        f.doc.touch();
        f.observe(false);
        assert_eq!(
            f.last_toast(),
            Some((
                ToastKind::Info,
                "Deleted model Acme custom-a-1",
                Some(&ToastAction::Undo)
            ))
        );
    }

    /// Counts write access, which the client's resources record as a change.
    struct Counting<T> {
        value: T,
        writes: usize,
    }

    impl<T> Tracked<T> for Counting<T> {
        fn get(&self) -> &T {
            &self.value
        }

        fn get_mut(&mut self) -> &mut T {
            self.writes += 1;
            &mut self.value
        }
    }

    #[test]
    fn a_step_writes_the_document_only_when_there_is_one() {
        let mut f = Fixture::new();
        let mut doc = Counting {
            value: std::mem::take(&mut f.doc),
            writes: 0,
        };
        let mut models = Counting {
            value: Models::new(Catalog::default()),
            writes: 0,
        };
        let none = f.history.step(
            HistoryOp::Redo,
            &mut doc,
            &mut models,
            &mut f.editor,
            &mut f.toasts,
        );
        assert_eq!((none, doc.writes, models.writes), (None, 0, 0));
        f.doc = doc.value;
        f.add("Porch");
        let mut doc = Counting {
            value: std::mem::take(&mut f.doc),
            writes: 0,
        };
        f.history.step(
            HistoryOp::Undo,
            &mut doc,
            &mut models,
            &mut f.editor,
            &mut f.toasts,
        );
        assert_eq!((doc.writes, models.writes), (1, 1));
    }
}
