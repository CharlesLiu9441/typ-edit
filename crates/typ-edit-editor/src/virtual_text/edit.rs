//! Buffer-editing primitives for the virtualized text path (Stage C).
//!
//! Every mutation goes through [`EditorState::edit_bytes`] /
//! [`EditorState::replace_all_text`], so the authoritative
//! [`typ_edit_buffer::TextBuffer`] and the shared undo group stay in sync. The
//! caret and selection are
//! updated by the buffer's [`super::caret_sync::CaretSyncHook`]; only
//! *semantic* placements (auto-indent, whole-line tab) are set explicitly.
//!
//! Multi-caret keys apply to every selection, **descending by edit position**:
//! higher edits never move lower positions, so a snapshot taken before the loop
//! stays valid and every sub-edit merges into the one open undo group.

use std::ops::Range;

use crate::{EditorInput, EditorState, INDENT, smart_edit};

impl EditorState {
    /// Sorted char range of the primary selection, or an empty range at the
    /// primary caret.
    fn selection_bounds(&self) -> Range<usize> {
        self.virtual_text.selection_range().unwrap_or_else(|| {
            let caret = self.virtual_text.caret();
            caret..caret
        })
    }

    /// Collapses caret `index` onto `range.start` before its buffer edit, so the
    /// sync hook leaves it after the replaced text and clears its h_pos.
    fn prepare_caret(&self, index: usize, range: &Range<usize>) {
        self.virtual_text.carets_mut().prepare_edit(index, range);
    }

    /// Deletes the primary selection, if any. Returns whether an edit applied.
    /// Primary-oriented: IME uses it; the clipboard and the multi-caret editing
    /// keys drive [`Self::delete_selections`]/[`Self::virtual_delete`] instead.
    pub(super) fn delete_selection(&mut self) -> bool {
        let Some(selection) = self.virtual_text.selection_range() else {
            return false;
        };
        let start = self.buffer.char_to_byte(selection.start);
        let end = self.buffer.char_to_byte(selection.end);
        self.edit_bytes(start..end, "");
        self.virtual_text.set_h_pos(None);
        true
    }

    /// Deletes every non-empty selection, descending by position, as one undo
    /// group. Collapsed carets contribute nothing and are left in place.
    /// Returns whether any edit applied.
    pub(super) fn delete_selections(&mut self) -> bool {
        let ranges = self.virtual_text.edit_ranges_descending();
        let mut changed = false;
        for (index, range) in ranges {
            if range.is_empty() {
                continue;
            }
            self.prepare_caret(index, &range);
            let start = self.buffer.char_to_byte(range.start);
            let end = self.buffer.char_to_byte(range.end);
            self.edit_bytes(start..end, "");
            changed = true;
        }
        if changed {
            self.virtual_text.set_h_pos(None);
        }
        changed
    }

    /// Replaces the primary selection (or inserts at the primary caret) with
    /// `text`; the caret lands after the insertion via the sync hook. Used by
    /// the queued `pending_insert`, which targets the primary only.
    fn insert_at_primary(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let selection = self.virtual_text.selection_range();
        let start = selection
            .as_ref()
            .map_or(self.virtual_text.caret(), |range| range.start);
        let start_byte = self.buffer.char_to_byte(start);
        let end_byte = selection
            .as_ref()
            .map_or(start_byte, |range| self.buffer.char_to_byte(range.end));
        self.edit_bytes(start_byte..end_byte, text);
        self.virtual_text.set_h_pos(None);
        true
    }

    /// Replaces every selection (or inserts at every caret) with `text`,
    /// descending by position. Returns whether an edit applied.
    pub(super) fn virtual_insert(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let ranges = self.virtual_text.edit_ranges_descending();
        if ranges.len() <= 1 {
            return self.insert_at_primary(text);
        }
        for (index, range) in ranges {
            self.prepare_caret(index, &range);
            let start = self.buffer.char_to_byte(range.start);
            let end = self.buffer.char_to_byte(range.end);
            self.edit_bytes(start..end, text);
        }
        true
    }

    /// Backspace at every caret, descending: a selection is removed whole,
    /// otherwise an empty delimiter pair collapses into nothing, otherwise the
    /// previous char is deleted.
    pub(super) fn virtual_backspace(&mut self) -> bool {
        let ranges = self.virtual_text.edit_ranges_descending();
        let mut changed = false;
        for (index, range) in ranges {
            if !range.is_empty() {
                self.prepare_caret(index, &range);
                let start = self.buffer.char_to_byte(range.start);
                let end = self.buffer.char_to_byte(range.end);
                self.edit_bytes(start..end, "");
                changed = true;
                continue;
            }
            let caret = range.start;
            if caret == 0 {
                continue;
            }
            self.prepare_caret(index, &range);
            if let Some(pair) = smart_edit::empty_pair_at_buffer(&self.buffer, caret) {
                let start = self.buffer.char_to_byte(pair.start);
                let end = self.buffer.char_to_byte(pair.end);
                self.edit_bytes(start..end, "");
            } else {
                let start = self.buffer.char_to_byte(caret - 1);
                let end = self.buffer.char_to_byte(caret);
                self.edit_bytes(start..end, "");
            }
            changed = true;
        }
        changed
    }

    /// Delete at every caret, descending: a selection is removed whole,
    /// otherwise an empty delimiter pair collapses, otherwise the next char is
    /// deleted.
    pub(super) fn virtual_delete(&mut self) -> bool {
        let ranges = self.virtual_text.edit_ranges_descending();
        let len = self.buffer.len_chars();
        let mut changed = false;
        for (index, range) in ranges {
            if !range.is_empty() {
                self.prepare_caret(index, &range);
                let start = self.buffer.char_to_byte(range.start);
                let end = self.buffer.char_to_byte(range.end);
                self.edit_bytes(start..end, "");
                changed = true;
                continue;
            }
            let caret = range.start;
            if caret >= len {
                continue;
            }
            self.prepare_caret(index, &range);
            if let Some(pair) = smart_edit::empty_pair_at_buffer(&self.buffer, caret) {
                let start = self.buffer.char_to_byte(pair.start);
                let end = self.buffer.char_to_byte(pair.end);
                self.edit_bytes(start..end, "");
            } else {
                let start = self.buffer.char_to_byte(caret);
                let end = self.buffer.char_to_byte(caret + 1);
                self.edit_bytes(start..end, "");
            }
            changed = true;
        }
        changed
    }

    /// Enter at every caret, descending: insert a newline, then port the
    /// `auto_indent` continuation rule for each caret.
    pub(super) fn virtual_enter(&mut self, input: &EditorInput<'_>) -> bool {
        let list_lines: Vec<usize> = input.list_lines.iter().map(|pos| pos.0).collect();
        let ranges = self.virtual_text.edit_ranges_descending();
        for (index, range) in ranges {
            self.prepare_caret(index, &range);
            let start = range.start;
            let start_byte = self.buffer.char_to_byte(start);
            let end_byte = self.buffer.char_to_byte(range.end);
            self.edit_bytes(start_byte..end_byte, "\n");
            let after_newline = start + 1;
            let caret_byte = self.buffer.char_to_byte(after_newline);
            if let Some((insert, caret_chars)) =
                smart_edit::auto_indent_buffer(&self.buffer, caret_byte, &list_lines)
                && !insert.is_empty()
            {
                self.edit_bytes(caret_byte..caret_byte, &insert);
                let target = after_newline + caret_chars;
                self.virtual_text.carets_mut().set_caret_at(index, target);
            }
        }
        true
    }

    /// Tab / Shift+Tab: insert one indent level at every caret, indent/unindent
    /// every touched line, or indent an empty list item as a whole line.
    pub(super) fn virtual_tab(&mut self, input: &EditorInput<'_>, shift: bool) -> bool {
        let list_lines: Vec<usize> = input.list_lines.iter().map(|pos| pos.0).collect();
        let ranges = self.virtual_text.edit_ranges_descending();
        let caret_only = !self.virtual_text.any_selection();

        if caret_only && !shift {
            // Preserve the empty-list-item whole-line rewrite for a lone caret.
            if ranges.len() == 1 {
                let caret = self.virtual_text.caret();
                let caret_byte = self.buffer.char_to_byte(caret);
                if let Some(line_start) =
                    smart_edit::empty_list_item_line_buffer(&self.buffer, caret_byte, &list_lines)
                {
                    let at = self.buffer.char_to_byte(line_start);
                    self.edit_bytes(at..at, INDENT);
                    self.virtual_text.set_caret(caret + INDENT.chars().count());
                    self.virtual_text.set_h_pos(None);
                    return true;
                }
            }
            for (index, range) in ranges {
                self.prepare_caret(index, &range);
                let at = self.buffer.char_to_byte(range.start);
                self.edit_bytes(at..at, INDENT);
            }
            self.virtual_text.set_h_pos(None);
            return true;
        }

        // Selection / Shift+Tab path: one per-line pass over the union of the
        // touched byte ranges. A lone selection or caret keeps the exact
        // previous range, so single-caret behavior is unchanged.
        let (start_byte, end_byte) = if ranges.len() == 1 {
            let range = &ranges[0].1;
            (
                self.buffer.char_to_byte(range.start),
                self.buffer.char_to_byte(range.end),
            )
        } else {
            let mut start = usize::MAX;
            let mut end = 0usize;
            for (_, range) in &ranges {
                start = start.min(self.buffer.char_to_byte(range.start));
                end = end.max(self.buffer.char_to_byte(range.end));
            }
            (start, end)
        };
        let caret = self.virtual_text.caret();
        let caret_byte = self.buffer.char_to_byte(caret);

        let primary_at_start = caret_byte == start_byte;
        let start_char = self.buffer.byte_to_char(start_byte);
        let end_char = self.buffer.byte_to_char(end_byte);
        let line_starts =
            smart_edit::touched_line_starts_buffer(&self.buffer, start_char..end_char);
        let line_starts: Vec<usize> = line_starts
            .into_iter()
            .map(|start| self.buffer.char_to_byte(start))
            .collect();
        let new_range = if shift {
            let removals: Vec<(usize, usize)> = line_starts
                .iter()
                .filter_map(|&line_start| {
                    let mut n = 0;
                    while n < INDENT.len() && line_start + n < self.buffer.len_bytes() {
                        let ch = self
                            .buffer
                            .char_at(self.buffer.byte_to_char(line_start + n))?;
                        if ch != ' ' && ch != '\t' {
                            break;
                        }
                        n += 1;
                    }
                    (n > 0).then_some((line_start, n))
                })
                .collect();
            if removals.is_empty() {
                return false;
            }
            for &(line_start, n) in removals.iter().rev() {
                self.edit_bytes(line_start..line_start + n, "");
            }
            let map = |byte: usize| {
                let removed: usize = removals
                    .iter()
                    .map(|&(line_start, n)| n.min(byte.saturating_sub(line_start)))
                    .sum();
                byte.saturating_sub(removed)
            };
            map(start_byte)..map(end_byte)
        } else {
            for &line_start in line_starts.iter().rev() {
                self.edit_bytes(line_start..line_start, INDENT);
            }
            let shift = |byte: usize| {
                byte + INDENT.len() * line_starts.iter().filter(|&&start| start <= byte).count()
            };
            shift(start_byte)..shift(end_byte)
        };
        let (primary_byte, secondary_byte) = if primary_at_start {
            (new_range.start, new_range.end)
        } else {
            (new_range.end, new_range.start)
        };
        self.virtual_text.set_selection(
            self.buffer.byte_to_char(secondary_byte),
            self.buffer.byte_to_char(primary_byte),
        );
        self.virtual_text.set_h_pos(None);
        true
    }

    /// A single typed character from `Event::Text`, with the smart-editing
    /// rules applied. Multi-char strings insert verbatim.
    ///
    /// Every selection is replaced (descending). v1 rule: the smart-editing
    /// rules ([`Self::after_typed_insert`], auto-dedent / auto-pairing) run for
    /// the primary selection only; the other carets keep the plain replacement.
    pub(super) fn virtual_type(&mut self, text: &str, input: &EditorInput<'_>) -> bool {
        if text.is_empty() || text == "\n" || text == "\r" {
            return false;
        }
        let single = {
            let mut chars = text.chars();
            matches!((chars.next(), chars.next()), (Some(_), None))
        };
        // Captured before the edit for the primary-only smart-edit pass.
        let primary_selection = self.virtual_text.selection_range();
        let primary_selection_bytes = primary_selection
            .as_ref()
            .map(|range| self.selection_bytes(range));
        let primary_selection_text = primary_selection
            .as_ref()
            .map(|range| self.buffer.slice(range.clone()));

        let ranges = self.virtual_text.edit_ranges_descending();
        for (index, range) in ranges {
            self.prepare_caret(index, &range);
            let start_byte = self.buffer.char_to_byte(range.start);
            let end_byte = self.buffer.char_to_byte(range.end);
            self.edit_bytes(start_byte..end_byte, text);
        }
        if !single {
            return true;
        }
        let caret = self.virtual_text.caret();
        let anchor = caret.saturating_sub(1);
        let anchor_byte = self.buffer.char_to_byte(anchor);
        self.after_typed_insert(
            text.chars().next().expect("single char"),
            anchor,
            anchor_byte,
            primary_selection_bytes,
            primary_selection_text,
            input,
        );
        true
    }

    /// Splices the queued `pending_insert` at the primary caret/selection as one
    /// undo group. Returns whether an edit applied.
    pub(super) fn apply_pending_insert(&mut self) -> bool {
        let Some(text) = self.pending_insert.take() else {
            return false;
        };
        self.insert_at_primary(&text)
    }

    /// Re-anchors the virtual-text caches and carets after an out-of-band
    /// buffer change (undo/redo), whose applied ops already bumped the buffer
    /// revision — invalidating the derived text cache.
    pub(super) fn after_history_navigation(&mut self) {
        self.virtual_text.clamp(self.buffer.len_chars());
        self.virtual_text.set_h_pos(None);
        self.virtual_text.preedit_range = None;
    }

    /// Clears the tracked IME preedit range (deleting its text) and returns the
    /// char index where the composition started. With no active composition the
    /// primary selection is removed instead, mirroring egui's `clear_preedit`.
    ///
    /// IME degrades to the primary in this step: during composition the other
    /// carets are left untouched.
    pub(super) fn clear_preedit(&mut self) -> usize {
        if let Some(range) = self.virtual_text.preedit_range.take() {
            let start = self.buffer.char_to_byte(range.start);
            let end = self.buffer.char_to_byte(range.end);
            self.edit_bytes(start..end, "");
            self.virtual_text.set_h_pos(None);
            return range.start;
        }
        let start = self.selection_bounds().start;
        self.delete_selection();
        start
    }

    /// Replaces the selected text with `text` without running any smart-edit
    /// rule; shared by IME commit/preedit. Primary-only: IME composition targets
    /// the primary caret while multi-caret composition is out of scope.
    pub(super) fn replace_selection_raw(&mut self, start: usize, text: &str) -> usize {
        let start_byte = self.buffer.char_to_byte(start);
        self.edit_bytes(start_byte..start_byte, text);
        self.virtual_text.set_h_pos(None);
        start + text.chars().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_text::select::Selection;
    use crate::{COMPLETION_MAX_DEFAULT, EditorState, WrapMode};

    fn input<'a>() -> EditorInput<'a> {
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            highlights: &[],
            diagnostics: &[],
            blocked_ranges: &[],
            code_ranges: &[],
            math_ranges: &[],
            math_atoms: &[],
            spell_issues: &[],
            debug_highlights: &[],
            debug_highlight_color: egui::Color32::TRANSPARENT,
            folds: &[],
            list_lines: &[],
            completions: &[],
            definitions: &[],
            world_completions: &[],
            completion_max: COMPLETION_MAX_DEFAULT,
            parse_generation: 0,
            call_params: None,
            font_family: "sans",
            font_size: 14.0,
            wrap_mode: WrapMode::Off,
            line_spacing: 1.0,
            wrap_line_spacing: 1.0,
            plain_text: false,
            request_focus: false,
            blame_inline: None,
        }
    }

    fn collapsed(caret: usize) -> Selection {
        Selection {
            caret,
            anchor: None,
            h_pos: None,
        }
    }

    fn set_carets(state: &mut EditorState, list: Vec<Selection>) {
        let mut carets = state.virtual_text.carets_mut();
        carets.list_mut().clear();
        for selection in list {
            carets.list_mut().push(selection);
        }
        carets.set_primary(0);
    }

    fn text(state: &EditorState) -> String {
        state.buffer.slice(0..state.buffer.len_chars())
    }

    #[test]
    fn two_collapsed_carets_both_receive_typed_text() {
        let mut state = EditorState::new();
        state.set_buffer_text("ab\ncd");
        set_carets(&mut state, vec![collapsed(1), collapsed(4)]);

        assert!(state.virtual_type("x", &input()));
        assert_eq!(text(&state), "axb\ncxd");
    }

    #[test]
    fn multiple_selections_are_all_replaced() {
        let mut state = EditorState::new();
        state.set_buffer_text("abcdef");
        set_carets(
            &mut state,
            vec![
                Selection {
                    caret: 2,
                    anchor: Some(1),
                    h_pos: None,
                },
                Selection {
                    caret: 5,
                    anchor: Some(4),
                    h_pos: None,
                },
            ],
        );

        assert!(state.virtual_type("X", &input()));
        assert_eq!(text(&state), "aXcdXf");
    }

    #[test]
    fn backspace_applies_at_every_caret_descending() {
        let mut state = EditorState::new();
        state.set_buffer_text("ab\ncd");
        set_carets(&mut state, vec![collapsed(1), collapsed(4)]);

        assert!(state.virtual_backspace());
        assert_eq!(text(&state), "b\nd");
    }

    #[test]
    fn enter_applies_at_every_caret() {
        let mut state = EditorState::new();
        state.set_buffer_text("ab\ncd");
        set_carets(&mut state, vec![collapsed(1), collapsed(4)]);

        assert!(state.virtual_enter(&input()));
        assert_eq!(text(&state), "a\nb\nc\nd");
    }

    #[test]
    fn one_undo_reverts_a_two_caret_edit() {
        let mut state = EditorState::new();
        state.set_buffer_text("ab\ncd");
        set_carets(&mut state, vec![collapsed(1), collapsed(4)]);

        assert!(state.virtual_type("x", &input()));
        assert_eq!(text(&state), "axb\ncxd");
        assert!(state.buffer.undo(), "the multi-edit is one undo group");
        assert_eq!(text(&state), "ab\ncd");
    }

    #[test]
    fn primary_h_pos_survives_an_edit() {
        let mut state = EditorState::new();
        state.set_buffer_text("abc");
        state.virtual_text.set_h_pos(Some(3.5));
        state.edit_bytes(0..0, "z");
        assert_eq!(state.virtual_text.h_pos(), Some(3.5));
    }
}
