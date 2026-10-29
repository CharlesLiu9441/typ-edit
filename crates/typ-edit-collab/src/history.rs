//! Server-side global undo/redo stacks (plan `§5.6`).
//!
//! The server owns one undo stack and one redo stack for the whole session,
//! holding [`ChangeSet`]s in application order. The discipline is deliberately
//! asymmetric so that undo/redo themselves do not corrupt the history:
//!
//! * A **regular** accepted change set is pushed to undo and **clears** redo.
//! * `undo` hands back the **inverse** of the top of undo ([`History::peek_undo`])
//!   and the caller calls [`History::commit_undo`] only once that inverse
//!   applied; the popped set then moves onto redo — never clearing redo, never
//!   pushing the inverse onto undo.
//! * `redo` hands back the top of redo ([`History::peek_redo`]) and
//!   [`History::commit_redo`] moves it onto undo — without clearing redo.
//!
//! Peeking before committing is what keeps the stacks honest: when the set fails
//! to apply, nothing moves, so `redo` never holds a change the content still has.
//!
//! Because the server applies accepted change sets strictly in order and only
//! ever touches the top of a stack, the popped set is always the last applied
//! change, so its inverse re-applies cleanly at the current frontier.

use crate::ops::ChangeSet;

/// The server's global undo/redo stacks.
#[derive(Clone, Debug, Default)]
pub struct History {
    undo: Vec<ChangeSet>,
    redo: Vec<ChangeSet>,
}

impl History {
    /// An empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a regular accepted change set: push it and clear the redo stack.
    pub fn record(&mut self, set: ChangeSet) {
        self.undo.push(set);
        self.redo.clear();
    }

    /// The set an undo would apply, together with the set it inverts, without
    /// moving either stack.
    ///
    /// Returns `None` when there is nothing to undo. The caller applies the
    /// returned set and then calls [`Self::commit_undo`]: moving the stacks first
    /// would leave redo holding a change that is still in the content whenever the
    /// inverse fails to apply.
    pub fn peek_undo(&self) -> Option<(ChangeSet, ChangeSet)> {
        let original = self.undo.last()?.clone();
        Some((original.invert(), original))
    }

    /// Moves the top of undo onto redo, after its inverse applied.
    pub fn commit_undo(&mut self) {
        if let Some(original) = self.undo.pop() {
            self.redo.push(original);
        }
    }

    /// The set a redo would apply, without moving either stack.
    ///
    /// Returns `None` when there is nothing to redo; the caller applies the
    /// returned set and then calls [`Self::commit_redo`].
    pub fn peek_redo(&self) -> Option<ChangeSet> {
        self.redo.last().cloned()
    }

    /// Moves the top of redo onto undo, after the change applied again.
    pub fn commit_redo(&mut self) {
        if let Some(set) = self.redo.pop() {
            self.undo.push(set);
        }
    }

    /// Whether an undo is available.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether a redo is available.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{ChangeOp, apply};

    fn set(text: &str) -> ChangeSet {
        ChangeSet::new(
            "a.typ",
            vec![ChangeOp::Insert {
                at: 0,
                text: text.to_owned(),
            }],
        )
    }

    #[test]
    fn peek_undo_returns_the_inverse_without_moving_the_stacks() {
        let mut history = History::new();
        history.record(set("A"));
        assert!(history.can_undo());
        assert!(!history.can_redo());

        let (inverse, original) = history.peek_undo().expect("undo");
        assert_eq!(
            inverse.ops,
            vec![ChangeOp::Delete {
                at: 0,
                text: "A".to_owned()
            }]
        );
        assert_eq!(original.ops, set("A").ops);
        // Peeking moves nothing: a failed apply must leave both stacks as they
        // were, or redo would advertise a change the content still has.
        assert!(history.can_undo());
        assert!(!history.can_redo());

        history.commit_undo();
        assert!(!history.can_undo());
        assert!(history.can_redo());
    }

    #[test]
    fn a_failed_undo_leaves_the_stacks_alone() {
        let mut history = History::new();
        history.record(set("A"));
        let (inverse, _) = history.peek_undo().expect("undo");
        // The caller's apply failed (the content drifted), so it never commits.
        assert!(apply("different", &inverse.ops).is_err());
        assert!(history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn undo_then_undo_pops_in_lifo_order() {
        let mut history = History::new();
        history.record(set("A"));
        history.record(set("B"));
        let (_inverse, original) = history.peek_undo().expect("undo B");
        assert_eq!(
            original.ops[0],
            ChangeOp::Insert {
                at: 0,
                text: "B".to_owned()
            }
        );
        history.commit_undo();
        let (_inverse, original) = history.peek_undo().expect("undo A");
        assert_eq!(
            original.ops[0],
            ChangeOp::Insert {
                at: 0,
                text: "A".to_owned()
            }
        );
        history.commit_undo();
        assert!(!history.can_undo());
    }

    #[test]
    fn redo_returns_original_set() {
        let mut history = History::new();
        history.record(set("A"));
        assert!(history.peek_undo().is_some());
        history.commit_undo();

        let redo = history.peek_redo().expect("redo");
        assert_eq!(
            redo.ops,
            vec![ChangeOp::Insert {
                at: 0,
                text: "A".to_owned()
            }]
        );
        history.commit_redo();
        assert!(history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn undo_redo_undo_cycles() {
        let mut history = History::new();
        history.record(set("A"));
        history.record(set("B"));

        let (inverse_b, _) = history.peek_undo().expect("undo B");
        history.commit_undo();
        assert!(history.can_redo());

        let redo_b = history.peek_redo().expect("redo B");
        assert_eq!(redo_b.ops, inverse_b.invert().ops);
        history.commit_redo();

        let (inverse_b_again, _) = history.peek_undo().expect("undo B again");
        assert_eq!(inverse_b_again.ops, inverse_b.ops);
        history.commit_undo();
        assert!(history.can_redo());
    }

    #[test]
    fn committing_an_undo_does_not_push_the_inverse_onto_undo() {
        let mut history = History::new();
        history.record(set("A"));
        assert!(history.peek_undo().is_some());
        history.commit_undo();
        assert!(!history.can_undo());
    }

    #[test]
    fn regular_change_clears_redo() {
        let mut history = History::new();
        history.record(set("A"));
        assert!(history.peek_undo().is_some());
        history.commit_undo();
        assert!(history.can_redo());

        history.record(set("B"));
        assert!(!history.can_redo());
        assert!(history.can_undo());
    }

    #[test]
    fn undo_redo_apply_back_to_original_content() {
        let mut history = History::new();
        let base = "hello";
        history.record(set("A"));
        let with_a = apply(base, &set("A").ops).expect("apply A");
        assert_eq!(with_a, "Ahello");

        let (inverse, _) = history.peek_undo().expect("undo");
        let undone = apply(&with_a, &inverse.ops).expect("apply inverse");
        assert_eq!(undone, base);
        history.commit_undo();

        let redo = history.peek_redo().expect("redo");
        let redone = apply(&undone, &redo.ops).expect("apply redo");
        assert_eq!(redone, with_a);
        history.commit_redo();
    }
}
