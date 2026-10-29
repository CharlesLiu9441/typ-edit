//! Pointer <-> text geometry for the virtualized text path (Stage B).
//!
//! The visible layout is captured as a list of [`LineSlot`]s: one per painted
//! logical line, each carrying its screen origin and a cheap `Arc` clone of the
//! shaped galley. [`hit_test`] maps a screen point to a global char index and
//! [`char_pos`] / [`caret_screen_rect`] map a global char index back to screen
//! space. Keeping owned `Arc`s here means the caller can keep using the
//! [`super::layout::LineCache`] while the layout is inspected.

use std::sync::Arc;

use egui::{Pos2, Rect};
use typ_edit_buffer::TextBuffer;

use super::VirtualTextState;
use super::layout::LineData;

/// One visible line's screen geometry.
pub struct LineSlot {
    /// Logical line index.
    pub line: usize,
    /// Total painted height, including soft-wrap continuation rows.
    pub height: f32,
    /// Screen position the galley is painted at (the argument to
    /// `Painter::galley`). For a windowed long line this is the window's start,
    /// not the logical line start.
    pub origin: Pos2,
    /// Shaped galley for the line (newline excluded).
    pub galley: Arc<egui::Galley>,
    /// Char offset of the shaped window within the logical line (0 whole-line).
    pub window_start: usize,
    /// Number of chars in the shaped window.
    pub window_len: usize,
}

impl LineSlot {
    /// Galley-space -> screen translation
    /// (`galley_pos - (rect.left, 0)`).
    pub fn screen_origin(&self) -> Pos2 {
        self.origin - egui::vec2(self.galley.rect.left(), 0.0)
    }

    /// Rebase a line-local char range into the shaped window, or `None` when it
    /// falls entirely outside it.
    pub fn window_local(&self, range: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        let lo = range.start.max(self.window_start);
        let hi = range.end.min(self.window_start + self.window_len);
        (lo < hi).then_some(lo - self.window_start..hi - self.window_start)
    }
}

/// Builds one [`LineSlot`] per painted line, cloning each cached galley `Arc`
/// so the caller can keep using the cache while the layout is inspected.
pub(super) fn build_slots(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    visible: &[(usize, f32, u32)],
    viewport_rect: Rect,
) -> Vec<LineSlot> {
    visible
        .iter()
        .map(|(line, y, _)| {
            let cached = state.cache.line_galley(ui, data, *line);
            let x =
                viewport_rect.left() - state.h_offset + cached.window_start as f32 * cached.advance;
            let origin = egui::pos2(x, viewport_rect.top() + y);
            LineSlot {
                line: *line,
                height: cached.height,
                origin,
                galley: Arc::clone(&cached.galley),
                window_start: cached.window_start,
                window_len: cached.window_len,
            }
        })
        .collect()
}

/// Records the frame's viewport and per-line screen geometry for pointer tests.
#[cfg(test)]
pub(super) fn record_test_geometry(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    visible: &[(usize, f32, u32)],
    viewport_rect: Rect,
) {
    state.test_viewport = Some(viewport_rect);
    state.test_line_origins.clear();
    for (line, y, _) in visible {
        let height = state.cache.line_height(ui, data, *line);
        state
            .test_line_origins
            .push((*line, viewport_rect.top() + y, height));
    }
}

/// Char index just past logical `line`'s content, excluding a trailing `\n`.
pub(super) fn line_content_end(buffer: &TextBuffer, line: usize) -> usize {
    let start = buffer.line_start(line);
    let mut end = buffer.line_start(line + 1);
    if end > start && buffer.char_at(end - 1) == Some('\n') {
        end -= 1;
        // Windows files commonly use CRLF. Leave the CR in the line and it
        // becomes a trailing glyph that can wrap into a visually empty row.
        if end > start && buffer.char_at(end - 1) == Some('\r') {
            end -= 1;
        }
    }
    end
}

/// Length of logical `line` in chars, excluding its trailing newline.
pub fn line_char_len(buffer: &TextBuffer, line: usize) -> usize {
    line_content_end(buffer, line).saturating_sub(buffer.line_start(line))
}

/// Global char index under `pos` (screen space), clamped to the first/last
/// visible line.
///
/// Left of the first line or above the viewport the first line is used; below
/// the last line the last visible line is used. Clicking to the right of a
/// line's last glyph clamps to the line end (before its `\n`), the common
/// editor behavior.
pub fn hit_test(buffer: &TextBuffer, slots: &[LineSlot], pos: Pos2) -> Option<usize> {
    let first = slots.first()?;
    let last = slots.last()?;
    let slot = if pos.y < first.origin.y {
        first
    } else if pos.y >= last.origin.y + last.height {
        last
    } else {
        slots
            .iter()
            .find(|s| pos.y >= s.origin.y && pos.y < s.origin.y + s.height)
            .unwrap_or_else(|| {
                // Rounding gaps between adjacent lines: fall back to whichever
                // line center is closest.
                slots
                    .iter()
                    .min_by(|a, b| {
                        let da = (pos.y - (a.origin.y + a.height * 0.5)).abs();
                        let db = (pos.y - (b.origin.y + b.height * 0.5)).abs();
                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap_or(first)
            })
    };
    let in_window = slot
        .galley
        .cursor_from_pos(pos - slot.origin)
        .index
        .0
        .min(slot.window_len);
    let local = (slot.window_start + in_window).min(line_char_len(buffer, slot.line));
    Some(buffer.line_start(slot.line) + local)
}

/// Screen-space caret rect for global `char`, when its line is among `slots`.
///
/// The rect is zero-width; it spans the caret's glyph row so callers can paint
/// a full-height caret or check the caret's visibility.
pub fn caret_screen_rect(buffer: &TextBuffer, slots: &[LineSlot], char: usize) -> Option<Rect> {
    let char = char.min(buffer.len_chars());
    let line = buffer.line_of_char(char);
    let slot = slots.iter().find(|s| s.line == line)?;
    let line_char = char
        .saturating_sub(buffer.line_start(line))
        .min(line_char_len(buffer, line));
    if line_char < slot.window_start || line_char > slot.window_start + slot.window_len {
        return None;
    }
    let local = line_char - slot.window_start;
    let rect = slot.galley.pos_from_cursor(egui::text::CCursor::new(local));
    let rect = if rect.height() > 0.0 {
        rect
    } else {
        // An empty line's galley has a zero-height row; span the line's height
        // at the line origin so the caret stays visible.
        Rect::from_min_size(egui::pos2(rect.left(), 0.0), egui::vec2(0.0, slot.height))
    };
    Some(rect.translate(slot.screen_origin().to_vec2()))
}

/// Top-left point of the caret at global `char` plus the line it sits on.
pub fn char_pos(buffer: &TextBuffer, slots: &[LineSlot], char: usize) -> Option<(usize, Pos2)> {
    let line = buffer.line_of_char(char.min(buffer.len_chars()));
    let rect = caret_screen_rect(buffer, slots, char)?;
    Some((line, rect.min))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_text::layout::{LineCache, LineData};
    use crate::virtual_text::scroll::ScrollAnchor;

    fn bind_fonts(ctx: &egui::Context) {
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
    }

    fn data<'a>(buffer: &'a TextBuffer) -> LineData<'a> {
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

    /// Builds the visible [`LineSlot`]s exactly as `render` does, inside a
    /// frame so the galley cache can be interrogated.
    fn with_slots<R>(
        ctx: &egui::Context,
        buffer: &TextBuffer,
        f: impl FnOnce(&[LineSlot]) -> R,
    ) -> R {
        let mut out = None;
        let mut cache = LineCache::new();
        let mut f = Some(f);
        let reference = data(buffer);
        ctx.run_ui(Default::default(), |ui| {
            let visible = cache.visible_lines(ui, &reference, &ScrollAnchor::new(), 400.0, 16.0);
            let slots: Vec<LineSlot> = visible
                .iter()
                .map(|(line, y, _)| {
                    let cached = cache.line_galley(ui, &reference, *line);
                    LineSlot {
                        line: *line,
                        height: cached.height,
                        origin: egui::pos2(0.0, *y),
                        galley: Arc::clone(&cached.galley),
                        window_start: cached.window_start,
                        window_len: cached.window_len,
                    }
                })
                .collect();
            out = f.take().map(|f| f(&slots));
        })
        .drop_without_applying_deltas();
        out.expect("a frame result")
    }

    #[test]
    fn hit_test_maps_the_left_side_of_each_glyph_to_that_char() {
        let ctx = egui::Context::default();
        bind_fonts(&ctx);
        let buffer = TextBuffer::from_text("你好世界\nabcdef");
        with_slots(&ctx, &buffer, |slots| {
            let first = &slots[0];
            let mut global = 0usize;
            for row in &first.galley.rows {
                for glyph in &row.glyphs {
                    let rect = glyph
                        .logical_rect()
                        .translate(row.pos.to_vec2() + first.screen_origin().to_vec2());
                    let probe = egui::pos2(rect.left() + 1.0, rect.center().y);
                    assert_eq!(
                        hit_test(&buffer, slots, probe),
                        Some(global),
                        "glyph {global}"
                    );
                    global += 1;
                }
            }
        });
    }

    #[test]
    fn hit_test_clamps_right_of_the_last_glyph_to_line_end() {
        let ctx = egui::Context::default();
        bind_fonts(&ctx);
        let buffer = TextBuffer::from_text("ab\ncd");
        with_slots(&ctx, &buffer, |slots| {
            let first = &slots[0];
            let far = egui::pos2(first.origin.x + 10_000.0, first.origin.y + 2.0);
            assert_eq!(hit_test(&buffer, slots, far), Some(2), "before the newline");
        });
    }

    #[test]
    fn char_pos_and_hit_test_round_trip() {
        let ctx = egui::Context::default();
        bind_fonts(&ctx);
        let buffer = TextBuffer::from_text("你好世界\nabcdef");
        with_slots(&ctx, &buffer, |slots| {
            for ch in [0usize, 1, 2, 3, 6, 7, 11] {
                let (line, pos) = char_pos(&buffer, slots, ch).expect("visible char");
                assert_eq!(line, buffer.line_of_char(ch));
                // Nudge just inside the character cell so the boundary rounding
                // in `cursor_from_pos` cannot flip to the previous char.
                let probe = pos + egui::vec2(0.5, 2.0);
                assert_eq!(hit_test(&buffer, slots, probe), Some(ch), "char {ch}");
            }
        });
    }

    #[test]
    fn caret_rect_on_an_empty_line_spans_the_line_height() {
        let ctx = egui::Context::default();
        bind_fonts(&ctx);
        let buffer = TextBuffer::from_text("a\n\nb");
        with_slots(&ctx, &buffer, |slots| {
            let line1 = buffer.line_start(1);
            let slot = slots
                .iter()
                .find(|s| s.line == 1)
                .expect("the empty line is visible");
            let rect = caret_screen_rect(&buffer, slots, line1).expect("empty-line caret");
            assert!(
                (rect.height() - slot.height).abs() < 0.001,
                "the caret must span the empty line's height"
            );
            assert!(
                (rect.left() - slot.screen_origin().x).abs() < 0.001,
                "the caret sits at the line origin"
            );
        });
    }

    #[test]
    fn hit_test_into_an_empty_line_maps_to_its_start() {
        let ctx = egui::Context::default();
        bind_fonts(&ctx);
        let buffer = TextBuffer::from_text("a\n\nb");
        with_slots(&ctx, &buffer, |slots| {
            let line1 = buffer.line_start(1);
            let slot = slots
                .iter()
                .find(|s| s.line == 1)
                .expect("the empty line is visible");
            let middle = egui::pos2(slot.origin.x + 40.0, slot.origin.y + slot.height * 0.5);
            assert_eq!(hit_test(&buffer, slots, middle), Some(line1));
            let far_right = egui::pos2(slot.origin.x + 10_000.0, slot.origin.y + 1.0);
            assert_eq!(hit_test(&buffer, slots, far_right), Some(line1));
        });
    }

    #[test]
    fn windowed_line_hit_test_round_trips_inside_the_window() {
        let ctx = egui::Context::default();
        bind_fonts(&ctx);
        let buffer = TextBuffer::from_text(&"a".repeat(1_000_000));
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            cache.begin_frame();
            cache.set_horizontal(0.0, 800.0);
            let cached = cache.line_galley(ui, &data, 0);
            let slot = LineSlot {
                line: 0,
                height: cached.height,
                origin: egui::pos2(0.0, 0.0),
                galley: Arc::clone(&cached.galley),
                window_start: cached.window_start,
                window_len: cached.window_len,
            };
            let slots = [slot];
            let in_window = buffer.line_start(0) + cached.window_start + 5;
            let rect = caret_screen_rect(&buffer, &slots, in_window).expect("in-window caret");
            let probe = rect.min + egui::vec2(0.5, 2.0);
            assert_eq!(hit_test(&buffer, &slots, probe), Some(in_window));

            let far = buffer.line_start(0) + 500_000;
            assert!(
                caret_screen_rect(&buffer, &slots, far).is_none(),
                "a caret outside the shaped window is not drawable"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn caret_screen_rect_sits_on_the_caret_row() {
        let ctx = egui::Context::default();
        bind_fonts(&ctx);
        let buffer = TextBuffer::from_text("abc\ndef");
        with_slots(&ctx, &buffer, |slots| {
            let rect = caret_screen_rect(&buffer, slots, 5).expect("line 1 caret");
            let second = slots.iter().find(|s| s.line == 1).expect("second line");
            assert!(rect.top() >= second.origin.y - 0.5);
            assert!(rect.bottom() <= second.origin.y + second.height + 0.5);
        });
    }

    #[test]
    fn caret_and_hit_test_round_trip_with_line_spacing() {
        let ctx = egui::Context::default();
        bind_fonts(&ctx);
        let buffer = TextBuffer::from_text("abc\ndef\nghi");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let d = LineData {
                line_spacing: 1.5,
                ..data(&buffer)
            };
            let visible = cache.visible_lines(ui, &d, &ScrollAnchor::new(), 400.0, 16.0);
            let slots: Vec<LineSlot> = visible
                .iter()
                .map(|(line, y, _)| {
                    let cached = cache.line_galley(ui, &d, *line);
                    LineSlot {
                        line: *line,
                        height: cached.height,
                        origin: egui::pos2(0.0, *y),
                        galley: Arc::clone(&cached.galley),
                        window_start: cached.window_start,
                        window_len: cached.window_len,
                    }
                })
                .collect();

            let second = buffer.line_start(1);
            let rect = caret_screen_rect(&buffer, &slots, second).expect("line 1 caret");
            let slot = slots.iter().find(|s| s.line == 1).expect("line 1 slot");
            assert!(
                rect.top() >= slot.origin.y - 0.5
                    && rect.bottom() <= slot.origin.y + slot.height + 0.5,
                "the caret stays inside line 1's band"
            );
            let probe = rect.min + egui::vec2(0.5, 2.0);
            assert_eq!(hit_test(&buffer, &slots, probe), Some(second));
        })
        .drop_without_applying_deltas();
    }
}
