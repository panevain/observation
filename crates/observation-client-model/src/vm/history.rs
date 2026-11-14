//! The Undo and Redo buttons.

use crate::history::{History, HistoryOp};

/// One of the two buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryButton {
    pub op: HistoryOp,
    /// Its keyboard shortcut, as shown.
    pub keys: &'static str,
    pub enabled: bool,
    /// What it would do, or that there is nothing to do.
    pub tip: String,
}

/// The Undo and Redo buttons, in that order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryVm(pub [HistoryButton; 2]);

impl HistoryVm {
    pub fn new(history: &History) -> Self {
        let button = |op, keys, enabled, what: Option<String>| {
            let name = match op {
                HistoryOp::Undo => "Undo",
                HistoryOp::Redo => "Redo",
            };
            let tip = match what {
                Some(w) => format!("{name} {w} ({keys})"),
                None => format!("Nothing to {} ({keys})", name.to_lowercase()),
            };
            HistoryButton {
                op,
                keys,
                enabled,
                tip,
            }
        };
        Self([
            button(
                HistoryOp::Undo,
                "Ctrl+Z",
                history.can_undo(),
                history.undo_label(),
            ),
            button(
                HistoryOp::Redo,
                "Ctrl+Shift+Z",
                history.can_redo(),
                history.redo_label(),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, Models};
    use crate::editor::Editor;
    use crate::toasts::Toasts;
    use observation_core::camera::{CameraPlacement, Catalog};
    use observation_core::scene::AreaRef;

    fn tips(vm: &HistoryVm) -> [(HistoryOp, bool, &str); 2] {
        vm.0.each_ref().map(|b| (b.op, b.enabled, b.tip.as_str()))
    }

    #[test]
    fn with_nothing_to_step_both_say_so() {
        assert_eq!(
            tips(&HistoryVm::new(&History::default())),
            [
                (HistoryOp::Undo, false, "Nothing to undo (Ctrl+Z)"),
                (HistoryOp::Redo, false, "Nothing to redo (Ctrl+Shift+Z)"),
            ]
        );
    }

    #[test]
    fn each_button_names_the_step_it_would_take() {
        let mut history = History::default();
        let mut doc = Document::default();
        doc.area = Some(AreaRef::Demo);
        let mut models = Models::new(Catalog::default());
        let mut toasts = Toasts::default();
        doc.touch();
        history.observe(&doc, &models, false, &mut toasts);
        doc.add_camera(CameraPlacement {
            id: 0,
            label: "Porch".to_owned(),
            model_id: "m".to_owned(),
            position: [0.0, 3.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        });
        history.observe(&doc, &models, false, &mut toasts);
        assert_eq!(
            tips(&HistoryVm::new(&history)),
            [
                (HistoryOp::Undo, true, "Undo add Porch (Ctrl+Z)"),
                (HistoryOp::Redo, false, "Nothing to redo (Ctrl+Shift+Z)"),
            ]
        );
        let mut editor = Editor::default();
        history.step(
            HistoryOp::Undo,
            &mut doc,
            &mut models,
            &mut editor,
            &mut toasts,
        );
        assert_eq!(
            tips(&HistoryVm::new(&history)),
            [
                (HistoryOp::Undo, false, "Nothing to undo (Ctrl+Z)"),
                (HistoryOp::Redo, true, "Redo add Porch (Ctrl+Shift+Z)"),
            ]
        );
    }
}
