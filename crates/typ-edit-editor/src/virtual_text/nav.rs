//! Caret navigation for the virtualized text path (Stage C).
//!
//! Horizontal navigation (character, word, Home/End) needs only the text; it
//! runs during the input pre-roll in [`super::keys`]. Vertical navigation
//! (Up/Down across wrapped rows with x preservation, PageUp/PageDown) needs the
//! shaped galleys, so [`handle_vertical_keys`] runs from the render pass where
//! the [`super::layout::LineCache`] and viewport height are known.

use egui::{Key, Modifiers};

use super::VirtualTextState;
use super::chrome;
use super::folds::{is_hidden, next_visible, prev_visible, snap_visible};
use super::hit;
use super::keys::consume;
use super::layout::LineData;
use super::select::Selection;
use super::window;
use crate::EditorState;

impl EditorState {
    /// Left/Right (word-wise with Ctrl/Cmd) and Home/End (document start/end
    /// with Ctrl/Cmd). Shift extends the selection. Every caret moves.
    pub(super) fn virtual_horizontal_nav(
        &mut self,
        ui: &egui::Ui,
        outcome: &mut super::keys::InputOutcome,
    ) {
        let len = self.buffer.len_chars();
        let shift = ui.input(|frame| frame.modifiers.shift);

        let left = consume(ui, Modifiers::COMMAND, Key::ArrowLeft)
            || consume(ui, Modifiers::NONE, Key::ArrowLeft);
        if left {
            let word = ui.input(|frame| frame.modifiers.ctrl || frame.modifiers.command);
            self.nav_horizontal(false, shift, word, len);
            outcome.caret_moved = true;
        } else if consume(ui, Modifiers::COMMAND, Key::ArrowRight)
            || consume(ui, Modifiers::NONE, Key::ArrowRight)
        {
            let word = ui.input(|frame| frame.modifiers.ctrl || frame.modifiers.command);
            self.nav_horizontal(true, shift, word, len);
            outcome.caret_moved = true;
        }

        if consume(ui, Modifiers::COMMAND, Key::Home) {
            self.apply_horizontal_all(0, len, shift);
            outcome.caret_moved = true;
        } else if consume(ui, Modifiers::NONE, Key::Home) {
            self.apply_horizontal_lines(len, shift, false);
            outcome.caret_moved = true;
        }
        if consume(ui, Modifiers::COMMAND, Key::End) {
            self.apply_horizontal_all(len, len, shift);
            outcome.caret_moved = true;
        } else if consume(ui, Modifiers::NONE, Key::End) {
            self.apply_horizontal_lines(len, shift, true);
            outcome.caret_moved = true;
        }
    }

    /// One Left/Right step applied to every caret; a non-extending move first
    /// collapses that caret's selection. Every horizontal move clears `h_pos`.
    fn nav_horizontal(&mut self, right: bool, shift: bool, word: bool, len: usize) {
        let targets: Vec<usize> = {
            let carets = self.virtual_text.carets();
            carets
                .list()
                .iter()
                .map(|sel| horizontal_target(&self.buffer, sel, right, word, len, shift))
                .collect()
        };
        apply_targets(&self.virtual_text, &targets, len, shift);
    }

    /// Moves every caret to the same absolute `target` (Ctrl+Home / Ctrl+End).
    fn apply_horizontal_all(&mut self, target: usize, len: usize, shift: bool) {
        let count = self.virtual_text.carets().len();
        let targets = vec![target; count];
        apply_targets(&self.virtual_text, &targets, len, shift);
    }

    /// Moves every caret to its own line start or line end.
    fn apply_horizontal_lines(&mut self, len: usize, shift: bool, end: bool) {
        let targets: Vec<usize> = {
            let carets = self.virtual_text.carets();
            carets
                .list()
                .iter()
                .map(|sel| {
                    if end {
                        self.line_end_at(sel.caret)
                    } else {
                        self.line_start_at(sel.caret)
                    }
                })
                .collect()
        };
        apply_targets(&self.virtual_text, &targets, len, shift);
    }

    fn line_start_at(&self, caret: usize) -> usize {
        let line = self.buffer.line_of_char(caret);
        self.buffer.line_start(line)
    }

    fn line_end_at(&self, caret: usize) -> usize {
        let line = self.buffer.line_of_char(caret);
        hit::line_content_end(&self.buffer, line)
    }
}

/// One Left/Right target for a caret (selection edge first when not extending).
fn horizontal_target(
    buffer: &typ_edit_buffer::TextBuffer,
    sel: &Selection,
    right: bool,
    word: bool,
    len: usize,
    shift: bool,
) -> usize {
    if !shift && let Some(selection) = sel.selection() {
        return if right {
            selection.end
        } else {
            selection.start
        };
    }
    let caret = sel.caret.min(len);
    if word {
        let (base, text) = word_window(buffer, caret, right);
        let cursor = egui::text::CCursor::new(caret - base);
        let moved = if right {
            egui::text_selection::text_cursor_state::ccursor_next_word(&text, cursor)
        } else {
            egui::text_selection::text_cursor_state::ccursor_previous_word(&text, cursor)
        };
        (base + moved.index.0).min(len)
    } else if right {
        (caret + 1).min(len)
    } else {
        caret.saturating_sub(1)
    }
}

/// A char window around `caret` large enough for egui's word-boundary scan.
/// Grows only when the caret sits on the window edge, so ordinary navigation
/// never materializes the document.
fn word_window(buffer: &typ_edit_buffer::TextBuffer, caret: usize, right: bool) -> (usize, String) {
    let len = buffer.len_chars();
    let mut radius = 512usize;
    loop {
        let start = caret.saturating_sub(radius);
        let end = caret.saturating_add(radius).min(len);
        let text = buffer.slice(start..end);
        if start == 0 && end == len {
            return (start, text);
        }
        let local = egui::text::CCursor::new(caret - start);
        let moved = if right {
            egui::text_selection::text_cursor_state::ccursor_next_word(&text, local)
        } else {
            egui::text_selection::text_cursor_state::ccursor_previous_word(&text, local)
        };
        let at_edge = if right {
            end < len && moved.index.0 == end - start
        } else {
            start > 0 && moved.index.0 == 0
        };
        if !at_edge {
            return (start, text);
        }
        radius = radius.saturating_mul(8);
    }
}

/// Moves/extends one caret, remembering `h_pos` for vertical continuation.
fn apply_nav_to(sel: &mut Selection, target: usize, len: usize, shift: bool, h_pos: Option<f32>) {
    let target = target.min(len);
    if shift {
        if sel.anchor.is_none() {
            sel.anchor = Some(sel.caret);
        }
        sel.caret = target;
    } else {
        sel.set_caret(target);
    }
    sel.h_pos = h_pos;
}

/// Applies one target per caret, clearing each remembered vertical x.
fn apply_targets(state: &VirtualTextState, targets: &[usize], len: usize, shift: bool) {
    let mut carets = state.carets_mut();
    for (index, target) in targets.iter().enumerate() {
        if let Some(sel) = carets.list_mut().get_mut(index) {
            apply_nav_to(sel, *target, len, shift, None);
        }
    }
}

/// Consumes Up/Down/PageUp/PageDown (with Shift extending and Ctrl/Cmd jumping
/// to the document ends, Ctrl+Alt adding a column caret) and moves every caret
/// across wrapped rows preserving each one's remembered x. Called from the
/// render pass, where the galley cache is available. Returns whether any caret
/// moved.
pub(super) fn handle_vertical_keys(
    ui: &egui::Ui,
    focused: bool,
    state: &mut VirtualTextState,
    data: &LineData<'_>,
    viewport_h: f32,
) -> bool {
    if !focused {
        return false;
    }
    let len = data.buffer.len_chars();
    let shift = ui.input(|frame| frame.modifiers.shift);

    // Ctrl+Alt+Up/Down adds a column caret. Checked before the plain
    // Ctrl+Arrow document jumps, whose logical modifier match would otherwise
    // also accept them.
    if consume(ui, Modifiers::COMMAND | Modifiers::ALT, Key::ArrowDown) {
        if add_column_caret(state, ui, data, true) {
            state.blink_origin = ui.input(|frame| frame.time);
            return true;
        }
        return false;
    }
    if consume(ui, Modifiers::COMMAND | Modifiers::ALT, Key::ArrowUp) {
        if add_column_caret(state, ui, data, false) {
            state.blink_origin = ui.input(|frame| frame.time);
            return true;
        }
        return false;
    }

    if consume(ui, Modifiers::COMMAND, Key::ArrowUp) {
        let count = state.carets().len();
        apply_targets(state, &vec![0; count], len, shift);
        return true;
    }
    if consume(ui, Modifiers::COMMAND, Key::ArrowDown) {
        let count = state.carets().len();
        apply_targets(state, &vec![len; count], len, shift);
        return true;
    }

    let mut moved = false;
    if consume(ui, Modifiers::SHIFT, Key::ArrowUp) || consume(ui, Modifiers::NONE, Key::ArrowUp) {
        moved = move_all_vertical(state, ui, data, false, shift);
    } else if consume(ui, Modifiers::SHIFT, Key::ArrowDown)
        || consume(ui, Modifiers::NONE, Key::ArrowDown)
    {
        moved = move_all_vertical(state, ui, data, true, shift);
    } else if consume(ui, Modifiers::SHIFT, Key::PageUp)
        || consume(ui, Modifiers::NONE, Key::PageUp)
    {
        moved = move_all_pages(state, ui, data, viewport_h, false, shift);
    } else if consume(ui, Modifiers::SHIFT, Key::PageDown)
        || consume(ui, Modifiers::NONE, Key::PageDown)
    {
        moved = move_all_pages(state, ui, data, viewport_h, true, shift);
    }
    if moved {
        state.blink_origin = ui.input(|frame| frame.time);
    }
    moved
}

/// Moves every caret one visual row, preserving each caret's own `h_pos`.
///
/// Multiple carets on one line reuse the same cache entry: a cached line takes
/// the exact galley path (a cache hit, never a reshape) while an off-screen
/// uncached line takes the stateless estimate path, so no per-call memo is
/// needed to avoid repeated shaping.
fn move_all_vertical(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData<'_>,
    down: bool,
    shift: bool,
) -> bool {
    let len = data.buffer.len_chars();
    let snapshot: Vec<(usize, Option<f32>)> = state
        .carets()
        .list()
        .iter()
        .map(|sel| (sel.caret, sel.h_pos))
        .collect();
    let mut moved = false;
    for (index, (caret, h_pos)) in snapshot.into_iter().enumerate() {
        if let Some((target, h)) = vertical_target(state, ui, data, down, caret, h_pos) {
            if let Some(sel) = state.carets_mut().list_mut().get_mut(index) {
                apply_nav_to(sel, target, len, shift, Some(h));
            }
            moved = true;
        }
    }
    moved
}

/// Moves every caret by roughly one viewport height, preserving `h_pos`.
fn move_all_pages(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData<'_>,
    viewport_h: f32,
    down: bool,
    shift: bool,
) -> bool {
    let len = data.buffer.len_chars();
    let snapshot: Vec<(usize, Option<f32>)> = state
        .carets()
        .list()
        .iter()
        .map(|sel| (sel.caret, sel.h_pos))
        .collect();
    let mut moved = false;
    for (index, (caret, h_pos)) in snapshot.into_iter().enumerate() {
        if let Some((target, h)) = page_target(state, ui, data, viewport_h, down, caret, h_pos) {
            if let Some(sel) = state.carets_mut().list_mut().get_mut(index) {
                apply_nav_to(sel, target, len, shift, Some(h));
            }
            moved = true;
        }
    }
    moved
}

/// Adds a collapsed caret at the primary's x on the next/previous visible line.
fn add_column_caret(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData<'_>,
    down: bool,
) -> bool {
    let buffer = data.buffer;
    let len = buffer.len_chars();
    let caret = state.caret().min(len);
    let h_pos = state.h_pos();
    let hidden = state.cache.hidden().to_vec();
    let line_count = buffer.line_count();
    let line = snap_visible(&hidden, buffer.line_of_char(caret), line_count);
    let target_line = if down {
        next_visible(&hidden, line + 1)
    } else if line == 0 {
        return false;
    } else {
        prev_visible(&hidden, line - 1)
    };
    if target_line >= line_count {
        return false;
    }
    let local = caret.saturating_sub(buffer.line_start(line));
    let h = caret_x(state, ui, data, line, local, h_pos);
    let target = char_at_x_on_line(state, ui, data, target_line, h, down);
    let mut carets = state.carets_mut();
    carets.push_selection(Selection {
        caret: target,
        anchor: None,
        h_pos: Some(h),
    });
    carets.dedup_exact();
    true
}

/// Desired caret x for a vertical move: the remembered `h_pos`, or the current
/// caret's x in galley space.
///
/// An off-screen line whose galley is not cached is measured with the
/// monospace advance estimate rather than shaped, so a vertical move never
/// forces a shape for a line outside the viewport. The estimate is exact for
/// monospace and approximate for mixed-width (CJK) off-screen content.
fn caret_x(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData<'_>,
    line: usize,
    local: usize,
    h_pos: Option<f32>,
) -> f32 {
    if let Some(h) = h_pos {
        return h;
    }
    if !state.cache.has_galley(line) {
        return local as f32 * window::advance_estimate(ui, data);
    }
    let cached = state.cache.line_galley(ui, data, line);
    chrome::caret_x(cached, local, cached.advance)
}

/// The char nearest `h` (galley x) on `line`, entering from the top when
/// moving down or from the bottom when moving up.
///
/// An off-screen line whose galley is not cached is estimated from the
/// monospace advance (column rounded and clamped to the line's char length)
/// instead of being shaped; a cached line keeps the exact `cursor_from_pos`
/// path.
fn char_at_x_on_line(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData<'_>,
    line: usize,
    h: f32,
    down: bool,
) -> usize {
    if !state.cache.has_galley(line) {
        let advance = window::advance_estimate(ui, data);
        let line_len = hit::line_char_len(data.buffer, line);
        let column = (h / advance).round().max(0.0) as usize;
        return data.buffer.line_start(line) + column.min(line_len);
    }
    let cached = state.cache.line_galley(ui, data, line);
    let x = h - cached.window_start as f32 * cached.advance;
    let galley = std::sync::Arc::clone(&cached.galley);
    let y = if down {
        galley.pos_from_cursor(galley.begin()).center().y
    } else {
        galley.pos_from_cursor(galley.end()).center().y
    };
    let next = galley.cursor_from_pos(egui::vec2(x, y));
    data.buffer.line_start(line) + next.index.0
}

/// Up/Down one visual row, crossing logical lines at the wrapped edges.
fn vertical_target(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData<'_>,
    down: bool,
    caret: usize,
    h_pos: Option<f32>,
) -> Option<(usize, f32)> {
    let buffer = data.buffer;
    let len = buffer.len_chars();
    let caret = caret.min(len);
    let hidden = state.cache.hidden().to_vec();
    let line = snap_visible(&hidden, buffer.line_of_char(caret), buffer.line_count());
    let local = caret.saturating_sub(buffer.line_start(line));
    let h = caret_x(state, ui, data, line, local, h_pos);

    // Resolving a move *within* the line's own soft-wrapped rows needs the
    // source galley; only consult it when it is already cached. An off-screen
    // uncached line is stepped a whole logical line instead of being shaped
    // (approximate for off-screen wrapped lines, exact on screen).
    if state.cache.has_galley(line) {
        let source = state.cache.line_galley(ui, data, line);
        let win_local = local
            .saturating_sub(source.window_start)
            .min(source.window_len);
        let galley = std::sync::Arc::clone(&source.galley);
        let cursor = egui::text::CCursor::new(win_local);

        let within = if down {
            let (next, h_pos) = galley.cursor_down_one_row(&cursor, Some(h));
            h_pos.map(|_| next)
        } else {
            let (next, h_pos) = galley.cursor_up_one_row(&cursor, Some(h));
            h_pos.map(|_| next)
        };
        if let Some(next) = within {
            return Some((buffer.line_start(line) + next.index.0, h));
        }
    }

    let target_line = if down {
        next_visible(&hidden, line + 1)
    } else {
        prev_visible(&hidden, line.checked_sub(1)?)
    };
    if target_line >= buffer.line_count() {
        let last = buffer.line_count() - 1;
        let target = if is_hidden(&hidden, last) {
            hit::line_content_end(buffer, prev_visible(&hidden, last))
        } else {
            len
        };
        return Some((target, h));
    }
    Some((char_at_x_on_line(state, ui, data, target_line, h, down), h))
}

/// PageUp/PageDown: step whole logical lines until roughly one viewport height
/// is covered, then land on the same x.
fn page_target(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData<'_>,
    viewport_h: f32,
    down: bool,
    caret: usize,
    h_pos: Option<f32>,
) -> Option<(usize, f32)> {
    let buffer = data.buffer;
    let len = buffer.len_chars();
    let caret = caret.min(len);
    let hidden = state.cache.hidden().to_vec();
    let line_count = buffer.line_count();
    let line = snap_visible(&hidden, buffer.line_of_char(caret), line_count);
    let local = caret.saturating_sub(buffer.line_start(line));
    let h = caret_x(state, ui, data, line, local, h_pos);

    let mut accumulated = 0.0f32;
    let mut target_line = line;
    if down {
        let mut current = line;
        while current + 1 < line_count {
            accumulated += page_line_height(state, ui, data, current);
            if accumulated >= viewport_h {
                break;
            }
            let next = next_visible(&hidden, current + 1);
            if next >= line_count {
                break;
            }
            current = next;
            target_line = current;
        }
    } else {
        let mut current = line;
        while current > 0 {
            let prev = prev_visible(&hidden, current - 1);
            accumulated += page_line_height(state, ui, data, prev);
            target_line = prev;
            if accumulated >= viewport_h {
                break;
            }
            current = prev;
        }
    }

    Some((char_at_x_on_line(state, ui, data, target_line, h, down), h))
}

/// Height used for one page step: the exact cached height when the line is
/// already shaped, or one fallback row for an off-screen line that must not be
/// shaped (approximate for off-screen wrapped lines).
fn page_line_height(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData<'_>,
    line: usize,
) -> f32 {
    if state.cache.has_galley(line) {
        state.cache.line_height(ui, data, line)
    } else {
        state.cache.row_height(ui, data)
    }
}
