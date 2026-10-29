//! Highlight spans -> `LayoutJob` conversion and theme colors.
//!
//! Spans are `(byte_range, kind)` pairs produced by the parse worker; the
//! conversion into an [`egui::text::LayoutJob`] happens on the UI thread every
//! frame (egui memoizes the actual text shaping inside the font atlas cache, so
//! only genuinely changed text is re-shaped).

use std::ops::Range;
use std::sync::Arc;
use typ_edit_model::HighlightKind;

use super::diagnostics::{ceil_char_boundary, floor_char_boundary};

/// Theme color for a highlight kind.
///
/// `dark` selects the palette for dark backgrounds; both palettes keep roughly
/// 4.5:1 contrast against the default editor background.
pub fn kind_color(kind: HighlightKind, dark: bool) -> egui::Color32 {
    use HighlightKind::*;
    use egui::Color32;

    if dark {
        match kind {
            Comment => Color32::from_rgb(106, 153, 85), // gray-green
            Punctuation => Color32::from_rgb(200, 200, 205),
            Escape => Color32::from_rgb(120, 200, 200),
            Strong => Color32::from_rgb(220, 220, 170),
            Emph => Color32::from_rgb(197, 134, 192),
            Link => Color32::from_rgb(79, 193, 255),
            Raw => Color32::from_rgb(215, 186, 125), // dark yellow
            Label => Color32::from_rgb(78, 201, 176),
            Ref => Color32::from_rgb(156, 220, 254),
            Heading => Color32::from_rgb(229, 192, 123),
            ListMarker => Color32::from_rgb(209, 154, 102),
            ListTerm => Color32::from_rgb(229, 192, 123),
            MathDelimiter => Color32::from_rgb(86, 156, 214),
            MathOperator => Color32::from_rgb(198, 120, 221),
            MathGroupingParens => Color32::from_rgb(200, 200, 205),
            MathSymbol => Color32::from_rgb(230, 180, 90), // amber
            Keyword => Color32::from_rgb(198, 120, 221),   // purple/blue
            Operator => Color32::from_rgb(190, 190, 200),
            Number => Color32::from_rgb(86, 182, 194), // teal
            String => Color32::from_rgb(206, 145, 120), // red-brown
            Function => Color32::from_rgb(97, 175, 239), // blue
            Interpolated => Color32::from_rgb(156, 220, 254),
            Error => Color32::from_rgb(255, 90, 90),
            ParamName => Color32::from_rgb(126, 198, 120), // green
        }
    } else {
        match kind {
            Comment => Color32::from_rgb(96, 139, 78),
            Punctuation => Color32::from_rgb(70, 70, 75),
            Escape => Color32::from_rgb(0, 130, 130),
            Strong => Color32::from_rgb(150, 105, 0),
            Emph => Color32::from_rgb(170, 0, 200),
            Link => Color32::from_rgb(0, 0, 210),
            Raw => Color32::from_rgb(145, 105, 0),
            Label => Color32::from_rgb(38, 127, 153),
            Ref => Color32::from_rgb(0, 100, 180),
            Heading => Color32::from_rgb(170, 110, 0),
            ListMarker => Color32::from_rgb(170, 90, 30),
            ListTerm => Color32::from_rgb(150, 105, 0),
            MathDelimiter => Color32::from_rgb(0, 90, 160),
            MathOperator => Color32::from_rgb(150, 50, 180),
            MathGroupingParens => Color32::from_rgb(70, 70, 75),
            MathSymbol => Color32::from_rgb(150, 90, 0), // amber
            Keyword => Color32::from_rgb(150, 40, 180),
            Operator => Color32::from_rgb(90, 90, 100),
            Number => Color32::from_rgb(25, 125, 135),
            String => Color32::from_rgb(160, 70, 50),
            Function => Color32::from_rgb(25, 95, 190),
            Interpolated => Color32::from_rgb(0, 100, 160),
            Error => Color32::from_rgb(200, 40, 40),
            ParamName => Color32::from_rgb(40, 130, 60), // green
        }
    }
}

/// Builds the `LayoutJob` for the buffer text given byte-range spans and an
/// explicit light/dark choice.
///
/// The job must be produced in whole-buffer coordinates (egui slices text itself).
///
/// Spans are sorted by start offset; empty, overlapping, duplicated and
/// out-of-range spans are skipped, and gaps are filled with a default format
/// whose color is [`egui::Color32::PLACEHOLDER`] (resolved to the widget's
/// theme text color when painted). The concatenated section text is always
/// byte-identical to `text`, which is what keeps the galley's char indices in
/// sync with the buffer.
pub fn build_layout_job_with_theme(
    text: &str,
    spans: &[(Range<usize>, HighlightKind)],
    font_family: &str,
    font_size: f32,
    dark: bool,
) -> egui::text::LayoutJob {
    let font_id = egui::FontId::new(font_size, egui::FontFamily::Name(Arc::from(font_family)));
    let default_format = egui::TextFormat {
        font_id: font_id.clone(),
        color: egui::Color32::PLACEHOLDER,
        ..Default::default()
    };

    let mut job = egui::text::LayoutJob {
        // Editors must show trailing spaces and must not soft-wrap
        // (`ScrollArea::both` scrolls long lines horizontally instead).
        keep_trailing_whitespace: true,
        wrap: egui::text::TextWrapping {
            max_width: f32::INFINITY,
            ..Default::default()
        },
        ..Default::default()
    };

    // Spans arrive from the parse worker possibly unsorted; sort by start.
    let mut sorted: Vec<&(Range<usize>, HighlightKind)> = spans.iter().collect();
    sorted.sort_by_key(|(range, _)| (range.start, range.end));

    let len = text.len();
    let mut cursor = 0usize;
    for (range, kind) in sorted {
        // Defensive clamping: spans are contractually char-boundary safe, but a
        // stale span (from an older revision) must never panic here.
        let start = floor_char_boundary(text, range.start);
        let end = ceil_char_boundary(text, range.end);
        if start >= end || start < cursor || start >= len {
            continue; // empty, overlapping, or fully out-of-range span
        }
        if start > cursor {
            job.append(&text[cursor..start], 0.0, default_format.clone());
        }
        job.append(
            &text[start..end],
            0.0,
            egui::TextFormat {
                font_id: font_id.clone(),
                color: kind_color(*kind, dark),
                ..Default::default()
            },
        );
        cursor = end;
    }
    if cursor < len {
        job.append(&text[cursor..], 0.0, default_format);
    }

    job
}

/// Clips whole-buffer byte spans to `[src_start, src_end)` and rebases them to
/// offsets relative to `src_start`. Empty and fully-out-of-range spans are
/// dropped; order is preserved.
///
/// Spans must be sorted by start and disjoint (the parse worker's contract), so
/// the leading disjoint prefix is skipped with a binary search instead of
/// scanning the whole document for every shaped line.
pub(crate) fn clip_rebase_spans(
    spans: &[(Range<usize>, HighlightKind)],
    src_start: usize,
    src_end: usize,
) -> Vec<(Range<usize>, HighlightKind)> {
    debug_assert!(
        spans
            .windows(2)
            .all(|pair| pair[0].0.end <= pair[1].0.start),
        "clip_rebase_spans requires sorted, disjoint spans"
    );
    let first = spans.partition_point(|(range, _)| range.end <= src_start);
    let mut out = Vec::new();
    for (range, kind) in &spans[first..] {
        if range.start >= src_end {
            break;
        }
        let lo = range.start.max(src_start) - src_start;
        let hi = range.end.min(src_end) - src_start;
        if lo < hi {
            out.push((lo..hi, *kind));
        }
    }
    out
}

/// Shapes `text` with `spans` under the active theme, laid out at `wrap_width`.
///
/// `wrap_width` is applied to the job only when finite, so an infinite width
/// reproduces the editor's unwrapped line layout (which is then re-wrapped by
/// [`crate::wrap::wrap_lines`]).
pub(crate) fn highlighted_galley(
    ui: &egui::Ui,
    text: &str,
    spans: &[(Range<usize>, HighlightKind)],
    font_family: &str,
    font_size: f32,
    wrap_width: f32,
) -> Arc<egui::Galley> {
    let mut job =
        build_layout_job_with_theme(text, spans, font_family, font_size, ui.visuals().dark_mode);
    if wrap_width.is_finite() {
        job.wrap.max_width = wrap_width;
    }
    ui.fonts_mut(|fonts| fonts.layout_job(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_rebase_spans_binary_searches_a_sorted_disjoint_list() {
        let spans = vec![
            (0..2, HighlightKind::Keyword),
            (4..8, HighlightKind::String),
            (10..12, HighlightKind::Comment),
        ];
        assert_eq!(
            clip_rebase_spans(&spans, 5, 11),
            vec![
                (0..3, HighlightKind::String),
                (5..6, HighlightKind::Comment),
            ]
        );
        assert!(clip_rebase_spans(&spans, 20, 30).is_empty());
    }

    #[test]
    fn clip_rebase_spans_keeps_a_span_that_starts_before_the_window() {
        let spans = vec![(0..7, HighlightKind::Strong)];
        assert_eq!(
            clip_rebase_spans(&spans, 5, 10),
            vec![(0..2, HighlightKind::Strong)]
        );
    }

    #[test]
    fn sections_are_contiguous_and_cover_the_text() {
        let text = "let x = \"hi\"";
        let spans = vec![
            (0..3, HighlightKind::Keyword),
            (8..12, HighlightKind::String),
        ];
        let job = build_layout_job_with_theme(text, &spans, "editor", 14.0, true);

        assert_eq!(job.text, text);
        assert!(job.keep_trailing_whitespace);
        assert_eq!(job.wrap.max_width, f32::INFINITY);
        assert_eq!(job.sections.first().unwrap().byte_range.start.0, 0);
        assert_eq!(job.sections.last().unwrap().byte_range.end.0, text.len());
        for pair in job.sections.windows(2) {
            assert_eq!(pair[0].byte_range.end.0, pair[1].byte_range.start.0);
        }
        // keyword span + gap + string span (the string runs to the end of the text)
        assert_eq!(job.sections.len(), 3);
        assert_eq!(
            job.sections[0].format.color,
            kind_color(HighlightKind::Keyword, true)
        );
        assert_eq!(job.sections[1].format.color, egui::Color32::PLACEHOLDER);
        assert_eq!(
            job.sections[2].format.color,
            kind_color(HighlightKind::String, true)
        );
    }

    #[test]
    fn overlapping_and_out_of_range_spans_are_skipped() {
        let text = "abcdef";
        let spans = vec![
            (0..3, HighlightKind::Keyword),
            (2..4, HighlightKind::String),    // overlaps -> skipped
            (100..120, HighlightKind::Error), // beyond the text -> skipped
        ];
        let job = build_layout_job_with_theme(text, &spans, "editor", 14.0, true);

        assert_eq!(job.text, text);
        assert_eq!(job.sections.len(), 2);
        assert_eq!(
            job.sections[0].format.color,
            kind_color(HighlightKind::Keyword, true)
        );
        assert_eq!(job.sections[1].format.color, egui::Color32::PLACEHOLDER);
    }

    #[test]
    fn spans_snapped_to_char_boundaries() {
        // 'é' is 1..3 and 'l' is 3..4, so 2..4 must widen to the whole "él".
        let text = "héllo";
        let job = build_layout_job_with_theme(
            text,
            &[(2..4, HighlightKind::String)],
            "editor",
            13.0,
            true,
        );
        assert_eq!(job.text, text);
        assert!(
            job.sections
                .iter()
                .any(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0] == "él")
        );
    }

    #[test]
    fn cjk_span_is_kept_whole() {
        let text = "你好世界";
        let job =
            build_layout_job_with_theme(text, &[(1..4, HighlightKind::Raw)], "editor", 13.0, true);
        assert_eq!(job.text, text);
        // 1..4 lies inside the first char; floor -> 0, ceil -> 6 => the whole "你好" is one highlight section.
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[0].byte_range.start.0, 0);
        assert_eq!(job.sections[0].byte_range.end.0, 6);
        assert_eq!(
            job.sections[0].format.color,
            kind_color(HighlightKind::Raw, true)
        );
        assert_eq!(job.sections[1].format.color, egui::Color32::PLACEHOLDER);
    }

    #[test]
    fn dark_and_light_palettes_differ() {
        for kind in [
            HighlightKind::Comment,
            HighlightKind::Keyword,
            HighlightKind::String,
            HighlightKind::Heading,
            HighlightKind::Raw,
            HighlightKind::MathSymbol,
            HighlightKind::Error,
        ] {
            assert_ne!(kind_color(kind, true), kind_color(kind, false), "{kind:?}");
        }
    }
}
