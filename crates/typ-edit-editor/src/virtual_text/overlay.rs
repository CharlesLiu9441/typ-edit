//! Static overlays for the virtualized text path (Stage D): the current-line
//! band, bracket-pair outlines and diagnostic squiggles. Find highlights live
//! in [`super::find`]. Every function walks only the visible
//! [`super::hit::LineSlot`]s, so the cost is bounded by the viewport.

use std::ops::Range;
use std::sync::Arc;

use egui::{Color32, Rect, Stroke, StrokeKind};
use typ_edit_buffer::TextBuffer;
use typ_edit_model::{Diag, SpellIssue};

use super::hit::{LineSlot, line_char_len};
use super::select::Carets;
use crate::{FindState, RemoteCaret};

/// Everything the text scene needs for one frame, grouped to keep the paint
/// entry point single-argument.
pub(super) struct Scene<'a> {
    pub clip: Rect,
    pub slots: &'a [LineSlot],
    pub buffer: &'a TextBuffer,
    pub carets: &'a Carets,
    pub preedit: Option<&'a std::ops::Range<usize>>,
    pub find: Option<&'a FindState>,
    pub diagnostics: &'a [Diag],
    pub spell_issues: &'a [SpellIssue],
    pub debug_highlights: &'a [std::ops::Range<usize>],
    pub debug_highlight_color: Color32,
    pub focused: bool,
    pub caret_char: usize,
    pub text_color: Color32,
    pub blink_origin: f64,
    pub blame_inline: Option<(usize, &'a str)>,
    pub font_family: &'a str,
    pub font_size: f32,
    /// Remote participants' cursors for the active file (byte offsets).
    pub remote_carets: &'a [RemoteCaret],
}

/// Paints the whole text scene in paint order: current-line
/// band, selection, find highlights, glyphs, caret, preedit, bracket outlines,
/// then diagnostic squiggles and spell underlines. Find highlights are rects
/// like the selection, so they must be painted before the glyphs to never cover
/// text.
pub(super) fn paint_scene(ui: &egui::Ui, scene: &Scene<'_>) {
    let clip = scene.clip;
    if scene.focused {
        paint_current_line_band(ui, clip, scene.slots, scene.buffer, scene.caret_char);
    }
    // Not gated on focus: the app drives hover highlights while the editor is
    // unfocused (the pointer is over the inspector, not the text).
    paint_debug_highlights(
        ui,
        clip,
        scene.slots,
        scene.buffer,
        scene.debug_highlight_color,
        scene.debug_highlights,
    );
    super::caret::paint_selection(
        ui,
        clip,
        scene.slots,
        scene.buffer,
        scene.carets,
        scene.focused,
    );
    if let Some(find) = scene.find {
        super::find::paint_matches(ui, clip, scene.slots, scene.buffer, find);
    }
    let painter = ui.painter().with_clip_rect(clip);
    for slot in scene.slots {
        painter.galley(slot.origin, Arc::clone(&slot.galley), scene.text_color);
    }
    if let Some((line, text)) = scene.blame_inline {
        paint_blame_inline(
            ui,
            clip,
            scene.slots,
            scene.buffer,
            line,
            text,
            scene.font_family,
            scene.font_size,
        );
    }
    super::caret::paint_caret(
        ui,
        clip,
        scene.slots,
        scene.buffer,
        scene.carets,
        scene.focused,
        scene.blink_origin,
    );
    super::remote::paint_remote(ui, clip, scene.slots, scene.buffer, scene.remote_carets);
    super::caret::paint_preedit(ui, clip, scene.slots, scene.buffer, scene.preedit);
    super::caret::schedule_blink(ui, scene.focused);
    if scene.focused {
        paint_bracket_pair(ui, clip, scene.slots, scene.buffer, scene.caret_char);
    }
    paint_squiggles(ui, clip, scene.slots, scene.buffer, scene.diagnostics);
    paint_spell_issues(ui, clip, scene.slots, scene.buffer, scene.spell_issues);
}

/// Paints the optional inline-blame suffix dimmed at the end of `line`.
///
/// The suffix is a separate overlay galley, so it never mutates the shaped line
/// cache and cannot affect layout, wrapping or the horizontal extent.
#[allow(clippy::too_many_arguments)]
fn paint_blame_inline(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    line: usize,
    text: &str,
    font_family: &str,
    font_size: f32,
) {
    if text.is_empty() || line >= buffer.line_count() {
        return;
    }
    let end_char = buffer.line_start(line) + line_char_len(buffer, line);
    let Some(rect) = super::hit::caret_screen_rect(buffer, slots, end_char) else {
        return;
    };
    let font = egui::FontId::new(
        font_size * 0.85,
        egui::FontFamily::Name(Arc::from(font_family)),
    );
    let color = ui.visuals().weak_text_color().gamma_multiply(0.85);
    let painter = ui.painter().with_clip_rect(clip);
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    painter.galley(egui::pos2(rect.right() + 6.0, rect.top()), galley, color);
}

/// Paints the current-line band behind the text, when the widget is focused.
pub(super) fn paint_current_line_band(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    caret_char: usize,
) {
    let line = buffer.line_of_char(caret_char.min(buffer.len_chars()));
    let Some(slot) = slots.iter().find(|slot| slot.line == line) else {
        return;
    };
    let band = Rect::from_min_max(
        slot.origin,
        egui::pos2(clip.right(), slot.origin.y + slot.height),
    );
    let fill = if ui.visuals().dark_mode {
        Color32::from_white_alpha(typ_edit_widgets::tokens::BAND_WHITE_ALPHA)
    } else {
        Color32::from_black_alpha(typ_edit_widgets::tokens::BAND_BLACK_ALPHA)
    };
    ui.painter()
        .with_clip_rect(clip)
        .rect_filled(band, 0.0, fill);
}

/// Paints the debug hover highlights as translucent bands behind the glyphs.
fn paint_debug_highlights(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    color: Color32,
    ranges: &[Range<usize>],
) {
    if ranges.is_empty() {
        return;
    }
    let painter = ui.painter().with_clip_rect(clip);
    for range in ranges {
        for slot in slots {
            let Some(local) = super::find::line_local_match(buffer, slot.line, range) else {
                continue;
            };
            let Some(local) = slot.window_local(local) else {
                continue;
            };
            for rect in
                crate::diagnostics::squiggle_rects(&slot.galley, slot.screen_origin(), local)
            {
                if rect.intersects(clip) {
                    painter.rect_filled(rect, 0.0, color);
                }
            }
        }
    }
}

/// Outlines the bracket pair around the caret, when the widget is focused.
pub(super) fn paint_bracket_pair(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    caret_char: usize,
) {
    let Some((open, close)) = crate::smart_edit::bracket_pair_at_caret(buffer, caret_char) else {
        return;
    };
    let open_rects = char_rects(slots, buffer, open);
    let close_rects = char_rects(slots, buffer, close);
    let visible = |rects: &[Rect]| rects.iter().any(|rect| rect.intersects(clip));
    if !(visible(&open_rects) && visible(&close_rects)) {
        return;
    }
    let painter = ui.painter().with_clip_rect(clip);
    let stroke = Stroke::new(1.0, ui.visuals().selection.bg_fill);
    for rect in open_rects.iter().chain(&close_rects) {
        if rect.intersects(clip) {
            painter.rect_stroke(rect.expand(1.0), 2.0, stroke, StrokeKind::Outside);
        }
    }
}

/// Screen-space rects for the single char at global `char`, when visible.
fn char_rects(slots: &[LineSlot], buffer: &TextBuffer, char: usize) -> Vec<Rect> {
    let line = buffer.line_of_char(char.min(buffer.len_chars()));
    let Some(slot) = slots.iter().find(|slot| slot.line == line) else {
        return Vec::new();
    };
    let local = char
        .saturating_sub(buffer.line_start(line))
        .min(line_char_len(buffer, line));
    let Some(local) = slot.window_local(local..local + 1) else {
        return Vec::new();
    };
    crate::diagnostics::squiggle_rects(&slot.galley, slot.screen_origin(), local)
}

/// Paints severity-colored squiggles for the diagnostics overlapping the
/// visible lines.
pub(super) fn paint_squiggles(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    diagnostics: &[Diag],
) {
    let (Some(first), Some(last)) = (slots.first(), slots.last()) else {
        return;
    };
    if diagnostics.is_empty() {
        return;
    }
    let byte_start = buffer.char_to_byte(buffer.line_start(first.line));
    let byte_end = buffer.char_to_byte(buffer.line_start(last.line + 1));
    let painter = ui.painter();
    for diag in diagnostics {
        if diag.range.end.0 <= byte_start || diag.range.start.0 >= byte_end {
            continue;
        }
        for slot in slots {
            let Some(local) = super::find::line_local_match(
                buffer,
                slot.line,
                &(diag.range.start.0..diag.range.end.0),
            ) else {
                continue;
            };
            let Some(local) = slot.window_local(local) else {
                continue;
            };
            for rect in
                crate::diagnostics::squiggle_rects(&slot.galley, slot.screen_origin(), local)
            {
                if rect.intersects(clip) {
                    crate::diagnostics::paint_squiggle(
                        painter,
                        rect,
                        clip,
                        diag.severity,
                        ui.visuals().dark_mode,
                    );
                }
            }
        }
    }
}

/// Paints the dotted blue underline for every spell issue overlapping the
/// visible lines. Shares [`paint_squiggles`]' visible-window scan; the style is
/// distinct from the severity squiggles so a spelling is never read as an error.
pub(super) fn paint_spell_issues(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    spell_issues: &[SpellIssue],
) {
    let (Some(first), Some(last)) = (slots.first(), slots.last()) else {
        return;
    };
    if spell_issues.is_empty() {
        return;
    }
    let byte_start = buffer.char_to_byte(buffer.line_start(first.line));
    let byte_end = buffer.char_to_byte(buffer.line_start(last.line + 1));
    let painter = ui.painter();
    for issue in spell_issues {
        if issue.range.end.0 <= byte_start || issue.range.start.0 >= byte_end {
            continue;
        }
        for slot in slots {
            let Some(local) = super::find::line_local_match(
                buffer,
                slot.line,
                &(issue.range.start.0..issue.range.end.0),
            ) else {
                continue;
            };
            let Some(local) = slot.window_local(local) else {
                continue;
            };
            for rect in
                crate::diagnostics::squiggle_rects(&slot.galley, slot.screen_origin(), local)
            {
                if rect.intersects(clip) {
                    crate::diagnostics::paint_dotted_underline(
                        painter,
                        rect,
                        clip,
                        crate::diagnostics::SPELL_COLOR,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_text::layout::{LineCache, LineData};
    use typ_edit_model::BytePos;

    #[test]
    fn find_highlights_paint_behind_the_glyphs() {
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

        let buffer = TextBuffer::from_text("foo");
        let find = crate::FindState {
            open: true,
            matches: std::iter::once(0..3).collect::<Vec<_>>(),
            ..crate::FindState::default()
        };

        let mut cache = LineCache::new();
        let mut base = None;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                buffer: &buffer,
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
            };
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
            let carets = Carets::default();
            base = Some(ui.visuals().selection.bg_fill);
            let scene = Scene {
                clip: ui.clip_rect(),
                slots: &slots,
                buffer: &buffer,
                carets: &carets,
                preedit: None,
                find: Some(&find),
                diagnostics: &[],
                spell_issues: &[],
                debug_highlights: &[],
                debug_highlight_color: egui::Color32::TRANSPARENT,
                focused: false,
                caret_char: 0,
                text_color: egui::Color32::GRAY,
                blink_origin: 0.0,
                blame_inline: None,
                font_family: "sans",
                font_size: 14.0,
                remote_carets: &[],
            };
            paint_scene(ui, &scene);
        });
        output.textures_delta.clear();

        let base = base.expect("a frame");
        let match_rect = output
            .shapes
            .iter()
            .position(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == base));
        let glyphs = output
            .shapes
            .iter()
            .position(|shape| matches!(&shape.shape, egui::Shape::Text(_)));
        assert!(match_rect.is_some(), "the active match paints a rect");
        assert!(glyphs.is_some(), "the glyphs paint a text shape");
        assert!(
            match_rect.unwrap() < glyphs.unwrap(),
            "the find highlight must be painted before (behind) the glyphs"
        );
    }

    #[test]
    fn debug_highlights_paint_behind_the_glyphs() {
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

        let buffer = TextBuffer::from_text("foo");
        let debug = std::slice::from_ref(&(0..3));
        let color = egui::Color32::from_rgba_unmultiplied(255, 0, 0, 60);

        let mut cache = LineCache::new();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                buffer: &buffer,
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
            };
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
            let carets = Carets::default();
            let scene = Scene {
                clip: ui.clip_rect(),
                slots: &slots,
                buffer: &buffer,
                carets: &carets,
                preedit: None,
                find: None,
                diagnostics: &[],
                spell_issues: &[],
                debug_highlights: debug,
                debug_highlight_color: color,
                focused: false,
                caret_char: 0,
                text_color: egui::Color32::GRAY,
                blink_origin: 0.0,
                blame_inline: None,
                font_family: "sans",
                font_size: 14.0,
                remote_carets: &[],
            };
            paint_scene(ui, &scene);
        });
        output.textures_delta.clear();

        let band = output.shapes.iter().position(
            |shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == color),
        );
        let glyphs = output
            .shapes
            .iter()
            .position(|shape| matches!(&shape.shape, egui::Shape::Text(_)));
        assert!(band.is_some(), "the debug highlight paints a rect");
        assert!(glyphs.is_some(), "the glyphs paint a text shape");
        assert!(
            band.unwrap() < glyphs.unwrap(),
            "the debug highlight must be painted before (behind) the glyphs"
        );
    }

    #[test]
    fn blame_inline_paints_a_dimmed_suffix_for_the_requested_line_only() {
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

        let buffer = TextBuffer::from_text("foo");
        let mut cache = LineCache::new();
        let mut render = |blame: Option<(usize, &str)>| {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                let data = LineData {
                    buffer: &buffer,
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
                };
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
                let carets = Carets::default();
                let scene = Scene {
                    clip: ui.clip_rect(),
                    slots: &slots,
                    buffer: &buffer,
                    carets: &carets,
                    preedit: None,
                    find: None,
                    diagnostics: &[],
                    spell_issues: &[],
                    debug_highlights: &[],
                    debug_highlight_color: egui::Color32::TRANSPARENT,
                    focused: false,
                    caret_char: 0,
                    text_color: egui::Color32::GRAY,
                    blink_origin: 0.0,
                    blame_inline: blame,
                    font_family: "sans",
                    font_size: 14.0,
                    remote_carets: &[],
                };
                paint_scene(ui, &scene);
            });
            output.textures_delta.clear();
            output
        };
        let has_blame = |output: &egui::FullOutput| {
            output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains("Alice"))
            })
        };

        assert!(
            has_blame(&render(Some((0, "  · Alice · just now")))),
            "the suffix paints for its line"
        );
        assert!(
            !has_blame(&render(Some((9, "  · Alice · just now")))),
            "a line past the buffer paints nothing"
        );
        assert!(!has_blame(&render(None)), "no blame paints no suffix");
    }

    #[test]
    fn spell_issues_paint_a_dotted_underline() {
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

        let buffer = TextBuffer::from_text("helo");
        let issues = [typ_edit_model::SpellIssue {
            range: BytePos(0)..BytePos(4),
            word: "helo".to_owned(),
            suggestions: vec!["hello".to_owned()],
        }];

        let mut cache = LineCache::new();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                buffer: &buffer,
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
            };
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
            let carets = Carets::default();
            let scene = Scene {
                clip: ui.clip_rect(),
                slots: &slots,
                buffer: &buffer,
                carets: &carets,
                preedit: None,
                find: None,
                diagnostics: &[],
                spell_issues: &issues,
                debug_highlights: &[],
                debug_highlight_color: egui::Color32::TRANSPARENT,
                focused: false,
                caret_char: 0,
                text_color: egui::Color32::GRAY,
                blink_origin: 0.0,
                blame_inline: None,
                font_family: "sans",
                font_size: 14.0,
                remote_carets: &[],
            };
            paint_scene(ui, &scene);
        });
        output.textures_delta.clear();

        assert!(
            output.shapes.iter().any(|shape| matches!(
                &shape.shape,
                egui::Shape::LineSegment { stroke, .. }
                    if stroke.color == crate::diagnostics::SPELL_COLOR
            )),
            "the spell underline paints dashed segments in the spell color"
        );
    }
}
