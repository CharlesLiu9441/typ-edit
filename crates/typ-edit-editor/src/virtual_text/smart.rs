//! Smart-editing rules for the virtualized text path (Stage C).
//!
//! Applies the pure [`crate::smart_edit`] transforms after a character was
//! inserted: closing-delimiter auto-dedent and markup
//! auto-pairing (wrap a selection, skip an identical symmetric delimiter,
//! insert the closer). The virtual path knows what was typed, so the
//! orchestration is direct instead of inferred from a caret delta.

use std::ops::Range;

use crate::{EditorInput, EditorState, smart_edit};

impl EditorState {
    /// Closing-delimiter auto-dedent then markup auto-pairing.
    pub(super) fn after_typed_insert(
        &mut self,
        typed_char: char,
        anchor: usize,
        typed_byte: usize,
        selection_bytes: Option<Range<usize>>,
        selection_text: Option<String>,
        input: &EditorInput<'_>,
    ) {
        if smart_edit::matching_opener(typed_char).is_some() {
            let caret_char = self.virtual_text.caret();
            let caret_byte = self.buffer.char_to_byte(caret_char);
            let line = self.buffer.line_of_char(anchor);
            let line_start_char = self.buffer.line_start(line);
            let line_start = self.buffer.char_to_byte(line_start_char);
            let above_is_list =
                smart_edit::previous_nonempty_line_start_buffer(&self.buffer, line_start_char)
                    .is_some_and(|start| input.list_lines.iter().any(|pos| pos.0 == start));
            if !above_is_list {
                // Closers still need the prefix to find the unmatched opener;
                // build it only on this path, never for ordinary characters.
                let prefix = self.buffer.slice(0..caret_byte);
                if let Some(n) = smart_edit::auto_dedent(&prefix, caret_byte) {
                    let start = line_start.min(caret_byte);
                    let end = start.saturating_add(n).min(caret_byte);
                    if end > start {
                        self.edit_bytes(start..end, "");
                        self.virtual_text
                            .set_caret(caret_char.saturating_sub(end - start));
                        self.virtual_text.set_h_pos(None);
                        return;
                    }
                }
            }
        }

        // A character typed right after a backslash is escaped: no pairing.
        if typed_byte > 0
            && anchor
                .checked_sub(1)
                .and_then(|before| self.buffer.char_at(before))
                == Some('\\')
        {
            return;
        }

        let Some(closer) = smart_edit::pair_closer(typed_char) else {
            return;
        };
        let is_bracket = matches!(typed_char, '(' | '[' | '{');
        // A double quote pairs inside code and math ranges even though both are
        // blocked to the markup helpers (`"..."` is a valid Typst string in
        // either); raw ranges stay suppressed because their content is literal
        // — pairing there would insert characters the user must delete.
        let in_code = |byte: usize| {
            input
                .code_ranges
                .iter()
                .any(|range| range.start.0 <= byte && byte <= range.end.0)
        };
        let in_math = |byte: usize| {
            input
                .math_ranges
                .iter()
                .any(|range| range.start.0 <= byte && byte <= range.end.0)
        };
        let code_quote = typed_char == '"'
            && (in_code(typed_byte) || in_math(typed_byte))
            && selection_bytes.as_ref().is_none_or(|sel| {
                (in_code(sel.start) || in_math(sel.start)) && (in_code(sel.end) || in_math(sel.end))
            });
        let blocked = |byte: usize| {
            input
                .blocked_ranges
                .iter()
                .any(|range| range.start.0 <= byte && byte < range.end.0)
        };
        let context_ok = is_bracket
            || code_quote
            || (!blocked(typed_byte)
                && selection_bytes
                    .as_ref()
                    .is_none_or(|sel| !blocked(sel.start) && !blocked(sel.end)));
        if !context_ok {
            return;
        }

        let after_index = self.buffer.char_to_byte(anchor + 1);
        let next_char = self.buffer.char_at(anchor + 1);
        // An explicit selection is an intentional wrap, so the unmatched-quote
        // rule below (which only guards the closer-insert branch) never applies
        // here.
        if let Some(selected) = selection_text {
            let end = typed_byte
                .saturating_add(typed_char.len_utf8())
                .min(self.buffer.len_bytes());
            let mut restored = String::with_capacity(selected.len() + 2);
            restored.push(typed_char);
            restored.push_str(&selected);
            restored.push(closer);
            self.edit_bytes(typed_byte..end, &restored);
            let selected_chars = selected.chars().count();
            self.virtual_text
                .set_selection(anchor + 1, anchor + 1 + selected_chars);
            self.virtual_text.set_h_pos(None);
        } else if next_char == Some(typed_char) {
            let end = typed_byte.saturating_add(typed_char.len_utf8());
            self.edit_bytes(typed_byte..end, "");
            self.virtual_text.set_caret(anchor + 1);
            self.virtual_text.set_h_pos(None);
        } else if !next_char.is_some_and(|c| c.is_alphanumeric())
            && !(typed_char == '"' && {
                let prefix = self.buffer.slice(0..typed_byte);
                smart_edit::unmatched_double_quote(&prefix, typed_byte)
            })
        {
            let mut buffer = [0u8; 4];
            self.edit_bytes(after_index..after_index, closer.encode_utf8(&mut buffer));
            // Land between the freshly paired delimiters, not after the closer.
            self.virtual_text.set_caret(anchor + 1);
            self.virtual_text.set_h_pos(None);
        }
    }
}
