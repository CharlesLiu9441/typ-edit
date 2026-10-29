//! Scroll math and gutter painting for the virtualized text path.
//!
//! Extracted from `render` so both the frame orchestrator and the caret/pointer
//! code can nudge the anchor without a cycle. Stage A behavior is unchanged.

use egui::{Align2, Color32, FontFamily, FontId, Rect, pos2, vec2};

use super::VirtualTextState;
use super::folds::snap_visible;
use super::layout::{CachedLine, LineCache, LineData};
use super::window;

/// Width of the gutter's fold band, in points. Its left half holds the fold
/// chevron; its right half holds the bookmark icon.
pub(super) const FOLD_BAND_W: f32 = 20.0;
/// Width of the fold-chevron half of the band, in points.
pub(super) const FOLD_CHEVRON_W: f32 = 10.0;
/// Inset of the line number / folded badge from the gutter's right edge, in
/// points.
const GUTTER_RIGHT_PAD: f32 = 6.0;

/// Scrolls the anchor vertically by `delta_px` and clamps it to the estimate.
pub(super) fn scroll_vertical(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    line_count: usize,
    viewport_h: f32,
    delta_px: f32,
) {
    let hidden = state.cache.hidden().to_vec();
    {
        let mut heights = |line| state.cache.line_height(ui, data, line);
        state
            .anchor
            .scroll_by(delta_px, &mut heights, line_count, &hidden);
    }
    let fallback = state.cache.row_height(ui, data);
    let total = state.cache.total_height(line_count, fallback);
    let prefix = |line| state.cache.prefix_height(line, line_count, fallback);
    state
        .anchor
        .clamp_end(total, viewport_h, prefix, line_count, &hidden);
}

/// Absolute scroll position of the anchor, from the cache's prefix sums so no
/// line above the viewport is walked or measured.
pub(super) fn anchor_abs(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    line_count: usize,
) -> f32 {
    let fallback = state.cache.row_height(ui, data);
    let prefix = |line| state.cache.prefix_height(line, line_count, fallback);
    state.anchor.abs_y(prefix, line_count)
}

/// Moves the anchor to an absolute offset and clamps it to the document end.
/// Takes `state` as a whole because the prefix closure and the anchor are
/// disjoint fields the borrow checker cannot see through from a
/// `&VirtualTextState`.
pub(super) fn set_anchor_abs(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    line_count: usize,
    target: f32,
    viewport_h: f32,
) {
    let hidden = state.cache.hidden().to_vec();
    let fallback = state.cache.row_height(ui, data);
    let total = state.cache.total_height(line_count, fallback);
    let prefix = |line| state.cache.prefix_height(line, line_count, fallback);
    state.anchor.set_abs_y(target, prefix, line_count, &hidden);
    state
        .anchor
        .clamp_end(total, viewport_h, prefix, line_count, &hidden);
}

/// Scrolls the anchor so `line` is inside the viewport with a small margin.
///
/// Unlike the per-slot [`super::caret::scroll_caret_into_view`], this works for
/// a caret on a line that is not currently painted (a jump or page move), so
/// keyboard navigation can always bring its target into view. Returns whether
/// the anchor changed.
pub(super) fn scroll_line_into_view(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    line_count: usize,
    viewport_h: f32,
    line: usize,
) -> bool {
    if viewport_h <= 0.0 || line_count == 0 {
        return false;
    }
    let hidden = state.cache.hidden().to_vec();
    let line = snap_visible(&hidden, line.min(line_count - 1), line_count);
    let line_height = state.cache.line_height(ui, data, line).max(0.0);
    let fallback = state.cache.row_height(ui, data);
    let target_top = state.cache.prefix_height(line, line_count, fallback);
    let target_bottom = target_top + line_height;
    let current = {
        let prefix = |l| state.cache.prefix_height(l, line_count, fallback);
        state.anchor.abs_y(prefix, line_count)
    };
    let margin = 4.0f32.min(viewport_h * 0.25);
    let target_abs = if target_top < current + margin {
        target_top - margin
    } else if target_bottom > current + viewport_h - margin {
        target_bottom + margin - viewport_h
    } else {
        return false;
    };
    set_anchor_abs(state, ui, data, line_count, target_abs, viewport_h);
    true
}

/// Frames a queued jump's scroll re-asserts before it gives up.
const SCROLL_SETTLE_MAX_FRAMES: u8 = 8;

/// Re-asserts a queued jump's scroll-into-view once the frame's painted slice is
/// known.
///
/// A jump's scroll estimate uses fallback row heights for lines that have not
/// been measured yet, so wrapped lines make it too short and the target can land
/// just outside the viewport. When the settle line is not in `visible`, shape it
/// and place it at the viewport top (which puts it in the slice independently of
/// how the lines above it later measure). Returns whether the anchor changed.
pub(super) fn settle_pending_scroll(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    line_count: usize,
    viewport_h: f32,
    visible: &[(usize, f32, u32)],
) -> bool {
    let Some(target) = state.pending_scroll_settle else {
        return false;
    };
    if visible.iter().any(|(line, _, _)| *line == target) {
        state.pending_scroll_settle = None;
        state.scroll_settle_frames = 0;
        return false;
    }
    if line_count == 0 || state.scroll_settle_frames >= SCROLL_SETTLE_MAX_FRAMES {
        state.pending_scroll_settle = None;
        state.scroll_settle_frames = 0;
        return false;
    }
    state.scroll_settle_frames += 1;
    let target = target.min(line_count - 1);
    let target_top = {
        let _ = state.cache.line_height(ui, data, target);
        let fallback = state.cache.row_height(ui, data);
        state.cache.prefix_height(target, line_count, fallback)
    };
    set_anchor_abs(state, ui, data, line_count, target_top, viewport_h);
    true
}

/// Scrolls so the caret is visible: the slot-accurate path first, then the
/// line-based fallback for a caret on a line that is not currently painted
/// (a jump or page move). Returns whether the anchor changed.
#[allow(clippy::too_many_arguments)]
pub(super) fn scroll_caret_visible(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    line_count: usize,
    viewport_rect: Rect,
    viewport_h: f32,
    slots: &[super::hit::LineSlot],
    from_input: bool,
) -> bool {
    let caret = state.caret();
    super::caret::scroll_caret_into_view(
        ui,
        state,
        data,
        line_count,
        viewport_rect,
        viewport_h,
        slots,
        from_input,
    ) || scroll_line_into_view(
        state,
        ui,
        data,
        line_count,
        viewport_h,
        data.buffer.line_of_char(caret),
    )
}

/// Caret x in galley space for a line-local char `local`: exact from the shaped
/// galley for a normal line, or the monospace estimate for a caret outside a
/// windowed long line's shaped window (matching [`window::window_for`] placement,
/// so the caret stays consistent with the horizontal extent).
pub(super) fn caret_x(cached: &CachedLine, local: usize, advance: f32) -> f32 {
    if !cached.windowed {
        return cached
            .galley
            .pos_from_cursor(egui::text::CCursor::new(local))
            .center()
            .x;
    }
    if local < cached.window_start || local > cached.window_start + cached.window_len {
        return local as f32 * advance;
    }
    let inner = cached
        .galley
        .pos_from_cursor(egui::text::CCursor::new(local - cached.window_start))
        .center()
        .x;
    cached.window_start as f32 * advance + inner
}

/// Scrolls `h_offset` so the caret at `caret` is inside the horizontal viewport
/// with a small margin, clamped to the shared per-frame maximum. Applies whenever
/// soft wrapping does not already handle the line (wrap-off, or any long line).
/// Returns whether `h_offset` changed.
pub(super) fn scroll_caret_horizontal(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    caret: usize,
) -> bool {
    let viewport_width = state.h_viewport_width;
    if viewport_width <= 0.0 {
        return false;
    }
    let buffer = data.buffer;
    let line = buffer.line_of_char(caret.min(buffer.len_chars()));
    let local = caret.saturating_sub(buffer.line_start(line));
    let advance = window::advance_estimate(ui, data);
    let (wrapped, x) = {
        let cached = state.cache.line_galley(ui, data, line);
        let wrapped = !cached.windowed && cached.rows > 1;
        (wrapped, caret_x(cached, local, advance))
    };
    if wrapped {
        return false;
    }
    let margin = 4.0f32;
    let target = if x < state.h_offset + margin {
        (x - margin).max(0.0)
    } else if x + advance > state.h_offset + viewport_width - margin {
        x + advance + margin - viewport_width
    } else {
        return false;
    };
    let target = target.clamp(0.0, state.max_h_offset);
    if (target - state.h_offset).abs() < 0.5 {
        return false;
    }
    state.h_offset = target;
    true
}

/// Logical line whose painted band contains `local_y` (viewport-relative), used
/// to resolve a gutter click. Lines outside the visible slice yield `None`.
pub(super) fn line_at_viewport_y(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    visible: &[(usize, f32, u32)],
    local_y: f32,
) -> Option<usize> {
    for (line, y, _) in visible {
        let height = state.cache.line_height(ui, data, *line);
        if local_y >= *y && local_y < *y + height {
            return Some(*line);
        }
    }
    None
}

/// Selects the logical line under a gutter click and scrolls it into view.
/// Returns whether the anchor changed (the caller must rebuild the layout).
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_gutter_click(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    line_count: usize,
    viewport_rect: Rect,
    viewport_h: f32,
    visible: &[(usize, f32, u32)],
    pointer_y: f32,
    now: f64,
) -> bool {
    let Some(line) = line_at_viewport_y(state, ui, data, visible, pointer_y - viewport_rect.top())
    else {
        return false;
    };
    state
        .selection
        .borrow_mut()
        .select_line_at(data.buffer, line);
    // The gutter click moved the caret, so drop the remembered vertical x the
    // same way a text-area click does (see `caret::interact_frame`).
    state.set_h_pos(None);
    state.blink_origin = now;
    scroll_line_into_view(state, ui, data, line_count, viewport_h, line)
}

/// Offset from a line's top to the vertical center of its gutter number.
///
/// A wrapped line is centered on its first visual row; an empty line has a
/// zero-height row, so it falls back to the font row height. Both the first
/// row and the fallback are pixel-rounded, so a truly empty line and a
/// whitespace-only line land on the identical center.
pub(super) fn gutter_number_center(cached: &CachedLine, row_height: f32, ppp: f32) -> f32 {
    let first = cached.galley.rows.first().map_or(0.0, |row| row.size.y);
    let base = if first > 0.0 { first } else { row_height };
    (base * ppp).round() / ppp * 0.5
}

/// Right-edge x for the gutter label (a line number, or the folded ellipsis).
pub(super) fn number_x(gutter_right: f32) -> f32 {
    gutter_right - GUTTER_RIGHT_PAD
}

/// The gutter's right-aligned label for a logical line. A collapsed fold shows
/// three dots instead of its start line's number and a separate count badge: at
/// narrow widths the badge squeezed the number into an unreadable sliver, and
/// the chevron already communicates that the range is foldable.
fn gutter_label(line: usize, folded: bool) -> String {
    if folded {
        "...".to_owned()
    } else {
        (line + 1).to_string()
    }
}

/// Paints the gutter line numbers for `visible` and the separator hairline.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_gutter(
    ui: &egui::Ui,
    gutter_rect: Rect,
    viewport_rect: Rect,
    visible: &[(usize, f32, u32)],
    cache: &mut LineCache,
    data: &LineData,
    font_size: f32,
    // `(start line, collapsed, hidden lines)` per fold candidate; only markers
    // whose start line is painted get a chevron.
    markers: &[(usize, bool, usize)],
    // Logical lines with a bookmark; only painted lines get an icon.
    bookmarked: &[usize],
) {
    let clip = gutter_rect.intersect(ui.clip_rect());
    if clip.width() <= 0.0 || clip.height() <= 0.0 {
        return;
    }
    let painter = ui.painter().with_clip_rect(clip);
    let font_id = FontId::new(font_size * 0.85, FontFamily::Monospace);
    let color = ui.visuals().weak_text_color();
    let row_height = cache.row_height(ui, data);
    let ppp = ui.ctx().pixels_per_point();
    for (line, y, _rows) in visible {
        let cached = cache.line_galley(ui, data, *line);
        let center_y = viewport_rect.top() + y + gutter_number_center(cached, row_height, ppp);
        let mut folded = false;
        if let Some((_, collapsed, _)) = markers
            .iter()
            .find(|(marker_line, _, _)| marker_line == line)
        {
            folded = *collapsed;
            let band_center = pos2(gutter_rect.left() + FOLD_CHEVRON_W * 0.5, center_y);
            typ_edit_widgets::icons::chevron(
                ui,
                Rect::from_center_size(band_center, vec2(FOLD_CHEVRON_W, row_height)),
                !collapsed,
                color,
            );
        }
        if bookmarked.contains(line) {
            let center = pos2(
                gutter_rect.left() + FOLD_CHEVRON_W + (FOLD_BAND_W - FOLD_CHEVRON_W) * 0.5,
                center_y,
            );
            typ_edit_widgets::icons::bookmark(
                ui,
                Rect::from_center_size(center, vec2(FOLD_CHEVRON_W, FOLD_CHEVRON_W)),
                true,
                typ_edit_widgets::theme::Theme::from_ui(ui).colors.accent,
            );
        }
        painter.text(
            pos2(number_x(gutter_rect.right()), center_y),
            Align2::RIGHT_CENTER,
            gutter_label(*line, folded),
            font_id.clone(),
            color,
        );
    }

    let separator = if ui.visuals().dark_mode {
        Color32::from_white_alpha(typ_edit_widgets::tokens::HAIRLINE_ALPHA)
    } else {
        Color32::from_black_alpha(typ_edit_widgets::tokens::HAIRLINE_ALPHA)
    };
    ui.painter().line_segment(
        [
            pos2(gutter_rect.right(), clip.top()),
            pos2(gutter_rect.right(), clip.bottom()),
        ],
        egui::Stroke::new(1.0, separator),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let chain = fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        fonts
            .families
            .insert(egui::FontFamily::Name("sans".into()), chain);
        ctx.set_fonts(fonts);
        ctx
    }

    fn data<'a>(buffer: &'a typ_edit_buffer::TextBuffer) -> LineData<'a> {
        LineData {
            buffer,
            text_rev: 1,
            parse_generation: 0,
            highlights: &[],
            blocked_ranges: &[],
            code_ranges: &[],
            math_ranges: &[],
            math_atoms: &[],
            font_family: "sans",
            font_size: 14.0,
            wrap_mode: crate::WrapMode::Off,
            line_spacing: 1.0,
            wrap_line_spacing: 1.0,
            wrap_width: f32::INFINITY,
        }
    }

    #[test]
    fn gutter_label_replaces_a_folded_number_with_three_dots() {
        assert_eq!(number_x(80.0), 80.0 - GUTTER_RIGHT_PAD);
        assert_eq!(gutter_label(0, false), "1");
        assert_eq!(gutter_label(41, false), "42");
        assert_eq!(gutter_label(41, true), "...");
    }

    #[test]
    fn gutter_number_center_is_identical_for_empty_and_whitespace_lines() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("a\n\n \nb");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            for line_spacing in [1.0f32, 1.5] {
                let data = LineData {
                    line_spacing,
                    ..data(&buffer)
                };
                let row_height = cache.row_height(ui, &data);
                let ppp = ui.ctx().pixels_per_point();
                let mut center = |line| {
                    gutter_number_center(cache.line_galley(ui, &data, line), row_height, ppp)
                };

                let content = center(0);
                let empty = center(1);
                let whitespace = center(2);
                assert!(
                    content > 0.0,
                    "a content row's number must sit below its top"
                );
                assert!(
                    (empty - whitespace).abs() < 0.01,
                    "spacing {line_spacing}: empty {empty} vs whitespace {whitespace} must align"
                );
                assert!(
                    (empty - content).abs() < 0.01,
                    "spacing {line_spacing}: empty {empty} vs content {content} must align"
                );
            }
        })
        .drop_without_applying_deltas();
    }
}
