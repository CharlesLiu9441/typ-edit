//! Find/replace integration for the virtualized text path (Stage D).
//!
//! Wires the shared [`crate::find_bar`] state into the custom widget: F3
//! stepping, applying a queued match selection to the virtual
//! [`super::select::Selection`], and painting the match highlights. The buffer
//! mutation core itself is widget-agnostic
//! ([`crate::EditorState::run_find_actions`]).

use std::ops::Range;

use egui::{Key, Modifiers, Rect};
use typ_edit_buffer::TextBuffer;

use super::hit::{LineSlot, line_char_len};
use super::keys::{InputOutcome, consume};
use crate::find_bar::step_match;
use crate::{EditorState, FindState};

impl EditorState {
    /// Consumes F3/Shift+F3 while focused and the bar is open, then applies any
    /// match selection queued by the bar (next/previous buttons, a query change,
    /// a replace-current). Runs in the input pre-roll so the selection and the
    /// scroll-into-view decision are ready before the render pass.
    pub(super) fn virtual_find_input(
        &mut self,
        ui: &egui::Ui,
        focused: bool,
        outcome: &mut InputOutcome,
    ) {
        if focused && self.find.open {
            let shift_f3 = consume(ui, Modifiers::SHIFT, Key::F3);
            let f3 = !shift_f3 && consume(ui, Modifiers::NONE, Key::F3);
            if f3 || shift_f3 {
                let delta = if shift_f3 { -1 } else { 1 };
                self.find.active = step_match(self.find.active, self.find.matches.len(), delta);
                self.find.pending_select = self.find.matches.get(self.find.active).cloned();
                ui.ctx().request_repaint();
            }
        }
        if let Some(range) = self.find.pending_select.take() {
            let start = self.buffer.byte_to_char(range.start);
            let end = self.buffer.byte_to_char(range.end);
            if end > start {
                self.virtual_text.set_selection(start, end);
            } else {
                self.virtual_text.set_caret(start);
            }
            self.virtual_text.set_h_pos(None);
            outcome.caret_moved = true;
        }
    }
}

/// Line-local char range of the byte `range` clipped to logical `line`: the
/// per-line unit the highlight and squiggle painters consume. `None` when the
/// range does not intersect the line.
pub(super) fn line_local_match(
    buffer: &TextBuffer,
    line: usize,
    range: &Range<usize>,
) -> Option<Range<usize>> {
    let line_start_char = buffer.line_start(line);
    let line_end_char = line_start_char + line_char_len(buffer, line);
    let line_start_byte = buffer.char_to_byte(line_start_char);
    let line_end_byte = buffer.char_to_byte(line_end_char);
    let lo = range.start.max(line_start_byte);
    let hi = range.end.min(line_end_byte);
    if lo >= hi {
        return None;
    }
    let chars = buffer.byte_to_char(lo)..buffer.byte_to_char(hi);
    let local =
        chars.start.saturating_sub(line_start_char)..chars.end.saturating_sub(line_start_char);
    (local.start < local.end).then_some(local)
}

/// Paints translucent fills for the find matches intersecting the visible
/// slots; the active match gets the full selection color, the rest a dimmed one.
pub(super) fn paint_matches(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    find: &FindState,
) {
    if find.matches.is_empty() {
        return;
    }
    let (Some(first), Some(last)) = (slots.first(), slots.last()) else {
        return;
    };
    let byte_start = buffer.char_to_byte(buffer.line_start(first.line));
    let byte_end = buffer.char_to_byte(buffer.line_start(last.line + 1));
    let base = ui.visuals().selection.bg_fill;
    let painter = ui.painter().with_clip_rect(clip);
    let start = find
        .matches
        .partition_point(|range| range.end <= byte_start);
    for (index, range) in find.matches.iter().enumerate().skip(start) {
        if range.start >= byte_end {
            break;
        }
        let color = if index == find.active {
            base
        } else {
            base.gamma_multiply(0.5)
        };
        for slot in slots {
            let Some(local) = line_local_match(buffer, slot.line, range) else {
                continue;
            };
            let Some(local) = slot.window_local(local) else {
                continue;
            };
            for rect in
                crate::diagnostics::squiggle_rects(&slot.galley, slot.screen_origin(), local)
            {
                if rect.intersects(clip) {
                    painter.rect_filled(rect, 2.0, color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_local_match_clips_a_match_to_each_line() {
        let text = "foo bar\nfoo baz";
        let buffer = TextBuffer::from_text(text);
        let first = 0..3;
        let second = 8..11;

        assert_eq!(line_local_match(&buffer, 0, &first), Some(0..3));
        assert_eq!(line_local_match(&buffer, 1, &second), Some(0..3));
        assert_eq!(
            line_local_match(&buffer, 0, &second),
            None,
            "a later line's match does not intersect line 0"
        );
    }

    #[test]
    fn line_local_match_handles_cjk_bytes() {
        let text = "你好\n世界";
        let buffer = TextBuffer::from_text(text);
        // '世' = bytes 7..10, the first char of line 1.
        assert_eq!(line_local_match(&buffer, 1, &(7..10)), Some(0..1));
    }
}
