//! Completion input and buffer accept for the virtualized text path (Stage D2).
//!
//! The completion *logic* is widget-agnostic and lives in [`crate::completion`]
//! (`refresh_completion`, `popup_owns_enter`, the trigger predicates). This
//! module only wires it to the virtual caret and the authoritative buffer:
//! [`EditorState::virtual_completion_keys`] owns the keys that must be consumed
//! before any other handler, [`EditorState::virtual_completion_refresh`] runs
//! after the frame's edits, and [`EditorState::virtual_apply_completion`] splices
//! the chosen candidate through [`EditorState::edit_bytes`].
//!
//! Layout note (P6): this module is input/logic only and contains no UI
//! layout; the popup itself is painted by [`crate::completion`] as an `Area`
//! plus one ScrollArea whose rows are single painted leaves (whitelisted).

use egui::{Key, Modifiers};
use typ_edit_model::CompletionItem;

use super::keys::{InputOutcome, consume};
use crate::{EditorInput, EditorState, completion};

impl EditorState {
    /// Completion keys that must be consumed before every other handler: the
    /// manual `Ctrl/Cmd+Space`, and — while the popup is open — Escape, Up/Down,
    /// Enter/Tab. Enter is consumed only when [`completion::popup_owns_enter`]
    /// allows it (an unfiltered parameter list still opens a new argument line);
    /// Tab is always consumed while the popup is open so it cannot indent.
    /// Returns whether a key was handled, which suppresses the refresh pass.
    pub(super) fn virtual_completion_keys(
        &mut self,
        ui: &egui::Ui,
        input: &EditorInput<'_>,
        outcome: &mut InputOutcome,
    ) -> bool {
        if input.plain_text {
            return false;
        }
        let manual = consume(ui, Modifiers::COMMAND, Key::Space);
        let mut handled = false;
        if self.completion.is_some() {
            if consume(ui, Modifiers::NONE, Key::Escape) {
                let caret = self.virtual_text.caret();
                self.dismiss_completion(caret);
                handled = true;
            }
            if !handled {
                let up = consume(ui, Modifiers::NONE, Key::ArrowUp);
                let down = !up && consume(ui, Modifiers::NONE, Key::ArrowDown);
                if (up || down)
                    && let Some(popup) = self.completion.as_mut()
                    && !popup.items.is_empty()
                {
                    let len = popup.items.len();
                    if up {
                        popup.selected = (popup.selected + len - 1) % len;
                    }
                    if down {
                        popup.selected = (popup.selected + 1) % len;
                    }
                    popup.navigated = true;
                    handled = true;
                    ui.ctx().request_repaint();
                }
            }
            if !handled {
                let caret = self.virtual_text.caret();
                let owns_enter = self.completion.as_ref().is_none_or(|popup| {
                    completion::popup_owns_enter(
                        popup.params,
                        popup.navigated,
                        popup.word_start != caret,
                    )
                });
                let enter = owns_enter && consume(ui, Modifiers::NONE, Key::Enter);
                let tab = consume(ui, Modifiers::NONE, Key::Tab);
                if (enter || tab)
                    && let Some((word_start, item)) = self.completion.take().and_then(|popup| {
                        popup
                            .items
                            .get(popup.selected)
                            .cloned()
                            .map(|item| (popup.word_start, item))
                    })
                    && self.virtual_apply_completion(word_start, caret, &item)
                {
                    outcome.changed = true;
                    outcome.caret_moved = true;
                    self.after_completion_accept(&item);
                    handled = true;
                }
            }
        }
        if !handled && manual {
            let caret = self.virtual_text.caret();
            self.dismissed_at = None;
            self.refresh_completion(caret, input, completion::CompletionGate::Manual);
            let code_ranges: Vec<std::ops::Range<usize>> = input
                .code_ranges
                .iter()
                .map(|range| range.start.0..range.end.0)
                .collect();
            let math_ranges: Vec<std::ops::Range<usize>> = input
                .math_ranges
                .iter()
                .map(|range| range.start.0..range.end.0)
                .collect();
            // An explicit request also asks the world, but only where members
            // exist: Ctrl+Space in markup must not fire a useless query.
            if completion::member_completion_context(
                &self.buffer,
                caret,
                &code_ranges,
                &math_ranges,
            ) != completion::CompletionContext::Markup
            {
                outcome.completion_request = Some(self.buffer.char_to_byte(caret));
            }
            handled = true;
        }
        handled
    }

    /// Refreshes the popup after the frame's edits: a typed trigger opens or
    /// refreshes (a comma in a call slot, a `.` extending a dotted head, `#`,
    /// letters), and a fresh parse generation recomputes without a keystroke.
    pub(super) fn virtual_completion_refresh(
        &mut self,
        input: &EditorInput<'_>,
        outcome: &InputOutcome,
    ) {
        if input.plain_text {
            return;
        }
        let caret = self.virtual_text.caret();
        if outcome.changed {
            let typed_char = outcome.typed;
            let may_refresh = self.completion.is_some()
                || completion::completion_trigger(typed_char)
                || typed_char == Some(',')
                || typed_char.is_some_and(|c| c.is_ascii_digit());
            if may_refresh {
                self.dismissed_at = None;
                self.refresh_completion(
                    caret,
                    input,
                    completion::CompletionGate::Keystroke(typed_char),
                );
            }
        }
        // An asynchronously fetched member list folds into the popup without a
        // keystroke; a dismissed popup stays dismissed. The list is skipped on a
        // keystroke frame because it was fetched for the pre-keystroke text (the
        // app clears it a frame later, so no stale members survive).
        let world_signature = completion::world_completions_signature(input.world_completions);
        if world_signature != self.last_world_completions {
            self.last_world_completions = world_signature;
            if !input.world_completions.is_empty()
                && !outcome.changed
                && (self.completion.is_some()
                    || self.dismissed_at != Some((self.buffer.len_bytes(), caret)))
            {
                self.refresh_completion(caret, input, completion::CompletionGate::Manual);
            }
        }
        if input.parse_generation != self.last_parse_generation
            && self.dismissed_at != Some((self.buffer.len_bytes(), caret))
        {
            self.refresh_completion(caret, input, completion::CompletionGate::Parse);
        }
    }

    /// Replaces the typed prefix `word_start..caret_char` with `item.insert`,
    /// landing the caret `item.cursor_back` chars before the inserted end
    /// (saturating). Returns `false` when the range is not char-boundary safe.
    /// The edit shares the virtual path's undo group via `edit_bytes`.
    pub(super) fn virtual_apply_completion(
        &mut self,
        word_start: usize,
        caret_char: usize,
        item: &CompletionItem,
    ) -> bool {
        let start = word_start.min(caret_char);
        let start_byte = self.buffer.char_to_byte(start);
        let end_byte = self.buffer.char_to_byte(caret_char);
        self.edit_bytes(start_byte..end_byte, &item.insert);
        let caret = (start + item.insert.chars().count()).saturating_sub(item.cursor_back.0);
        self.virtual_text
            .set_caret(caret.min(self.buffer.len_chars()));
        self.virtual_text.set_h_pos(None);
        true
    }

    /// Records the popup state after an accepted candidate, shared by the
    /// keyboard and click accept paths. Non-callable items keep the dismiss
    /// guard (the popup stays closed); callable items clear it so the next parse
    /// generation auto-opens the parameter list at the new caret.
    pub(super) fn after_completion_accept(&mut self, item: &CompletionItem) {
        self.completion = None;
        if completion::is_callable_accept(item) {
            self.dismissed_at = None;
        } else {
            let caret = self.virtual_text.caret();
            self.dismissed_at = Some((self.buffer.len_bytes(), caret));
        }
    }
}
