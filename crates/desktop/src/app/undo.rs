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

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn snapshot(id: u128) -> Option<Revision> {
        Some(Revision {
            id: Uuid::from_u128(id),
            parent: None,
            name: String::new(),
            operation: serde_json::Value::Null,
            scans: vec![],
            layers: vec![],
            labels: vec![],
            transforms: Default::default(),
            groups: vec![],
            scan_groups: Default::default(),
            saved_at: None,
            registrations: Default::default(),
            scan_names: Default::default(),
            panoramas: vec![],
            panorama_pairs: Default::default(),
        })
    }

    fn identity(state: Option<Option<Revision>>) -> Option<Option<Uuid>> {
        state.map(|state| state.map(|revision| revision.id))
    }

    #[test]
    fn undo_redo_restore_saved_and_draft_states_and_new_edits_discard_redo() {
        let mut stack = UndoStack::default();
        assert!(stack.undo(None).is_none());
        assert!(stack.redo(None).is_none());
        stack.record(None);
        assert_eq!(identity(stack.undo(snapshot(1))), Some(None));
        assert_eq!(identity(stack.redo(None)), Some(Some(Uuid::from_u128(1))));
        stack.record(snapshot(1));
        assert_eq!(
            identity(stack.undo(snapshot(2))),
            Some(Some(Uuid::from_u128(1)))
        );
        assert!(stack.can_redo());
        stack.record(snapshot(1));
        assert!(!stack.can_redo());
        assert!(stack.redo(snapshot(3)).is_none());
        assert_eq!(
            identity(stack.undo(snapshot(3))),
            Some(Some(Uuid::from_u128(1)))
        );
    }

    #[test]
    fn failed_restores_leave_undo_and_redo_available_for_retry() {
        let mut stack = UndoStack::default();
        stack.record(snapshot(1));
        let target = stack.undo(snapshot(2)).unwrap();
        stack.reverse_undo(target);
        assert!(stack.can_undo());
        assert!(!stack.can_redo());
        assert_eq!(
            identity(stack.undo(snapshot(2))),
            Some(Some(Uuid::from_u128(1)))
        );
        let target = stack.redo(snapshot(1)).unwrap();
        stack.reverse_redo(target);
        assert!(!stack.can_undo());
        assert!(stack.can_redo());
        assert_eq!(
            identity(stack.redo(snapshot(1))),
            Some(Some(Uuid::from_u128(2)))
        );
    }

    #[test]
    fn the_history_limit_keeps_the_latest_states_and_clear_discards_both_directions() {
        let mut stack = UndoStack::default();
        for id in 0..=UndoStack::LIMIT {
            stack.record(snapshot(id as u128));
        }
        for id in (1..=UndoStack::LIMIT).rev() {
            assert_eq!(
                identity(stack.undo(snapshot(1000))),
                Some(Some(Uuid::from_u128(id as u128)))
            );
        }
        assert!(!stack.can_undo());
        assert!(stack.undo(None).is_none());
        assert!(stack.can_redo());
        stack.redo(None).unwrap();
        assert!(stack.can_undo());
        stack.clear();
        assert!(!stack.can_undo());
        assert!(!stack.can_redo());
    }
}
