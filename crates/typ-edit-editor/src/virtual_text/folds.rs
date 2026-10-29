//! Fold candidates and collapsed-fold state for code folding.
//!
//! The parse layer reports foldable constructs as byte ranges
//! ([`typ_edit_model::FoldRange`]); this module converts them to char offsets in
//! the authoritative buffer, tracks which folds the user collapsed, and derives
//! the hidden logical lines the layout, scroll and navigation passes must skip.
//!
//! A collapsed fold hides the logical lines strictly after its start line and up
//! to and including its end line: `(line_of(start), line_of(end)]`. The start
//! line itself always stays visible. Candidates are rebuilt every frame from the
//! (possibly delta-adjusted) parse ranges, so a stale fold survives an edit the
//! same way the other parse-derived ranges do.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;

use typ_edit_buffer::{EditHook, Operation, TextBuffer};
use typ_edit_model::FoldRange;

use super::caret_sync::shift_position;
use crate::EditorState;

/// One foldable construct as char offsets into the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FoldCandidate {
    pub start_char: usize,
    pub end_char: usize,
}

/// Fold candidates plus the collapsed fold starts (char offsets).
pub(crate) struct Folds {
    pub(super) candidates: Vec<FoldCandidate>,
    pub(super) collapsed: Rc<RefCell<BTreeSet<usize>>>,
}

impl Default for Folds {
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
            collapsed: Rc::new(RefCell::new(BTreeSet::new())),
        }
    }
}

impl Folds {
    /// Rebuilds the candidates from the parse's byte ranges and drops collapsed
    /// offsets whose fold no longer exists.
    pub(super) fn sync(&mut self, buffer: &TextBuffer, folds: &[FoldRange]) {
        self.candidates = folds
            .iter()
            .map(|fold| FoldCandidate {
                start_char: buffer.byte_to_char(fold.start.0),
                end_char: buffer.byte_to_char(fold.end.0),
            })
            .collect();
        self.collapsed.borrow_mut().retain(|start| {
            self.candidates
                .iter()
                .any(|candidate| candidate.start_char == *start)
        });
    }

    /// The hidden logical line ranges for the collapsed folds, merged and
    /// sorted. Empty when nothing is collapsed.
    pub(crate) fn hidden_lines(&self, buffer: &TextBuffer) -> Vec<Range<usize>> {
        let collapsed = self.collapsed.borrow();
        if collapsed.is_empty() {
            return Vec::new();
        }
        let mut ranges = Vec::new();
        for candidate in &self.candidates {
            if !collapsed.contains(&candidate.start_char) {
                continue;
            }
            let start_line = buffer.line_of_char(candidate.start_char);
            let end_line = buffer.line_of_char(candidate.end_char);
            let lo = start_line + 1;
            let hi = end_line + 1;
            if lo < hi {
                ranges.push(lo..hi);
            }
        }
        merge_hidden(ranges)
    }

    /// Whether the fold starting at `start_char` is collapsed.
    pub(super) fn is_collapsed(&self, start_char: usize) -> bool {
        self.collapsed.borrow().contains(&start_char)
    }

    /// Collapses the fold at `start_char`; `true` when it was not already
    /// collapsed.
    fn collapse(&mut self, start_char: usize) -> bool {
        self.collapsed.borrow_mut().insert(start_char)
    }

    /// Expands the fold at `start_char`; `true` when it was collapsed.
    fn expand(&mut self, start_char: usize) -> bool {
        self.collapsed.borrow_mut().remove(&start_char)
    }

    /// Collapses every candidate; `true` when the set changed.
    fn collapse_all(&mut self) -> bool {
        let mut collapsed = self.collapsed.borrow_mut();
        let before = collapsed.len();
        for candidate in &self.candidates {
            collapsed.insert(candidate.start_char);
        }
        collapsed.len() != before
    }

    /// Expands every collapsed fold; `true` when anything was collapsed.
    fn expand_all(&mut self) -> bool {
        let mut collapsed = self.collapsed.borrow_mut();
        let changed = !collapsed.is_empty();
        collapsed.clear();
        changed
    }

    /// The `start_char` of the candidate whose start line is `line`, if any.
    fn candidate_start_at_line(&self, buffer: &TextBuffer, line: usize) -> Option<usize> {
        self.candidates
            .iter()
            .find(|candidate| buffer.line_of_char(candidate.start_char) == line)
            .map(|candidate| candidate.start_char)
    }

    /// Toggles the fold whose start line is `line`; `true` when the set changed.
    pub(super) fn toggle_at_line(&mut self, buffer: &TextBuffer, line: usize) -> bool {
        let Some(start) = self.candidate_start_at_line(buffer, line) else {
            return false;
        };
        if self.is_collapsed(start) {
            self.expand(start)
        } else {
            self.collapse(start)
        }
    }

    /// `(start line, collapsed, hidden line count)` for every candidate, for the
    /// gutter chevrons and collapse badges.
    pub(super) fn markers(&self, buffer: &TextBuffer) -> Vec<(usize, bool, usize)> {
        self.candidates
            .iter()
            .map(|candidate| {
                let start_line = buffer.line_of_char(candidate.start_char);
                let end_line = buffer.line_of_char(candidate.end_char);
                (
                    start_line,
                    self.is_collapsed(candidate.start_char),
                    end_line.saturating_sub(start_line),
                )
            })
            .collect()
    }

    /// Number of collapsed folds (tests).
    #[cfg(test)]
    pub(super) fn collapsed_count(&self) -> usize {
        self.collapsed.borrow().len()
    }
}

/// Sorts hidden ranges by start and merges overlapping or adjacent ones, so a
/// run of consecutive hidden lines is one range.
pub(super) fn merge_hidden(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Whether `line` is hidden by `hidden`.
pub(super) fn is_hidden(hidden: &[Range<usize>], line: usize) -> bool {
    hidden
        .iter()
        .any(|range| range.start <= line && line < range.end)
}

/// The first visible line at or after `line` (or `line` itself when it is not
/// hidden). May exceed the document end when every following line is hidden.
pub(super) fn next_visible(hidden: &[Range<usize>], line: usize) -> usize {
    let mut line = line;
    for range in hidden {
        if range.end <= line {
            continue;
        }
        if range.start > line {
            break;
        }
        line = range.end;
    }
    line
}

/// The last visible line at or before `line` (or `line` itself when it is not
/// hidden). Saturates at 0 when every line up to `line` is hidden.
pub(super) fn prev_visible(hidden: &[Range<usize>], line: usize) -> usize {
    let mut line = line;
    for range in hidden.iter().rev() {
        if range.end <= line {
            break;
        }
        if range.start <= line {
            line = range.start.saturating_sub(1);
        }
    }
    line
}

/// The nearest visible line to `line`: the first visible line at or after it,
/// falling back to the previous visible line when none follows.
pub(super) fn snap_visible(hidden: &[Range<usize>], line: usize, line_count: usize) -> usize {
    if line_count == 0 {
        return 0;
    }
    let line = line.min(line_count - 1);
    if !is_hidden(hidden, line) {
        return line;
    }
    let next = next_visible(hidden, line);
    if next < line_count {
        next
    } else {
        prev_visible(hidden, line_count - 1)
    }
}

/// Buffer hook that keeps the collapsed offsets aligned with every edit,
/// including undo/redo, exactly like [`super::caret_sync::CaretSyncHook`].
pub(super) struct FoldSyncHook {
    collapsed: Rc<RefCell<BTreeSet<usize>>>,
}

impl FoldSyncHook {
    pub(super) fn new(collapsed: Rc<RefCell<BTreeSet<usize>>>) -> Self {
        Self { collapsed }
    }
}

impl EditHook for FoldSyncHook {
    fn on_edit(&mut self, op: &Operation, buffer: &TextBuffer) {
        let len = buffer.len_chars();
        let mut collapsed = self.collapsed.borrow_mut();
        let shifted: BTreeSet<usize> = collapsed
            .iter()
            .map(|pos| shift_position(op, *pos, len))
            .collect();
        *collapsed = shifted;
    }
}

impl EditorState {
    /// Collapses the fold whose start line is the caret's line. Returns whether
    /// the collapsed set changed.
    pub fn fold_at_caret(&mut self) -> bool {
        let line = self.buffer.line_of_char(self.virtual_text.caret());
        self.change_fold_at_line(line, true)
    }

    /// Expands the collapsed fold whose start line is the caret's line. Returns
    /// whether the collapsed set changed.
    pub fn unfold_at_caret(&mut self) -> bool {
        let line = self.buffer.line_of_char(self.virtual_text.caret());
        self.change_fold_at_line(line, false)
    }

    /// Collapses every candidate. Returns whether the collapsed set changed.
    pub fn fold_all(&mut self) -> bool {
        let changed = self.virtual_text.folds.collapse_all();
        if changed {
            self.virtual_text.relocate_out_of_hidden(&self.buffer);
        }
        changed
    }

    /// Expands every collapsed fold. Returns whether anything was collapsed.
    pub fn unfold_all(&mut self) -> bool {
        self.virtual_text.folds.expand_all()
    }

    /// Toggles the fold whose start line is `line` (gutter chevron click).
    /// Returns whether the collapsed set changed.
    pub fn toggle_fold_line(&mut self, line: usize) -> bool {
        let changed = self.virtual_text.folds.toggle_at_line(&self.buffer, line);
        if changed {
            self.virtual_text.relocate_out_of_hidden(&self.buffer);
        }
        changed
    }

    /// Collapses or expands the fold whose start line is `line`, relocating the
    /// caret out of any region the collapse hides.
    fn change_fold_at_line(&mut self, line: usize, collapse: bool) -> bool {
        let Some(start) = self
            .virtual_text
            .folds
            .candidate_start_at_line(&self.buffer, line)
        else {
            return false;
        };
        let changed = if collapse {
            self.virtual_text.folds.collapse(start)
        } else {
            self.virtual_text.folds.expand(start)
        };
        if changed {
            self.virtual_text.relocate_out_of_hidden(&self.buffer);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_model::{BytePos, FoldKind};

    fn fold(start: usize, end: usize) -> FoldRange {
        FoldRange {
            start: BytePos(start),
            end: BytePos(end),
            kind: FoldKind::Block,
        }
    }

    #[test]
    fn hidden_helpers_see_through_ranges() {
        let hidden = [2..5, 7..9];
        assert!(!is_hidden(&hidden, 1));
        assert!(is_hidden(&hidden, 2));
        assert!(is_hidden(&hidden, 4));
        assert!(!is_hidden(&hidden, 5));
        assert_eq!(next_visible(&hidden, 1), 1);
        assert_eq!(next_visible(&hidden, 2), 5);
        assert_eq!(next_visible(&hidden, 5), 5);
        assert_eq!(next_visible(&hidden, 8), 9);
        assert_eq!(prev_visible(&hidden, 10), 10);
        assert_eq!(prev_visible(&hidden, 5), 5);
        assert_eq!(prev_visible(&hidden, 4), 1);
        assert_eq!(prev_visible(&hidden, 8), 6);
    }

    #[test]
    fn snap_visible_falls_back_to_the_previous_visible_line() {
        let hidden = std::slice::from_ref(&(1..3));
        assert_eq!(snap_visible(hidden, 0, 4), 0);
        assert_eq!(snap_visible(hidden, 1, 4), 3);
        assert_eq!(snap_visible(hidden, 3, 4), 3);
        assert_eq!(snap_visible(hidden, 1, 2), 0, "no following visible line");
        assert_eq!(snap_visible(hidden, 5, 0), 0, "empty document");
    }

    #[test]
    fn sync_converts_bytes_to_chars_and_retains_live_folds() {
        let buffer = TextBuffer::from_text("你好\nabc\nxyz");
        let start = buffer.char_to_byte(buffer.line_start(1));
        let end = buffer.len_bytes();
        let mut folds = Folds::default();
        folds.sync(&buffer, &[fold(start, end)]);
        assert_eq!(folds.candidates.len(), 1);
        let collapsed = folds.candidates[0].start_char;
        assert!(folds.collapse(collapsed));
        assert!(!folds.collapse(collapsed), "already collapsed");

        // Re-syncing the same range keeps the collapsed fold.
        folds.sync(&buffer, &[fold(start, end)]);
        assert!(folds.is_collapsed(collapsed));

        // Dropping the candidate drops the collapsed offset.
        folds.sync(&buffer, &[]);
        assert_eq!(folds.collapsed_count(), 0);
    }

    #[test]
    fn hidden_lines_cover_the_lines_after_the_start() {
        let buffer = TextBuffer::from_text("h\nb\nc\nd");
        let mut folds = Folds::default();
        folds.sync(&buffer, &[fold(0, 4)]);
        folds.collapse(folds.candidates[0].start_char);
        assert_eq!(
            folds.hidden_lines(&buffer),
            vec![1..3],
            "lines 1 and 2 are hidden"
        );
    }

    #[test]
    fn hidden_lines_merge_overlapping_and_adjacent_folds() {
        let buffer = TextBuffer::from_text("0\n1\n2\n3\n4\n5\n6");
        let mut folds = Folds::default();
        // Fold A hides line 1; fold B hides lines 2..=3; fold C hides line 4.
        folds.sync(&buffer, &[fold(0, 2), fold(2, 6), fold(6, 8)]);
        let starts: Vec<usize> = folds.candidates.iter().map(|c| c.start_char).collect();
        for start in starts {
            folds.collapse(start);
        }
        assert_eq!(folds.hidden_lines(&buffer), vec![1..5]);
    }

    #[test]
    fn sync_guards_out_of_range_bytes() {
        let buffer = TextBuffer::from_text("abc");
        let mut folds = Folds::default();
        folds.sync(&buffer, &[fold(1_000, 2_000)]);
        assert_eq!(folds.candidates.len(), 1);
        assert_eq!(folds.candidates[0].start_char, 3);
        assert_eq!(folds.candidates[0].end_char, 3);
        folds.collapse(folds.candidates[0].start_char);
        assert!(folds.hidden_lines(&buffer).is_empty());
    }

    #[test]
    fn fold_hook_shifts_collapsed_offsets_across_edits() {
        let collapsed = Rc::new(RefCell::new(BTreeSet::new()));
        collapsed.borrow_mut().insert(5);
        let mut buffer = TextBuffer::from_text("abcdefgh");
        buffer.add_hook(Box::new(FoldSyncHook::new(collapsed.clone())));
        buffer.insert(0, "XY", true);
        assert_eq!(*collapsed.borrow(), BTreeSet::from([7]));
        buffer.delete(0..2, true);
        assert_eq!(*collapsed.borrow(), BTreeSet::from([5]));
    }
}
