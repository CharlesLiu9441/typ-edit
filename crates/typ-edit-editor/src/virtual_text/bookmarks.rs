//! Per-tab bookmark state for the gutter.
//!
//! A bookmark is a char offset anchored to a logical line start. The offsets
//! are shifted across every edit by [`BookmarkSyncHook`], exactly like the
//! collapsed folds, so a bookmark keeps tracking the same text as the buffer
//! changes (including undo/redo and each sub-edit of `replace_all`).

use std::cell::RefCell;
use std::rc::Rc;

use typ_edit_buffer::{EditHook, Operation, TextBuffer};

use super::caret_sync::shift_position;
use crate::EditorState;

/// Sorted, unique char offsets of bookmarked line starts.
pub(crate) struct Bookmarks {
    pub(super) positions: Rc<RefCell<Vec<usize>>>,
}

impl Default for Bookmarks {
    fn default() -> Self {
        Self {
            positions: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl Bookmarks {
    /// Toggles the bookmark on `line`: removes every position on that line, or
    /// inserts the line start when none exists. Returns whether the set changed.
    pub(super) fn toggle_line(&mut self, buffer: &TextBuffer, line: usize) -> bool {
        let mut positions = self.positions.borrow_mut();
        let before = positions.len();
        positions.retain(|pos| buffer.line_of_char(*pos) != line);
        if positions.len() != before {
            return true;
        }
        let start = buffer.line_start(line);
        match positions.binary_search(&start) {
            Ok(_) => false,
            Err(index) => {
                positions.insert(index, start);
                true
            }
        }
    }

    /// First bookmark strictly after `caret`.
    pub(super) fn next_from(&self, caret: usize) -> Option<usize> {
        let positions = self.positions.borrow();
        let index = positions.partition_point(|pos| *pos <= caret);
        positions.get(index).copied()
    }

    /// Last bookmark strictly before `caret`.
    pub(super) fn prev_from(&self, caret: usize) -> Option<usize> {
        let positions = self.positions.borrow();
        let index = positions.partition_point(|pos| *pos < caret);
        index
            .checked_sub(1)
            .and_then(|index| positions.get(index).copied())
    }

    /// Empties the set; returns whether anything was removed.
    pub(super) fn clear(&mut self) -> bool {
        let mut positions = self.positions.borrow_mut();
        let changed = !positions.is_empty();
        positions.clear();
        changed
    }

    /// Sorted, deduped logical lines for gutter painting.
    pub(super) fn lines(&self, buffer: &TextBuffer) -> Vec<usize> {
        let positions = self.positions.borrow();
        let mut lines: Vec<usize> = positions
            .iter()
            .map(|pos| buffer.line_of_char(*pos))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Number of bookmarks (tests).
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.positions.borrow().len()
    }
}

/// Buffer hook that keeps the bookmark offsets aligned with every edit.
pub(super) struct BookmarkSyncHook {
    positions: Rc<RefCell<Vec<usize>>>,
}

impl BookmarkSyncHook {
    pub(super) fn new(positions: Rc<RefCell<Vec<usize>>>) -> Self {
        Self { positions }
    }
}

impl EditHook for BookmarkSyncHook {
    fn on_edit(&mut self, op: &Operation, buffer: &TextBuffer) {
        let len = buffer.len_chars();
        let mut positions = self.positions.borrow_mut();
        // A deletion can collapse two positions onto the same offset; sort and
        // dedup so the invariant (sorted, unique) survives the edit.
        let mut shifted: Vec<usize> = positions
            .iter()
            .map(|pos| shift_position(op, *pos, len))
            .collect();
        shifted.sort_unstable();
        shifted.dedup();
        *positions = shifted;
    }
}

impl EditorState {
    /// Toggles a bookmark on the caret's line. Returns whether the set changed.
    pub fn toggle_bookmark(&mut self) -> bool {
        let line = self.buffer.line_of_char(self.virtual_text.caret());
        self.toggle_bookmark_line(line)
    }

    /// Toggles a bookmark on `line` (gutter click). Returns whether the set
    /// changed.
    pub fn toggle_bookmark_line(&mut self, line: usize) -> bool {
        self.virtual_text.bookmarks.toggle_line(&self.buffer, line)
    }

    /// Jumps the caret to the next bookmark after it, scrolling it into view on
    /// the next frame. Returns whether a bookmark existed.
    pub fn next_bookmark(&mut self) -> bool {
        let caret = self.virtual_text.caret();
        let Some(position) = self.virtual_text.bookmarks.next_from(caret) else {
            return false;
        };
        self.jump_to_char(position);
        true
    }

    /// Jumps the caret to the previous bookmark before it, scrolling it into
    /// view on the next frame. Returns whether a bookmark existed.
    pub fn prev_bookmark(&mut self) -> bool {
        let caret = self.virtual_text.caret();
        let Some(position) = self.virtual_text.bookmarks.prev_from(caret) else {
            return false;
        };
        self.jump_to_char(position);
        true
    }

    /// Removes every bookmark. Returns whether the set changed.
    pub fn clear_bookmarks(&mut self) -> bool {
        self.virtual_text.bookmarks.clear()
    }

    /// Queues a caret move (char offset) through the byte-space jump API.
    fn jump_to_char(&mut self, caret: usize) {
        let byte = self.buffer.char_to_byte(caret);
        self.jump_to_byte(byte);
    }

    /// Bookmarked logical lines, sorted and deduped (tests).
    #[cfg(test)]
    pub(crate) fn bookmark_lines(&self) -> Vec<usize> {
        self.virtual_text.bookmarks.lines(&self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_adds_then_removes_a_line() {
        let buffer = TextBuffer::from_text("aa\nbb\ncc");
        let mut bookmarks = Bookmarks::default();
        assert!(bookmarks.toggle_line(&buffer, 1));
        assert_eq!(*bookmarks.positions.borrow(), vec![buffer.line_start(1)]);
        assert!(bookmarks.toggle_line(&buffer, 1), "removing is a change");
        assert!(bookmarks.positions.borrow().is_empty());
    }

    #[test]
    fn toggle_keeps_positions_sorted_and_unique() {
        let buffer = TextBuffer::from_text("aa\nbb\ncc\ndd");
        let mut bookmarks = Bookmarks::default();
        bookmarks.toggle_line(&buffer, 2);
        bookmarks.toggle_line(&buffer, 0);
        assert_eq!(
            *bookmarks.positions.borrow(),
            vec![buffer.line_start(0), buffer.line_start(2)],
            "inserted in sorted order"
        );
        // Toggling line 0 again removes only that entry.
        assert!(bookmarks.toggle_line(&buffer, 0));
        assert_eq!(*bookmarks.positions.borrow(), vec![buffer.line_start(2)]);
    }

    #[test]
    fn toggle_removes_every_position_on_the_line() {
        let buffer = TextBuffer::from_text("aa\nbb");
        let mut bookmarks = Bookmarks::default();
        bookmarks.positions.borrow_mut().extend([1, 3]);
        assert!(bookmarks.toggle_line(&buffer, 0));
        assert_eq!(
            *bookmarks.positions.borrow(),
            vec![3],
            "only line 0 removed"
        );
    }

    #[test]
    fn next_and_prev_are_strict() {
        let bookmarks = Bookmarks::default();
        *bookmarks.positions.borrow_mut() = vec![2, 5, 9];
        assert_eq!(bookmarks.next_from(0), Some(2));
        assert_eq!(bookmarks.next_from(2), Some(5), "strictly greater");
        assert_eq!(bookmarks.next_from(8), Some(9));
        assert_eq!(bookmarks.next_from(9), None, "no wrap-around");
        assert_eq!(bookmarks.prev_from(9), Some(5));
        assert_eq!(bookmarks.prev_from(6), Some(5));
        assert_eq!(bookmarks.prev_from(2), None, "strictly less");
        assert_eq!(bookmarks.prev_from(0), None);
    }

    #[test]
    fn clear_reports_whether_anything_was_removed() {
        let mut bookmarks = Bookmarks::default();
        assert!(!bookmarks.clear(), "already empty");
        *bookmarks.positions.borrow_mut() = vec![1, 4];
        assert!(bookmarks.clear());
        assert!(bookmarks.positions.borrow().is_empty());
    }

    #[test]
    fn lines_maps_positions_to_sorted_unique_lines() {
        let buffer = TextBuffer::from_text("aa\nbb\ncc");
        let bookmarks = Bookmarks::default();
        *bookmarks.positions.borrow_mut() = vec![0, 1, 6];
        assert_eq!(bookmarks.lines(&buffer), vec![0, 2]);
    }

    #[test]
    fn hook_shifts_offsets_and_dedups_a_collision() {
        let positions = Rc::new(RefCell::new(vec![0, 3]));
        let mut buffer = TextBuffer::from_text("ab\ncd");
        buffer.add_hook(Box::new(BookmarkSyncHook::new(positions.clone())));
        // Deleting "ab\n" pulls both positions (0 and 3) onto 0.
        buffer.delete(0..3, true);
        assert_eq!(*positions.borrow(), vec![0], "collision dedups");
    }

    #[test]
    fn hook_shifts_offsets_across_insert_and_delete() {
        let positions = Rc::new(RefCell::new(vec![5]));
        let mut buffer = TextBuffer::from_text("abcdefgh");
        buffer.add_hook(Box::new(BookmarkSyncHook::new(positions.clone())));
        buffer.insert(0, "XY", true);
        assert_eq!(*positions.borrow(), vec![7]);
        buffer.delete(0..2, true);
        assert_eq!(*positions.borrow(), vec![5]);
    }
}
