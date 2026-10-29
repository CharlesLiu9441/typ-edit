//! Caret + selection model for the virtualized text path (Stage B).
//!
//! Positions are global **char** indices into the buffer's rope; every helper
//! converts through [`typ_edit_buffer::TextBuffer`]. The caret is always in the
//! buffer; the anchor is set only while a selection is being dragged out.
//!
//! A [`Carets`] holds an ordered, never-empty list of [`Selection`]s plus a
//! primary index. The single-caret API on [`Carets`] delegates to the primary so
//! call sites that only ever see one caret keep working unchanged; multi-caret
//! edits go through [`Carets::edit_ranges_descending`].

use std::ops::Range;

use typ_edit_buffer::TextBuffer;

/// Caret position and optional selection anchor, in global char indices.
///
/// `selection()` returns the sorted range between the two, or `None` when the
/// anchor is absent or coincides with the caret. `h_pos` remembers the desired
/// vertical x (galley-space points) for this caret across consecutive Up/Down
/// moves; it lives per caret so a future column-cursor feature can preserve one
/// column per caret.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Selection {
    /// The moving end of the selection (where a new drag or click points).
    pub caret: usize,
    /// The fixed end; `None` means "no selection".
    pub anchor: Option<usize>,
    /// Remembered vertical x for this caret, or `None` when idle.
    pub h_pos: Option<f32>,
}

impl Selection {
    /// The sorted selected char range, or `None` when there is no selection.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.caret).then(|| anchor.min(self.caret)..anchor.max(self.caret))
    }

    /// Moves the caret and clears the selection. `h_pos` is left to the caller:
    /// the horizontal-move and edit sites clear it explicitly.
    pub fn set_caret(&mut self, caret: usize) {
        self.caret = caret;
        self.anchor = None;
    }

    /// Moves the caret while keeping the anchor (drag / Shift+click).
    pub fn extend_to(&mut self, to: usize) {
        self.caret = to;
    }

    /// Begins a drag at `at`: caret and anchor both land there.
    pub fn start_drag(&mut self, at: usize) {
        self.caret = at;
        self.anchor = Some(at);
    }

    /// Selects the word (a run of `char::is_alphanumeric` or `_`) that the
    /// caret boundary `char` points into.
    ///
    /// Because [`Self::select_word_at`] receives a caret boundary rather than a
    /// glyph index, a caret immediately after a word still selects that word
    /// (the common double-click-at-word-end behavior). With no adjacent word
    /// char the caret simply moves and the selection is cleared.
    pub fn select_word_at(&mut self, buffer: &TextBuffer, char: usize) {
        let len = buffer.len_chars();
        let char = char.min(len);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let probe = if char < len {
            char
        } else if char > 0 {
            char - 1
        } else {
            0
        };
        let probe = if buffer.char_at(probe).is_some_and(is_word) {
            probe
        } else if probe > 0 && buffer.char_at(probe - 1).is_some_and(is_word) {
            probe - 1
        } else {
            self.set_caret(char);
            return;
        };
        let mut start = probe;
        while start > 0 && buffer.char_at(start - 1).is_some_and(is_word) {
            start -= 1;
        }
        let mut end = probe + 1;
        while end < len && buffer.char_at(end).is_some_and(is_word) {
            end += 1;
        }
        self.anchor = Some(start);
        self.caret = end;
    }

    /// Selects logical `line`, excluding its trailing newline.
    pub fn select_line_at(&mut self, buffer: &TextBuffer, line: usize) {
        self.anchor = Some(buffer.line_start(line));
        self.caret = super::hit::line_content_end(buffer, line);
    }

    /// Clamps caret and anchor into `[0, len]` after a buffer change.
    pub fn clamp(&mut self, len: usize) {
        self.caret = self.caret.min(len);
        if let Some(anchor) = self.anchor {
            self.anchor = Some(anchor.min(len));
        }
    }
}

/// An ordered set of carets with one primary; the list is never empty.
///
/// Order is stable across edits so the index returned by
/// [`Self::edit_ranges_descending`] stays valid for [`Self::prepare_edit`]. The
/// single-caret compatibility methods operate on the primary entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Carets {
    list: Vec<Selection>,
    primary: usize,
}

impl Default for Carets {
    fn default() -> Self {
        Self {
            list: vec![Selection::default()],
            primary: 0,
        }
    }
}

impl Carets {
    /// The primary caret selection (a copy).
    pub fn primary(&self) -> Selection {
        self.list.get(self.primary).copied().unwrap_or_default()
    }

    /// Mutable access to the primary caret selection, re-seeding an empty list.
    pub fn primary_mut(&mut self) -> &mut Selection {
        if self.is_empty() {
            self.list.push(Selection::default());
        }
        if self.primary >= self.list.len() {
            self.primary = self.list.len() - 1;
        }
        &mut self.list[self.primary]
    }

    /// All caret selections in order.
    pub fn list(&self) -> &[Selection] {
        &self.list
    }

    /// Mutable access to the raw caret list (order must be preserved).
    pub fn list_mut(&mut self) -> &mut Vec<Selection> {
        &mut self.list
    }

    /// Number of carets.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether there are no carets (only possible after a direct
    /// [`Self::list_mut`] mutation; accessors re-seed on demand).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Moves the primary index, clamped to the last caret.
    pub fn set_primary(&mut self, index: usize) {
        if self.is_empty() {
            self.list.push(Selection::default());
        }
        self.primary = index.min(self.list.len() - 1);
    }

    /// Whether any caret carries a non-empty selection.
    pub fn any_selection(&self) -> bool {
        self.list.iter().any(|sel| sel.selection().is_some())
    }

    // ---- multi-caret mutation ----------------------------------------------

    /// Drops every caret but the primary one. Re-seeds an empty list.
    pub fn collapse_to_primary(&mut self) {
        if self.is_empty() {
            self.list.push(Selection::default());
            self.primary = 0;
            return;
        }
        let primary = self.primary();
        self.list.clear();
        self.list.push(primary);
        self.primary = 0;
    }

    /// Replaces the whole list with a single caret selection.
    pub fn set_single(&mut self, selection: Selection) {
        self.list.clear();
        self.list.push(selection);
        self.primary = 0;
    }

    /// Appends `selection` and makes it the primary.
    pub fn push_selection(&mut self, selection: Selection) {
        self.list.push(selection);
        self.primary = self.list.len() - 1;
    }

    /// Removes caret `index`, keeping the primary valid. Never empties the list,
    /// so a stale index or a lone caret is a no-op.
    pub fn remove_at(&mut self, index: usize) {
        if self.list.len() <= 1 || index >= self.list.len() {
            return;
        }
        self.list.remove(index);
        if self.primary > index {
            self.primary -= 1;
        } else if self.primary == index {
            self.primary = self.primary.min(self.list.len() - 1);
        }
    }

    /// Removes duplicate `(caret, anchor)` entries, keeping the first in order.
    /// The primary follows its surviving entry (or the kept duplicate).
    pub fn dedup_exact(&mut self) {
        let mut kept: Vec<Selection> = Vec::with_capacity(self.list.len());
        let mut primary = None;
        for (index, sel) in self.list.iter().enumerate() {
            match kept
                .iter()
                .position(|k| k.caret == sel.caret && k.anchor == sel.anchor)
            {
                Some(pos) => {
                    if index == self.primary {
                        primary = Some(pos);
                    }
                }
                None => {
                    if index == self.primary {
                        primary = Some(kept.len());
                    }
                    kept.push(*sel);
                }
            }
        }
        if kept.is_empty() {
            kept.push(Selection::default());
        }
        self.list = kept;
        self.primary = primary.unwrap_or(0).min(self.list.len() - 1);
    }

    /// Index of the first collapsed caret exactly at `caret` (anchor absent).
    pub fn index_for_caret(&self, caret: usize) -> Option<usize> {
        self.list
            .iter()
            .position(|sel| sel.caret == caret && sel.anchor.is_none())
    }

    /// Clears every caret's remembered vertical x (a non-vertical move/edit).
    pub fn clear_h_pos(&mut self) {
        for sel in &mut self.list {
            sel.h_pos = None;
        }
    }

    // ---- single-caret compatibility (primary) ------------------------------

    /// The primary caret as a global char index.
    pub fn caret(&self) -> usize {
        self.primary().caret
    }

    /// Moves the primary caret and clears its selection.
    pub fn set_caret(&mut self, caret: usize) {
        self.primary_mut().set_caret(caret);
    }

    /// Moves caret `index` and clears its selection (no-op for a stale index).
    pub fn set_caret_at(&mut self, index: usize, caret: usize) {
        if let Some(sel) = self.list.get_mut(index) {
            sel.set_caret(caret);
        }
    }

    /// The primary selection range, if any.
    pub fn selection(&self) -> Option<Range<usize>> {
        self.primary().selection()
    }

    /// Sets the primary anchor + caret pair.
    pub fn set_selection(&mut self, anchor: usize, caret: usize) {
        let primary = self.primary_mut();
        primary.anchor = Some(anchor);
        primary.caret = caret;
    }

    /// Moves the primary caret keeping its anchor.
    pub fn extend_to(&mut self, to: usize) {
        self.primary_mut().extend_to(to);
    }

    /// Begins a primary drag at `at`.
    pub fn start_drag(&mut self, at: usize) {
        self.primary_mut().start_drag(at);
    }

    /// Selects the logical line under the primary caret.
    pub fn select_line_at(&mut self, buffer: &TextBuffer, line: usize) {
        self.primary_mut().select_line_at(buffer, line);
    }

    /// Clamps **every** caret and anchor into `[0, len]`.
    pub fn clamp(&mut self, len: usize) {
        if self.is_empty() {
            self.list.push(Selection::default());
        }
        for sel in &mut self.list {
            sel.clamp(len);
        }
        self.primary = self.primary.min(self.list.len() - 1);
    }

    /// Replaces every caret with a single full-document selection (`0..len`).
    pub fn select_all(&mut self, len: usize) {
        self.list.clear();
        self.list.push(Selection {
            caret: len,
            anchor: Some(0),
            h_pos: None,
        });
        self.set_primary(0);
    }

    /// The primary caret's remembered vertical x.
    pub fn h_pos(&self) -> Option<f32> {
        self.primary().h_pos
    }

    /// Sets the primary caret's remembered vertical x.
    pub fn set_h_pos(&mut self, h_pos: Option<f32>) {
        self.primary_mut().h_pos = h_pos;
    }

    /// Every caret as `(list index, normalized edit range)`, sorted **descending**
    /// by edit position. A collapsed caret yields an empty range at itself. The
    /// index is stable and can be handed to [`Self::prepare_edit`].
    pub fn edit_ranges_descending(&self) -> Vec<(usize, Range<usize>)> {
        let mut items: Vec<(usize, Range<usize>)> = self
            .list()
            .iter()
            .enumerate()
            .map(|(index, sel)| (index, sel.selection().unwrap_or(sel.caret..sel.caret)))
            .collect();
        items.sort_by(|a, b| {
            b.1.start
                .cmp(&a.1.start)
                .then_with(|| b.1.end.cmp(&a.1.end))
        });
        items
    }

    /// Collapses caret `index` onto `range.start` and clears its anchor + h_pos,
    /// preparing it for a replace of `range`. Called before the buffer edit so
    /// the caret-sync hook leaves it after the inserted text.
    pub fn prepare_edit(&mut self, index: usize, range: &Range<usize>) {
        if let Some(sel) = self.list.get_mut(index) {
            sel.caret = range.start;
            sel.anchor = None;
            sel.h_pos = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> TextBuffer {
        TextBuffer::from_text(text)
    }

    #[test]
    fn selection_is_sorted_and_absent_when_empty() {
        let mut sel = Selection::default();
        assert!(sel.selection().is_none());
        sel.start_drag(5);
        assert!(sel.selection().is_none(), "anchor == caret is empty");
        sel.extend_to(2);
        assert_eq!(sel.selection(), Some(2..5));
        sel.extend_to(9);
        assert_eq!(sel.selection(), Some(5..9));
        sel.set_caret(9);
        assert!(sel.selection().is_none());
    }

    #[test]
    fn word_selection_covers_alphanumerics_and_underscores() {
        let buf = buffer("foo_bar baz");
        let mut sel = Selection::default();
        sel.select_word_at(&buf, 3);
        assert_eq!(sel.selection(), Some(0..7));
        sel.select_word_at(&buf, 8);
        assert_eq!(sel.selection(), Some(8..11));
    }

    #[test]
    fn word_selection_prefers_the_word_left_of_the_caret() {
        let buf = buffer("hello world");
        let mut sel = Selection::default();
        // Caret at index 5 is the boundary between "hello" and the space.
        sel.select_word_at(&buf, 5);
        assert_eq!(sel.selection(), Some(0..5));
        // Caret at the very end still selects the trailing word.
        sel.select_word_at(&buf, buf.len_chars());
        assert_eq!(sel.selection(), Some(6..11));
    }

    #[test]
    fn word_selection_on_punctuation_clears_it() {
        let buf = buffer("a  b");
        let mut sel = Selection::default();
        sel.select_word_at(&buf, 1);
        assert_eq!(
            sel.selection(),
            Some(0..1),
            "the word to the left survives at a boundary"
        );
        sel.select_word_at(&buf, 2);
        assert!(sel.selection().is_none(), "no adjacent word char");
    }

    #[test]
    fn word_selection_handles_cjk() {
        let buf = buffer("你好 world");
        let mut sel = Selection::default();
        sel.select_word_at(&buf, 1);
        assert_eq!(sel.selection(), Some(0..2));
    }

    #[test]
    fn line_selection_excludes_the_trailing_newline() {
        let buf = buffer("aaa\nbb\n");
        let mut sel = Selection::default();
        sel.select_line_at(&buf, 0);
        assert_eq!(sel.selection(), Some(0..3));
        sel.select_line_at(&buf, 1);
        assert_eq!(sel.selection(), Some(4..6));
        sel.select_line_at(&buf, 2);
        assert!(sel.selection().is_none(), "the empty last line is empty");
    }

    #[test]
    fn clamp_pulls_positions_into_the_buffer() {
        let mut sel = Selection {
            caret: 99,
            anchor: Some(50),
            h_pos: Some(1.5),
        };
        sel.clamp(10);
        assert_eq!(
            sel,
            Selection {
                caret: 10,
                anchor: Some(10),
                h_pos: Some(1.5),
            }
        );
    }

    #[test]
    fn carets_default_is_one_empty_primary() {
        let carets = Carets::default();
        assert_eq!(carets.len(), 1);
        assert!(!carets.is_empty());
        assert!(!carets.any_selection());
        assert_eq!(carets.caret(), 0);
        assert!(carets.selection().is_none());
    }

    #[test]
    fn carets_single_caret_methods_delegate_to_primary() {
        let buf = buffer("hello world");
        let mut carets = Carets::default();
        carets.set_selection(0, 5);
        assert_eq!(carets.selection(), Some(0..5));
        carets.primary_mut().select_word_at(&buf, 8);
        assert_eq!(carets.selection(), Some(6..11));
        carets.select_line_at(&buf, 0);
        assert_eq!(carets.selection(), Some(0..11));
        carets.set_caret(3);
        assert_eq!(carets.caret(), 3);
        assert!(carets.selection().is_none());
    }

    #[test]
    fn select_all_replaces_every_caret() {
        let mut carets = Carets::default();
        carets.list_mut().push(Selection {
            caret: 4,
            anchor: None,
            h_pos: None,
        });
        carets.set_primary(1);
        carets.select_all(10);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets.primary().caret, 10);
        assert_eq!(carets.selection(), Some(0..10));
    }

    #[test]
    fn clamp_affects_every_caret() {
        let mut carets = Carets::default();
        carets.list_mut().push(Selection {
            caret: 99,
            anchor: Some(50),
            h_pos: None,
        });
        carets.clamp(10);
        assert_eq!(carets.list()[0].caret, 0);
        assert_eq!(carets.list()[1].caret, 10);
        assert_eq!(carets.list()[1].anchor, Some(10));
    }

    #[test]
    fn h_pos_proxy_targets_the_primary() {
        let mut carets = Carets::default();
        assert_eq!(carets.h_pos(), None);
        carets.set_h_pos(Some(7.0));
        assert_eq!(carets.h_pos(), Some(7.0));
        carets.set_h_pos(None);
        assert_eq!(carets.h_pos(), None);
    }

    #[test]
    fn edit_ranges_sort_descending_and_normalize() {
        let mut carets = Carets::default();
        carets.list_mut().clear();
        carets.list_mut().push(Selection {
            caret: 2,
            anchor: None,
            h_pos: None,
        });
        carets.list_mut().push(Selection {
            caret: 9,
            anchor: Some(5),
            h_pos: None,
        });
        let ranges = carets.edit_ranges_descending();
        assert_eq!(ranges[0].1, 5..9, "highest edit first");
        assert_eq!(ranges[1].1, 2..2, "collapsed caret as an empty range");
    }

    #[test]
    fn prepare_edit_collapses_and_clears_the_anchor() {
        let mut carets = Carets::default();
        carets.set_selection(1, 5);
        carets.set_h_pos(Some(3.0));
        carets.prepare_edit(0, &(2..7));
        let primary = carets.primary();
        assert_eq!(primary.caret, 2);
        assert_eq!(primary.anchor, None);
        assert_eq!(primary.h_pos, None);
    }
}
