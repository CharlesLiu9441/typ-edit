//! Caret/selection synchronization with the authoritative buffer.
//!
//! The widget keeps its caret and selection anchor in a shared
//! [`SelectionHandle`]. The [`CaretSyncHook`] registered on the buffer shifts
//! them after every applied operation, including undo/redo and each sub-edit of
//! `replace_all`, so no edit site recomputes positions by hand.

use std::cell::RefCell;
use std::rc::Rc;

use typ_edit_buffer::{EditHook, Operation, TextBuffer};

use super::select::Carets;

/// Shared, single-threaded caret-list handle.
pub(crate) type SelectionHandle = Rc<RefCell<Carets>>;

/// Creates a handle holding one empty caret.
pub(crate) fn new_handle() -> SelectionHandle {
    Rc::new(RefCell::new(Carets::default()))
}

/// Shifts `pos` by the character delta of `op`, clamped to `len`.
///
/// An insertion at or before the position pushes it right; a deletion swallows
/// a position inside the removed range and pulls every later position left.
pub(crate) fn shift_position(op: &Operation, pos: usize, len: usize) -> usize {
    let shifted = match op {
        Operation::Insert { at, text, .. } => {
            if pos >= *at {
                pos + text.chars().count()
            } else {
                pos
            }
        }
        Operation::Delete { at, text, .. } => {
            let n = text.chars().count();
            let end = at + n;
            if pos <= *at {
                pos
            } else if pos <= end {
                *at
            } else {
                pos - n
            }
        }
        Operation::UndoRedoFlag => pos,
    };
    shifted.min(len)
}

/// Buffer hook that keeps a [`SelectionHandle`] aligned with every edit.
pub(crate) struct CaretSyncHook {
    selection: SelectionHandle,
}

impl CaretSyncHook {
    pub(crate) fn new(selection: SelectionHandle) -> Self {
        Self { selection }
    }
}

impl EditHook for CaretSyncHook {
    fn on_edit(&mut self, op: &Operation, buffer: &TextBuffer) {
        let len = buffer.len_chars();
        let mut carets = self.selection.borrow_mut();
        for selection in carets.list_mut() {
            selection.caret = shift_position(op, selection.caret, len);
            if let Some(anchor) = selection.anchor {
                selection.anchor = Some(shift_position(op, anchor, len));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_text::select::Selection;

    fn insert(at: usize, text: &str) -> Operation {
        Operation::Insert {
            at,
            text: text.to_owned(),
        }
    }

    fn delete(at: usize, text: &str) -> Operation {
        Operation::Delete {
            at,
            text: text.to_owned(),
        }
    }

    #[test]
    fn insert_shifts_only_positions_at_or_after_it() {
        let op = insert(3, "xy");
        assert_eq!(shift_position(&op, 2, 10), 2);
        assert_eq!(shift_position(&op, 3, 10), 5);
        assert_eq!(shift_position(&op, 4, 10), 6);
    }

    #[test]
    fn delete_maps_positions_inside_the_removed_range_to_its_start() {
        let op = delete(3, "xy");
        assert_eq!(shift_position(&op, 2, 10), 2);
        assert_eq!(shift_position(&op, 3, 10), 3);
        assert_eq!(shift_position(&op, 4, 10), 3, "inside the range");
        assert_eq!(shift_position(&op, 5, 10), 3, "at the range end");
        assert_eq!(shift_position(&op, 6, 10), 4, "after the range");
    }

    #[test]
    fn shift_clamps_to_the_new_length() {
        let op = insert(0, "abcdef");
        assert_eq!(shift_position(&op, 0, 3), 3);
    }

    #[test]
    fn hook_tracks_inserts_and_a_separate_undo_group() {
        let handle = new_handle();
        let mut buffer = TextBuffer::new();
        buffer.add_hook(Box::new(CaretSyncHook::new(handle.clone())));

        buffer.insert(0, "ab", true);
        handle.borrow_mut().set_caret(2);
        buffer.insert(1, "X", true);
        assert_eq!(
            handle.borrow().caret(),
            3,
            "an insert before the caret shifts it"
        );

        assert!(buffer.undo());
        assert_eq!(
            handle.borrow().caret(),
            2,
            "the hook reverses the shift on undo"
        );
        assert!(buffer.redo());
        assert_eq!(handle.borrow().caret(), 3);
    }

    #[test]
    fn hook_moves_the_anchor_when_a_selection_replacement_collapses() {
        let handle = new_handle();
        let mut buffer = TextBuffer::new();
        buffer.add_hook(Box::new(CaretSyncHook::new(handle.clone())));
        buffer.insert(0, "hello", true);
        handle.borrow_mut().set_selection(1, 4);
        buffer.delete(1..4, true);
        assert_eq!(handle.borrow().caret(), 1);
        assert_eq!(handle.borrow().primary().anchor, Some(1));
        assert!(handle.borrow().selection().is_none());
    }

    #[test]
    fn hook_shifts_every_entry_in_the_list() {
        let handle = new_handle();
        let mut buffer = TextBuffer::new();
        buffer.add_hook(Box::new(CaretSyncHook::new(handle.clone())));
        buffer.insert(0, "hello", true);
        {
            let mut carets = handle.borrow_mut();
            carets.list_mut().clear();
            carets.list_mut().push(Selection {
                caret: 1,
                anchor: None,
                h_pos: None,
            });
            carets.list_mut().push(Selection {
                caret: 4,
                anchor: Some(3),
                h_pos: None,
            });
            carets.set_primary(1);
        }
        buffer.insert(0, "X", true);
        let carets = handle.borrow();
        assert_eq!(carets.list()[0].caret, 2);
        assert_eq!(carets.list()[1].caret, 5);
        assert_eq!(carets.list()[1].anchor, Some(4));
        assert_eq!(carets.primary().caret, 5, "primary index is unchanged");
    }
}
