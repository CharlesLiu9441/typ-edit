//! Byte-space edit deltas that keep parse-derived byte ranges visually stable
//! while the asynchronous parser lags one frame behind an edit.
//!
//! The buffer's [`ParseDeltaHook`] records every applied operation as a byte
//! shift; the widget replays the log over the still-stale parse ranges, so a
//! range after the edit lands where the next parse will put it. A range that
//! overlaps the edited span is dropped, because the fresh parse restores it.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use typ_edit_buffer::{EditHook, Operation, TextBuffer};
use typ_edit_model::{BytePos, ByteRange, Diag, FoldRange, HighlightKind};

use crate::EditorInput;

/// One applied operation expressed as a byte-space shift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ByteDelta {
    pub byte_at: usize,
    pub inserted: usize,
    pub removed: usize,
}

/// Deltas applied since the rendered parse generation.
pub(crate) type DeltaHandle = Rc<RefCell<Vec<ByteDelta>>>;

pub(crate) fn new_delta_handle() -> DeltaHandle {
    Rc::new(RefCell::new(Vec::new()))
}

/// Buffer hook that logs every edit as a byte-space delta.
pub(crate) struct ParseDeltaHook {
    deltas: DeltaHandle,
}

impl ParseDeltaHook {
    pub(crate) fn new(deltas: DeltaHandle) -> Self {
        Self { deltas }
    }
}

impl EditHook for ParseDeltaHook {
    fn on_edit(&mut self, op: &Operation, buffer: &TextBuffer) {
        let (at, text, is_delete) = match op {
            Operation::Insert { at, text, .. } => (at, text, false),
            Operation::Delete { at, text, .. } => (at, text, true),
            Operation::UndoRedoFlag => return,
        };
        self.deltas.borrow_mut().push(ByteDelta {
            byte_at: buffer.char_to_byte(*at),
            inserted: if is_delete { 0 } else { text.len() },
            removed: if is_delete { text.len() } else { 0 },
        });
    }
}

/// Shifts `range` across the log (recorded in application order); `None` when
/// it overlaps a removed/replaced span and must be dropped.
fn shift_range(range: Range<usize>, deltas: &[ByteDelta]) -> Option<Range<usize>> {
    let mut current = range;
    for delta in deltas {
        let removed_end = delta.byte_at + delta.removed;
        if current.end <= delta.byte_at {
            continue;
        }
        if current.start >= removed_end {
            let shift = delta.inserted as isize - delta.removed as isize;
            current = shifted(current.start, shift)..shifted(current.end, shift);
            continue;
        }
        return None;
    }
    Some(current)
}

fn shifted(value: usize, shift: isize) -> usize {
    (value as isize + shift).max(0) as usize
}

/// Like [`shift_range`], but keeps a range that an insertion lands inside.
///
/// Code/math/raw ranges describe the line's wrapping mode: dropping one because
/// the caret inserted a character inside it makes a `Markup` line temporarily
/// wrap (and its horizontal extent disappear) until the next parse arrives.
/// Extend the range over the inserted text instead.
fn shift_range_covering(range: Range<usize>, deltas: &[ByteDelta]) -> Option<Range<usize>> {
    let mut current = range;
    for delta in deltas {
        let removed_end = delta.byte_at + delta.removed;
        if current.end < delta.byte_at {
            continue;
        }
        if current.end == delta.byte_at && delta.removed == 0 {
            // Insertion exactly at the end of a blocked/code span belongs to
            // that span; shifting it would leave the typed character unblocked
            // for one parse-lag frame and wrap the line.
            current.end += delta.inserted;
            continue;
        }
        if current.start >= removed_end {
            let shift = delta.inserted as isize - delta.removed as isize;
            current = shifted(current.start, shift)..shifted(current.end, shift);
            continue;
        }
        if delta.removed == 0 {
            if delta.byte_at < current.start {
                current = shifted(current.start, delta.inserted as isize)
                    ..shifted(current.end, delta.inserted as isize);
            } else {
                current.end += delta.inserted;
            }
            continue;
        }
        let start = if current.start <= delta.byte_at {
            current.start
        } else {
            delta.byte_at
        };
        let end = if current.end <= removed_end {
            delta.byte_at
        } else {
            current.end - delta.removed
        };
        if start >= end {
            return None;
        }
        current = start..end;
    }
    Some(current)
}

#[cfg(test)]
pub(crate) fn shift_ranges_covering(
    ranges: &[Range<usize>],
    deltas: &[ByteDelta],
) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter_map(|range| shift_range_covering(range.clone(), deltas))
        .collect()
}

#[cfg(test)]
pub(crate) fn shift_ranges(ranges: &[Range<usize>], deltas: &[ByteDelta]) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter_map(|range| shift_range(range.clone(), deltas))
        .collect()
}

/// [`shift_range_covering`] over typed [`ByteRange`]s. Used for the ranges that
/// decide whether a `Markup` line wraps.
fn shift_byte_ranges_covering(ranges: &[ByteRange], deltas: &[ByteDelta]) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter_map(|range| shift_range_covering(range.start.0..range.end.0, deltas))
        .collect()
}

/// The parse-derived byte ranges shifted into the buffer's current coordinates.
pub(crate) struct AdjustedInput {
    pub highlights: Vec<(Range<usize>, HighlightKind)>,
    pub blocked_ranges: Vec<Range<usize>>,
    pub code_ranges: Vec<Range<usize>>,
    pub math_ranges: Vec<Range<usize>>,
    pub math_atoms: Vec<Range<usize>>,
    pub folds: Vec<FoldRange>,
    pub diagnostics: Vec<Diag>,
}

impl AdjustedInput {
    pub(crate) fn build(input: &EditorInput<'_>, deltas: &[ByteDelta]) -> Self {
        Self {
            highlights: input
                .highlights
                .iter()
                .filter_map(|(range, kind)| {
                    shift_range(range.start.0..range.end.0, deltas).map(|range| (range, *kind))
                })
                .collect(),
            blocked_ranges: shift_byte_ranges_covering(input.blocked_ranges, deltas),
            code_ranges: shift_byte_ranges_covering(input.code_ranges, deltas),
            math_ranges: shift_byte_ranges_covering(input.math_ranges, deltas),
            math_atoms: shift_byte_ranges_covering(input.math_atoms, deltas),
            folds: input
                .folds
                .iter()
                .filter_map(|fold| {
                    shift_range(fold.start.0..fold.end.0, deltas).map(|range| FoldRange {
                        start: BytePos(range.start),
                        end: BytePos(range.end),
                        kind: fold.kind,
                    })
                })
                .collect(),
            diagnostics: input
                .diagnostics
                .iter()
                .filter_map(|diag| {
                    shift_range(diag.range.start.0..diag.range.end.0, deltas).map(|range| Diag {
                        range: BytePos(range.start)..BytePos(range.end),
                        ..diag.clone()
                    })
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_model::{BytePos, Severity};

    fn delta(byte_at: usize, inserted: usize, removed: usize) -> ByteDelta {
        ByteDelta {
            byte_at,
            inserted,
            removed,
        }
    }

    #[test]
    fn insert_before_a_range_shifts_it_right() {
        let log = [delta(1, 1, 0)];
        assert_eq!(shift_range(3..6, &log), Some(4..7));
        assert_eq!(shift_range(0..1, &log), Some(0..1), "end at the edit stays");
    }

    #[test]
    fn insert_inside_a_range_drops_it() {
        let log = [delta(2, 1, 0)];
        assert_eq!(shift_range(0..4, &log), None);
    }

    #[test]
    fn covering_range_absorbs_an_insert_inside_it() {
        let log = [delta(5, 1, 0)];
        let range = 4..7;
        let ranges = std::slice::from_ref(&range);
        assert_eq!(shift_ranges_covering(ranges, &log), vec![4..8]);
        assert_eq!(shift_ranges(ranges, &log), Vec::<Range<usize>>::new());
    }

    #[test]
    fn covering_range_absorbs_an_insert_at_its_end() {
        let log = [delta(7, 1, 0)];
        let range = 4..7;
        assert_eq!(
            shift_ranges_covering(std::slice::from_ref(&range), &log),
            vec![4..8]
        );
    }

    #[test]
    fn delete_before_a_range_shifts_it_left() {
        let log = [delta(1, 0, 2)];
        assert_eq!(shift_range(4..6, &log), Some(2..4));
        assert_eq!(shift_range(0..1, &log), Some(0..1));
    }

    #[test]
    fn delete_overlapping_a_range_drops_it() {
        let log = [delta(1, 0, 2)];
        assert_eq!(shift_range(0..3, &log), None);
    }

    #[test]
    fn a_replacement_shifts_through_both_halves() {
        let log = [delta(2, 0, 2), delta(2, 3, 0)];
        assert_eq!(shift_range(6..9, &log), Some(7..10));
        assert_eq!(shift_range(0..2, &log), Some(0..2));
    }

    #[test]
    fn hook_records_byte_offsets_and_covers_undo() {
        let log = new_delta_handle();
        let mut buffer = TextBuffer::new();
        buffer.add_hook(Box::new(ParseDeltaHook::new(log.clone())));
        buffer.insert(0, "a你", true);
        buffer.insert(1, "X", true);
        assert_eq!(
            *log.borrow(),
            vec![delta(0, 4, 0), delta(1, 1, 0)],
            "offsets are bytes, in application order"
        );

        assert!(buffer.undo());
        assert_eq!(log.borrow().len(), 3, "undo records its own delta");
        assert_eq!(*log.borrow().last().unwrap(), delta(1, 0, 1));
    }

    #[test]
    fn adjusted_input_shifts_every_list() {
        let input = EditorInput {
            collab_history: false,
            remote_carets: &[],
            highlights: &[(BytePos(4)..BytePos(7), HighlightKind::Strong)],
            diagnostics: &[Diag {
                path: std::path::PathBuf::from("probe.typ"),
                range: BytePos(4)..BytePos(7),
                message: "boom".to_owned(),
                severity: Severity::Error,
            }],
            blocked_ranges: &[BytePos(4)..BytePos(7)],
            code_ranges: &[],
            math_ranges: &[BytePos(4)..BytePos(7)],
            math_atoms: &[BytePos(4)..BytePos(5), BytePos(5)..BytePos(7)],
            spell_issues: &[],
            debug_highlights: &[],
            debug_highlight_color: egui::Color32::TRANSPARENT,
            folds: &[],
            list_lines: &[],
            completions: &[],
            definitions: &[],
            world_completions: &[],
            completion_max: crate::COMPLETION_MAX_DEFAULT,
            parse_generation: 1,
            call_params: None,
            font_family: "sans",
            font_size: 14.0,
            wrap_mode: crate::WrapMode::Off,
            line_spacing: 1.0,
            wrap_line_spacing: 1.0,
            plain_text: false,
            request_focus: false,
            blame_inline: None,
        };
        let adjusted = AdjustedInput::build(&input, &[delta(1, 2, 0)]);
        assert_eq!(adjusted.highlights, vec![(6..9, HighlightKind::Strong)]);
        assert_eq!(adjusted.blocked_ranges, vec![6..9]);
        assert_eq!(adjusted.math_ranges, vec![6..9]);
        assert_eq!(adjusted.math_atoms, vec![6..7, 7..9]);
        assert_eq!(adjusted.diagnostics[0].range, BytePos(6)..BytePos(9));
    }

    fn mono_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let chain = fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default();
        fonts
            .families
            .insert(egui::FontFamily::Name("sans".into()), chain);
        ctx.set_fonts(fonts);
        ctx
    }

    fn mono_char_width(ui: &egui::Ui) -> f32 {
        let mut job = egui::text::LayoutJob::simple(
            "a".to_owned(),
            egui::FontId::new(14.0, egui::FontFamily::Name("sans".into())),
            egui::Color32::WHITE,
            f32::INFINITY,
        );
        job.wrap.max_width = f32::INFINITY;
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        galley.rows[0].glyphs[0].advance_width
    }

    fn row_starts(galley: &std::sync::Arc<egui::Galley>) -> Vec<usize> {
        let mut starts = Vec::new();
        let mut at = 0;
        for row in &galley.rows {
            starts.push(at);
            at += row.glyphs.len();
        }
        starts
    }

    fn wrap_rows(
        ctx: &egui::Context,
        buffer: &TextBuffer,
        span: &[Range<usize>],
        width: f32,
    ) -> Vec<usize> {
        use crate::virtual_text::layout::{LineCache, LineData};
        let mut cache = LineCache::new();
        let mut rows = Vec::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                buffer,
                text_rev: 1,
                parse_generation: 0,
                highlights: &[],
                blocked_ranges: span,
                code_ranges: &[],
                math_ranges: span,
                math_atoms: span,
                font_family: "sans",
                font_size: 14.0,
                wrap_mode: crate::WrapMode::Markup,
                line_spacing: 1.0,
                wrap_line_spacing: 1.0,
                wrap_width: width,
            };
            rows = row_starts(&cache.line_galley(ui, &data, 0).galley);
        })
        .drop_without_applying_deltas();
        rows
    }

    #[test]
    fn shifted_atoms_keep_the_wrap_identical_to_the_fresh_parse() {
        let ctx = mono_ctx();
        let width = {
            let mut width = 0.0f32;
            ctx.run_ui(Default::default(), |ui| width = mono_char_width(ui) * 10.0)
                .drop_without_applying_deltas();
            width
        };
        let buffer = TextBuffer::from_text("bbaa $xxxx$ tail tail");

        let stale = std::slice::from_ref(&(3..9));
        let fresh = std::slice::from_ref(&(5..11));
        let shifted = shift_ranges(stale, &[delta(0, 2, 0)]);
        assert_eq!(shifted, vec![5..11], "the stale atom moves with the insert");

        let fresh_rows = wrap_rows(&ctx, &buffer, fresh, width);
        let shifted_rows = wrap_rows(&ctx, &buffer, &shifted, width);
        let stale_rows = wrap_rows(&ctx, &buffer, stale, width);
        assert_eq!(
            shifted_rows, fresh_rows,
            "the shifted atom wraps exactly like the fresh parse"
        );
        assert_ne!(
            stale_rows, fresh_rows,
            "an unshifted atom would move the break"
        );
    }
}
