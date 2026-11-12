//! Short messages at the bottom of the viewport: what just happened, what
//! went wrong, and a button to undo or retry.

use crate::document::LoadArea;

/// How long an info toast stays, seconds.
pub const INFO_SECS: f32 = 4.0;
/// How long a toast with a button, or a passing error, stays, seconds.
pub const ACTION_SECS: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

/// A button on a toast.
#[derive(Debug, Clone, PartialEq)]
pub enum ToastAction {
    Undo,
    /// Send this area request again.
    Retry(LoadArea),
}

impl ToastAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Retry(_) => "Retry",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub action: Option<ToastAction>,
    /// Seconds remaining; errors stay until dismissed.
    pub ttl: f32,
}

/// The toasts on screen, oldest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Toasts {
    list: Vec<Toast>,
}

impl Toasts {
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Info, text.into(), None, INFO_SECS);
    }

    /// An info toast with a button; it stays a little longer to be reached.
    pub fn info_with(&mut self, text: impl Into<String>, action: ToastAction) {
        self.push(ToastKind::Info, text.into(), Some(action), ACTION_SECS);
    }

    /// A passing problem with what the user just did (a refused click, an
    /// invalid form): shown for a while, then gone.
    pub fn error(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Error, text.into(), None, ACTION_SECS);
    }

    /// A failure that loses or blocks work (a load, an import, a restore):
    /// stays until dismissed, so it is never missed.
    pub fn error_sticky(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Error, text.into(), None, f32::INFINITY);
    }

    /// A sticky error with a button (Retry).
    pub fn error_with(&mut self, text: impl Into<String>, action: ToastAction) {
        self.push(ToastKind::Error, text.into(), Some(action), f32::INFINITY);
    }

    fn push(&mut self, kind: ToastKind, text: String, action: Option<ToastAction>, ttl: f32) {
        // A newer Undo supersedes an older one: only the latest step is one
        // click away.
        if matches!(action, Some(ToastAction::Undo)) {
            self.forget_undo();
        }
        // The same message again replaces the old one instead of piling up.
        self.list.retain(|t| t.kind != kind || t.text != text);
        self.list.push(Toast {
            kind,
            text,
            action,
            ttl,
        });
    }

    /// Removes every toast with an Undo button.
    pub fn forget_undo(&mut self) {
        self.list
            .retain(|t| !matches!(t.action, Some(ToastAction::Undo)));
    }

    /// Removes every toast with a Retry button.
    pub fn forget_retry(&mut self) {
        self.list
            .retain(|t| !matches!(t.action, Some(ToastAction::Retry(_))));
    }

    /// Removes the toast at `index` (oldest first); false if there is none.
    pub fn dismiss(&mut self, index: usize) -> bool {
        if index < self.list.len() {
            self.list.remove(index);
            true
        } else {
            false
        }
    }

    /// Counts `dt` seconds of wall-clock time off every toast and drops the
    /// ones whose time is up.
    pub fn tick(&mut self, dt: f32) {
        if self.list.is_empty() {
            return;
        }
        for t in &mut self.list {
            t.ttl -= dt;
        }
        self.list.retain(|t| t.ttl > 0.0);
    }

    /// The toasts, oldest first.
    pub fn iter(&self) -> std::slice::Iter<'_, Toast> {
        self.list.iter()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(t: &Toasts) -> Vec<(ToastKind, &str, f32)> {
        t.iter().map(|t| (t.kind, t.text.as_str(), t.ttl)).collect()
    }

    #[test]
    fn each_kind_gets_its_lifetime() {
        let mut t = Toasts::default();
        t.info("a");
        t.info_with("b", ToastAction::Undo);
        t.error("c");
        t.error_sticky("d");
        t.error_with("e", ToastAction::Retry(LoadArea::demo()));
        assert_eq!(
            texts(&t),
            [
                (ToastKind::Info, "a", 4.0),
                (ToastKind::Info, "b", 8.0),
                (ToastKind::Error, "c", 8.0),
                (ToastKind::Error, "d", f32::INFINITY),
                (ToastKind::Error, "e", f32::INFINITY),
            ]
        );
    }

    #[test]
    fn the_same_message_again_moves_to_the_end_with_a_fresh_lifetime() {
        let mut t = Toasts::default();
        t.info("saved");
        t.error("saved");
        t.info("other");
        t.tick(3.0);
        t.info("saved");
        // Same kind and text: replaced. Same text of another kind: kept.
        assert_eq!(
            texts(&t),
            [
                (ToastKind::Error, "saved", 5.0),
                (ToastKind::Info, "other", 1.0),
                (ToastKind::Info, "saved", 4.0),
            ]
        );
    }

    #[test]
    fn a_newer_undo_supersedes_the_older() {
        let mut t = Toasts::default();
        t.info_with("Deleted Camera 1", ToastAction::Undo);
        t.info("between");
        t.info_with("Deleted Camera 2", ToastAction::Undo);
        let undos: Vec<_> = t
            .iter()
            .filter(|t| t.action == Some(ToastAction::Undo))
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(undos, ["Deleted Camera 2"]);
        assert_eq!(t.len(), 2);
        // A Retry is not an Undo: it stays.
        t.error_with("load failed", ToastAction::Retry(LoadArea::demo()));
        t.info_with("Deleted Camera 3", ToastAction::Undo);
        assert_eq!(t.len(), 3);
        t.forget_undo();
        assert_eq!(
            texts(&t),
            [
                (ToastKind::Info, "between", 4.0),
                (ToastKind::Error, "load failed", f32::INFINITY)
            ]
        );
        t.forget_retry();
        assert_eq!(texts(&t), [(ToastKind::Info, "between", 4.0)]);
    }

    #[test]
    fn time_runs_out_but_sticky_errors_stay() {
        let mut t = Toasts::default();
        t.info("info");
        t.error("error");
        t.error_sticky("sticky");
        t.tick(3.5);
        assert_eq!(t.len(), 3);
        t.tick(0.5);
        // At exactly zero the info toast is gone.
        assert_eq!(
            texts(&t),
            [
                (ToastKind::Error, "error", 4.0),
                (ToastKind::Error, "sticky", f32::INFINITY)
            ]
        );
        t.tick(1_000_000.0);
        assert_eq!(texts(&t), [(ToastKind::Error, "sticky", f32::INFINITY)]);
        assert!(t.dismiss(0));
        assert!(!t.dismiss(0));
        assert!(t.is_empty());
        t.tick(1.0);
        assert!(t.is_empty());
    }

    #[test]
    fn the_dedupe_key_is_kind_and_text_not_the_button() {
        let mut t = Toasts::default();
        t.error_with("load failed", ToastAction::Retry(LoadArea::demo()));
        t.info("between");
        // Same kind and text without a button: replaces the one with Retry.
        t.error("load failed");
        assert_eq!(t.len(), 2);
        let last = t.iter().last().expect("two toasts");
        assert_eq!(
            (last.text.as_str(), &last.action, last.ttl),
            ("load failed", &None, 8.0)
        );
        // And a button on the same kind and text replaces the plain one.
        t.info("note");
        t.info_with("note", ToastAction::Undo);
        let notes: Vec<_> = t.iter().filter(|t| t.text == "note").collect();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].action, Some(ToastAction::Undo));
    }

    #[test]
    fn dismiss_removes_exactly_the_toast_at_its_index() {
        let mut t = Toasts::default();
        t.error_sticky("first");
        t.error_sticky("second");
        t.error_sticky("third");
        assert!(t.dismiss(1));
        let left: Vec<_> = t.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(left, ["first", "third"]);
        assert!(!t.dismiss(2));
        assert_eq!(t.len(), 2);
    }
}
