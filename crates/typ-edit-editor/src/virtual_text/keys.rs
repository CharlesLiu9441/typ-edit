//! Keyboard input, clipboard and queue routing for the virtualized text path
//! (Stage C).
//!
//! [`EditorState::virtual_input`] is the frame's input pre-roll, run before the
//! geometry-dependent rendering pass: it applies the queued
//! `pending_insert`/`pending_jump`/`command` actions, routes undo/redo through
//! the shared [`typ_edit_buffer::TextBuffer`], handles clipboard and text/IME
//! events, and delegates navigation to [`super::nav`].

use std::ops::Range;

use egui::{Event, Key, Modifiers};

use crate::{EditorCommand, EditorInput, EditorState, HistoryRequest};

/// Result of the pre-geometry input pass, folded into [`crate::EditorOutput`]
/// and the caret scroll decision.
#[derive(Default)]
pub(super) struct InputOutcome {
    pub(super) changed: bool,
    pub(super) caret_moved: bool,
    /// The single char typed this frame that advanced the caret by exactly one,
    /// used to gate completion refreshes.
    pub(super) typed: Option<char>,
    /// A bare F2 requested a rename at the caret.
    pub(super) rename: bool,
    /// Caret byte at which world-backed completions should be fetched this frame
    /// (a `.` member trigger, or Ctrl/Cmd+Space).
    pub(super) completion_request: Option<usize>,
    /// Collab mode: an undo/redo the host must route to the server-global stacks.
    pub(super) history_request: Option<HistoryRequest>,
}

/// Consumes a press of `key` whose own modifiers are all clear.
///
/// F2 is editor-local and must not swallow a modified press: the tree pane owns
/// plain F2 (file rename) while other modifiers may belong to shell shortcuts.
pub(super) fn consume_bare_key(ui: &egui::Ui, key: Key) -> bool {
    let mut found = false;
    ui.input_mut(|input| {
        input.events.retain(|event| match event {
            Event::Key {
                key: event_key,
                pressed: true,
                modifiers,
                ..
            } if *event_key == key && no_modifiers(modifiers) => {
                found = true;
                false
            }
            _ => true,
        });
    });
    found
}

/// Whether no modifier (Ctrl/Alt/Shift/Command) is held.
fn no_modifiers(modifiers: &Modifiers) -> bool {
    !(modifiers.alt || modifiers.ctrl || modifiers.shift || modifiers.mac_cmd || modifiers.command)
}

/// Consumes `key` with `modifiers` from the frame's input, returning whether it
/// was present. `Modifiers::matches_logically` ignores extra Shift/Alt, so
/// callers match the most specific combination first.
pub(super) fn consume(ui: &egui::Ui, modifiers: Modifiers, key: Key) -> bool {
    ui.input_mut(|input| input.consume_key(modifiers, key))
}

/// Ctrl/Cmd+V with a bitmap in the clipboard.
///
/// A screenshot carries no text, so egui produces no `Event::Paste` for it; the
/// release of V with the command modifier held is the only signal that
/// survives. `read_for_paste` yields a bitmap only without meaningful text, so
/// rich clipboard content still pastes as text. A stray whitespace paste is
/// dropped when the clipboard actually holds a bitmap.
pub(super) fn detect_paste_image(
    ui: &egui::Ui,
    focused: bool,
) -> Option<crate::clipboard_image::ClipboardImage> {
    if !focused {
        return None;
    }
    let whitespace_paste = ui.input(|i| {
        i.events
            .iter()
            .any(|event| matches!(event, Event::Paste(text) if text.trim().is_empty()))
    });
    if whitespace_paste && crate::clipboard_image::read_for_paste().is_some() {
        ui.input_mut(|i| i.events.retain(|event| !matches!(event, Event::Paste(_))));
    }
    let released = ui.input(|i| {
        i.events.iter().any(|event| {
            matches!(
                event,
                Event::Key {
                    key: Key::V,
                    pressed: false,
                    modifiers,
                    ..
                } if modifiers.command
            )
        })
    });
    if released {
        crate::clipboard_image::read_for_paste()
    } else {
        None
    }
}

impl EditorState {
    /// Applies the queues plus this frame's editing/navigation input. Runs
    /// before the render pass so all buffer mutations happen while the buffer is
    /// not borrowed by the layout.
    pub(super) fn virtual_input(
        &mut self,
        ui: &egui::Ui,
        id: egui::Id,
        input: &EditorInput<'_>,
    ) -> InputOutcome {
        let mut outcome = InputOutcome::default();
        let focused = ui.memory(|memory| memory.has_focus(id));
        if !focused {
            self.virtual_text.preedit_range = None;
            self.completion = None;
        }
        if input.plain_text {
            self.completion = None;
        }

        // Escape collapses extra carets before completion or focus navigation
        // can claim the key.
        if focused
            && self.virtual_text.carets().len() > 1
            && consume(ui, Modifiers::NONE, Key::Escape)
        {
            self.collapse_carets_on_escape();
            outcome.caret_moved = true;
        }

        // A bare F2 requests a rename from the editor; modified presses are left
        // for the shell (the tree pane owns plain F2 for file rename).
        if focused && consume_bare_key(ui, Key::F2) {
            outcome.rename = true;
        }

        let queued_command = self.command.take();
        let completion_handled = focused && self.virtual_completion_keys(ui, input, &mut outcome);
        if focused && self.virtual_history(ui, input.collab_history, &mut outcome) {
            outcome.changed = true;
        }

        // A queued jump repositions first, so a queued insert lands at the
        // jumped caret rather than the caret from before the jump.
        if let Some(byte) = self.pending_jump.take() {
            let caret = self.buffer.byte_to_char(byte);
            self.virtual_text.set_caret(caret);
            self.virtual_text.clamp(self.buffer.len_chars());
            self.virtual_text.set_h_pos(None);
            let line = self.buffer.line_of_char(self.virtual_text.caret());
            self.virtual_text.arm_scroll_settle(line);
            outcome.caret_moved = true;
        }
        // A queued range is applied after the jump, so it overrides the plain
        // caret move above (byte offsets, clamped, collapsed to a caret).
        if let Some(range) = self.pending_select.take() {
            let len = self.buffer.len_chars();
            let start = self.buffer.byte_to_char(range.start).min(len);
            let end = self.buffer.byte_to_char(range.end).min(len);
            if end > start {
                self.virtual_text.set_selection(start, end);
            } else {
                self.virtual_text.set_caret(start);
            }
            self.virtual_text.set_h_pos(None);
            let line = self.buffer.line_of_char(self.virtual_text.caret());
            self.virtual_text.arm_scroll_settle(line);
            outcome.caret_moved = true;
        }
        if self.pending_insert.is_some() && self.apply_pending_insert() {
            outcome.changed = true;
            outcome.caret_moved = true;
        }

        if focused {
            self.virtual_clipboard_and_text(ui, input, &mut outcome);
            self.virtual_edit_keys(ui, input, &mut outcome);
            self.virtual_horizontal_nav(ui, &mut outcome);
            self.virtual_multi_caret_keys(ui, &mut outcome);
        }
        // A typed `.` after an identifier-ish char opens a field access; the
        // app fetches that member's candidates from the compiled world. Only a
        // code or math member position may do so: a half-width `.` in markup
        // must neither request nor open completion.
        if let Some(typed) = outcome.typed {
            let caret = self.virtual_text.caret();
            let prev = crate::completion::char_before_caret(&self.buffer, caret.saturating_sub(1));
            if crate::completion::is_member_completion_trigger(prev, typed) {
                let code_ranges: Vec<Range<usize>> = input
                    .code_ranges
                    .iter()
                    .map(|range| range.start.0..range.end.0)
                    .collect();
                let math_ranges: Vec<Range<usize>> = input
                    .math_ranges
                    .iter()
                    .map(|range| range.start.0..range.end.0)
                    .collect();
                if crate::completion::member_completion_context(
                    &self.buffer,
                    caret,
                    &code_ranges,
                    &math_ranges,
                ) != crate::completion::CompletionContext::Markup
                {
                    outcome.completion_request = Some(self.buffer.char_to_byte(caret));
                }
            }
        }
        self.virtual_find_input(ui, focused, &mut outcome);

        if let Some(command) = queued_command {
            match command {
                EditorCommand::Undo if input.collab_history => {
                    outcome.history_request = Some(HistoryRequest::Undo);
                }
                EditorCommand::Redo if input.collab_history => {
                    outcome.history_request = Some(HistoryRequest::Redo);
                }
                command => {
                    let rev_before = self.buffer.rev();
                    self.virtual_command(ui, id, command);
                    outcome.changed |= self.buffer.rev() != rev_before;
                    outcome.caret_moved = true;
                }
            }
        }
        if focused && !completion_handled {
            self.virtual_completion_refresh(input, &outcome);
        }
        if outcome.caret_moved {
            self.virtual_text.blink_origin = ui.input(|frame| frame.time);
        }
        outcome
    }

    /// Re-arms the focus lock filter at the end of the frame, once pointer
    /// interaction has possibly changed the caret count. Tab and arrow keys stay
    /// in the editor; Escape is swallowed while completion is open or extra
    /// carets exist, so the collapse/dismiss handlers receive it.
    pub(super) fn virtual_focus_filter(&self, ui: &egui::Ui, id: egui::Id) {
        if !ui.memory(|memory| memory.has_focus(id)) {
            return;
        }
        let escape = self.completion.is_some() || self.virtual_text.carets().len() > 1;
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape,
                },
            );
        });
    }

    /// Undo (`Ctrl/Cmd+Z`), redo (`Ctrl/Cmd+Y` or `Shift+Ctrl/Cmd+Z`). History
    /// navigation never opens a new edit group.
    ///
    /// In collab mode the keys consume the same combos but emit a
    /// [`HistoryRequest`] (the host routes it to the server-global stacks) and
    /// leave the local buffer/history untouched, returning `false`.
    fn virtual_history(
        &mut self,
        ui: &egui::Ui,
        collab_history: bool,
        outcome: &mut InputOutcome,
    ) -> bool {
        let redo = consume(ui, Modifiers::SHIFT | Modifiers::COMMAND, Key::Z);
        let undo = !redo && consume(ui, Modifiers::COMMAND, Key::Z);
        let redo_y = !redo && !undo && consume(ui, Modifiers::COMMAND, Key::Y);
        if !(undo || redo || redo_y) {
            return false;
        }
        if collab_history {
            outcome.history_request = Some(if undo {
                HistoryRequest::Undo
            } else {
                HistoryRequest::Redo
            });
            return false;
        }
        self.apply_history(!undo)
    }

    /// Shared undo/redo core for the keyboard path ([`Self::virtual_history`])
    /// and the queued menu command ([`Self::virtual_command`]). History
    /// navigation never opens a new edit group; on success the virtual-text
    /// caches and carets are re-anchored.
    fn apply_history(&mut self, redo: bool) -> bool {
        self.group_open.set(false);
        let applied = if redo {
            self.buffer.redo()
        } else {
            self.buffer.undo()
        };
        if applied {
            self.after_history_navigation();
        }
        applied
    }

    /// Clipboard events (`Copy`/`Cut`) plus text and IME edits.
    fn virtual_clipboard_and_text(
        &mut self,
        ui: &egui::Ui,
        input: &EditorInput<'_>,
        outcome: &mut InputOutcome,
    ) {
        let events = ui.input(|frame| frame.events.clone());
        if events.is_empty() {
            return;
        }
        ui.input_mut(|frame| {
            frame.events.retain(|event| {
                !matches!(
                    event,
                    Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut | Event::Ime(_)
                )
            });
        });
        for event in &events {
            match event {
                Event::Copy => {
                    self.virtual_copy(ui);
                }
                Event::Cut => {
                    if self.virtual_cut(ui) {
                        outcome.changed = true;
                        outcome.caret_moved = true;
                    }
                }
                Event::Paste(text) => {
                    if self.virtual_insert(text) {
                        outcome.changed = true;
                        outcome.caret_moved = true;
                    }
                }
                Event::Text(text) => {
                    let pre_anchor = self
                        .virtual_text
                        .selection_range()
                        .map_or(self.virtual_text.caret(), |range| range.start);
                    if self.virtual_type(text, input) {
                        outcome.changed = true;
                        outcome.caret_moved = true;
                        let mut chars = text.chars();
                        let single = matches!((chars.next(), chars.next()), (Some(_), None));
                        if single
                            && self.virtual_text.caret() == pre_anchor + 1
                            && let Some(ch) = text.chars().next()
                        {
                            outcome.typed = Some(ch);
                        }
                    }
                }
                Event::Ime(ime) => self.virtual_ime(ime.clone(), outcome),
                _ => {}
            }
        }
    }

    /// Copies every non-empty selection to the system clipboard, ascending by
    /// start and joined with `\n`; returns whether there was one. A lone
    /// selection copies exactly its text.
    fn virtual_copy(&self, ui: &egui::Ui) -> bool {
        let mut ranges: Vec<Range<usize>> = self
            .virtual_text
            .carets()
            .list()
            .iter()
            .filter_map(|sel| sel.selection())
            .collect();
        if ranges.is_empty() {
            return false;
        }
        ranges.sort_by_key(|range| range.start);
        let parts: Vec<String> = ranges
            .into_iter()
            .map(|range| self.buffer.slice(range))
            .collect();
        ui.ctx().copy_text(parts.join("\n"));
        true
    }

    /// Copies every selection, then removes them as one undo group. Returns
    /// whether an edit applied; with nothing to copy the clipboard is untouched.
    fn virtual_cut(&mut self, ui: &egui::Ui) -> bool {
        if !self.virtual_copy(ui) {
            return false;
        }
        self.delete_selections()
    }

    /// Backspace/Delete/Enter/Tab/Shift+Tab and Ctrl/Cmd+A.
    fn virtual_edit_keys(
        &mut self,
        ui: &egui::Ui,
        input: &EditorInput<'_>,
        outcome: &mut InputOutcome,
    ) {
        if consume(ui, Modifiers::NONE, Key::Backspace) && self.virtual_backspace() {
            outcome.changed = true;
            outcome.caret_moved = true;
        }
        if consume(ui, Modifiers::NONE, Key::Delete) && self.virtual_delete() {
            outcome.changed = true;
            outcome.caret_moved = true;
        }
        if consume(ui, Modifiers::SHIFT, Key::Tab) {
            if self.virtual_tab(input, true) {
                outcome.changed = true;
                outcome.caret_moved = true;
            }
        } else if consume(ui, Modifiers::NONE, Key::Tab) && self.virtual_tab(input, false) {
            outcome.changed = true;
            outcome.caret_moved = true;
        }
        if consume(ui, Modifiers::NONE, Key::Enter) && self.virtual_enter(input) {
            outcome.changed = true;
            outcome.caret_moved = true;
        }
        if consume(ui, Modifiers::COMMAND, Key::A) {
            let end = self.buffer.len_chars();
            self.virtual_text.select_all(end);
            outcome.caret_moved = true;
        }
    }

    /// Applies a queued menu command (the context menu and menu bar share it).
    pub(super) fn virtual_command(&mut self, ui: &egui::Ui, id: egui::Id, command: EditorCommand) {
        match command {
            EditorCommand::Copy => {
                self.virtual_copy(ui);
            }
            EditorCommand::Cut => {
                if self.virtual_cut(ui) {
                    ui.ctx().request_repaint();
                }
            }
            EditorCommand::Paste => {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                ui.memory_mut(|memory| memory.request_focus(id));
                ui.ctx().request_repaint();
            }
            EditorCommand::SelectAll => {
                let end = self.buffer.len_chars();
                self.virtual_text.select_all(end);
                ui.memory_mut(|memory| memory.request_focus(id));
            }
            EditorCommand::Undo => {
                if self.apply_history(false) {
                    ui.ctx().request_repaint();
                }
            }
            EditorCommand::Redo => {
                if self.apply_history(true) {
                    ui.ctx().request_repaint();
                }
            }
        }
    }
}
