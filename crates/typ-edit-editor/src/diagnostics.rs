//! Diagnostic ranges -> squiggle geometry and char-boundary snapping.
//!
//! egui has no wavy underline: the squiggle is an overlay painted from the
//! galley geometry of the virtualized layout. Stale byte ranges from an older
//! revision are snapped to char boundaries here and never panic; byte↔char
//! index conversion itself lives on the rope-backed
//! [`typ_edit_buffer::TextBuffer`].

use std::ops::Range;

use typ_edit_model::Severity;

/// Peak height of the squiggle, in points.
const SQUIGGLE_AMPLITUDE: f32 = 1.6;
/// Distance between two peaks (= half of the wavelength), in points.
const SQUIGGLE_HALF_PERIOD: f32 = 2.0;
/// Width of the squiggle line, in points.
const SQUIGGLE_STROKE_WIDTH: f32 = 1.4;
/// Color of an error squiggle (red), identical in both themes.
const SQUIGGLE_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 60, 60);
/// Amber warning squiggle on dark backgrounds (`#FFB74D`).
const SQUIGGLE_WARNING_DARK: egui::Color32 = egui::Color32::from_rgb(255, 183, 77);
/// Dark-amber warning squiggle on light backgrounds (`#B26A00`).
const SQUIGGLE_WARNING_LIGHT: egui::Color32 = egui::Color32::from_rgb(178, 106, 0);
/// Color of the spell-check dotted underline (blue), legible in both themes.
pub const SPELL_COLOR: egui::Color32 = egui::Color32::from_rgb(66, 133, 244);
/// Length of one spell-underline dash, in points.
const SPELL_DASH: f32 = 2.0;
/// Gap between two spell-underline dashes, in points.
const SPELL_GAP: f32 = 2.0;
/// Thickness of the spell-underline dash, in points.
const SPELL_STROKE_WIDTH: f32 = 1.5;

/// The squiggle color for `severity` under the active theme.
///
/// Errors keep the same red in both themes; warnings use an amber that stays
/// legible against the editor background.
pub fn squiggle_color(severity: Severity, dark_mode: bool) -> egui::Color32 {
    match severity {
        Severity::Error => SQUIGGLE_COLOR,
        Severity::Warning => {
            if dark_mode {
                SQUIGGLE_WARNING_DARK
            } else {
                SQUIGGLE_WARNING_LIGHT
            }
        }
    }
}

/// Rounds `i` down to the closest char boundary, clamped to the text length.
pub fn floor_char_boundary(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Rounds `i` up to the closest char boundary, clamped to the text length.
pub(crate) fn ceil_char_boundary(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// [`squiggle_rects`] restricted to the rows from `first_row` on, starting the
/// char counter at `row_char_start` and stopping once the walk passes
/// `char_range`.
///
/// Callers that have a [`Window`] pass `window.first_row` and
/// `window.char_start`; every earlier row is skipped and every later row is
/// abandoned, so the cost is bounded by the visible rows instead of the whole
/// document.
pub(crate) fn squiggle_rects_from(
    galley: &egui::Galley,
    origin: egui::Pos2,
    char_range: Range<usize>,
    first_row: usize,
    row_char_start: usize,
) -> Vec<egui::Rect> {
    let mut rects = Vec::new();
    if char_range.start >= char_range.end || first_row >= galley.rows.len() {
        return rects;
    }

    let mut row_char_start = row_char_start;
    for row in &galley.rows[first_row..] {
        if row_char_start >= char_range.end {
            break;
        }
        let row_char_end = row_char_start + row.glyphs.len();

        // Chars of this row that fall into `char_range`.
        let lo = char_range.start.max(row_char_start);
        let hi = char_range.end.min(row_char_end);
        if lo < hi {
            let mut union: Option<egui::Rect> = None;
            for glyph in &row.glyphs[(lo - row_char_start)..(hi - row_char_start)] {
                // `glyph.pos` is the baseline, relative to the row; `row.pos` is
                // the row position relative to the galley; `origin` is the
                // galley -> screen translation egui itself uses when painting.
                let rect = glyph
                    .logical_rect()
                    .translate(row.pos.to_vec2() + origin.to_vec2());
                union = Some(union.map_or(rect, |u| u.union(rect)));
            }
            if let Some(rect) = union.filter(|r| r.width() > 0.0 && r.height() > 0.0) {
                rects.push(rect);
            }
        }

        row_char_start = row_char_end + usize::from(row.ends_with_newline);
    }

    rects
}

/// Screen-space rects covering `char_range` within a galley drawn at `origin`.
///
/// `origin` is the paint origin (`galley_pos - vec2(galley.rect.left(), 0.0)`).
/// Returns one rect per row segment (zero-height rects omitted).
///
/// The char index of a row is advanced by [`egui::epaint::text::PlacedRow::char_count_including_newline`]
/// semantics: `\n` produces no glyph, but still consumes one char index and
/// starts the next row.
pub fn squiggle_rects(
    galley: &egui::Galley,
    origin: egui::Pos2,
    char_range: Range<usize>,
) -> Vec<egui::Rect> {
    squiggle_rects_from(galley, origin, char_range, 0, 0)
}

/// Paints a severity-colored wavy underline inside `rect` (clipped to `clip`).
///
/// The wave geometry is identical for every severity; only the color differs,
/// and warnings adapt to the active theme.
pub fn paint_squiggle(
    painter: &egui::Painter,
    rect: egui::Rect,
    clip: egui::Rect,
    severity: Severity,
    dark_mode: bool,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || clip.width() <= 0.0 || clip.height() <= 0.0 {
        return;
    }

    // Same layer, but hard-clipped to the editor/scroll viewport.
    let painter = painter.with_clip_rect(clip);

    // Sit the wave just below the text baseline, kept inside the clip rect when
    // the row is partially scrolled out of view.
    let mid_y = (rect.bottom() - 1.0)
        .min(clip.bottom() - SQUIGGLE_AMPLITUDE)
        .max(clip.top() + SQUIGGLE_AMPLITUDE);

    let mut points = Vec::new();
    let mut x = rect.left();
    let mut up = false;
    while x <= rect.right() {
        let dy = if up {
            SQUIGGLE_AMPLITUDE
        } else {
            -SQUIGGLE_AMPLITUDE
        };
        points.push(egui::pos2(x, mid_y + dy));
        up = !up;
        x += SQUIGGLE_HALF_PERIOD;
    }
    if points.len() < 2 {
        // Degenerate (very narrow) rect: a short dash still marks the spot.
        points.push(egui::pos2((rect.left() + 1.0).min(rect.right()), mid_y));
    }

    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(SQUIGGLE_STROKE_WIDTH, squiggle_color(severity, dark_mode)),
    ));
}

/// x-ranges of every dash across `left..right`, a fixed on/off pattern ending
/// at `right`. The single source of the underline's dash segmentation.
fn dash_x_ranges(left: f32, right: f32) -> Vec<(f32, f32)> {
    let mut ranges = Vec::new();
    let mut x = left;
    while x < right {
        ranges.push((x, (x + SPELL_DASH).min(right)));
        x += SPELL_DASH + SPELL_GAP;
    }
    ranges
}

/// Paints a dotted underline along the bottom edge of `rect`, hard-clipped to
/// `clip` the same way [`paint_squiggle`] is (so a partially scrolled row stays
/// inside the viewport).
pub fn paint_dotted_underline(
    painter: &egui::Painter,
    rect: egui::Rect,
    clip: egui::Rect,
    color: egui::Color32,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || clip.width() <= 0.0 || clip.height() <= 0.0 {
        return;
    }

    let painter = painter.with_clip_rect(clip);
    let y = (rect.bottom() - 1.0).min(clip.bottom()).max(clip.top());
    let stroke = egui::Stroke::new(SPELL_STROKE_WIDTH, color);
    for (start, end) in dash_x_ranges(rect.left(), rect.right()) {
        if end <= clip.left() || start >= clip.right() {
            continue;
        }
        painter.line_segment([egui::pos2(start, y), egui::pos2(end, y)], stroke);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squiggle_color_is_theme_aware_for_warnings() {
        assert_eq!(squiggle_color(Severity::Error, true), SQUIGGLE_COLOR);
        assert_eq!(squiggle_color(Severity::Error, false), SQUIGGLE_COLOR);
        assert_eq!(
            squiggle_color(Severity::Warning, true),
            SQUIGGLE_WARNING_DARK
        );
        assert_eq!(
            squiggle_color(Severity::Warning, false),
            SQUIGGLE_WARNING_LIGHT
        );
    }

    #[test]
    fn dotted_dashes_follow_the_on_off_pattern() {
        assert_eq!(
            dash_x_ranges(0.0, 10.0),
            vec![(0.0, 2.0), (4.0, 6.0), (8.0, 10.0)],
            "2px on / 2px off, truncated at the rect edge"
        );
        assert_eq!(
            dash_x_ranges(0.0, 5.0),
            vec![(0.0, 2.0), (4.0, 5.0)],
            "the last dash is clipped to the rect's right edge"
        );
        assert!(dash_x_ranges(0.0, 0.0).is_empty());
        assert!(dash_x_ranges(3.0, 3.0).is_empty());
    }

    fn layout(text: &str) -> std::sync::Arc<egui::Galley> {
        let ctx = egui::Context::default();
        let mut galley = None;
        ctx.run_ui(Default::default(), |ui| {
            let job = egui::text::LayoutJob::simple(
                text.to_owned(),
                egui::FontId::new(14.0, egui::FontFamily::Monospace),
                egui::Color32::WHITE,
                f32::INFINITY,
            );
            galley = Some(ui.fonts_mut(|fonts| fonts.layout_job(job)));
        })
        .drop_without_applying_deltas();
        galley.expect("a galley")
    }

    #[test]
    fn one_rect_per_row_with_newlines_counted() {
        let galley = layout("hello\nworld");
        assert_eq!(galley.rows.len(), 2);

        // "hello" = chars 0..5, the newline = char 5, "world" = chars 6..11.
        assert_eq!(squiggle_rects(&galley, egui::Pos2::ZERO, 1..3).len(), 1);
        assert_eq!(
            squiggle_rects(&galley, egui::Pos2::ZERO, 4..7).len(),
            2,
            "a range crossing the newline must produce one rect per row"
        );
        assert_eq!(squiggle_rects(&galley, egui::Pos2::ZERO, 6..11).len(), 1);
    }

    #[test]
    fn empty_and_out_of_range_ranges_produce_no_rects() {
        let galley = layout("abc");
        assert!(squiggle_rects(&galley, egui::Pos2::ZERO, 2..2).is_empty());
        assert!(squiggle_rects(&galley, egui::Pos2::ZERO, 10..20).is_empty());
    }

    #[test]
    fn origin_translates_the_rects() {
        let galley = layout("abc");
        let base = squiggle_rects(&galley, egui::Pos2::ZERO, 0..1);
        let moved = squiggle_rects(&galley, egui::pos2(10.0, 5.0), 0..1);
        assert_eq!(base.len(), 1);
        assert_eq!(moved.len(), 1);
        let delta = moved[0].min - base[0].min;
        assert!((delta - egui::vec2(10.0, 5.0)).length() < 0.001);
    }

    #[test]
    fn squiggle_rects_from_matches_the_full_walk_at_row_zero() {
        let galley = layout("hello\nworld");
        for range in [0..1, 1..3, 4..7, 6..11, 2..2, 10..20] {
            assert_eq!(
                squiggle_rects_from(&galley, egui::Pos2::ZERO, range.clone(), 0, 0),
                squiggle_rects(&galley, egui::Pos2::ZERO, range.clone()),
                "{range:?}"
            );
        }
    }

    #[test]
    fn squiggle_rects_from_starts_at_a_mid_row_offset() {
        let galley = layout("aaa\nbbb\nccc");
        let from_row_one = squiggle_rects_from(&galley, egui::Pos2::ZERO, 5..7, 1, 4);
        assert_eq!(
            from_row_one,
            squiggle_rects(&galley, egui::Pos2::ZERO, 5..7)
        );
        let crossing = squiggle_rects_from(&galley, egui::Pos2::ZERO, 5..9, 1, 4);
        assert_eq!(crossing, squiggle_rects(&galley, egui::Pos2::ZERO, 5..9));
        assert_eq!(crossing.len(), 2, "one rect on each of rows 1 and 2");
    }

    #[test]
    fn squiggle_rects_from_skips_rows_before_the_start() {
        let galley = layout("aaa\nbbb\nccc");
        // Range 5..9 also covers row 1, but the walk starts at row 2.
        let culled = squiggle_rects_from(&galley, egui::Pos2::ZERO, 5..9, 2, 8);
        assert_eq!(
            culled,
            squiggle_rects(&galley, egui::Pos2::ZERO, 8..9),
            "only row 2 is walked"
        );
        assert!(squiggle_rects_from(&galley, egui::Pos2::ZERO, 1..3, 2, 8).is_empty());
        assert!(squiggle_rects_from(&galley, egui::Pos2::ZERO, 8..9, 9, 12).is_empty());
    }
}
