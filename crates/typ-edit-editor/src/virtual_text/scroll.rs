//! Vertical scroll model for the virtualized text path.
//!
//! The position is `(top_line, offset)`: `top_line` is the first logical line
//! touching the top of the viewport and `offset` is how far (points) that line
//! has been pushed above it. `offset` is only ever normalized into
//! `[0, height(top_line))` while more lines follow, so the pair is a compact,
//! stable representation that survives as lines are measured lazily.

use std::ops::Range;

use super::folds::snap_visible;

/// Scroll position: first visible logical line plus its sub-line pixel offset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollAnchor {
    /// Index of the logical line at the top of the viewport.
    pub top_line: usize,
    /// How far `top_line` is pushed above the viewport, in points.
    pub offset: f32,
}

impl ScrollAnchor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rewrites the anchor so `offset` lies in `[0, height(top_line))` (or
    /// `[0, height(last)]` on the final line), carrying whole lines between
    /// `top_line` and `offset` in both directions.
    ///
    /// Idempotent: normalizing an already-normalized anchor changes nothing.
    /// `line_count` may be 0 (empty buffer), which resets the anchor to `(0, 0)`.
    pub fn normalize(
        &mut self,
        mut height: impl FnMut(usize) -> f32,
        line_count: usize,
        hidden: &[Range<usize>],
    ) {
        if line_count == 0 {
            self.top_line = 0;
            self.offset = 0.0;
            return;
        }
        if self.top_line >= line_count {
            self.top_line = line_count - 1;
            self.offset = 0.0;
        }
        // Carry whole lines downward while the offset reaches the next line.
        while self.top_line + 1 < line_count {
            let h = height(self.top_line).max(0.0);
            if self.offset < h {
                break;
            }
            self.offset -= h;
            self.top_line += 1;
        }
        // Carry upward while the offset is above the current line's top.
        while self.offset < 0.0 && self.top_line > 0 {
            self.top_line -= 1;
            self.offset += height(self.top_line).max(0.0);
        }
        // A hidden line has zero height, so the walk can stop inside one; snapping
        // forward keeps the offset (hidden lines contribute nothing).
        self.top_line = snap_visible(hidden, self.top_line, line_count);
        let h = height(self.top_line).max(0.0);
        self.offset = self.offset.clamp(0.0, h);
    }

    /// Shifts the offset by `delta_px` and normalizes the result.
    pub fn scroll_by(
        &mut self,
        delta_px: f32,
        mut height: impl FnMut(usize) -> f32,
        line_count: usize,
        hidden: &[Range<usize>],
    ) {
        self.offset += delta_px;
        self.normalize(&mut height, line_count, hidden);
    }

    /// Prevents scrolling past the document end.
    ///
    /// The scrollable extent is the measured/estimated total text height with a
    /// bottom pad equal to the viewport height, so the last line can be scrolled
    /// to the top of the viewport.
    pub fn clamp_end(
        &mut self,
        total_height: f32,
        viewport_height: f32,
        prefix: impl Fn(usize) -> f32,
        line_count: usize,
        hidden: &[Range<usize>],
    ) {
        let pad = viewport_height.max(0.0);
        let max_abs = (total_height + pad - viewport_height).max(0.0);
        let current = self.abs_y(&prefix, line_count);
        if current > max_abs {
            self.set_abs_y_impl(max_abs, &prefix, line_count, hidden);
        } else if current < 0.0 {
            self.set_abs_y_impl(0.0, &prefix, line_count, hidden);
        }
    }

    /// Absolute scroll position in points: the summed heights of all lines
    /// above `top_line` plus `offset`. The `prefix` closure returns the sum of
    /// the (clamped) heights of `0..line`; it must be cheap — the layout layer
    /// answers it in O(log N).
    pub fn abs_y(&self, prefix: impl Fn(usize) -> f32, line_count: usize) -> f32 {
        prefix(self.top_line.min(line_count)) + self.offset
    }

    /// Inverse of [`Self::abs_y`]: positions the anchor at `target` points from
    /// the top. `target` past the document end leaves the last line selected
    /// with the excess in `offset`.
    pub fn set_abs_y(
        &mut self,
        target: f32,
        prefix: impl Fn(usize) -> f32,
        line_count: usize,
        hidden: &[Range<usize>],
    ) {
        self.set_abs_y_impl(target, &prefix, line_count, hidden);
    }

    /// Binary-searches the line whose top is the last one at or above `target`,
    /// so a far document position costs O(log N) prefix queries instead of a
    /// walk over every preceding line.
    fn set_abs_y_impl(
        &mut self,
        target: f32,
        prefix: &impl Fn(usize) -> f32,
        line_count: usize,
        hidden: &[Range<usize>],
    ) {
        if line_count == 0 {
            self.top_line = 0;
            self.offset = 0.0;
            return;
        }
        let target = target.max(0.0);
        let mut lo = 0usize;
        let mut hi = line_count;
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if prefix(mid) <= target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        // Hidden lines contribute no prefix height, so snapping forward leaves
        // `target - prefix` unchanged. The fallback (no following visible line)
        // still has `prefix <= target`, since the prefix never decreases.
        self.top_line = snap_visible(hidden, lo, line_count);
        self.offset = target - prefix(self.top_line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uniform(_: usize) -> f32 {
        10.0
    }

    fn variable(line: usize) -> f32 {
        10.0 + line as f32
    }

    fn uniform_prefix(line: usize) -> f32 {
        line as f32 * 10.0
    }

    #[test]
    fn normalize_carries_whole_lines_downwards() {
        let mut anchor = ScrollAnchor {
            top_line: 0,
            offset: 25.0,
        };
        anchor.normalize(uniform, 10, &[]);
        assert_eq!((anchor.top_line, anchor.offset), (2, 5.0));
    }

    #[test]
    fn normalize_carries_whole_lines_upwards() {
        let mut anchor = ScrollAnchor {
            top_line: 5,
            offset: -5.0,
        };
        anchor.normalize(uniform, 10, &[]);
        assert_eq!((anchor.top_line, anchor.offset), (4, 5.0));
    }

    #[test]
    fn normalize_carries_with_variable_heights() {
        // Heights: 10, 11, 12, 13, ... A 25pt offset from line 0 reaches line 2
        // (10 + 11 = 21 consumed, leaving 4).
        let mut anchor = ScrollAnchor {
            top_line: 0,
            offset: 25.0,
        };
        anchor.normalize(variable, 10, &[]);
        assert_eq!((anchor.top_line, anchor.offset), (2, 4.0));

        let mut up = ScrollAnchor {
            top_line: 3,
            offset: -3.0,
        };
        // Line 2 has height 12, so -3 from line 3 lands at line 2 + 9.
        up.normalize(variable, 10, &[]);
        assert_eq!((up.top_line, up.offset), (2, 9.0));
    }

    #[test]
    fn normalize_clamps_at_both_ends() {
        let mut above = ScrollAnchor {
            top_line: 0,
            offset: -20.0,
        };
        above.normalize(uniform, 10, &[]);
        assert_eq!((above.top_line, above.offset), (0, 0.0));

        let mut below = ScrollAnchor {
            top_line: 0,
            offset: 1_000.0,
        };
        below.normalize(uniform, 10, &[]);
        assert_eq!((below.top_line, below.offset), (9, 10.0));
    }

    #[test]
    fn normalize_is_idempotent() {
        let mut anchor = ScrollAnchor {
            top_line: 3,
            offset: 7.0,
        };
        anchor.normalize(variable, 20, &[]);
        let once = anchor;
        anchor.normalize(variable, 20, &[]);
        assert_eq!(anchor, once);
    }

    #[test]
    fn normalize_handles_an_empty_document() {
        let mut anchor = ScrollAnchor {
            top_line: 4,
            offset: 12.0,
        };
        anchor.normalize(uniform, 0, &[]);
        assert_eq!((anchor.top_line, anchor.offset), (0, 0.0));
    }

    #[test]
    fn scroll_by_carries_both_directions() {
        let mut anchor = ScrollAnchor::new();
        anchor.scroll_by(25.0, uniform, 10, &[]);
        assert_eq!((anchor.top_line, anchor.offset), (2, 5.0));
        anchor.scroll_by(-8.0, uniform, 10, &[]);
        assert_eq!((anchor.top_line, anchor.offset), (1, 7.0));
    }

    #[test]
    fn abs_and_set_round_trip() {
        let mut source = ScrollAnchor::new();
        let target = 37.0;
        source.set_abs_y(target, uniform_prefix, 10, &[]);
        assert_eq!(source.abs_y(uniform_prefix, 10), target);
        assert_eq!((source.top_line, source.offset), (3, 7.0));
    }

    #[test]
    fn prefix_based_abs_matches_the_naive_walk_with_variable_heights() {
        let height = |line: usize| 10.0 + line as f32;
        let prefix = |line: usize| (0..line).map(height).sum::<f32>();
        let mut anchor = ScrollAnchor::new();
        // The old walk consumes 10 + 11 + 12 = 33 and lands 4pt into line 3.
        anchor.set_abs_y(37.0, prefix, 10, &[]);
        assert_eq!((anchor.top_line, anchor.offset), (3, 4.0));
        assert_eq!(anchor.abs_y(prefix, 10), 37.0);
    }

    #[test]
    fn clamp_end_stops_at_the_document_end() {
        let mut anchor = ScrollAnchor::new();
        anchor.scroll_by(1_000.0, uniform, 5, &[]);
        // total = 50, viewport = 40: max abs = 50 (50 + 40 pad - 40).
        anchor.clamp_end(50.0, 40.0, uniform_prefix, 5, &[]);
        assert_eq!(anchor.abs_y(uniform_prefix, 5), 50.0);
    }

    #[test]
    fn clamp_end_leaves_short_documents_alone() {
        let mut anchor = ScrollAnchor::new();
        anchor.set_abs_y(5.0, uniform_prefix, 3, &[]);
        anchor.clamp_end(30.0, 100.0, uniform_prefix, 3, &[]);
        assert_eq!(anchor.abs_y(uniform_prefix, 3), 5.0);
    }

    #[test]
    fn clamp_end_pulls_a_manually_overscrolled_anchor_back() {
        let mut anchor = ScrollAnchor {
            top_line: 2,
            offset: 15.0,
        };
        anchor.clamp_end(30.0, 100.0, uniform_prefix, 3, &[]);
        assert_eq!(anchor.abs_y(uniform_prefix, 3), 30.0);
        assert_eq!((anchor.top_line, anchor.offset), (2, 10.0));
    }

    #[test]
    fn set_abs_y_queries_the_prefix_logarithmically() {
        let count = 1_000_000usize;
        let queries = std::cell::Cell::new(0usize);
        let prefix = |line: usize| {
            queries.set(queries.get() + 1);
            line as f32 * 10.0
        };
        let mut anchor = ScrollAnchor::new();
        anchor.set_abs_y(5_000_000.0, prefix, count, &[]);
        assert_eq!((anchor.top_line, anchor.offset), (500_000, 0.0));
        assert!(
            queries.get() <= 64,
            "{} prefix queries for {count} lines is not logarithmic",
            queries.get()
        );
    }

    #[test]
    fn normalize_and_set_abs_y_snap_onto_visible_lines() {
        let hidden = std::slice::from_ref(&(2..4));
        // Heights: 10pt each; hidden lines contribute 0 through the prefix.
        let prefix = |line: usize| {
            (0..line)
                .map(|index| {
                    if hidden[0].contains(&index) {
                        0.0
                    } else {
                        10.0
                    }
                })
                .sum::<f32>()
        };
        let height = |line: usize| {
            if hidden[0].contains(&line) { 0.0 } else { 10.0 }
        };
        let mut anchor = ScrollAnchor::new();
        anchor.set_abs_y(25.0, prefix, 6, hidden);
        assert_eq!(
            anchor.top_line, 4,
            "line 2 is hidden, so it snaps to line 4"
        );
        assert_eq!(anchor.offset, 5.0);

        let mut carrying = ScrollAnchor {
            top_line: 1,
            offset: 10.0,
        };
        carrying.normalize(height, 6, hidden);
        assert!(!hidden[0].contains(&carrying.top_line));
    }
}
