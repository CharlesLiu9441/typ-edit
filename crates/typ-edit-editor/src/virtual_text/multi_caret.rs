//! Multi-caret selection commands for the virtualized text path (P2-3b):
//! Ctrl+D (add the next occurrence), Ctrl+Shift+L (select all occurrences) and
//! Escape (collapse to the primary caret). The occurrence scan works over the
//! `text` mirror's char indices; every command is editor-local and operates on
//! the shared [`super::select::Carets`] list.

use std::ops::Range;

use egui::{Key, Modifiers};

use super::keys::{InputOutcome, consume};
use super::select::Selection;
use crate::EditorState;

impl EditorState {
    /// Esc collapses multiple carets to the primary; Ctrl+D adds the next
    /// occurrence of the primary pattern; Ctrl+Shift+L selects every
    /// occurrence. Consumed keys set `caret_moved` so the caret scrolls.
    pub(super) fn virtual_multi_caret_keys(&mut self, ui: &egui::Ui, outcome: &mut InputOutcome) {
        if consume(ui, Modifiers::COMMAND | Modifiers::SHIFT, Key::L) {
            if self.select_all_occurrences() {
                outcome.caret_moved = true;
            }
            return;
        }
        if consume(ui, Modifiers::COMMAND, Key::D) && self.select_next_occurrence() {
            outcome.caret_moved = true;
        }
    }

    /// Collapses every caret to the primary when there is more than one. Returns
    /// whether anything was dropped.
    pub(super) fn collapse_carets_on_escape(&mut self) -> bool {
        if self.virtual_text.carets().len() <= 1 {
            return false;
        }
        self.virtual_text.carets_mut().collapse_to_primary();
        true
    }

    /// The pattern Ctrl+D / Ctrl+Shift+L operate on: the primary selection's
    /// text, or the word under the primary caret. `None` when there is neither.
    fn multi_caret_pattern(&self) -> Option<String> {
        if let Some(range) = self.virtual_text.selection_range() {
            let text = self.buffer.slice(range);
            if !text.is_empty() {
                return Some(text);
            }
        }
        let word = self.word_selection_at_primary()?;
        let range = word.selection()?;
        let text = self.buffer.slice(range);
        (!text.is_empty()).then_some(text)
    }

    /// The word selection under the primary caret, if the boundary touches a
    /// word char. Does not mutate the caret list.
    fn word_selection_at_primary(&self) -> Option<Selection> {
        let caret = self.virtual_text.caret();
        let mut word = Selection::default();
        word.select_word_at(&self.buffer, caret);
        word.selection().map(|_| word)
    }

    /// All non-overlapping, left-to-right occurrences of `pattern` in the
    /// mirror, as char ranges. Patterns are matched greedily so `aa` in `aaa`
    /// yields one occurrence. The scan works over bounded rope slices with
    /// `str::find`; it never materializes the whole document.
    fn find_occurrences(&self, pattern: &str) -> Vec<Range<usize>> {
        if pattern.is_empty() {
            return Vec::new();
        }
        let pattern_chars = pattern.chars().count();
        let total = self.buffer.len_chars();
        let chunk = (16 * 1024).max(pattern_chars.saturating_mul(2));
        let overlap = pattern_chars.saturating_sub(1);
        let mut occurrences = Vec::new();
        let mut next = 0;
        while next < total {
            let end = (next + chunk).min(total);
            let window = self.buffer.slice(next..end);
            if let Some(offset) = window.find(pattern) {
                let start = next + window[..offset].chars().count();
                let end = start + pattern_chars;
                occurrences.push(start..end);
                // Skip overlaps, matching the old greedy scan.
                next = end;
            } else if end == total {
                break;
            } else {
                next = end.saturating_sub(overlap);
            }
        }
        occurrences
    }

    /// Adds the next occurrence of the primary pattern strictly after the last
    /// existing selection, wrapping around once. With no selection the word at
    /// the caret is selected first. Returns whether a caret was added.
    fn select_next_occurrence(&mut self) -> bool {
        let primary = self.virtual_text.carets().primary();
        let (pattern, pending_word) = if let Some(range) = primary.selection() {
            (self.buffer.slice(range), None)
        } else if let Some(word) = self.word_selection_at_primary() {
            let range = word.selection().expect("a word selection");
            (self.buffer.slice(range), Some(word))
        } else {
            return false;
        };
        if pattern.is_empty() {
            return false;
        }

        let occurrences = self.find_occurrences(&pattern);
        let mut reserved: Vec<Range<usize>> = self
            .virtual_text
            .carets()
            .list()
            .iter()
            .filter_map(|sel| sel.selection())
            .collect();
        if let Some(word) = &pending_word {
            reserved.extend(word.selection());
        }
        let last_end = reserved
            .iter()
            .map(|range| range.end)
            .max()
            .unwrap_or(primary.caret);
        let overlaps = |range: &Range<usize>| {
            reserved
                .iter()
                .any(|r| r.start < range.end && range.start < r.end)
        };
        let candidate = occurrences
            .iter()
            .find(|r| r.start >= last_end && !overlaps(r))
            .or_else(|| occurrences.iter().find(|r| !overlaps(r)));
        let Some(range) = candidate else {
            return false;
        };

        let mut carets = self.virtual_text.carets_mut();
        if let Some(word) = pending_word {
            let primary = carets.primary_mut();
            primary.anchor = word.anchor;
            primary.caret = word.caret;
        }
        carets.push_selection(Selection {
            caret: range.end,
            anchor: Some(range.start),
            h_pos: None,
        });
        carets.dedup_exact();
        true
    }

    /// Replaces the caret list with one selection per occurrence of the primary
    /// pattern, in document order. Returns whether the list changed.
    fn select_all_occurrences(&mut self) -> bool {
        let Some(pattern) = self.multi_caret_pattern() else {
            return false;
        };
        let occurrences = self.find_occurrences(&pattern);
        if occurrences.is_empty() {
            return false;
        }
        let selections: Vec<Selection> = occurrences
            .iter()
            .map(|range| Selection {
                caret: range.end,
                anchor: Some(range.start),
                h_pos: None,
            })
            .collect();
        let primary = self.virtual_text.carets().primary();
        let primary_index = selections
            .iter()
            .position(|sel| sel.caret == primary.caret && sel.anchor == primary.anchor)
            .or_else(|| selections.iter().position(|sel| sel.caret == primary.caret))
            .unwrap_or(0);

        let mut carets = self.virtual_text.carets_mut();
        carets.list_mut().clear();
        for selection in selections {
            carets.list_mut().push(selection);
        }
        carets.set_primary(primary_index);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occurrence_scan_is_greedy_and_multibyte_safe() {
        let mut editor = EditorState::new();
        editor.set_text("aa aa aaa 中文 中文 中".to_owned());
        assert_eq!(editor.find_occurrences("aa"), vec![0..2, 3..5, 6..8]);
        assert_eq!(
            editor.find_occurrences("中文"),
            vec![10..12, 13..15],
            "byte search still reports char ranges"
        );
        assert!(editor.find_occurrences("zz").is_empty());
    }
}
