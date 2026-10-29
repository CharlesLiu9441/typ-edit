//! Edit operations: insertion, deletion and whole-buffer replacement, plus the
//! internal apply path that keeps the rope, line index, revision and hooks in
//! sync.

use std::ops::Range;

use similar::{DiffTag, TextDiff};

use crate::buffer::TextBuffer;
use crate::operation::Operation;

impl TextBuffer {
    /// Inserts `text` at char index `at` and returns the inserted char range.
    ///
    /// Empty `text` or `at > len_chars()` is a no-op: nothing is recorded in
    /// history and no hook fires. When `new_op` is `false` the edit merges into
    /// the previous undo group.
    pub fn insert(&mut self, at: usize, text: &str, new_op: bool) -> Range<usize> {
        let len = self.len_chars();
        if text.is_empty() {
            let at = at.min(len);
            return at..at;
        }
        if at > len {
            return len..len;
        }
        self.apply_insert_raw(at, text);
        let inverse = Operation::Delete {
            at,
            text: text.to_string(),
        };
        self.record(inverse, new_op);
        at..at + text.chars().count()
    }

    /// Deletes char range `range` and returns the removed text.
    ///
    /// An empty (or fully clamped) range is a no-op. When `new_op` is `false`
    /// the edit merges into the previous undo group.
    pub fn delete(&mut self, range: Range<usize>, new_op: bool) -> String {
        let len = self.len_chars();
        let start = range.start.min(len);
        let end = range.end.min(len);
        if start >= end {
            return String::new();
        }
        let removed = self.rope.slice(start..end).to_string();
        self.apply_delete_raw(start, end, &removed);
        let inverse = Operation::Insert {
            at: start,
            text: removed.clone(),
        };
        self.record(inverse, new_op);
        removed
    }

    /// Rewrites the whole buffer to `new_text`.
    ///
    /// Uses `similar`'s char diff so only the changed spans are touched; every
    /// span fires hooks as its own operation, but all spans are recorded as a
    /// single undo group (one flag when `new_op`). Identical text is a no-op.
    pub fn replace_all(&mut self, new_text: &str, new_op: bool) {
        let old = self.rope.to_string();
        if old == new_text {
            return;
        }
        let new_chars: Vec<char> = new_text.chars().collect();
        let diff = TextDiff::from_chars(old.as_str(), new_text);
        let mut inverses: Vec<Operation> = Vec::new();
        // Current buffer position = original char index + offset, where offset
        // tracks inserted minus deleted chars so far. It is never negative for
        // an op that actually exists.
        let mut offset: isize = 0;
        for op in diff.ops() {
            match op.tag() {
                DiffTag::Equal => {}
                DiffTag::Delete => {
                    let old_range = op.old_range();
                    let start = (old_range.start as isize + offset) as usize;
                    let end = (old_range.end as isize + offset) as usize;
                    inverses.push(self.delete_grouped(start, end));
                    offset -= (old_range.end - old_range.start) as isize;
                }
                DiffTag::Insert => {
                    let at = (op.old_range().start as isize + offset) as usize;
                    let inserted: String = new_chars[op.new_range()].iter().collect();
                    inverses.push(self.insert_grouped(at, &inserted));
                    offset += (op.new_range().end - op.new_range().start) as isize;
                }
                DiffTag::Replace => {
                    let old_range = op.old_range();
                    let new_range = op.new_range();
                    let start = (old_range.start as isize + offset) as usize;
                    let end = (old_range.end as isize + offset) as usize;
                    inverses.push(self.delete_grouped(start, end));
                    offset -= (old_range.end - old_range.start) as isize;
                    let inserted: String = new_chars[new_range.clone()].iter().collect();
                    inverses.push(self.insert_grouped(start, &inserted));
                    offset += (new_range.end - new_range.start) as isize;
                }
            }
        }
        self.record_group(inverses, new_op);
    }

    /// Applies a recorded operation (used by undo/redo); flags are rejected.
    pub(crate) fn apply_op(&mut self, op: &Operation) {
        match op {
            Operation::Insert { at, text } => self.apply_insert_raw(*at, text),
            Operation::Delete { at, text } => {
                let end = at + text.chars().count();
                self.apply_delete_raw(*at, end, text);
            }
            Operation::UndoRedoFlag => debug_assert!(false, "flag must never be applied"),
        }
    }

    /// Mutates rope/index/rev/hooks for an insertion.
    pub(crate) fn apply_insert_raw(&mut self, at: usize, text: &str) {
        let n = text.chars().count();
        self.index.shift_ge(at, n as i64);
        for (i, ch) in text.chars().enumerate() {
            if ch == '\n' {
                self.index.insert(at + i);
            }
        }
        self.rope.insert(at, text);
        self.rev += 1;
        self.notify_hooks(&Operation::Insert {
            at,
            text: text.to_string(),
        });
    }

    /// Mutates rope/index/rev/hooks for a deletion.
    pub(crate) fn apply_delete_raw(&mut self, start: usize, end: usize, text: &str) {
        debug_assert_eq!(text.chars().count(), end - start, "delete length mismatch");
        for (i, ch) in text.chars().enumerate() {
            if ch == '\n' {
                self.index.remove(start + i);
            }
        }
        self.index.shift_ge(end, -((end - start) as i64));
        self.rope.remove(start..end);
        self.rev += 1;
        self.notify_hooks(&Operation::Delete {
            at: start,
            text: text.to_string(),
        });
    }

    /// Applies a deletion and returns its inverse without recording it.
    fn delete_grouped(&mut self, start: usize, end: usize) -> Operation {
        let removed = self.rope.slice(start..end).to_string();
        self.apply_delete_raw(start, end, &removed);
        Operation::Insert {
            at: start,
            text: removed,
        }
    }

    /// Applies an insertion and returns its inverse without recording it.
    fn insert_grouped(&mut self, at: usize, text: &str) -> Operation {
        self.apply_insert_raw(at, text);
        Operation::Delete {
            at,
            text: text.to_string(),
        }
    }
}
