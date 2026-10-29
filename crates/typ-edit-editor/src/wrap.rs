//! Soft-wrap geometry: re-wrap an unwrapped galley so continuation rows are
//! indented to their logical line's content without overflowing the wrap width,
//! and map galley rows back to buffer line numbers.

use std::ops::Range;
use std::sync::Arc;

use egui::epaint::text::{Glyph, PlacedRow, RowVisuals};
use typ_edit_model::is_cjk;

/// Atoms and math ranges that refine the wrap beyond plain text geometry.
///
/// `atoms` are line-local **char** ranges that never split while they fit a
/// row: raw spans (blocked ranges minus code/math), the engine's leaf-level math
/// units (every math leaf; only a function name plus its `(` and a number plus
/// its unit are glued) and the editor's CJK kinsoku pairs. Every atom start is a
/// break boundary, so two adjacent atoms let the wrap break between them — that
/// is how a break lands right after `(`, between `2` and `)`, or around an
/// operator or comma. `math` are the line-local char ranges of math mode, which
/// grant an after-comma break opportunity when an atom must degrade. `code`
/// marks code atoms in Markup mode: those move as a whole even when wider than
/// the row, so function arguments never wrap while the markup body does. All
/// ranges are sorted, non-overlapping and clipped to the line; empty for plain
/// text. `wrap_gap` is the extra vertical space between a logical line's
/// continuation rows (`0.0` reproduces today's tight stacking).
#[derive(Clone, Copy, Default)]
pub struct WrapContext<'a> {
    pub atoms: &'a [Range<usize>],
    pub math: &'a [Range<usize>],
    pub code: &'a [Range<usize>],
    pub wrap_gap: f32,
}

/// Re-wraps an unwrapped galley so no row exceeds `wrap_width`.
///
/// The input must be an unwrapped layout: one row per logical line, glyph x
/// positions absolute within the line (`LayoutJob::wrap.max_width == INFINITY`).
/// The first row of a logical line spans the full `wrap_width`; every
/// continuation row is indented to the line's content and spans
/// `wrap_width - indent`, so all rows end at or before `wrap_width` by
/// construction (except a unit too wide to fit a fresh row, which overruns).
///
/// `context.atoms` are char ranges that stay whole while they fit: every atom
/// start is a break boundary, so the wrap breaks before an atom rather than in
/// the middle of a word that merely spans it. When the width break falls inside
/// an atom and the atom starts after the row start, the break moves to the atom
/// start (the whole atom drops to the next row). An atom that already starts the
/// row and still overflows degrades — word-level at a space (or right after a
/// comma inside `context.math`), then char-level at the width as the last
/// resort. Non-atom text keeps the word/CJK units.
///
/// Returns the input `Arc` unchanged when nothing needs wrapping, so
/// unindented/short documents stay O(rows) and allocation-free.
///
/// Used by both [`crate::WrapMode::Always`] and [`crate::WrapMode::Markup`]; the
/// markup *eligibility* (whether a line should wrap at all) is decided by the
/// caller, so inline math/raw/styled text wraps as ordinary text here unless
/// the caller supplies its atoms.
pub fn wrap_lines(
    galley: &Arc<egui::Galley>,
    wrap_width: f32,
    pixels_per_point: f32,
    context: WrapContext<'_>,
) -> Arc<egui::Galley> {
    rewrap(galley, wrap_width, pixels_per_point, context)
}

fn rewrap(
    galley: &Arc<egui::Galley>,
    wrap_width: f32,
    pixels_per_point: f32,
    context: WrapContext<'_>,
) -> Arc<egui::Galley> {
    if galley.rows.iter().all(|row| row_width(row) <= wrap_width) {
        return Arc::clone(galley);
    }

    let mut wrapped = galley.as_ref().clone();
    wrapped.rows = reflow_rows(&galley.rows, wrap_width, pixels_per_point, context);
    wrapped.rect = rows_rect(&wrapped.rows);
    wrapped.mesh_bounds = rows_mesh_bounds(&wrapped.rows);
    wrapped.num_vertices = wrapped
        .rows
        .iter()
        .map(|row| row.visuals.mesh.vertices.len())
        .sum();
    wrapped.num_indices = wrapped
        .rows
        .iter()
        .map(|row| row.visuals.mesh.indices.len())
        .sum();
    Arc::new(wrapped)
}

/// Per-row 1-based logical line number, `None` for the continuation rows of a
/// wrapped line. The counter advances after every row that ends with `\n`.
#[cfg(test)]
pub fn row_line_numbers(rows: &[egui::epaint::text::PlacedRow]) -> Vec<Option<usize>> {
    let mut numbers = Vec::with_capacity(rows.len());
    let mut line = 1usize;
    let mut starts_line = true;
    for row in rows {
        numbers.push(if starts_line { Some(line) } else { None });
        starts_line = row.ends_with_newline;
        if row.ends_with_newline {
            line += 1;
        }
    }
    numbers
}

/// Width a row needs: its reported size or the last glyph's right edge,
/// whichever is larger (the two can disagree by a pixel after rounding).
fn row_width(row: &PlacedRow) -> f32 {
    let glyph_right = row.glyphs.last().map_or(0.0, |glyph| glyph.max_x());
    row.size.x.max(glyph_right)
}

/// Half-open glyph range of one wrapped row within its logical line.
#[derive(Clone, Copy)]
struct Segment {
    start: usize,
    end: usize,
}

/// Splits logical lines that exceed `wrap_width` into wrapped sub-rows,
/// stacking everything vertically: a line that wraps into `k` rows consumes
/// `k` line heights, and every later row shifts down by the extra height so
/// rows can never overlap.
fn reflow_rows(
    rows: &[PlacedRow],
    wrap_width: f32,
    pixels_per_point: f32,
    context: WrapContext<'_>,
) -> Vec<PlacedRow> {
    let mut out = Vec::with_capacity(rows.len());
    let mut extra = 0.0f32;
    let gap = context.wrap_gap;
    for row in rows {
        let base = row.pos.y + extra;
        let height = row.size.y;
        if row.glyphs.is_empty() || row_width(row) <= wrap_width {
            let mut row = row.clone();
            row.pos.y = round_pixels(base, pixels_per_point);
            out.push(row);
            continue;
        }
        // Round the indent once so `indent + continuation_width == wrap_width`
        // holds exactly (no sub-pixel overflow from the row's own shift).
        let indent = round_pixels(line_indent(row, wrap_width), pixels_per_point);
        let segments = split_line(&row.glyphs, wrap_width, indent, context);
        let count = segments.len();
        let stride = height + gap;
        for (index, segment) in segments.into_iter().enumerate() {
            let mut sub = sub_row(row, segment, indent);
            sub.pos.y = round_pixels(base + index as f32 * stride, pixels_per_point);
            out.push(sub);
        }
        extra += count.saturating_sub(1) as f32 * stride;
    }
    out
}

/// Rounds a coordinate to the physical pixel grid (crisp text, like epaint).
fn round_pixels(value: f32, pixels_per_point: f32) -> f32 {
    (value * pixels_per_point).round() / pixels_per_point
}

/// Builds one wrapped sub-row for `segment` of `original`.
///
/// Glyph and mesh coordinates are rebased so the segment starts at x = 0; the
/// row's `pos.x` then carries the visual indent (0 for the first row of a
/// logical line, the line's indent for continuations).
fn sub_row(original: &PlacedRow, segment: Segment, indent: f32) -> PlacedRow {
    let glyphs = &original.glyphs;
    let segment_x = glyphs[segment.start].pos.x;
    let vertex_start = glyphs[segment.start].first_vertex as usize;

    let mut row = original.row.as_ref().clone();
    row.glyphs = glyphs[segment.start..segment.end]
        .iter()
        .map(|glyph| {
            let mut glyph = *glyph;
            glyph.pos.x -= segment_x;
            glyph.first_vertex -= vertex_start as u32;
            glyph
        })
        .collect();
    row.size.x = glyphs[segment.end - 1].max_x() - segment_x;
    row.visuals = split_visuals(&original.visuals, glyphs, segment);

    PlacedRow {
        pos: egui::pos2(
            if segment.start == 0 { 0.0 } else { indent },
            original.pos.y,
        ),
        row: Arc::new(row),
        ends_with_newline: segment.end == glyphs.len() && original.ends_with_newline,
    }
}

/// Extracts the part of `original`'s mesh that belongs to `segment`.
///
/// epaint only ever adds atlas quads here: every glyph with a texture rect
/// contributes exactly four vertices and six indices, in glyph order. The
/// editor's `LayoutJob` sets no background/underline/strikethrough, so the
/// mesh holds nothing else to carry over.
fn split_visuals(original: &RowVisuals, glyphs: &[Glyph], segment: Segment) -> RowVisuals {
    let source = &original.mesh;
    let vertex_start = glyphs[segment.start].first_vertex as usize;
    let x_shift = glyphs[segment.start].pos.x;
    let vertex_end = if segment.end < glyphs.len() {
        glyphs[segment.end].first_vertex as usize
    } else {
        original.glyph_vertex_range.end
    };

    let mut visuals = RowVisuals::default();
    visuals.mesh.vertices = source.vertices[vertex_start..vertex_end]
        .iter()
        .map(|vertex| {
            let mut vertex = *vertex;
            vertex.pos.x -= x_shift;
            vertex
        })
        .collect();

    let mut index = original.glyph_index_start;
    for glyph_index in 0..segment.start {
        if has_quad(glyphs, glyph_index, original.glyph_vertex_range.end) {
            index += 6;
        }
    }
    for glyph_index in segment.start..segment.end {
        if has_quad(glyphs, glyph_index, original.glyph_vertex_range.end) {
            for &vertex in &source.indices[index..index + 6] {
                visuals.mesh.indices.push(vertex - vertex_start as u32);
            }
            index += 6;
        }
    }

    visuals.glyph_index_start = 0;
    visuals.glyph_vertex_range = 0..visuals.mesh.vertices.len();
    visuals.mesh_bounds = visuals.mesh.calc_bounds();
    visuals
}

/// Whether `glyphs[index]` contributed an atlas quad to the original mesh.
fn has_quad(glyphs: &[Glyph], index: usize, vertex_end: usize) -> bool {
    let start = glyphs[index].first_vertex as usize;
    let end = if index + 1 < glyphs.len() {
        glyphs[index + 1].first_vertex as usize
    } else {
        vertex_end
    };
    end > start
}

/// Greedy line split by indivisible units and atoms.
///
/// A text unit is a maximal run of non-whitespace, non-CJK chars (a word) or one
/// CJK char. An atom is a char range that must not split while it fits. The first
/// segment may fill `wrap_width`; continuations get `wrap_width - indent`, clamped
/// to a quarter of the width for pathological deep indents. When the unit or atom
/// crossing the boundary can move to the next row it does so whole; one that
/// already starts the row and cannot fit even alone degrades (word-level, then
/// char-level) so every row makes progress.
fn split_line(
    glyphs: &[Glyph],
    wrap_width: f32,
    indent: f32,
    context: WrapContext<'_>,
) -> Vec<Segment> {
    let continuation_width = (wrap_width - indent).max(wrap_width * 0.25);
    let mut segments = Vec::new();
    let mut row_start = 0usize;
    let mut available = wrap_width;

    while row_start < glyphs.len() {
        let row_start_x = glyphs[row_start].pos.x;
        let mut unit_start = None;
        let mut index = row_start;
        while index < glyphs.len() && available >= glyphs[index].max_x() - row_start_x {
            if index > row_start && is_unit_start(glyphs, index, context.atoms) {
                unit_start = Some(index);
            }
            index += 1;
        }
        let end = if index >= glyphs.len() {
            glyphs.len()
        } else {
            break_position(glyphs, index, row_start, unit_start, context)
                .clamp(row_start + 1, glyphs.len())
        };
        segments.push(Segment {
            start: row_start,
            end,
        });
        row_start = end;
        available = continuation_width;
    }
    segments
}

/// Break boundary for the row whose first overflowing glyph is `index`.
fn break_position(
    glyphs: &[Glyph],
    index: usize,
    row_start: usize,
    unit_start: Option<usize>,
    context: WrapContext<'_>,
) -> usize {
    if let Some(atom) = context
        .atoms
        .iter()
        .find(|atom| atom.start <= index && index < atom.end)
    {
        if atom.start > row_start {
            return atom.start;
        }
        if overlaps(atom, context.code) {
            return atom.end;
        }
        return degrade_break(glyphs, index, row_start, atom.end, context.math);
    }
    let boundary = break_at(glyphs, index, row_start, unit_start, context.atoms);
    guard_atom(glyphs, boundary, index, row_start, context)
}

/// Keeps a textual boundary out of every atom: after the atom when it fits,
/// before it when it fits on the next row, degrading when it cannot fit at all.
fn guard_atom(
    glyphs: &[Glyph],
    boundary: usize,
    index: usize,
    row_start: usize,
    context: WrapContext<'_>,
) -> usize {
    let Some(atom) = atom_around(context.atoms, boundary) else {
        return boundary;
    };
    if atom.end <= index {
        return atom.end;
    }
    if atom.start > row_start {
        return atom.start;
    }
    degrade_break(glyphs, index, row_start, atom.end, context.math)
}

/// The atom strictly containing the interior char `position`, if any.
fn atom_around(atoms: &[Range<usize>], position: usize) -> Option<&Range<usize>> {
    atoms
        .iter()
        .find(|atom| atom.start < position && position < atom.end)
}

fn overlaps(atom: &Range<usize>, ranges: &[Range<usize>]) -> bool {
    ranges
        .iter()
        .any(|range| range.start < atom.end && atom.start < range.end)
}

/// Word-level then char-level fallback for an atom that cannot fit a row.
///
/// Opportunities are boundaries before a glyph: the end of a whitespace run (all
/// text) or right after a comma inside math. The last opportunity at or before
/// the width boundary wins; if none fits, the first one still makes progress.
/// With no opportunity at all the width boundary itself splits the atom.
fn degrade_break(
    glyphs: &[Glyph],
    index: usize,
    row_start: usize,
    atom_end: usize,
    math: &[Range<usize>],
) -> usize {
    let last = atom_end.min(glyphs.len());
    let mut first = None;
    let mut fitting = None;
    for position in row_start + 1..=last {
        if !is_break_opportunity(glyphs, position, math) {
            continue;
        }
        if first.is_none() {
            first = Some(position);
        }
        if position <= index {
            fitting = Some(position);
        } else {
            break;
        }
    }
    fitting.or(first).unwrap_or(index)
}

/// Whether a row may break before `position` inside a degrading atom: after a
/// whitespace run, or right after a comma inside math.
fn is_break_opportunity(glyphs: &[Glyph], position: usize, math: &[Range<usize>]) -> bool {
    if position == 0 || position > glyphs.len() {
        return false;
    }
    if glyphs[position - 1].chr == ',' && inside(math, position - 1) {
        return true;
    }
    if !glyphs[position - 1].chr.is_whitespace() {
        return false;
    }
    // Only the end of a whitespace run is an opportunity, and a run directly
    // after a math comma belongs to that comma's break instead.
    if position < glyphs.len() && glyphs[position].chr.is_whitespace() {
        return false;
    }
    let mut run_start = position - 1;
    while run_start > 0 && glyphs[run_start - 1].chr.is_whitespace() {
        run_start -= 1;
    }
    !(run_start > 0 && glyphs[run_start - 1].chr == ',' && inside(math, run_start - 1))
}

/// Whether `position` lies in one of the sorted char ranges.
fn inside(ranges: &[Range<usize>], position: usize) -> bool {
    ranges
        .iter()
        .any(|range| range.start <= position && position < range.end)
}

/// Break boundary for the row whose first overflowing glyph is `index`.
///
/// The whole unit containing `index` moves to the next row (`index` when it
/// starts a unit, else the tracked `unit_start`); whitespace runs are kept as a
/// trailing separator on the current row. If that unit already starts the row
/// (`unit_start` absent and `index == row_start`) the row splits at the width
/// boundary so it still advances.
fn break_at(
    glyphs: &[Glyph],
    index: usize,
    row_start: usize,
    unit_start: Option<usize>,
    atoms: &[Range<usize>],
) -> usize {
    let end = if index > row_start && is_unit_start(glyphs, index, atoms) {
        index
    } else if glyphs[index].chr.is_whitespace() {
        let mut run_end = index;
        while run_end < glyphs.len() && glyphs[run_end].chr.is_whitespace() {
            run_end += 1;
        }
        run_end
    } else if let Some(start) = unit_start {
        start
    } else {
        index
    };
    end.clamp(row_start + 1, glyphs.len())
}

/// Whether `index` begins an indivisible unit: an atom boundary, one CJK char,
/// or the first char of a non-whitespace, non-CJK word (preceded by
/// whitespace/CJK or the start).
fn is_unit_start(glyphs: &[Glyph], index: usize, atoms: &[Range<usize>]) -> bool {
    let chr = glyphs[index].chr;
    if chr.is_whitespace() {
        return false;
    }
    if starts_atom(atoms, index) {
        return true;
    }
    if is_cjk(chr) {
        return true;
    }
    index == 0 || {
        let previous = glyphs[index - 1].chr;
        previous.is_whitespace() || is_cjk(previous)
    }
}

/// Whether an atom starts exactly at `index` (atoms are sorted by start).
fn starts_atom(atoms: &[Range<usize>], index: usize) -> bool {
    atoms
        .binary_search_by_key(&index, |atom| atom.start)
        .is_ok()
}

/// Visual indent of a logical line's continuations, in points.
///
/// The x of the first non-whitespace glyph, or the end of the leading
/// whitespace run when the whole line is whitespace or its first word does not
/// fit on the first row.
fn line_indent(row: &PlacedRow, wrap_width: f32) -> f32 {
    let glyphs = &row.glyphs;
    let Some(first_content) = glyphs.iter().position(|glyph| !glyph.chr.is_whitespace()) else {
        return leading_whitespace_end(glyphs);
    };
    let word_end = glyphs[first_content..]
        .iter()
        .position(|glyph| glyph.chr.is_whitespace())
        .map_or(glyphs.len(), |offset| first_content + offset);
    if glyphs[word_end - 1].max_x() > wrap_width {
        leading_whitespace_end(glyphs)
    } else {
        glyphs[first_content].pos.x
    }
}

/// Right edge of the leading whitespace run, `0.0` when the line starts with
/// content.
fn leading_whitespace_end(glyphs: &[Glyph]) -> f32 {
    let mut end = 0.0;
    for glyph in glyphs {
        if !glyph.chr.is_whitespace() {
            break;
        }
        end = glyph.max_x();
    }
    end
}

/// Bounding rect of the rows, forced to a `(0, 0)` origin like epaint's own
/// left-aligned galleys.
fn rows_rect(rows: &[PlacedRow]) -> egui::Rect {
    let mut rect = egui::Rect::ZERO;
    for row in rows {
        rect = rect.union(row.rect());
    }
    egui::Rect::from_min_max(egui::Pos2::ZERO, rect.max)
}

/// Union of every row mesh's bounds in galley coordinates.
fn rows_mesh_bounds(rows: &[PlacedRow]) -> egui::Rect {
    let mut bounds = egui::Rect::NOTHING;
    for row in rows {
        bounds = bounds.union(row.visuals.mesh_bounds.translate(row.pos.to_vec2()));
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT_SIZE: f32 = 14.0;

    fn layout(text: &str, max_width: f32) -> Arc<egui::Galley> {
        let ctx = egui::Context::default();
        let mut galley = None;
        ctx.run_ui(Default::default(), |ui| {
            let job = egui::text::LayoutJob::simple(
                text.to_owned(),
                egui::FontId::new(FONT_SIZE, egui::FontFamily::Monospace),
                egui::Color32::WHITE,
                max_width,
            );
            galley = Some(ui.fonts_mut(|fonts| fonts.layout_job(job)));
        })
        .drop_without_applying_deltas();
        galley.expect("a galley")
    }

    /// Unwrapped layout: one row per logical line, used by `wrap_lines`.
    fn unwrapped(text: &str) -> Arc<egui::Galley> {
        layout(text, f32::INFINITY)
    }

    fn char_width(galley: &Arc<egui::Galley>) -> f32 {
        galley.rows[0].glyphs[0].advance_width
    }

    /// Right edge of a row in galley coordinates.
    fn right(row: &PlacedRow) -> f32 {
        row.pos.x + row.size.x
    }

    #[test]
    fn indented_long_line_wraps_without_overflow() {
        let text = "    aaaa bbbb cccc dddd eeee ffff gggg hhhh";
        let galley = unwrapped(text);
        let wrap_width = char_width(&galley) * 20.0;
        let out = wrap_lines(&galley, wrap_width, 1.0, WrapContext::default());

        assert!(out.rows.len() >= 2, "the logical line must wrap");
        let indent = galley.rows[0]
            .glyphs
            .iter()
            .find(|glyph| !glyph.chr.is_whitespace())
            .map(|glyph| glyph.pos.x)
            .expect("an indented glyph");
        assert!(indent > 0.0);

        assert_eq!(out.rows[0].pos.x, 0.0);
        for (index, row) in out.rows.iter().enumerate() {
            if index > 0 {
                assert!(
                    (row.pos.x - indent).abs() < 0.01,
                    "row {index} starts at {} instead of {indent}",
                    row.pos.x
                );
            }
            assert!(
                right(row) <= wrap_width + 0.01,
                "row {index} overflows: {} > {wrap_width}",
                right(row)
            );
        }
    }

    #[test]
    fn unindented_long_line_wraps_at_zero() {
        let text = "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii";
        let galley = unwrapped(text);
        let wrap_width = char_width(&galley) * 18.0;
        let out = wrap_lines(&galley, wrap_width, 1.0, WrapContext::default());

        assert!(out.rows.len() >= 2);
        for (index, row) in out.rows.iter().enumerate() {
            assert_eq!(row.pos.x, 0.0, "row {index}");
            assert!(right(row) <= wrap_width + 0.01, "row {index} overflows");
        }
    }

    /// The canonical `is_cjk` union makes fullwidth forms and Hangul break
    /// before a following Latin run as their own unit; the narrowed editor set
    /// kept the whole run as one word.
    #[test]
    fn newly_unified_cjk_breaks_before_a_latin_run() {
        for lead in ['ｘ', '가'] {
            let text = format!("{lead}abc");
            let galley = unwrapped(&text);
            let width = galley.rows[0].glyphs[2].pos.x - 0.5;
            let out = wrap_lines(&galley, width, 1.0, WrapContext::default());
            let rows: Vec<String> = out.rows.iter().map(row_text).collect();
            assert_eq!(
                rows[0],
                lead.to_string(),
                "{lead}: the run moves whole: {rows:?}"
            );
            assert_eq!(rows.concat(), text);
        }
    }

    #[test]
    fn cjk_long_line_breaks_between_characters() {
        let text = "你好世界这是一段很长的中文文本用于测试折行行为";
        let galley = unwrapped(text);
        // Three wide glyphs per row forces character-level breaks.
        let wrap_width = char_width(&galley) * 6.0;
        let out = wrap_lines(&galley, wrap_width, 1.0, WrapContext::default());

        assert!(out.rows.len() >= 2, "the CJK line must wrap");
        for (index, row) in out.rows.iter().enumerate() {
            assert_eq!(row.pos.x, 0.0, "row {index}");
            assert!(
                right(row) <= wrap_width + 0.01,
                "row {index} overflows: {} > {wrap_width}",
                right(row)
            );
        }
    }

    #[test]
    fn oversized_unbreakable_token_may_overrun() {
        let text = "a".repeat(30);
        let galley = unwrapped(&text);
        let wrap_width = char_width(&galley) * 0.5;
        let out = wrap_lines(&galley, wrap_width, 1.0, WrapContext::default());

        assert!(out.rows.len() >= 2, "a character per row");
        assert!(
            out.rows.iter().any(|row| right(row) > wrap_width + 0.01),
            "an unbreakable token is expected to overrun"
        );
    }

    #[test]
    fn wrapped_rows_map_back_to_logical_lines() {
        let text = "aaaaaaaa\nbb";
        let galley = unwrapped(text);
        let out = wrap_lines(
            &galley,
            char_width(&galley) * 4.0,
            1.0,
            WrapContext::default(),
        );

        assert!(
            out.rows.len() >= 3,
            "the first line wraps, the second does not"
        );
        let numbers = row_line_numbers(&out.rows);
        assert_eq!(numbers.first(), Some(&Some(1)));
        assert_eq!(numbers.last(), Some(&Some(2)));

        // The last row of the first logical line carries its newline; every
        // earlier continuation row does not.
        let second_line = numbers
            .iter()
            .position(|number| *number == Some(2))
            .expect("the second logical line");
        assert!(second_line >= 2, "the first line wrapped");
        assert!(out.rows[second_line - 1].ends_with_newline);
        assert!(
            out.rows[..second_line - 1]
                .iter()
                .all(|row| !row.ends_with_newline)
        );
        assert!(!out.rows.last().unwrap().ends_with_newline);
    }

    #[test]
    fn wrapped_rows_stack_vertically_without_overlap() {
        let text = "aaaa bbbb cccc dddd eeee ffff gggg\nnext";
        let galley = unwrapped(text);
        let out = wrap_lines(
            &galley,
            char_width(&galley) * 12.0,
            1.0,
            WrapContext::default(),
        );

        assert!(out.rows.len() >= 3, "the first logical line wraps");
        for pair in out.rows.windows(2) {
            let bottom = pair[0].pos.y + pair[0].size.y;
            assert!(
                pair[1].pos.y >= bottom - 0.01,
                "rows overlap: row at {} then {} (previous bottom {bottom})",
                pair[0].pos.y,
                pair[1].pos.y
            );
        }
        let last = out.rows.last().unwrap();
        assert!(
            out.rect.max.y >= last.pos.y + last.size.y - 0.01,
            "the galley rect must cover the last row"
        );
    }

    #[test]
    fn empty_paragraphs_keep_their_row() {
        let text = format!("{}\n\nb", "a".repeat(40));
        let galley = unwrapped(&text);
        let out = wrap_lines(
            &galley,
            char_width(&galley) * 4.0,
            1.0,
            WrapContext::default(),
        );

        let empty = out
            .rows
            .iter()
            .position(|row| row.glyphs.is_empty())
            .expect("the empty paragraph is kept");
        assert_eq!(out.rows[empty].pos.x, 0.0);
        assert!(out.rows[empty].ends_with_newline);
        let numbers = row_line_numbers(&out.rows);
        assert_eq!(
            numbers[empty],
            Some(2),
            "the blank paragraph is logical line 2"
        );
        assert_eq!(
            numbers[empty - 1],
            None,
            "the previous row is a continuation"
        );
        assert_eq!(numbers[0], Some(1), "the wrapped line is logical line 1");
    }

    #[test]
    fn short_text_returns_the_same_arc() {
        let galley = unwrapped("hello\nworld");
        let out = wrap_lines(&galley, 1000.0, 1.0, WrapContext::default());
        assert!(Arc::ptr_eq(&galley, &out));
    }

    #[test]
    fn row_line_numbers_skip_continuation_rows() {
        let probe = layout("a", f32::INFINITY);
        let width = probe.rows[0].glyphs[0].advance_width * 5.5; // five chars per row
        let galley = layout(&format!("{}\nbb", "a".repeat(8)), width);
        assert_eq!(galley.rows.len(), 3, "the first line wraps into two rows");
        assert_eq!(row_line_numbers(&galley.rows), vec![Some(1), None, Some(2)]);
    }

    fn row_text(row: &PlacedRow) -> String {
        row.glyphs.iter().map(|glyph| glyph.chr).collect()
    }

    #[test]
    fn mid_word_break_is_never_taken() {
        let text = "aaaa bbbb";
        let galley = unwrapped(text);
        // The width reaches into the second 'b', so the whole word must move.
        let width = galley.rows[0].glyphs[6].pos.x - 0.5;
        let out = wrap_lines(&galley, width, 1.0, WrapContext::default());

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert_eq!(rows[0], "aaaa ", "the space stays trailing");
        assert!(
            rows[1].starts_with("bbbb"),
            "the whole word moved: {rows:?}"
        );
    }

    #[test]
    fn long_word_falls_back_to_a_width_split() {
        let text = "abcdefghij";
        let galley = unwrapped(text);
        let out = wrap_lines(
            &galley,
            char_width(&galley) * 4.0,
            1.0,
            WrapContext::default(),
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert_eq!(rows.concat(), text, "no glyph is lost");
        assert!(
            rows.iter().all(|row| !row.is_empty()),
            "the fallback must still make progress: {rows:?}"
        );
        assert_eq!(
            rows[0], "abcd",
            "the long unit splits at the width boundary"
        );
    }

    #[test]
    fn whitespace_run_is_kept_on_the_previous_row() {
        let text = "aaaa bbbb";
        let galley = unwrapped(text);
        let width = galley.rows[0].glyphs[5].pos.x - 0.5;
        let out = wrap_lines(&galley, width, 1.0, WrapContext::default());
        let rows: Vec<String> = out.rows.iter().map(row_text).collect();

        assert!(
            rows.iter().all(|row| !row.starts_with(' ')),
            "no leading space: {rows:?}"
        );
        assert_eq!(rows.concat().trim_start(), "aaaa bbbb");
    }

    /// Wraps `galley` with char-range atoms and math ranges at 1.0 ppp.
    fn wrap_atoms(
        galley: &Arc<egui::Galley>,
        width: f32,
        atoms: &[Range<usize>],
        math: &[Range<usize>],
    ) -> Arc<egui::Galley> {
        wrap_lines(
            galley,
            width,
            1.0,
            WrapContext {
                atoms,
                math,
                code: &[],
                wrap_gap: 0.0,
            },
        )
    }

    fn boundaries(rows: &[PlacedRow]) -> Vec<usize> {
        let mut starts = Vec::new();
        let mut at = 0;
        for row in rows {
            starts.push(at);
            at += row.glyphs.len();
        }
        starts
    }

    #[test]
    fn math_atom_crossing_the_width_moves_whole_to_the_next_row() {
        let text = "aaaa bbbb $sqrt(x)$ cccc";
        let atom = 10..19;
        let galley = unwrapped(text);
        let out = wrap_atoms(
            &galley,
            char_width(&galley) * 14.0,
            std::slice::from_ref(&atom),
            &[],
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert_eq!(rows[0], "aaaa bbbb ", "the atom starts the next row whole");
        assert!(rows[1].starts_with("$sqrt(x)$"), "atom intact: {rows:?}");
        assert!(
            boundaries(&out.rows)
                .iter()
                .all(|start| !(atom.start < *start && *start < atom.end)),
            "no row may start inside the atom"
        );
    }

    #[test]
    fn number_atom_is_not_split_by_the_width() {
        let text = "aa $x + 3.14$ bb";
        let atom = 8..12;
        let galley = unwrapped(text);
        let out = wrap_atoms(
            &galley,
            char_width(&galley) * 10.0,
            std::slice::from_ref(&atom),
            &[],
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert_eq!(rows[0], "aa $x + ");
        assert!(
            rows[1].starts_with("3.14"),
            "the number moved whole: {rows:?}"
        );
        assert!(
            boundaries(&out.rows)
                .iter()
                .all(|start| !(atom.start < *start && *start < atom.end)),
            "no row may start inside the number"
        );
    }

    #[test]
    fn raw_atom_with_spaces_never_splits_when_it_fits() {
        let text = "xx `a b c` yy";
        let atom = 3..10;
        let galley = unwrapped(text);
        let out = wrap_atoms(
            &galley,
            char_width(&galley) * 8.0,
            std::slice::from_ref(&atom),
            &[],
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert_eq!(rows[0], "xx ");
        assert!(rows[1].starts_with("`a b c`"), "raw intact: {rows:?}");
        assert!(
            boundaries(&out.rows)
                .iter()
                .all(|start| !(atom.start < *start && *start < atom.end)),
            "no row may start inside the raw span"
        );
    }

    #[test]
    fn oversized_raw_without_spaces_degrades_to_a_char_split() {
        let text = format!("`{}`", "a".repeat(30));
        let galley = unwrapped(&text);
        let out = wrap_atoms(
            &galley,
            char_width(&galley) * 10.0,
            std::slice::from_ref(&(0..text.len())),
            &[],
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert!(rows.len() >= 2, "an oversized atom still wraps");
        assert!(rows.iter().all(|row| !row.is_empty()), "progress: {rows:?}");
        assert_eq!(rows.concat(), text, "no glyph is lost");
    }

    #[test]
    fn oversized_math_atom_degrades_at_a_space() {
        let text = "$x plus y plus z$";
        let atom = 0..text.len();
        let galley = unwrapped(text);
        let out = wrap_atoms(
            &galley,
            char_width(&galley) * 8.0,
            &[atom],
            std::slice::from_ref(&(0..text.len())),
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert_eq!(rows[0], "$x plus ", "the last fitting space wins");
        assert_eq!(rows.concat(), text);
    }

    #[test]
    fn oversized_math_atom_breaks_after_a_comma_and_prefers_it_over_a_space() {
        let text = "$a, b, c, d, e$";
        let atom = 0..text.len();
        let galley = unwrapped(text);
        let out = wrap_atoms(
            &galley,
            char_width(&galley) * 8.0,
            &[atom],
            std::slice::from_ref(&(0..text.len())),
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert_eq!(
            rows[0], "$a, b,",
            "break right after the comma, not after the space"
        );
        assert_eq!(rows.concat(), text);
    }

    #[test]
    fn oversized_math_atom_without_spaces_breaks_after_commas() {
        let text = "$a,b,c,d,e,f,g$";
        let atom = 0..text.len();
        let galley = unwrapped(text);
        let out = wrap_atoms(
            &galley,
            char_width(&galley) * 8.0,
            &[atom],
            std::slice::from_ref(&(0..text.len())),
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert_eq!(
            rows[0], "$a,b,c,",
            "a comma inside math is a break opportunity"
        );
        assert_eq!(rows.concat(), text);
    }

    /// Flattened leaf-level atoms of `$sqrt(2)+frac(sqrt(2),root(10,13))$`
    /// shifted by `offset` (exactly what the engine emits for the formula).
    fn formula_atoms(offset: usize) -> Vec<Range<usize>> {
        [
            (1, 6),
            (6, 7),
            (7, 8),
            (8, 9),
            (9, 14),
            (14, 19),
            (19, 20),
            (20, 21),
            (21, 22),
            (22, 27),
            (27, 29),
            (29, 30),
            (30, 32),
            (32, 33),
            (33, 34),
        ]
        .iter()
        .map(|(start, end)| (start + offset)..(end + offset))
        .collect()
    }

    /// Every break resolves at an atom boundary: after `(`, between `2` and `)`,
    /// and right after a comma are all reachable, and no glued pair or number is
    /// ever split. A space before the math exercises the word-unit fallback the
    /// atom starts must override.
    #[test]
    fn math_leaf_atoms_break_at_their_boundaries() {
        let prefix = "aa ";
        let text = format!("{prefix}$sqrt(2)+frac(sqrt(2),root(10,13))$");
        let offset = prefix.chars().count();
        let atoms = formula_atoms(offset);
        let math = offset..offset + 35;
        let galley = unwrapped(&text);
        let unit = char_width(&galley);
        let glued: Vec<Range<usize>> = atoms
            .iter()
            .filter(|atom| atom.end - atom.start > 1)
            .cloned()
            .collect();

        let mut seen = Vec::new();
        for width in 6..=16 {
            let out = wrap_atoms(
                &galley,
                unit * width as f32,
                &atoms,
                std::slice::from_ref(&math),
            );
            let starts = boundaries(&out.rows);
            for &start in &starts {
                assert!(
                    glued
                        .iter()
                        .all(|atom| !(atom.start < start && start < atom.end)),
                    "width {width}: row starts inside glued atom at {start}"
                );
            }
            seen.extend(starts);
        }
        assert!(seen.contains(&(offset + 6)), "a break after `(`: {seen:?}");
        assert!(
            seen.contains(&(offset + 7)),
            "a break between `2` and `)`: {seen:?}"
        );
        assert!(
            seen.contains(&(offset + 22)),
            "a break after a comma: {seen:?}"
        );
    }

    /// `|` is its own leaf atom, so a break is allowed on either side of it.
    #[test]
    fn math_bar_atoms_allow_breaks_on_either_side() {
        let prefix = "aa ";
        let text = format!("{prefix}$|sqrt(2)|$ z");
        let offset = prefix.chars().count();
        let atoms = [
            offset + 1..offset + 2,
            offset + 2..offset + 7,
            offset + 7..offset + 8,
            offset + 8..offset + 9,
            offset + 9..offset + 10,
        ];
        let math = offset..offset + 11;
        let bar = offset + 1;
        let galley = unwrapped(&text);
        let unit = char_width(&galley);

        let mut seen = Vec::new();
        for width in 4..=12 {
            let out = wrap_atoms(
                &galley,
                unit * width as f32,
                &atoms,
                std::slice::from_ref(&math),
            );
            seen.extend(boundaries(&out.rows));
        }
        assert!(seen.contains(&bar), "a break before `|`: {seen:?}");
        assert!(seen.contains(&(bar + 1)), "a break after `|`: {seen:?}");
    }

    /// A number too wide for a fresh row still falls back to a char split so the
    /// wrap makes progress.
    #[test]
    fn oversized_number_atom_degrades_to_a_char_split() {
        let text = "aa $12345678901234567890$ bb";
        let atom = 4..24;
        let math = 3..25;
        let galley = unwrapped(text);
        let out = wrap_atoms(
            &galley,
            char_width(&galley) * 8.0,
            std::slice::from_ref(&atom),
            &[math],
        );

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert!(rows.len() >= 2, "an oversized number still wraps: {rows:?}");
        assert_eq!(rows.concat(), text, "no glyph is lost");
        assert!(rows.iter().all(|row| !row.is_empty()), "progress: {rows:?}");
        assert!(
            boundaries(&out.rows)
                .iter()
                .any(|start| atom.start < *start && *start < atom.end),
            "the number is char-split: {rows:?}"
        );
    }

    /// A kinsoku pair (`好，`) is one atom: a break between its chars moves the
    /// whole pair, so no row ends with `好` or starts with `，`.
    #[test]
    fn kinsoku_atom_moves_with_the_preceding_char() {
        let text = "你好，世界";
        let galley = unwrapped(text);
        let glyphs = &galley.rows[0].glyphs;
        let width = (glyphs[1].max_x() + glyphs[2].max_x()) / 2.0;
        let out = wrap_atoms(&galley, width, std::slice::from_ref(&(1..3)), &[]);

        let rows: Vec<String> = out.rows.iter().map(row_text).collect();
        assert!(rows.len() >= 2, "the line wraps: {rows:?}");
        assert!(!rows[0].ends_with('好'), "the pair moved: {rows:?}");
        assert!(
            !rows.iter().any(|row| row.starts_with('，')),
            "no row starts with kinsoku: {rows:?}"
        );
        assert_eq!(rows.concat(), text);
    }

    /// Every terminal punctuation in the kinsoku set keeps its preceding char:
    /// no row starts with the punctuation.
    #[test]
    fn kinsoku_moves_each_terminal_punctuation_with_its_previous_char() {
        for punct in ['。', '！', '？', '—', '”', '）', '，', '、', '：', '；'] {
            let text = format!("你甲{punct}乙丙丁戊");
            let galley = unwrapped(&text);
            let glyphs = &galley.rows[0].glyphs;
            let width = (glyphs[1].max_x() + glyphs[2].max_x()) / 2.0;
            let out = wrap_atoms(&galley, width, std::slice::from_ref(&(1..3)), &[]);

            let rows: Vec<String> = out.rows.iter().map(row_text).collect();
            assert!(
                !rows[0].ends_with('甲'),
                "{punct}: the pair must move: {rows:?}"
            );
            assert!(
                !rows.iter().any(|row| row.starts_with(punct)),
                "{punct}: no row may start with kinsoku: {rows:?}"
            );
            assert_eq!(rows.concat(), text, "{punct}");
        }
    }
}
