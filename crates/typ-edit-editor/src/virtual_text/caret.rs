//! Pointer interaction, selection/caret painting and caret scrolling for the
//! virtualized text path (Stage B).
//!
//! All of this runs only in virtual mode. Keyboard, IME, clipboard and undo are
//! deliberately absent (Stage C).

use std::ops::Range;
use std::time::Duration;

use egui::{Color32, Galley, Pos2, Rect, pos2};
use typ_edit_buffer::TextBuffer;

use super::VirtualTextState;
use super::caret_sync::SelectionHandle;
use super::chrome;
use super::hit::{LineSlot, caret_screen_rect, char_pos, hit_test};
use super::layout::LineData;
use super::nav;
use super::select::{Carets, Selection};
use crate::paint::drag_autoscroll_delta;

/// Caret blink half-period, in seconds (solid then off, 0.5 s each).
const BLINK_HALF_PERIOD: f64 = 0.5;
/// Caret stroke width, in points.
const CARET_WIDTH: f32 = 1.8;
/// Margin kept around the caret when it is scrolled into view, in points.
const CARET_SCROLL_MARGIN: f32 = 8.0;
/// Repaint cadence while the caret is blinking, in milliseconds.
const BLINK_REPAINT_MS: u64 = 500;
/// Color multiplier applied to the caret while the widget is unfocused.
const UNFOCUSED_CARET_DIM: f32 = 0.6;
/// Base gray of the unfocused selection fill.
const UNFOCUSED_SELECTION_GRAY: u8 = 160;
/// Alpha of the unfocused selection fill (0..=1), applied to the base gray.
const UNFOCUSED_SELECTION_ALPHA: f32 = 0.25;

/// How the caret is painted for one focus state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CaretPaintStyle {
    /// Whether the blink phase gate applies. Focused carets blink; unfocused
    /// carets are static (always visible).
    pub blink: bool,
    /// Multiplier for the caret color; `1.0` reproduces the focused color.
    pub dim: f32,
}

/// Decides the caret's paint style from the widget's focus state.
///
/// Focused keeps today's behavior exactly — blinking at full color. Unfocused
/// keeps the caret visible but static, at a dimmed color.
pub(super) fn caret_paint_style(focused: bool) -> CaretPaintStyle {
    if focused {
        CaretPaintStyle {
            blink: true,
            dim: 1.0,
        }
    } else {
        CaretPaintStyle {
            blink: false,
            dim: UNFOCUSED_CARET_DIM,
        }
    }
}

/// The selection fill for one focus state.
///
/// Focused returns `active` (the configured selection color) unchanged.
/// Unfocused returns a light neutral gray, so the selection reads as inactive.
pub(super) fn selection_fill(focused: bool, active: Color32) -> Color32 {
    if focused {
        active
    } else {
        Color32::from_gray(UNFOCUSED_SELECTION_GRAY).gamma_multiply(UNFOCUSED_SELECTION_ALPHA)
    }
}

/// Whether the caret is in its visible blink phase at `now`.
///
/// The caret is solid for `BLINK_HALF_PERIOD` after `blink_origin`, hidden for
/// the next, and so on, so a fresh interaction resets it to solid.
pub(super) fn caret_visible(now: f64, blink_origin: f64) -> bool {
    (now - blink_origin).rem_euclid(BLINK_HALF_PERIOD * 2.0) < BLINK_HALF_PERIOD
}

/// Pointer results the caller folds into [`crate::EditorOutput`].
#[derive(Default)]
pub(super) struct PointerOutcome {
    pub ctrl_click: Option<usize>,
    pub hover_byte: Option<usize>,
    /// Byte boundary nearest the pointer over the text viewport, computed every
    /// frame regardless of button state or focus (for drop insertion).
    pub pointer_byte: Option<usize>,
    pub caret_moved: bool,
    /// A primary click landed on the text (used to leave in-place cell editing).
    pub clicked: bool,
}

/// Applies this frame's pointer gestures to the selection.
///
/// Order matters: triple/double click win over a plain click (egui reports a
/// click for those too), and a drag start seeds the anchor at the press origin.
/// Returns the Ctrl+click and hover report plus whether the caret moved (used to
/// trigger scroll-into-view).
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_pointer(
    ui: &egui::Ui,
    response: &egui::Response,
    viewport_rect: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    selection: &SelectionHandle,
    blink_origin: &mut f64,
    dragging_scrollbar: bool,
) -> PointerOutcome {
    let mut outcome = PointerOutcome::default();
    // Drop-insertion target: the byte boundary nearest the pointer whenever it
    // is over the text viewport. Computed before the scrollbar early-return and
    // without the `any_down` gate, so a drag heading into the editor still has
    // a target even though `hover_byte` (plain-hover only) does not.
    if let Some(pos) = ui.input(|i| i.pointer.hover_pos())
        && viewport_rect.contains(pos)
        && let Some(ch) = hit_test(buffer, slots, pos)
    {
        outcome.pointer_byte = Some(buffer.char_to_byte(ch));
    }
    let mut selection = selection.borrow_mut();
    if response.is_pointer_button_down_on() {
        response.request_focus();
    }
    if dragging_scrollbar {
        return outcome;
    }

    let modifiers = ui.input(|i| i.modifiers);
    let pointer = response
        .interact_pointer_pos()
        .or_else(|| ui.input(|i| i.pointer.interact_pos()));
    let now = ui.input(|i| i.time);
    let hit_at = |pos: Pos2| hit_test(buffer, slots, pos);
    // Ctrl/Cmd and Alt clicks are their own gestures: they must not be folded
    // into a word/line multi-click even when the pointer repeats quickly.
    let plain_gesture = !(modifiers.alt || modifiers.ctrl || modifiers.command);

    if plain_gesture && response.triple_clicked() {
        if let Some(ch) = pointer.and_then(hit_at) {
            let line = buffer.line_of_char(ch);
            let mut single = Selection::default();
            single.select_line_at(buffer, line);
            selection.set_single(single);
            *blink_origin = now;
            outcome.caret_moved = true;
        }
    } else if plain_gesture && response.double_clicked() {
        if let Some(ch) = pointer.and_then(hit_at) {
            let mut single = Selection::default();
            single.select_word_at(buffer, ch);
            selection.set_single(single);
            *blink_origin = now;
            outcome.caret_moved = true;
        }
    } else if response.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
        if let Some(ch) = origin.and_then(hit_at) {
            if modifiers.shift {
                selection.start_drag(ch);
            } else {
                let mut single = Selection::default();
                single.start_drag(ch);
                selection.set_single(single);
            }
            *blink_origin = now;
            outcome.caret_moved = true;
        }
    } else if response.dragged() {
        if let Some(ch) = pointer.and_then(hit_at) {
            if ch != selection.caret() {
                selection.extend_to(ch);
            }
            outcome.caret_moved = true;
        }
    } else if response.clicked() {
        outcome.clicked = true;
        if let Some(ch) = pointer.and_then(hit_at) {
            if modifiers.alt {
                toggle_caret_at(&mut selection, ch);
            } else if modifiers.ctrl || modifiers.command {
                outcome.ctrl_click = Some(buffer.char_to_byte(ch));
                selection.primary_mut().set_caret(ch);
            } else if modifiers.shift {
                let anchor = selection.caret();
                selection.set_selection(anchor, ch);
            } else {
                selection.set_single(Selection {
                    caret: ch,
                    anchor: None,
                    h_pos: None,
                });
            }
            *blink_origin = now;
            outcome.caret_moved = true;
        }
    }

    // Hover report (no button down), gated to the text viewport and to a real
    // glyph: blank space past a line's end (or trailing whitespace) reports
    // nothing, so tooltips only appear over actual content.
    if !ui.input(|i| i.pointer.any_down())
        && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
        && viewport_rect.contains(pos)
        && let Some(ch) = hit_at(pos)
        && buffer.char_at(ch).is_some_and(|c| !c.is_whitespace())
    {
        outcome.hover_byte = Some(buffer.char_to_byte(ch));
    }
    outcome
}

/// Adds a collapsed caret at `ch`, or removes an existing one exactly there.
/// The last caret is never removed; duplicates are collapsed afterwards.
fn toggle_caret_at(carets: &mut Carets, ch: usize) {
    match carets.index_for_caret(ch) {
        Some(index) if carets.len() > 1 => carets.remove_at(index),
        Some(_) => {}
        None => carets.push_selection(Selection {
            caret: ch,
            anchor: None,
            h_pos: None,
        }),
    }
    carets.dedup_exact();
}

/// One frame's interaction inputs, grouped to keep the entry point small.
pub(super) struct FrameInteraction<'a> {
    pub response: &'a egui::Response,
    pub viewport_rect: Rect,
    pub slots: &'a [LineSlot],
    pub data: &'a LineData<'a>,
    pub line_count: usize,
    pub viewport_h: f32,
    pub focused: bool,
    pub input_caret_moved: bool,
}

/// Runs pointer interaction, vertical navigation and the scroll-into-view pass
/// for one frame. Returns the pointer outcome and whether the anchor moved (the
/// caller must then rebuild the visible slots).
pub(super) fn interact_frame(
    ui: &egui::Ui,
    state: &mut VirtualTextState,
    frame: &FrameInteraction<'_>,
) -> (PointerOutcome, bool) {
    let dragging_scrollbar = state.dragging_scrollbar || state.dragging_hscrollbar;
    // A two-finger pan owns the pointer; a single touch stays a selection.
    let outcome = if state.multi_touch {
        PointerOutcome::default()
    } else {
        handle_pointer(
            ui,
            frame.response,
            frame.viewport_rect,
            frame.slots,
            frame.data.buffer,
            &state.selection,
            &mut state.blink_origin,
            dragging_scrollbar,
        )
    };
    // Any pointer-driven caret move resets the remembered vertical x, matching
    // the contract that horizontal moves/edits clear it. This runs before
    // `handle_vertical_keys` so a same-frame click + Up/Down cannot consume the
    // stale column.
    if outcome.caret_moved {
        state.carets_mut().clear_h_pos();
    }
    let nav_moved =
        nav::handle_vertical_keys(ui, frame.focused, state, frame.data, frame.viewport_h);
    let mut anchor_changed = autoscroll_dragging(
        ui,
        frame.response,
        frame.viewport_rect,
        state,
        frame.data,
        frame.line_count,
        frame.viewport_h,
    );
    if outcome.caret_moved || nav_moved || frame.input_caret_moved {
        anchor_changed |= chrome::scroll_caret_visible(
            state,
            ui,
            frame.data,
            frame.line_count,
            frame.viewport_rect,
            frame.viewport_h,
            frame.slots,
            frame.input_caret_moved && !outcome.caret_moved && !nav_moved,
        );
    }
    (outcome, anchor_changed)
}

/// Drag auto-scroll: while a text drag reaches beyond the viewport edge, nudge
/// the anchor each frame. Returns whether the anchor changed.
#[allow(clippy::too_many_arguments)]
pub(super) fn autoscroll_dragging(
    ui: &egui::Ui,
    response: &egui::Response,
    viewport_rect: Rect,
    state: &mut VirtualTextState,
    data: &LineData,
    line_count: usize,
    viewport_h: f32,
) -> bool {
    if !response.dragged() {
        return false;
    }
    let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) else {
        return false;
    };
    let dt = ui.input(|i| i.stable_dt);
    let delta = drag_autoscroll_delta(pointer.y, viewport_rect, dt);
    if delta == 0.0 {
        return false;
    }
    chrome::scroll_vertical(state, ui, data, line_count, viewport_h, delta);
    ui.ctx().request_repaint();
    true
}

/// Scrolls the anchor so the caret is visible with a small margin, when the
/// caret moved to a partially visible row. Returns whether the anchor changed.
#[allow(clippy::too_many_arguments)]
pub(super) fn scroll_caret_into_view(
    ui: &egui::Ui,
    state: &mut VirtualTextState,
    data: &LineData,
    line_count: usize,
    viewport_rect: Rect,
    viewport_h: f32,
    slots: &[LineSlot],
    from_input: bool,
) -> bool {
    let caret = state.caret();
    let vertical = if let Some((_line, pos)) = char_pos(data.buffer, slots, caret) {
        // Typing must not nudge a caret that is already inside the viewport:
        // the normal caret margin made the viewport jump one row when the
        // caret sat on its top edge. Explicit navigation keeps the margin.
        let margin = if from_input {
            0.0
        } else {
            CARET_SCROLL_MARGIN.min(viewport_h * 0.25)
        };
        let delta = if pos.y < viewport_rect.top() + margin {
            pos.y - (viewport_rect.top() + margin)
        } else if pos.y > viewport_rect.bottom() - margin {
            pos.y - (viewport_rect.bottom() - margin)
        } else {
            0.0
        };
        if delta != 0.0 {
            chrome::scroll_vertical(state, ui, data, line_count, viewport_h, delta);
            true
        } else {
            false
        }
    } else {
        false
    };
    let horizontal = chrome::scroll_caret_horizontal(state, ui, data, caret);
    if vertical || horizontal {
        ui.ctx().request_repaint();
    }
    vertical || horizontal
}

/// Fills `local` chars of `galley` (painted at `origin`) with `fill`, returning
/// the rects painted. The galley is never mutated.
pub(super) fn paint_galley_selection(
    painter: &egui::Painter,
    clip: Rect,
    galley: &Galley,
    origin: Pos2,
    local: Range<usize>,
    fill: Color32,
    corner: f32,
) -> Vec<Rect> {
    let mut painted = Vec::new();
    for rect in crate::diagnostics::squiggle_rects(galley, origin, local) {
        if rect.intersects(clip) {
            painter.rect_filled(rect, corner, fill);
            painted.push(rect);
        }
    }
    painted
}

/// Fills the caret stroke (width [`CARET_WIDTH`]) for a zero-width caret `rect`.
pub(super) fn paint_caret_rect(painter: &egui::Painter, clip: Rect, rect: Rect, color: Color32) {
    let caret = Rect::from_min_max(
        pos2(rect.left(), rect.top()),
        pos2(rect.left() + CARET_WIDTH, rect.bottom()),
    );
    if caret.intersects(clip) {
        painter.rect_filled(caret, 0.0, color);
    }
}

/// Paints the preedit band plus underline over `local` chars of `galley`,
/// returning the rects painted.
pub(super) fn paint_galley_preedit(
    painter: &egui::Painter,
    clip: Rect,
    galley: &Galley,
    origin: Pos2,
    local: Range<usize>,
    fill: Color32,
    underline: egui::Stroke,
) -> Vec<Rect> {
    let mut painted = Vec::new();
    for rect in crate::diagnostics::squiggle_rects(galley, origin, local) {
        if !rect.intersects(clip) {
            continue;
        }
        painter.rect_filled(rect, 1.0, fill);
        painter.line_segment([rect.left_bottom(), rect.right_bottom()], underline);
        painted.push(rect);
    }
    painted
}

/// Paints the selection as translucent rects behind the glyphs.
///
/// Per visible line the global selection is intersected with the line's char
/// range (excluding its `\n`) and converted to a line-local range for
/// [`crate::diagnostics::squiggle_rects`]. The cached galleys are never
/// mutated. The fill follows the widget's focus state via [`selection_fill`].
pub(super) fn paint_selection(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    carets: &Carets,
    focused: bool,
) {
    let painter = ui.painter().with_clip_rect(clip);
    let fill = selection_fill(focused, ui.visuals().selection.bg_fill);
    for selection in carets.list() {
        let Some(sel) = selection.selection() else {
            continue;
        };
        for slot in slots {
            let start = buffer.line_start(slot.line);
            let end = start + super::hit::line_char_len(buffer, slot.line);
            let lo = sel.start.max(start);
            let hi = sel.end.min(end);
            if lo >= hi {
                continue;
            }
            let Some(local) = slot.window_local((lo - start)..(hi - start)) else {
                continue;
            };
            let _ = paint_galley_selection(
                &painter,
                clip,
                &slot.galley,
                slot.screen_origin(),
                local,
                fill,
                2.0,
            );
        }
    }
}

/// Paints the caret after the glyphs.
///
/// Focused: the blinking caret at full color. Unfocused: the caret stays
/// visible but static (no blink), dimmed per [`caret_paint_style`], at the same
/// width and geometry.
pub(super) fn paint_caret(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    carets: &Carets,
    focused: bool,
    blink_origin: f64,
) {
    let style = caret_paint_style(focused);
    if style.blink {
        let now = ui.input(|i| i.time);
        if !caret_visible(now, blink_origin) {
            return;
        }
    }
    let color = ui.visuals().text_color().gamma_multiply(style.dim);
    let painter = ui.painter().with_clip_rect(clip);
    for selection in carets.list() {
        let Some(rect) = caret_screen_rect(buffer, slots, selection.caret) else {
            continue;
        };
        paint_caret_rect(&painter, clip, rect, color);
    }
}

/// Paints the active IME preedit range as a translucent band plus underline,
/// using the same per-line rects as the selection.
pub(super) fn paint_preedit(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    preedit: Option<&std::ops::Range<usize>>,
) {
    let Some(range) = preedit else {
        return;
    };
    let painter = ui.painter().with_clip_rect(clip);
    let fill = ui.visuals().selection.bg_fill.gamma_multiply(0.6);
    let underline = egui::Stroke::new(1.5, ui.visuals().text_color());
    for slot in slots {
        let start = buffer.line_start(slot.line);
        let end = start + super::hit::line_char_len(buffer, slot.line);
        let lo = range.start.max(start);
        let hi = range.end.min(end);
        if lo >= hi {
            continue;
        }
        let Some(local) = slot.window_local((lo - start)..(hi - start)) else {
            continue;
        };
        let _ = paint_galley_preedit(
            &painter,
            clip,
            &slot.galley,
            slot.screen_origin(),
            local,
            fill,
            underline,
        );
    }
}

/// Schedules the next blink repaint while the widget owns focus.
pub(super) fn schedule_blink(ui: &egui::Ui, focused: bool) {
    if focused {
        ui.ctx()
            .request_repaint_after(Duration::from_millis(BLINK_REPAINT_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_blinks_off_then_on() {
        assert!(caret_visible(0.0, 0.0));
        assert!(caret_visible(0.4, 0.0));
        assert!(!caret_visible(0.6, 0.0));
        assert!(!caret_visible(0.99, 0.0));
        assert!(caret_visible(1.1, 0.0));
        // A fresh interaction resets to solid.
        assert!(caret_visible(10.2, 10.0));
    }

    #[test]
    fn focused_caret_blinks_at_full_color_unfocused_is_static_and_dim() {
        let focused = caret_paint_style(true);
        assert!(focused.blink, "focused caret blinks");
        assert_eq!(focused.dim, 1.0, "focused caret color is unchanged");

        let unfocused = caret_paint_style(false);
        assert!(!unfocused.blink, "unfocused caret never blinks");
        assert!(
            unfocused.dim > 0.0 && unfocused.dim < 1.0,
            "unfocused caret is dimmed but visible, got {}",
            unfocused.dim
        );
    }

    #[test]
    fn selection_fill_is_active_when_focused_and_gray_when_unfocused() {
        let active = Color32::from_rgb(10, 20, 30);
        assert_eq!(selection_fill(true, active), active);

        let unfocused = selection_fill(false, active);
        assert_ne!(unfocused, active);
        assert!(unfocused.a() > 0, "unfocused selection stays visible");
    }
}
