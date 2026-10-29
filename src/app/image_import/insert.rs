//! Pure insertion-position classifier for image paste/drag: decides the snippet
//! form (markup vs code) and refuses positions where an insert would corrupt the
//! source (math, raw blocks, comments, or an identifier split).

use std::ops::Range;
use typ_edit_model::{HighlightKind, ParseResult};

/// The syntax form an image snippet is inserted as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageForm {
    /// Markup mode: `#image("...")`.
    Markup,
    /// Code mode: `image("...")` (no leading `#`).
    Code,
}

/// Why an image insertion at a byte position was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RejectReason {
    /// The position lies inside a math range (`$...$`).
    Math,
    /// The position lies inside a raw block or a comment.
    Blocked,
    /// A Unicode alphanumeric or `_` abuts the position, so the snippet would
    /// split an identifier.
    AdjacentIdentifier,
}

/// The classification of an image insertion at one byte position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InsertDecision {
    /// Insert the snippet in the given syntax form.
    Insert(ImageForm),
    /// Refuse and leave the document untouched.
    Reject(RejectReason),
}

/// Classifies an image insertion at `byte` in `text`.
///
/// Rules, in order: a position inside `math` is refused; inside `blocked` (raw
/// blocks and comments) it is refused; abutting a Unicode alphanumeric or `_` it
/// is refused because the snippet would split an identifier; inside a `code`
/// range the snippet drops its `#`; anywhere else it keeps markup form.
///
/// `byte` is clamped to `text.len()` and snapped to the nearest `char` boundary
/// first, so a caret reported mid-codepoint can never slice the string.
pub(crate) fn decide_insertion(
    text: &str,
    byte: usize,
    code: &[Range<usize>],
    math: &[Range<usize>],
    blocked: &[Range<usize>],
) -> InsertDecision {
    let byte = snap_to_boundary(text, byte);
    if in_any(math, byte) {
        return InsertDecision::Reject(RejectReason::Math);
    }
    if in_any(blocked, byte) {
        return InsertDecision::Reject(RejectReason::Blocked);
    }
    if abuts_identifier(text, byte) {
        return InsertDecision::Reject(RejectReason::AdjacentIdentifier);
    }
    if in_any(code, byte) {
        InsertDecision::Insert(ImageForm::Code)
    } else {
        InsertDecision::Insert(ImageForm::Markup)
    }
}

/// The byte ranges where an image must not be inserted for a non-math reason.
///
/// The engine's `blocked_ranges` is the union of code, math and raw; subtracting
/// the code and math ranges leaves raw. Comment spans (which the engine does not
/// fold into `blocked_ranges`) are taken from the highlight kinds.
pub(super) fn image_blocked_ranges(parse: &ParseResult) -> Vec<Range<usize>> {
    let layout = &parse.layout;
    let mut holes: Vec<Range<usize>> = layout
        .code_ranges
        .iter()
        .chain(&layout.math_ranges)
        .map(|range| range.start.0..range.end.0)
        .collect();
    holes.sort_by_key(|range| range.start);
    let blocked_source: Vec<Range<usize>> = layout
        .blocked_ranges
        .iter()
        .map(|range| range.start.0..range.end.0)
        .collect();
    let mut blocked = subtract_ranges(&blocked_source, &holes);
    blocked.extend(
        parse
            .highlights
            .iter()
            .filter(|(_, kind)| *kind == HighlightKind::Comment)
            .map(|(range, _)| range.start.0..range.end.0),
    );
    blocked
}

/// Every part of the start-sorted `base` not covered by the start-sorted `holes`.
fn subtract_ranges(base: &[Range<usize>], holes: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for range in base {
        let mut cursor = range.start;
        for hole in holes {
            if hole.end <= cursor {
                continue;
            }
            if hole.start >= range.end {
                break;
            }
            if hole.start > cursor {
                out.push(cursor..hole.start);
            }
            cursor = cursor.max(hole.end);
            if cursor >= range.end {
                break;
            }
        }
        if cursor < range.end {
            out.push(cursor..range.end);
        }
    }
    out
}

/// `byte` clamped to `text.len()` and moved to the nearest `char` boundary.
fn snap_to_boundary(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    if text.is_char_boundary(byte) {
        return byte;
    }
    // `byte < len` and not a boundary, so a boundary exists on both sides.
    let floor = (0..byte)
        .rev()
        .find(|&index| text.is_char_boundary(index))
        .unwrap_or(0);
    let ceil = (byte..=text.len())
        .find(|&index| text.is_char_boundary(index))
        .unwrap_or(text.len());
    if byte - floor <= ceil - byte {
        floor
    } else {
        ceil
    }
}

/// Whether `byte` falls inside any half-open `[start, end)` range.
fn in_any(ranges: &[Range<usize>], byte: usize) -> bool {
    ranges
        .iter()
        .any(|range| range.start <= byte && byte < range.end)
}

/// Whether the `char` immediately before or after `byte` is an identifier
/// character (Unicode alphanumeric or `_`).
fn abuts_identifier(text: &str, byte: usize) -> bool {
    text[..byte]
        .chars()
        .next_back()
        .into_iter()
        .chain(text[byte..].chars().next())
        .any(is_identifier_char)
}

/// A Typst identifier character: a Unicode letter/digit or `_`.
fn is_identifier_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}
