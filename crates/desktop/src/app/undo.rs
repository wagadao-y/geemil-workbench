//! Undo and redo over working-state snapshots. Point data and masks are
//! immutable, so a snapshot of the working state is all an edit changes.
use geemil_core::Revision;

/// Snapshots older (undo) and newer (redo) than the current working state.
/// `None` is "no unsaved changes", i.e. the current revision itself.
#[derive(Default)]
pub(super) struct UndoStack {
    undo: Vec<Option<Revision>>,
    redo: Vec<Option<Revision>>,
}
impl UndoStack {
    const LIMIT: usize = 200;

    /// Records the working state from before an edit.
    pub(super) fn record(&mut self, before: Option<Revision>) {
        if self.undo.len() == Self::LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
    }
    pub(super) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub(super) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    /// The state to restore for undo; `current` becomes redoable.
    pub(super) fn undo(&mut self, current: Option<Revision>) -> Option<Option<Revision>> {
        let state = self.undo.pop()?;
        self.redo.push(current);
        Some(state)
    }
    /// The state to restore for redo; `current` becomes undoable.
    pub(super) fn redo(&mut self, current: Option<Revision>) -> Option<Option<Revision>> {
        let state = self.redo.pop()?;
        self.undo.push(current);
        Some(state)
    }
    /// For a failed restore: puts the state back where it came from.
    pub(super) fn reverse_undo(&mut self, state: Option<Revision>) {
        self.redo.pop();
        self.undo.push(state);
    }
    pub(super) fn reverse_redo(&mut self, state: Option<Revision>) {
        self.undo.pop();
        self.redo.push(state);
    }
    pub(super) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}
