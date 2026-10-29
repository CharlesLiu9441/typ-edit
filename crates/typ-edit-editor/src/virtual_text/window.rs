//! Horizontal window math for pathological long lines.
//!
//! A logical line longer than [`LONG_LINE_CHARS`] is never shaped whole: the
//! cache shapes only the visible horizontal range plus a margin, as one
//! unwrapped row, so a multi-megabyte line cannot stall a frame. Placement uses
//! a monospace advance estimate, so mixed-width (CJK) content in such a line is
//! approximate.

use std::sync::Arc;

use super::layout::LineData;

/// A logical line with more chars than this is shaped through a window.
pub const LONG_LINE_CHARS: usize = 20_000;
/// Extra chars shaped on both sides of the visible horizontal range.
pub const WINDOW_MARGIN_CHARS: usize = 2_000;
/// Chars shaped in one frame before later lines are skipped (safety net).
pub const SHAPING_BUDGET_CHARS: usize = 128_000;

/// A shaped char window within one logical line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    pub start: usize,
    pub len: usize,
}

impl Window {
    /// One past the last char.
    pub const fn end(self) -> usize {
        self.start + self.len
    }

    /// Whether `self` fully contains `other`.
    pub const fn covers(self, other: Window) -> bool {
        self.start <= other.start && self.end() >= other.end()
    }
}

impl Window {
    /// The whole line as a window.
    pub const fn whole(len: usize) -> Self {
        Self { start: 0, len }
    }
}

/// Monospace advance estimate in points for the configured font.
pub fn advance_estimate(ui: &egui::Ui, data: &LineData) -> f32 {
    let font_id = egui::FontId::new(
        data.font_size,
        egui::FontFamily::Name(Arc::from(data.font_family)),
    );
    ui.fonts_mut(|fonts| fonts.glyph_width(&font_id, 'M'))
        .max(1.0)
}

/// The window covering the visible range under `h_offset` plus margins.
///
/// Short lines return their whole range; long lines return a sub-range clamped
/// to the line. The widget clamps `h_offset` to the visible extent before
/// calling, so the returned window is never empty for a non-empty line.
pub fn window_for(line_len: usize, h_offset: f32, viewport_width: f32, advance: f32) -> Window {
    if line_len <= LONG_LINE_CHARS {
        return Window::whole(line_len);
    }
    let advance = advance.max(1.0);
    let first = (h_offset.max(0.0) / advance) as usize;
    let visible = (viewport_width.max(0.0) / advance).ceil() as usize + 1;
    let start = first.saturating_sub(WINDOW_MARGIN_CHARS).min(line_len);
    let end = first
        .saturating_add(visible)
        .saturating_add(WINDOW_MARGIN_CHARS)
        .min(line_len);
    Window {
        start,
        len: end.saturating_sub(start),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_lines_keep_their_whole_range() {
        assert_eq!(window_for(100, 5_000.0, 800.0, 8.0), Window::whole(100));
        assert_eq!(
            window_for(LONG_LINE_CHARS, 0.0, 800.0, 8.0),
            Window::whole(LONG_LINE_CHARS)
        );
    }

    #[test]
    fn long_lines_shape_a_bounded_margin_around_the_visible_range() {
        let window = window_for(1_000_000, 0.0, 800.0, 8.0);
        assert_eq!(window.start, 0);
        assert!(
            window.len < 10_000,
            "the window must be bounded, got {}",
            window.len
        );
        assert!(window.len > WINDOW_MARGIN_CHARS);
    }

    #[test]
    fn a_shifted_offset_moves_the_window_and_clamps_at_the_end() {
        let window = window_for(1_000_000, 400_000.0, 800.0, 8.0);
        assert_eq!(window.start, 50_000 - WINDOW_MARGIN_CHARS);
        let past_end = window_for(100_000, 10_000_000.0, 800.0, 8.0);
        assert_eq!(past_end.start, 100_000);
        assert_eq!(past_end.len, 0);
    }

    #[test]
    fn covers_is_inclusive() {
        let outer = Window {
            start: 10,
            len: 100,
        };
        assert!(outer.covers(Window { start: 20, len: 50 }));
        assert!(outer.covers(Window {
            start: 10,
            len: 100
        }));
        assert!(!outer.covers(Window { start: 5, len: 50 }));
        assert!(!outer.covers(Window { start: 90, len: 50 }));
    }
}
