//! Wheel input, the custom scrollbars, anchor normalization and the visible-line
//! computation for the virtualized text path.
//!
//! Extracted from `render` so the frame orchestrator stays readable; the wheel,
//! vertical scrollbar and normalization are the Stage A-C scroll model, plus the
//! horizontal bar and the shift-wheel/two-finger touch interaction.

use egui::{Rect, Sense, Vec2, pos2, vec2};

use super::VirtualTextState;
use super::chrome;
use super::layout::LineData;
use super::touch;
use crate::WrapMode;

/// Width of a custom scrollbar, in points.
const SCROLLBAR_WIDTH: f32 = 12.0;
/// Painted thickness of an idle (not hovered/dragged) scrollbar, in points.
const SCROLLBAR_THIN: f32 = 4.0;
/// Inset of the full-width thumb from the bar's edge, in points.
const SCROLLBAR_INSET: f32 = 1.0;
/// Minimum scrollbar thumb length, in points.
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// Height reserved at the bottom of the text viewport for the horizontal bar.
const HSCROLLBAR_HEIGHT: f32 = 12.0;

/// Painted cross-axis size for an animated hover factor `grow` in `0..=1`:
/// thin and flush when idle, full and inset when hovered/dragged. The reserved
/// layout space stays [`SCROLLBAR_WIDTH`] regardless (the interaction strip is
/// always full width), so hovering never reflows the text.
pub(super) fn bar_width(grow: f32) -> f32 {
    SCROLLBAR_THIN + (SCROLLBAR_WIDTH - SCROLLBAR_THIN) * grow.clamp(0.0, 1.0)
}

/// Painted thumb inset from the bar's edge for `grow`: flush when idle, the
/// usual [`SCROLLBAR_INSET`] when fully grown.
fn bar_inset(grow: f32) -> f32 {
    SCROLLBAR_INSET * grow.clamp(0.0, 1.0)
}

/// Track/thumb geometry plus the interaction response for one frame.
pub(super) struct Scrollbar {
    pub bar_rect: Rect,
    pub thumb_rect: Rect,
    pub response: egui::Response,
    /// Animated idle→hover factor for the paint pass (0 thin, 1 full).
    pub grow: f32,
}

/// The horizontal bar, drawn only when unwrapped content overflows.
pub(super) struct HScrollbar {
    pub bar_rect: Rect,
    pub thumb_rect: Rect,
    pub response: egui::Response,
    /// Animated idle→hover factor for the paint pass (0 thin, 1 full).
    pub grow: f32,
}

/// Per-frame scroll inputs that do not fit in a natural small tuple.
pub(super) struct ScrollFrame<'a> {
    pub data: &'a LineData<'a>,
    pub line_count: usize,
    pub viewport_rect: Rect,
    pub viewport_h: f32,
    pub fallback: f32,
    pub hovered: bool,
    pub wrap_mode: WrapMode,
    pub serial: u64,
    pub now: f64,
}

/// One frame's scroll result: the scrollbar geometry plus the lines to paint.
pub(super) struct ScrollOutcome {
    pub visible: Vec<(usize, f32, u32)>,
    pub scrollbar: Scrollbar,
    pub hscrollbar: Option<HScrollbar>,
}

/// Applies the wheel/touch, runs the scrollbar interactions, normalizes the
/// anchor and returns the visible lines.
pub(super) fn run_scroll(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    frame: &ScrollFrame<'_>,
) -> ScrollOutcome {
    state.cache.begin_frame();
    // A wider viewport or a shorter buffer can invalidate last frame's maximum;
    // pull `h_offset` in before the wheel/touch pass consumes it.
    state.h_offset = state.h_offset.clamp(0.0, state.max_h_offset);
    wheel(state, ui, frame);
    touch::apply(
        state,
        ui,
        frame.data,
        frame.viewport_rect,
        frame.wrap_mode,
        frame.line_count,
        frame.viewport_h,
        frame.now,
    );
    state
        .cache
        .set_horizontal(state.h_offset, frame.viewport_rect.width());

    // The horizontal bar's presence depends on the extent of the visible lines
    // that do not wrap, so probe the full-height set first. Lowering the
    // viewport by the bar height only drops trailing lines, and a long line's
    // height is independent of `h_offset`, so the extent stays valid.
    let probe = state.cache.visible_lines(
        ui,
        frame.data,
        &state.anchor,
        frame.viewport_h,
        frame.fallback,
    );
    let track_w = (frame.viewport_rect.width() - SCROLLBAR_WIDTH).max(0.0);
    let extent = state.cache.horizontal_extent(ui, frame.data, &probe);
    // The one maximum every horizontal mutation clamps to this frame. Its width
    // is the same `track_w` the scrollbar and caret use, so all three agree.
    state.h_viewport_width = track_w;
    state.max_h_offset = (extent - track_w).max(0.0);
    state.h_offset = state.h_offset.clamp(0.0, state.max_h_offset);
    let hscrollbar = h_scrollbar(state, ui, frame, extent, track_w);
    if hscrollbar.is_none() {
        state.dragging_hscrollbar = false;
    }
    state
        .cache
        .set_horizontal(state.h_offset, frame.viewport_rect.width());
    let text_h =
        (frame.viewport_h - hscrollbar.as_ref().map_or(0.0, |_| HSCROLLBAR_HEIGHT)).max(0.0);

    let scrollbar = scrollbar(state, ui, frame, text_h);
    normalize(state, ui, frame);
    let visible = state
        .cache
        .visible_lines(ui, frame.data, &state.anchor, text_h, frame.fallback);
    ScrollOutcome {
        visible,
        scrollbar,
        hscrollbar,
    }
}

/// Splits one frame's smoothed scroll delta into the per-axis part this widget
/// consumes and the residual left for any outer scroll area.
///
/// This mirrors egui's `ScrollArea`: only the axes the widget actually moves are
/// zeroed (`take_x` / `take_y`), so an unconsumed axis still reaches a parent.
pub(super) fn split_axes(wheel: Vec2, take_x: bool, take_y: bool) -> (Vec2, Vec2) {
    let taken = vec2(
        if take_x { wheel.x } else { 0.0 },
        if take_y { wheel.y } else { 0.0 },
    );
    (taken, wheel - taken)
}

/// Whether the frame must request an immediate repaint to keep draining the
/// smoothed delta: true while any scroll is applied or remains. egui only knows
/// to repaint while its own unprocessed residual is non-zero, so the widget must
/// drive the frames itself for the residual it leaves for a parent (or while its
/// applied scroll is still settling).
pub(super) fn needs_repaint(taken: Vec2, residual: Vec2) -> bool {
    taken != Vec2::ZERO || residual != Vec2::ZERO
}

/// Consumes the wheel only when the pointer hovers the viewport, so an outer
/// scroll area still receives it everywhere else.
///
/// Shift folds the vertical wheel into the horizontal axis (egui's default
/// `horizontal_scroll_modifier` is Shift), so `x` normally carries it and `y`
/// is zero; the `-wheel.y` fallback covers a backend that delivers a raw vertical
/// wheel. The sign matches the plain horizontal wheel: a positive content delta
/// pans the content right/down and so decreases `h_offset`.
///
/// Only the consumed axes are cleared from `smooth_scroll_delta`, and an
/// immediate repaint is requested while the delta is being drained. The first
/// subsequent idle frame normalizes the anchor and lets layout settle.
fn wheel(state: &mut VirtualTextState, ui: &egui::Ui, frame: &ScrollFrame<'_>) {
    if !frame.hovered || state.dragging_scrollbar || state.dragging_hscrollbar {
        state.wheel_resampler.reset();
        return;
    }

    let source = ui.input(|i| i.smooth_scroll_delta());
    let horizontal = !matches!(frame.wrap_mode, WrapMode::Always);
    let step =
        state
            .wheel_resampler
            .step(ui, source, vec2(if horizontal { 1.0 } else { 0.0 }, 1.0));
    let scrolling = ui.input(|i| i.is_scrolling());
    if state.wheel_scrolling && !scrolling && !step.active {
        state.wheel_scrolling = false;
    }
    if scrolling || step.active {
        state.wheel_scrolling = true;
    }
    if state.wheel_scrolling {
        // Keep the frame loop alive through zero-delta smoothing frames and
        // through the resampler's pending trackpad scroll.
        ui.ctx().request_repaint();
    }

    let shift = ui.input(|i| i.modifiers.shift);
    let (taken, _) = split_axes(step.delta, horizontal, true);
    let (_, source_residual) = split_axes(source, horizontal, true);
    if taken == Vec2::ZERO && source == Vec2::ZERO {
        return;
    }
    if shift && horizontal {
        state.h_offset =
            (state.h_offset - step.delta.x - step.delta.y).clamp(0.0, state.max_h_offset);
    } else {
        chrome::scroll_vertical(
            state,
            ui,
            frame.data,
            frame.line_count,
            frame.viewport_h,
            -taken.y,
        );
        if horizontal {
            state.h_offset = (state.h_offset - taken.x).clamp(0.0, state.max_h_offset);
        }
    }

    state.momentum = None;
    state.last_scroll_time = frame.now;
    // Consume the whole source, not only the part released this frame: the
    // remainder is held in `step` and must not be seen by a parent scroll area.
    ui.input_mut(|i| i.smooth_scroll_delta = source_residual);
    if needs_repaint(taken, source_residual) || step.active {
        ui.ctx().request_repaint();
    }
}

/// Builds the horizontal bar when unwrapped content overflows the text width and
/// applies its track-click/drag scrolling. `Always` never shows one.
fn h_scrollbar(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    frame: &ScrollFrame<'_>,
    extent: f32,
    track_w: f32,
) -> Option<HScrollbar> {
    if matches!(frame.wrap_mode, WrapMode::Always) || track_w <= 0.0 || extent <= track_w {
        return None;
    }
    let bar_rect = Rect::from_min_size(
        pos2(
            frame.viewport_rect.left(),
            frame.viewport_rect.bottom() - HSCROLLBAR_HEIGHT,
        ),
        vec2(track_w, HSCROLLBAR_HEIGHT),
    );
    let response = ui.interact(
        bar_rect,
        egui::Id::new((crate::SCROLL_ID, frame.serial, "vt-hscrollbar")),
        Sense::click_and_drag(),
    );
    let grow = ui.ctx().animate_bool(
        egui::Id::new((crate::SCROLL_ID, frame.serial, "vt-hscrollbar-grow")),
        response.hovered() || response.dragged(),
    );
    let max_abs = state.max_h_offset;
    let thumb_w = (track_w / extent * track_w).clamp(SCROLLBAR_MIN_THUMB.min(track_w), track_w);
    let travel = (track_w - thumb_w).max(0.0);
    let thumb_x = bar_rect.left()
        + if max_abs > 0.0 {
            (state.h_offset / max_abs).clamp(0.0, 1.0) * travel
        } else {
            0.0
        };
    let thumb_rect = Rect::from_min_size(
        pos2(thumb_x, bar_rect.top() + 1.0),
        vec2(thumb_w, (HSCROLLBAR_HEIGHT - 2.0).max(0.0)),
    );

    let mut target = state.h_offset;
    if response.dragged() {
        target += response.drag_delta().x / travel.max(1.0) * max_abs;
        state.dragging_hscrollbar = true;
        state.last_scroll_time = frame.now;
    } else {
        state.dragging_hscrollbar = false;
        if response.clicked()
            && let Some(pointer) = response.interact_pointer_pos()
            && !thumb_rect.contains(pointer)
        {
            let fraction =
                ((pointer.x - bar_rect.left() - thumb_w * 0.5) / travel.max(1.0)).clamp(0.0, 1.0);
            target = fraction * max_abs;
            state.last_scroll_time = frame.now;
        }
    }
    state.h_offset = target.clamp(0.0, max_abs);
    Some(HScrollbar {
        bar_rect,
        thumb_rect,
        response,
        grow,
    })
}

/// Draws the vertical scrollbar track/thumb geometry and applies drag/click
/// scrolling within a text viewport `track_h` points tall.
fn scrollbar(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    frame: &ScrollFrame<'_>,
    track_h: f32,
) -> Scrollbar {
    let viewport_rect = frame.viewport_rect;
    let bar_rect = Rect::from_min_size(
        pos2(viewport_rect.right() - SCROLLBAR_WIDTH, viewport_rect.top()),
        vec2(SCROLLBAR_WIDTH, track_h),
    );
    let bar = ui.interact(
        bar_rect,
        egui::Id::new((crate::SCROLL_ID, frame.serial, "vt-scrollbar")),
        Sense::click_and_drag(),
    );
    let grow = ui.ctx().animate_bool(
        egui::Id::new((crate::SCROLL_ID, frame.serial, "vt-scrollbar-grow")),
        bar.hovered() || bar.dragged(),
    );

    let total = state.cache.total_height(frame.line_count, frame.fallback);
    let content_h = (total + track_h).max(1.0);
    let max_abs = (content_h - track_h).max(0.0);
    let thumb_h = (track_h / content_h * track_h).clamp(SCROLLBAR_MIN_THUMB.min(track_h), track_h);
    let travel = (track_h - thumb_h).max(0.0);
    let current_abs = chrome::anchor_abs(state, ui, frame.data, frame.line_count);
    let thumb_y = bar_rect.top()
        + if max_abs > 0.0 {
            (current_abs / max_abs).clamp(0.0, 1.0) * travel
        } else {
            0.0
        };
    let thumb_rect = Rect::from_min_size(
        pos2(bar_rect.left() + 1.0, thumb_y),
        vec2(SCROLLBAR_WIDTH - 2.0, thumb_h),
    );

    let mut target_abs = current_abs;
    if bar.dragged() {
        target_abs += bar.drag_delta().y / travel.max(1.0) * max_abs;
        state.dragging_scrollbar = true;
        state.last_scroll_time = frame.now;
    } else {
        state.dragging_scrollbar = false;
        if bar.clicked()
            && let Some(pointer) = bar.interact_pointer_pos()
            && !thumb_rect.contains(pointer)
        {
            let fraction =
                ((pointer.y - bar_rect.top() - thumb_h * 0.5) / travel.max(1.0)).clamp(0.0, 1.0);
            target_abs = fraction * max_abs;
            state.last_scroll_time = frame.now;
        }
    }
    if (target_abs - current_abs).abs() > f32::EPSILON {
        chrome::set_anchor_abs(state, ui, frame.data, frame.line_count, target_abs, track_h);
    }

    Scrollbar {
        bar_rect,
        thumb_rect,
        response: bar,
        grow,
    }
}

/// Whether this frame is a scroll-idle frame that should normalize the anchor.
fn should_normalize(
    dragging_vertical: bool,
    dragging_horizontal: bool,
    has_momentum: bool,
    idle_this_frame: bool,
) -> bool {
    idle_this_frame && !dragging_vertical && !dragging_horizontal && !has_momentum
}

/// Normalizes the anchor as soon as neither a scrollbar drag nor an active
/// coast owns the scroll, and immediately when the buffer shrank under it.
///
/// The old implementation waited a fixed 120 ms after the last scroll. That
/// timer outlived the inertia and made the post-scroll layout wait for a
/// scheduled wake-up (or the next pointer move) before it settled.
fn normalize(state: &mut VirtualTextState, ui: &egui::Ui, frame: &ScrollFrame<'_>) {
    let idle_this_frame = frame.now > state.last_scroll_time;
    if should_normalize(
        state.dragging_scrollbar,
        state.dragging_hscrollbar,
        state.momentum.is_some(),
        idle_this_frame,
    ) || state.anchor.top_line >= frame.line_count
    {
        let hidden = state.cache.hidden().to_vec();
        let mut heights = |line| state.cache.line_height(ui, frame.data, line);
        state
            .anchor
            .normalize(&mut heights, frame.line_count, &hidden);
    }
}

/// Paints the vertical scrollbar: a thin strip flush to the right edge when
/// idle, growing to the full inset track on hover/drag.
pub(super) fn paint(ui: &egui::Ui, clip: Rect, bar: &Scrollbar) {
    let (track, thumb) = grow_vertical(bar.bar_rect, bar.thumb_rect, bar.grow);
    paint_bar(ui, clip, bar.bar_rect, track, thumb, &bar.response);
}

/// Paints the horizontal scrollbar with the same idle/hover thickness rule.
pub(super) fn paint_horizontal(ui: &egui::Ui, clip: Rect, bar: &HScrollbar) {
    let (track, thumb) = grow_horizontal(bar.bar_rect, bar.thumb_rect, bar.grow);
    paint_bar(ui, clip, bar.bar_rect, track, thumb, &bar.response);
}

/// Grows the vertical track/thumb from a thin flush strip to the full inset
/// track; `full` is the interaction rect, which never changes (so the text
/// layout reserved for the bar is constant across hover states).
fn grow_vertical(full: Rect, thumb: Rect, grow: f32) -> (Rect, Rect) {
    let width = bar_width(grow);
    let inset = bar_inset(grow);
    let track = Rect::from_min_max(pos2(full.right() - width, full.top()), full.max);
    let thumb = Rect::from_min_max(
        pos2(full.right() - width + inset, thumb.top()),
        pos2(full.right() - inset, thumb.bottom()),
    );
    (track, thumb)
}

/// The horizontal counterpart of [`grow_vertical`].
fn grow_horizontal(full: Rect, thumb: Rect, grow: f32) -> (Rect, Rect) {
    let width = bar_width(grow);
    let inset = bar_inset(grow);
    let track = Rect::from_min_max(pos2(full.left(), full.bottom() - width), full.max);
    let thumb = Rect::from_min_max(
        pos2(thumb.left(), full.bottom() - width + inset),
        pos2(thumb.right(), full.bottom() - inset),
    );
    (track, thumb)
}

fn paint_bar(
    ui: &egui::Ui,
    clip: Rect,
    interaction: Rect,
    track: Rect,
    thumb: Rect,
    response: &egui::Response,
) {
    let painter = ui.painter().with_clip_rect(clip.union(interaction));
    let track_color = ui.visuals().widgets.inactive.bg_fill.gamma_multiply(0.35);
    let thumb_color = if response.hovered() || response.dragged() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        ui.visuals().widgets.active.bg_fill
    };
    painter.rect_filled(track, 2.0, track_color);
    painter.rect_filled(thumb, 2.0, thumb_color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_axes_keeps_an_unconsumed_axis_for_the_parent() {
        let wheel = vec2(3.0, -7.0);
        let (taken, residual) = split_axes(wheel, false, true);
        assert_eq!(taken, vec2(0.0, -7.0));
        assert_eq!(
            residual,
            vec2(3.0, 0.0),
            "horizontal is left for the parent"
        );
    }

    #[test]
    fn split_axes_takes_both_axes_when_horizontal_is_enabled() {
        let wheel = vec2(3.0, -7.0);
        let (taken, residual) = split_axes(wheel, true, true);
        assert_eq!(taken, wheel);
        assert_eq!(residual, Vec2::ZERO);
    }

    #[test]
    fn needs_repaint_while_anything_is_applied_or_left() {
        assert!(needs_repaint(vec2(0.0, -7.0), Vec2::ZERO));
        assert!(needs_repaint(Vec2::ZERO, vec2(3.0, 0.0)));
        assert!(!needs_repaint(Vec2::ZERO, Vec2::ZERO));
    }

    #[test]
    fn normalize_wait_is_only_until_scrolling_and_coasting_stop() {
        assert!(should_normalize(false, false, false, true));
        assert!(
            !should_normalize(false, false, true, true),
            "an active coast owns the scroll"
        );
        assert!(
            !should_normalize(false, false, false, false),
            "the frame that scrolled must not normalize"
        );
        assert!(!should_normalize(true, false, false, true));
        assert!(!should_normalize(false, true, false, true));
    }

    #[test]
    fn bar_width_interpolates_from_thin_to_full() {
        assert_eq!(bar_width(0.0), SCROLLBAR_THIN);
        assert_eq!(bar_width(1.0), SCROLLBAR_WIDTH);
        let half = bar_width(0.5);
        assert!(
            half > SCROLLBAR_THIN && half < SCROLLBAR_WIDTH,
            "got {half}"
        );
        assert_eq!(bar_width(-1.0), SCROLLBAR_THIN, "clamped low");
        assert_eq!(bar_width(2.0), SCROLLBAR_WIDTH, "clamped high");
    }
}
