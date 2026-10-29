//! Reverse caret lookup: maps an editor byte offset in a source file to a caret
//! position in the compiled [`PagedDocument`](typst_layout::PagedDocument).
//!
//! Unlike `typst_ide::jump_from_cursor` (which matches whole spans and returns
//! the run start), this walks document frames and matches individual glyph
//! clusters, so it can place a caret at the correct horizontal offset within a
//! text run.

use std::ops::Range;
use std::path::Path;

use crate::results::CompileResult;
use crate::world::WorldState;
use typst::WorldExt;
use typst::layout::{Abs, Frame, FrameItem, Point, Rect, Transform};
use typst::syntax::{FileId, Span};
use typst::text::{Glyph, TextItem};

/// A caret anchor in the compiled document: 0-based page plus the matched
/// node's box in page-local points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorAnchor {
    pub page: usize,
    /// Caret position: the matched node's right edge, or its left edge when the
    /// cursor sits at the node's first byte.
    pub x: typst::layout::Abs,
    /// Left edge of the matched node's box (advance box / shape area), page-local.
    pub left: typst::layout::Abs,
    /// Right edge of the matched node's box, page-local.
    pub right: typst::layout::Abs,
    pub top: typst::layout::Abs,
    pub bottom: typst::layout::Abs,
}

impl CompileResult {
    /// Maps a byte offset in `path` to its caret position in the compiled
    /// document via a frame-span reverse walk. When the byte maps to no leaf
    /// (e.g. markup syntax that renders nothing, like a heading's `=` marker) it
    /// falls back to the nearest preceding leaf end. Returns `None` only when
    /// the file is not part of the compilation or nothing rendered precedes the
    /// byte.
    pub fn cursor_anchor(&self, path: &Path, byte: usize) -> Option<CursorAnchor> {
        let world = self.world.as_deref()?;
        let document = self.document.as_deref()?;
        let source = world.source_of(path)?;
        let target = source.id();

        // Tiers in priority order: unlinked exact (returns immediately), linked
        // exact, unlinked containing, linked containing, then the nearest
        // preceding leaf end. The preceding tier prefers the largest end and,
        // on ties, the unlinked copy (keeping the outline/TOC fix one tier down).
        let mut linked_exact: Option<(usize, Hit)> = None;
        let mut containing: Option<(usize, Hit)> = None;
        let mut linked_containing: Option<(usize, Hit)> = None;
        let mut preceding: Option<(usize, usize, Hit)> = None;
        let mut linked_preceding: Option<(usize, usize, Hit)> = None;
        for (page, paged) in document.pages().iter().enumerate() {
            match find_in_frame(world, &paged.frame, target, byte, false) {
                Scan::Exact(hit, false) => return Some(anchor(page, hit)),
                Scan::Exact(hit, true) => {
                    if linked_exact.is_none() {
                        linked_exact = Some((page, hit));
                    }
                }
                Scan::Containing(hit, false) => {
                    if containing.is_none() {
                        containing = Some((page, hit));
                    }
                }
                Scan::Containing(hit, true) => {
                    if linked_containing.is_none() {
                        linked_containing = Some((page, hit));
                    }
                }
                Scan::Preceding(end, hit, false) => {
                    keep_page_preceding(&mut preceding, page, end, hit);
                }
                Scan::Preceding(end, hit, true) => {
                    keep_page_preceding(&mut linked_preceding, page, end, hit);
                }
                Scan::None => {}
            }
        }

        if let Some((page, hit)) = linked_exact.or(containing).or(linked_containing) {
            return Some(anchor(page, hit));
        }

        let (page, hit) = match (preceding, linked_preceding) {
            (Some((page, end, hit)), Some((linked_page, linked_end, linked_hit))) => {
                if linked_end > end {
                    (linked_page, linked_hit)
                } else {
                    (page, hit)
                }
            }
            (Some((page, _, hit)), None) => (page, hit),
            (None, Some((page, _, hit))) => (page, hit),
            (None, None) => return None,
        };
        Some(anchor(page, hit))
    }

    /// Page-local boxes covering `range` (byte offsets in `path`), in reading
    /// order: every leaf node (glyph cluster / shape / image) whose effective
    /// source range intersects `range`. Contiguous selected glyphs within one
    /// text run are merged into a single box. Empty when the file is not part
    /// of the compilation or nothing intersects.
    pub fn selection_masks(
        &self,
        path: &Path,
        range: Range<usize>,
    ) -> Vec<(usize, typst::layout::Rect)> {
        let Some(world) = self.world.as_deref() else {
            return Vec::new();
        };
        let Some(document) = self.document.as_deref() else {
            return Vec::new();
        };
        if range.start >= range.end {
            return Vec::new();
        }
        let Some(source) = world.source_of(path) else {
            return Vec::new();
        };
        let target = source.id();

        let mut out = Vec::new();
        for (page, paged) in document.pages().iter().enumerate() {
            collect_in_frame(world, &paged.frame, target, &range, page, &mut out);
        }
        out
    }
}

/// How specific a candidate match is; `Exact` outranks `Containing`.
#[derive(Clone, Copy)]
enum MatchKind {
    Exact,
    Containing,
}

/// A matched node: its box plus the caret position inside/at its edge.
#[derive(Clone, Copy)]
struct Hit {
    node: Rect,
    caret_x: Abs,
}

/// Outcome of scanning one frame: `Exact` aborts the whole walk, `Containing`
/// carries the first partial match, `Preceding` the nearest leaf ending at or
/// before the byte, `None` found nothing. The `bool` is the `linked` context:
/// `true` when the match sits inside a frame carrying a `FrameItem::Link`
/// overlay (e.g. an outline/TOC entry). `Preceding` carries the leaf's span end
/// (a source byte, so it survives a group transform unchanged).
enum Scan {
    Exact(Hit, bool),
    Containing(Hit, bool),
    Preceding(usize, Hit, bool),
    None,
}

/// Walks `frame`'s items in order, matching `byte` against glyph clusters.
///
/// A match found in a group is mapped back to this frame's coordinates via the
/// group transform and the item position. An exact match returns immediately;
/// otherwise the best containing match is returned once the frame is exhausted.
/// When neither exists, the nearest preceding leaf end is returned.
///
/// `linked` is the incoming context ("this frame has a link-wrapped ancestor").
/// An unlinked exact match wins immediately; a linked exact is only remembered
/// (a later unlinked exact in the same frame must still beat it), while exact
/// still outranks any containing match. The preceding tier keeps the largest
/// end, preferring the unlinked copy on ties.
fn find_in_frame(
    world: &WorldState,
    frame: &Frame,
    target: FileId,
    byte: usize,
    linked: bool,
) -> Scan {
    // An outline entry re-lays-out the heading's body, so its glyphs keep the
    // heading's source spans; a Link overlay on the frame is the only signal
    // that distinguishes the TOC copy from the real heading body.
    let linked = linked
        || frame
            .items()
            .any(|(_, item)| matches!(item, FrameItem::Link(..)));

    // The first linked exact (an unlinked exact returns immediately below).
    let mut exact: Option<Hit> = None;
    // First unlinked and first linked containing matches, kept apart so the
    // unlinked copy is preferred without discarding the fallback.
    let mut containing: Option<Hit> = None;
    let mut linked_containing: Option<Hit> = None;
    // Nearest preceding leaf ends, likewise split by linkedness.
    let mut preceding: Option<(usize, Hit)> = None;
    let mut linked_preceding: Option<(usize, Hit)> = None;

    for &(pos, ref item) in frame.items() {
        match item {
            FrameItem::Group(group) => {
                match find_in_frame(world, &group.frame, target, byte, linked) {
                    Scan::Exact(hit, false) => {
                        return Scan::Exact(map_hit(hit, group.transform, pos), false);
                    }
                    Scan::Exact(hit, true) => {
                        if exact.is_none() {
                            exact = Some(map_hit(hit, group.transform, pos));
                        }
                    }
                    Scan::Containing(hit, false) => {
                        if containing.is_none() {
                            containing = Some(map_hit(hit, group.transform, pos));
                        }
                    }
                    Scan::Containing(hit, true) => {
                        if linked_containing.is_none() {
                            linked_containing = Some(map_hit(hit, group.transform, pos));
                        }
                    }
                    Scan::Preceding(end, hit, false) => {
                        keep_preceding(&mut preceding, end, map_hit(hit, group.transform, pos));
                    }
                    Scan::Preceding(end, hit, true) => {
                        keep_preceding(
                            &mut linked_preceding,
                            end,
                            map_hit(hit, group.transform, pos),
                        );
                    }
                    Scan::None => {}
                }
            }
            FrameItem::Text(text) => {
                let (primary, pre) = find_in_text(world, text, pos, target, byte);
                if let Some((kind, hit)) = primary {
                    match kind {
                        MatchKind::Exact if !linked => return Scan::Exact(hit, false),
                        MatchKind::Exact => {
                            if exact.is_none() {
                                exact = Some(hit);
                            }
                        }
                        MatchKind::Containing if linked => {
                            if linked_containing.is_none() {
                                linked_containing = Some(hit);
                            }
                        }
                        MatchKind::Containing => {
                            if containing.is_none() {
                                containing = Some(hit);
                            }
                        }
                    }
                }
                if let Some((end, hit)) = pre {
                    let bucket = if linked {
                        &mut linked_preceding
                    } else {
                        &mut preceding
                    };
                    keep_preceding(bucket, end, hit);
                }
            }
            FrameItem::Shape(shape, span) => {
                let (primary, pre) =
                    find_in_area(world, *span, pos, shape.bbox(false), target, byte);
                if let Some(hit) = primary {
                    if linked {
                        if linked_containing.is_none() {
                            linked_containing = Some(hit);
                        }
                    } else if containing.is_none() {
                        containing = Some(hit);
                    }
                }
                if let Some((end, hit)) = pre {
                    let bucket = if linked {
                        &mut linked_preceding
                    } else {
                        &mut preceding
                    };
                    keep_preceding(bucket, end, hit);
                }
            }
            FrameItem::Image(_, size, span) => {
                let area = Rect::from_pos_size(Point::zero(), *size);
                let (primary, pre) = find_in_area(world, *span, pos, area, target, byte);
                if let Some(hit) = primary {
                    if linked {
                        if linked_containing.is_none() {
                            linked_containing = Some(hit);
                        }
                    } else if containing.is_none() {
                        containing = Some(hit);
                    }
                }
                if let Some((end, hit)) = pre {
                    let bucket = if linked {
                        &mut linked_preceding
                    } else {
                        &mut preceding
                    };
                    keep_preceding(bucket, end, hit);
                }
            }
            FrameItem::Link(_, _) | FrameItem::Tag(_) => {}
        }
    }

    // A remembered exact is necessarily link-wrapped; it still outranks any
    // containing match. Prefer the unlinked containing copy when no exact exists.
    if let Some(hit) = exact {
        return Scan::Exact(hit, true);
    }
    if let Some(hit) = containing {
        return Scan::Containing(hit, false);
    }
    if let Some(hit) = linked_containing {
        return Scan::Containing(hit, true);
    }
    // Preceding tier: the largest end wins; an equal end prefers the unlinked copy.
    match (preceding, linked_preceding) {
        (Some((end, hit)), Some((linked_end, linked_hit))) => {
            if linked_end > end {
                Scan::Preceding(linked_end, linked_hit, true)
            } else {
                Scan::Preceding(end, hit, false)
            }
        }
        (Some((end, hit)), None) => Scan::Preceding(end, hit, false),
        (None, Some((end, hit))) => Scan::Preceding(end, hit, true),
        (None, None) => Scan::None,
    }
}

type CursorHit = (Option<(MatchKind, Hit)>, Option<(usize, Hit)>);

/// Scans one text run for `byte`, returning the most specific match with its
/// node box and caret x in the current frame's coordinates, plus the nearest
/// preceding cluster (the largest end strictly before `byte`, first on ties).
fn find_in_text(
    world: &WorldState,
    text: &TextItem,
    pos: Point,
    target: FileId,
    byte: usize,
) -> CursorHit {
    let metrics = text.font.metrics();
    let top = pos.y - metrics.ascender.at(text.size);
    let bottom = pos.y - metrics.descender.at(text.size);

    let mut pen = Abs::zero();
    let mut containing: Option<Hit> = None;
    let mut preceding: Option<(usize, Hit)> = None;

    for glyph in &text.glyphs {
        let before = pen;
        pen += glyph.x_advance.at(text.size);

        if glyph.span.0.id() != Some(target) {
            continue;
        }
        let Some(range) = world.range(glyph.span.0) else {
            continue;
        };

        let cluster = glyph.range();
        let start = range.start.saturating_add(usize::from(glyph.span.1));
        let end = start.saturating_add(cluster.end.saturating_sub(cluster.start));

        let node = Rect::new(
            Point::new(pos.x + before, top),
            Point::new(pos.x + pen, bottom),
        );

        if end == byte {
            return (
                Some((
                    MatchKind::Exact,
                    Hit {
                        node,
                        caret_x: pos.x + pen,
                    },
                )),
                None,
            );
        }
        if start <= byte && byte < end {
            // A caret at the cluster's first byte belongs at its left edge.
            let caret_x = if byte == start {
                pos.x + before
            } else {
                pos.x + pen
            };
            if containing.is_none() {
                containing = Some(Hit { node, caret_x });
            }
        }
        if end < byte {
            // Preceding candidate: the caret sits at the cluster's right edge.
            let better = match preceding {
                Some((best, _)) => end > best,
                None => true,
            };
            if better {
                preceding = Some((
                    end,
                    Hit {
                        node,
                        caret_x: pos.x + pen,
                    },
                ));
            }
        }
    }

    (
        containing.map(|hit| (MatchKind::Containing, hit)),
        preceding,
    )
}

/// Fallback for non-text leaves (shapes, images): a containing match when the
/// byte lies inside the span, or the nearest preceding match when the span ends
/// at or before the byte. The node box is the area; the caret sits at the
/// span's right edge, or its left edge for the first byte.
fn find_in_area(
    world: &WorldState,
    span: Span,
    pos: Point,
    area: Rect,
    target: FileId,
    byte: usize,
) -> (Option<Hit>, Option<(usize, Hit)>) {
    if span.id() != Some(target) {
        return (None, None);
    }
    let Some(range) = world.range(span) else {
        return (None, None);
    };

    let node = Rect::new(pos + area.min, pos + area.max);
    if byte < range.start || byte >= range.end {
        // Past the span's end: a preceding candidate with the caret at the right
        // edge (distance 0 allowed). Before its start: neither match.
        if range.end <= byte {
            return (
                None,
                Some((
                    range.end,
                    Hit {
                        node,
                        caret_x: pos.x + area.max.x,
                    },
                )),
            );
        }
        return (None, None);
    }

    let caret_x = if byte == range.start {
        pos.x + area.min.x
    } else {
        pos.x + area.max.x
    };
    (Some(Hit { node, caret_x }), None)
}

/// Keeps the preceding candidate with the largest `end`; equal ends keep the
/// first found (page/item order).
fn keep_preceding(bucket: &mut Option<(usize, Hit)>, end: usize, hit: Hit) {
    let better = match *bucket {
        Some((best, _)) => end > best,
        None => true,
    };
    if better {
        *bucket = Some((end, hit));
    }
}

/// Page-level [`keep_preceding`]: tracks the owning page alongside the end.
fn keep_page_preceding(
    bucket: &mut Option<(usize, usize, Hit)>,
    page: usize,
    end: usize,
    hit: Hit,
) {
    let better = match *bucket {
        Some((_, best, _)) => end > best,
        None => true,
    };
    if better {
        *bucket = Some((page, end, hit));
    }
}

/// Recursively collects page-local boxes for every leaf selected by `range`.
/// Text is matched per glyph; shapes and images are selected only when their
/// whole source span is inside `range` (a partially selected drawing must not
/// paint as if it were fully selected).
fn collect_in_frame(
    world: &WorldState,
    frame: &Frame,
    target: FileId,
    range: &Range<usize>,
    page: usize,
    out: &mut Vec<(usize, Rect)>,
) {
    for &(pos, ref item) in frame.items() {
        match item {
            FrameItem::Group(group) => {
                let mut inner = Vec::new();
                collect_in_frame(world, &group.frame, target, range, page, &mut inner);
                out.extend(
                    inner
                        .into_iter()
                        .map(|(page, rect)| (page, map_frame_rect(rect, group.transform, pos))),
                );
            }
            FrameItem::Text(text) => {
                collect_in_text(world, text, pos, target, range, page, out);
            }
            FrameItem::Shape(shape, span) => {
                if span_contained(world, *span, target, range) {
                    let area = shape.bbox(false);
                    out.push((page, Rect::new(pos + area.min, pos + area.max)));
                }
            }
            FrameItem::Image(_, size, span) => {
                if span_contained(world, *span, target, range) {
                    let area = Rect::from_pos_size(Point::zero(), *size);
                    out.push((page, Rect::new(pos + area.min, pos + area.max)));
                }
            }
            FrameItem::Link(_, _) | FrameItem::Tag(_) => {}
        }
    }
}

/// Collects a text run's intersecting glyph boxes, merging contiguous selected
/// glyphs into one box and breaking the run at any skipped or unselected glyph.
fn collect_in_text(
    world: &WorldState,
    text: &TextItem,
    pos: Point,
    target: FileId,
    range: &Range<usize>,
    page: usize,
    out: &mut Vec<(usize, Rect)>,
) {
    let metrics = text.font.metrics();
    let top = pos.y - metrics.ascender.at(text.size);
    let bottom = pos.y - metrics.descender.at(text.size);

    let mut pen = Abs::zero();
    let mut pending: Option<Rect> = None;

    for glyph in &text.glyphs {
        let before = pen;
        pen += glyph.x_advance.at(text.size);

        let selected = glyph_source_range(world, glyph, target).and_then(|(start, end)| {
            (start < range.end && end > range.start).then(|| {
                Rect::new(
                    Point::new(pos.x + before, top),
                    Point::new(pos.x + pen, bottom),
                )
            })
        });

        match selected {
            Some(hit) => {
                pending = Some(match pending {
                    Some(rect) => Rect::new(rect.min.min(hit.min), rect.max.max(hit.max)),
                    None => hit,
                });
            }
            None => {
                if let Some(rect) = pending.take() {
                    out.push((page, rect));
                }
            }
        }
    }

    if let Some(rect) = pending.take() {
        out.push((page, rect));
    }
}

/// The effective byte range of a glyph's source span in `target`, or `None`
/// when the glyph does not belong to that file.
fn glyph_source_range(world: &WorldState, glyph: &Glyph, target: FileId) -> Option<(usize, usize)> {
    if glyph.span.0.id() != Some(target) {
        return None;
    }
    let source = world.range(glyph.span.0)?;
    let cluster = glyph.range();
    let start = source.start.saturating_add(usize::from(glyph.span.1));
    let end = start.saturating_add(cluster.end.saturating_sub(cluster.start));
    Some((start, end))
}

/// Whether a leaf span belongs to `target` and is fully contained in `range`.
/// Partial overlap is intentionally rejected: a shape/image is one visual unit,
/// so the editor selection must cover its whole source span before it counts as
/// selected.
fn span_contained(world: &WorldState, span: Span, target: FileId, range: &Range<usize>) -> bool {
    span.id() == Some(target)
        && world
            .range(span)
            .is_some_and(|source| source.start >= range.start && source.end <= range.end)
}

/// Maps a match found inside a group's frame to the parent frame: the node box
/// via the four-corner method (rotation-safe), the caret point via the same
/// transform, then translate by the group item's position.
fn map_hit(hit: Hit, transform: Transform, pos: Point) -> Hit {
    let node = map_frame_rect(hit.node, transform, pos);
    let caret = pos + Point::new(hit.caret_x, hit.node.min.y).transform(transform);
    Hit {
        node,
        caret_x: caret.x,
    }
}

/// Maps a rectangle from a group's frame into the parent frame's coordinates:
/// rotate the four corners, take their bounding box, then translate.
pub fn map_frame_rect(rect: Rect, transform: Transform, pos: Point) -> Rect {
    let corners = [
        Point::new(rect.min.x, rect.min.y),
        Point::new(rect.max.x, rect.min.y),
        Point::new(rect.min.x, rect.max.y),
        Point::new(rect.max.x, rect.max.y),
    ];

    let mut min = Point::splat(Abs::inf());
    let mut max = Point::splat(-Abs::inf());
    for corner in corners {
        let mapped = pos + corner.transform(transform);
        min = min.min(mapped);
        max = max.max(mapped);
    }

    Rect::new(min, max)
}

/// Builds the public anchor from a page and a matched node.
fn anchor(page: usize, hit: Hit) -> CursorAnchor {
    CursorAnchor {
        page,
        x: hit.caret_x,
        left: hit.node.min.x,
        right: hit.node.max.x,
        top: hit.node.min.y,
        bottom: hit.node.max.y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::FontCatalog;
    use std::sync::Arc;
    use typst_layout::PagedDocument;

    /// Compiles `text` as the entry document and returns the result plus its path.
    fn compiled(text: &str) -> (CompileResult, std::path::PathBuf) {
        let fonts = FontCatalog::new();
        let root = std::env::temp_dir();
        let main = root.join("typ-edit-cursor-test.typ");
        let world = WorldState::new(
            root,
            main.clone(),
            vec![(main.clone(), text.to_owned())],
            &fonts,
            Arc::new(crate::packages::system_packages()),
        );
        let document = typst::compile::<PagedDocument>(&world)
            .output
            .expect("compile cursor test document");
        let page_count = document.pages().len();
        let result = CompileResult {
            generation: 1,
            document: Some(Arc::new(document)),
            world: Some(Arc::new(world)),
            diagnostics: Vec::new(),
            error: None,
            page_count,
            missing: Vec::new(),
        };
        (result, main)
    }

    #[test]
    fn caret_advances_across_a_line() {
        let text = "#set page(margin: 0pt)\nHello, world! \\\nSecond line\n";
        let (result, path) = compiled(text);
        let hello = text.find("Hello").expect("Hello in text");

        let anchors: Vec<CursorAnchor> = (0.."Hello".len())
            .filter_map(|i| result.cursor_anchor(&path, hello + i))
            .collect();
        assert_eq!(anchors.len(), "Hello".len(), "every byte of Hello maps");

        for anchor in &anchors {
            assert_eq!(anchor.page, 0);
            assert!(anchor.top < anchor.bottom, "line extent is ordered");
            assert!(
                anchor.left <= anchor.x && anchor.x <= anchor.right,
                "caret x must lie within the node box: {:?} in {:?}..{:?}",
                anchor.x,
                anchor.left,
                anchor.right
            );
        }
        for pair in anchors.windows(2) {
            assert!(
                pair[1].x > pair[0].x,
                "caret x must advance right: {:?} -> {:?}",
                pair[0].x,
                pair[1].x
            );
        }
    }

    #[test]
    fn line_start_sits_at_left_edge() {
        let text = "#set page(margin: 0pt)\nHello, world! \\\nSecond line\n";
        let (result, path) = compiled(text);
        let line_start = text.find("Second").expect("Second in text");

        let start = result
            .cursor_anchor(&path, line_start)
            .expect("line start maps");
        let next = result
            .cursor_anchor(&path, line_start + 1)
            .expect("next byte maps");

        assert!(
            start.x < next.x,
            "left edge precedes the first glyph's right edge"
        );
        assert!(start.x < Abs::pt(1.0), "line start hugs the left edge");
        assert!(start.top < start.bottom);
        assert_eq!(
            start.x, start.left,
            "line start sits on the node's left edge"
        );
        assert!(start.left < start.right, "the node has horizontal extent");
    }

    #[test]
    fn exact_match_moves_the_caret() {
        let text = "#set page(margin: 0pt)\nHello, world! \\\nSecond line\n";
        let (result, path) = compiled(text);
        let h = text.find('H').expect("H in text");

        let start_of_h = result.cursor_anchor(&path, h).expect("start of H maps");
        let after_h = result.cursor_anchor(&path, h + 1).expect("end of H maps");
        assert!(
            after_h.x > start_of_h.x,
            "the caret moved past the glyph: {:?} -> {:?}",
            start_of_h.x,
            after_h.x
        );
        assert_eq!(
            after_h.x, after_h.right,
            "the byte after the glyph sits on the node's right edge"
        );
        assert!(
            after_h.left < after_h.right,
            "the matched node has horizontal extent"
        );
    }

    #[test]
    fn node_boxes_are_ordered_left_to_right() {
        let text = "#set page(margin: 0pt)\nHello, world! \\\nSecond line\n";
        let (result, path) = compiled(text);
        let hello = text.find("Hello").expect("Hello in text");

        let anchors: Vec<CursorAnchor> = (0.."Hello".len())
            .filter_map(|i| result.cursor_anchor(&path, hello + i))
            .collect();
        assert_eq!(anchors.len(), "Hello".len(), "every byte of Hello maps");
        for pair in anchors.windows(2) {
            assert!(
                pair[1].left >= pair[0].left,
                "node left edge must not move backwards: {:?} -> {:?}",
                pair[0].left,
                pair[1].left
            );
        }
    }

    #[test]
    fn multi_byte_cjk_maps() {
        let text = "#set page(margin: 0pt)\n汉字\n";
        let (result, path) = compiled(text);
        let han = text.find('汉').expect("汉 in text");

        // The byte just after the first char is a 3-byte boundary.
        let anchor = result
            .cursor_anchor(&path, han + "汉".len())
            .expect("byte after first CJK char maps");
        assert_eq!(anchor.page, 0);
        assert!(anchor.top < anchor.bottom);
    }

    #[test]
    fn unknown_file_yields_none() {
        let text = "#set page(margin: 0pt)\nHello, world!\n";
        let (result, _path) = compiled(text);
        assert!(
            result
                .cursor_anchor(&std::env::temp_dir().join("not-part-of-compilation.typ"), 0)
                .is_none()
        );
    }

    #[test]
    fn selection_masks_cover_a_selected_word() {
        let text = "#set page(margin: 0pt)\nHello, world!\n";
        let (result, path) = compiled(text);
        let hello = text.find("Hello").expect("Hello in text");

        let masks = result.selection_masks(&path, hello..hello + "Hello".len());
        assert!(!masks.is_empty(), "a selected word yields at least one box");
        for (page, rect) in &masks {
            assert_eq!(*page, 0);
            assert!(rect.min.x < rect.max.x, "box has horizontal extent");
            assert!(rect.min.y < rect.max.y, "box has vertical extent");
        }
    }

    #[test]
    fn selection_masks_require_a_shape_span_fully_contained() {
        let text = "#set page(margin: 0pt)\n#rect(width: 20pt, height: 10pt)\n";
        let (result, path) = compiled(text);
        let rect = text.find("#rect").expect("#rect in text");
        let line_end = text[rect..]
            .find('\n')
            .map(|offset| rect + offset)
            .unwrap_or(text.len());

        let full = result.selection_masks(&path, rect..line_end);
        assert!(!full.is_empty(), "a fully selected shape yields a mask");

        let partial = result.selection_masks(&path, rect..rect + "#rect".len());
        assert!(
            partial.is_empty(),
            "a partially selected shape must not paint as selected"
        );
    }

    #[test]
    fn selection_masks_box_advances_with_later_text() {
        let text = "#set page(margin: 0pt)\nHello, world!\n";
        let (result, path) = compiled(text);
        let hello = text.find("Hello").expect("Hello in text");
        let world = text.find("world").expect("world in text");

        let hello_min = result
            .selection_masks(&path, hello..hello + "Hello".len())
            .iter()
            .map(|(_, rect)| rect.min.x)
            .min()
            .expect("Hello boxes");
        let world_min = result
            .selection_masks(&path, world..world + "world".len())
            .iter()
            .map(|(_, rect)| rect.min.x)
            .min()
            .expect("world boxes");

        assert!(
            world_min > hello_min,
            "later text sits further right: {world_min:?} > {hello_min:?}"
        );
    }

    #[test]
    fn selection_masks_support_two_disjoint_ranges() {
        let text = "#set page(margin: 0pt)\nHello, world!\n";
        let (result, path) = compiled(text);
        let hello = text.find("Hello").expect("Hello in text");
        let world = text.find("world").expect("world in text");

        let first = result.selection_masks(&path, hello..hello + "Hello".len());
        let second = result.selection_masks(&path, world..world + "world".len());

        assert!(!first.is_empty(), "the first range yields boxes");
        assert!(!second.is_empty(), "the second range yields boxes");
        for (page, rect) in first.iter().chain(second.iter()) {
            assert_eq!(*page, 0);
            assert!(rect.min.x < rect.max.x, "box has horizontal extent");
            assert!(rect.min.y < rect.max.y, "box has vertical extent");
        }
    }

    #[test]
    fn selection_masks_empty_for_collapsed_or_empty_range() {
        let text = "#set page(margin: 0pt)\nHello, world!\n";
        let (result, path) = compiled(text);

        assert!(result.selection_masks(&path, 5..5).is_empty());
    }

    #[test]
    fn selection_masks_unknown_file_is_empty() {
        let text = "#set page(margin: 0pt)\nHello, world!\n";
        let (result, _path) = compiled(text);

        let masks = result.selection_masks(
            &std::env::temp_dir().join("not-part-of-compilation.typ"),
            0..1,
        );
        assert!(masks.is_empty());
    }

    #[test]
    fn selection_masks_cover_a_cjk_glyph() {
        let text = "#set page(margin: 0pt)\n汉字\n";
        let (result, path) = compiled(text);
        let han = text.find('汉').expect("汉 in text");

        let masks = result.selection_masks(&path, han..han + "汉".len());
        assert!(!masks.is_empty(), "a selected CJK glyph yields a box");
        for (page, rect) in &masks {
            assert_eq!(*page, 0);
            assert!(rect.min.x < rect.max.x);
            assert!(rect.min.y < rect.max.y);
        }
    }

    /// The first text run's glyph span anywhere in the page frames, if any.
    fn first_text_span(frame: &Frame) -> Option<Span> {
        for (_, item) in frame.items() {
            match item {
                FrameItem::Group(group) => {
                    if let Some(span) = first_text_span(&group.frame) {
                        return Some(span);
                    }
                }
                FrameItem::Text(text) => {
                    if let Some(glyph) = text.glyphs.first() {
                        return Some(glyph.span.0);
                    }
                }
                FrameItem::Shape(..)
                | FrameItem::Image(..)
                | FrameItem::Link(..)
                | FrameItem::Tag(..) => {}
            }
        }
        None
    }

    #[test]
    fn span_source_resolves_a_frame_text_span() {
        let text = "#set page(margin: 0pt)\nHello, world!\n";
        let (result, path) = compiled(text);
        let document = result.document.as_ref().expect("document");

        let span = document
            .pages()
            .iter()
            .find_map(|page| first_text_span(&page.frame))
            .expect("a text span in the frame");

        let (source_path, range) = result.span_source(span).expect("span resolves");
        assert_eq!(source_path, path, "the span belongs to the main file");
        assert!(range.start < range.end, "the range is non-empty");
        assert!(
            range.end <= text.len(),
            "the range stays inside the source: {range:?} vs {}",
            text.len()
        );
    }

    /// Collects every `FrameItem::Link` rectangle on a page, page-local points,
    /// recursing through groups with their transforms.
    fn collect_link_rects(frame: &Frame, out: &mut Vec<Rect>) {
        for &(item_pos, ref item) in frame.items() {
            match item {
                FrameItem::Group(group) => {
                    let mut inner = Vec::new();
                    collect_link_rects(&group.frame, &mut inner);
                    out.extend(
                        inner
                            .into_iter()
                            .map(|rect| map_frame_rect(rect, group.transform, item_pos)),
                    );
                }
                FrameItem::Link(_, size) => out.push(Rect::from_pos_size(item_pos, *size)),
                _ => {}
            }
        }
    }

    fn page_link_rects(result: &CompileResult, page: usize) -> Vec<Rect> {
        let document = result.document.as_ref().expect("document");
        let mut out = Vec::new();
        collect_link_rects(&document.pages()[page].frame, &mut out);
        out
    }

    fn rect_contains(rect: &Rect, point: Point) -> bool {
        rect.min.x <= point.x
            && point.x <= rect.max.x
            && rect.min.y <= point.y
            && point.y <= rect.max.y
    }

    fn anchor_mid(anchor: &CursorAnchor) -> Point {
        Point::new(
            Abs::pt((anchor.left + anchor.right).to_pt() / 2.0),
            Abs::pt((anchor.top + anchor.bottom).to_pt() / 2.0),
        )
    }

    #[test]
    fn heading_caret_prefers_the_heading_over_the_linked_toc_copy() {
        // The outline re-lays-out "Heading", so the TOC copy shares the heading
        // body's source spans; the caret must stay on the real heading.
        let text = "#set page(margin: 0pt)\n#outline()\n\n= Heading\n\nBody\n";
        let (result, path) = compiled(text);
        let heading = text.find("Heading").expect("Heading in text");

        let anchor = result.cursor_anchor(&path, heading).expect("heading maps");

        let links = page_link_rects(&result, anchor.page);
        assert!(!links.is_empty(), "the outline entry is link-wrapped");
        let mid = anchor_mid(&anchor);
        for rect in &links {
            assert!(
                !rect_contains(rect, mid),
                "caret landed on the link-wrapped TOC copy: {mid:?} inside {rect:?}",
            );
        }
    }

    #[test]
    fn link_wrapped_text_still_anchors_when_it_is_the_only_copy() {
        let text = "#set page(margin: 0pt)\n#link(\"https://example.com\")[clickable]\n";
        let (result, path) = compiled(text);
        let clickable = text.find("clickable").expect("clickable in text");

        let anchor = result
            .cursor_anchor(&path, clickable)
            .expect("link text maps via the fallback");

        assert_eq!(anchor.page, 0);
        assert!(anchor.left < anchor.right, "the node has horizontal extent");
        let links = page_link_rects(&result, anchor.page);
        assert!(
            links
                .iter()
                .any(|rect| rect_contains(rect, anchor_mid(&anchor))),
            "the caret sits on the link-wrapped copy",
        );
    }

    /// Effective source ranges (start, end) of every leaf in `frame` belonging
    /// to `target`, mirroring the production leaf walk.
    fn collect_leaf_ranges(
        world: &WorldState,
        frame: &Frame,
        target: FileId,
        out: &mut Vec<(usize, usize)>,
    ) {
        for (_, item) in frame.items() {
            match item {
                FrameItem::Group(group) => {
                    collect_leaf_ranges(world, &group.frame, target, out);
                }
                FrameItem::Text(text) => {
                    for glyph in &text.glyphs {
                        if let Some(range) = glyph_source_range(world, glyph, target) {
                            out.push(range);
                        }
                    }
                }
                FrameItem::Shape(_, span) => {
                    if span.id() == Some(target)
                        && let Some(range) = world.range(*span)
                    {
                        out.push((range.start, range.end));
                    }
                }
                FrameItem::Image(_, _, span) => {
                    if span.id() == Some(target)
                        && let Some(range) = world.range(*span)
                    {
                        out.push((range.start, range.end));
                    }
                }
                FrameItem::Link(_, _) | FrameItem::Tag(_) => {}
            }
        }
    }

    fn all_leaf_ranges(result: &CompileResult, path: &Path) -> Vec<(usize, usize)> {
        let world = result.world.as_ref().expect("world");
        let document = result.document.as_ref().expect("document");
        let target = world.source_of(path).expect("source").id();
        let mut out = Vec::new();
        for page in document.pages() {
            collect_leaf_ranges(world, &page.frame, target, &mut out);
        }
        out
    }

    #[test]
    fn caret_in_a_rendered_gap_snaps_to_the_nearest_preceding_leaf_end() {
        let text = "Intro\n\n= Heading\n\nBody\n";
        let (result, path) = compiled(text);
        // The heading marker renders nothing, so this byte maps to no leaf.
        let gap = text.find('=').expect("heading marker in text");

        let ranges = all_leaf_ranges(&result, &path);
        assert!(
            !ranges.iter().any(|(s, e)| *s <= gap && gap < *e),
            "gap byte {gap} is covered by no leaf",
        );
        assert!(
            !ranges.iter().any(|(_, e)| *e == gap),
            "gap byte {gap} equals no leaf end",
        );
        let nearest_end = ranges
            .iter()
            .filter(|(_, e)| *e < gap)
            .map(|(_, e)| *e)
            .max()
            .expect("some leaf precedes the gap");

        let snapped = result.cursor_anchor(&path, gap).expect("gap snaps");
        let exact = result
            .cursor_anchor(&path, nearest_end)
            .expect("leaf end maps");
        assert_eq!(
            snapped, exact,
            "the gap snaps to the nearest preceding leaf end {nearest_end}",
        );
    }

    #[test]
    fn caret_before_any_leaf_stays_none() {
        let text = "#set text(size: 11pt)\nHello\n";
        let (result, path) = compiled(text);
        let gap = text.find('#').expect("# in text") + 1;

        let ranges = all_leaf_ranges(&result, &path);
        assert!(
            ranges.iter().all(|(_, e)| *e > gap),
            "no leaf ends at or before byte {gap}",
        );
        assert!(
            !ranges.iter().any(|(s, e)| *s <= gap && gap < *e),
            "byte {gap} is covered by no leaf",
        );
        assert!(
            result.cursor_anchor(&path, gap).is_none(),
            "a byte before all rendered content stays None",
        );
    }

    #[test]
    fn preceding_tier_prefers_the_unlinked_heading_over_the_toc_copy() {
        let text = "#set page(margin: 0pt)\n#outline()\n\n= Heading\n\nBody\n";
        let (result, path) = compiled(text);
        let heading = text.find("Heading").expect("Heading in text");
        // The blank line just past the heading: no leaf covers it, but both the
        // TOC copy and the heading body share a span end at the heading's end.
        let gap = heading + "Heading".len() + 1;

        let ranges = all_leaf_ranges(&result, &path);
        assert!(
            !ranges.iter().any(|(s, e)| *s <= gap && gap < *e),
            "gap byte {gap} is covered by no leaf",
        );
        assert!(
            !ranges.iter().any(|(_, e)| *e == gap),
            "gap byte {gap} equals no leaf end",
        );

        let anchor = result
            .cursor_anchor(&path, gap)
            .expect("gap snaps to a preceding leaf");
        let links = page_link_rects(&result, anchor.page);
        assert!(!links.is_empty(), "the outline entry is link-wrapped");
        let mid = anchor_mid(&anchor);
        for rect in &links {
            assert!(
                !rect_contains(rect, mid),
                "the preceding tier landed on the linked TOC copy: {mid:?} inside {rect:?}",
            );
        }
    }
}
