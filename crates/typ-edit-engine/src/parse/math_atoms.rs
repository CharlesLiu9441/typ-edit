//! Math atoms: the units the editor's line wrap measures.
//!
//! The wrap may break *before* any atom, never inside one. Every leaf of the
//! math syntax tree is an atom, with two glues that keep a function name and
//! its argument opener together and a number with its unit:
//!
//! - A [`SyntaxKind::MathIdent`] followed by the `(` of its call — a
//!   [`SyntaxKind::LeftParen`], or the `(` the parser folds into a
//!   [`SyntaxKind::MathDelimited`] when insignificant whitespace separates the
//!   two — forms one atom spanning the name through the `(`. A break *after*
//!   the `(` is allowed: the `(` is the atom's last byte.
//! - A numeric leaf (a [`SyntaxKind::Numeric`], or a [`SyntaxKind::MathText`]
//!   starting with a digit) extends over an immediately adjacent unit token
//!   (`pt`, `mm`, `cm`, `in`, `deg`, `rad`, `em`, `fr`, `%`) — `12pt`, `50%`.
//! - Every other leaf (identifiers, operators, commas, parens, delimiters,
//!   [`SyntaxKind::MathShorthand`], single characters) is its own atom. A comma
//!   is a leaf, so a break may legally fall before or after it.
//!
//! Whitespace ([`SyntaxKind::Space`]) and the `#` code escape
//! ([`SyntaxKind::Hash`]) emit no atom, and leaves under an embedded code
//! region (`#(...)`, `#x`) never enter math mode, so nothing is emitted there.
//! The `$` delimiters sit in [`SyntaxKind::Equation`], outside the `Math` node,
//! so they are not atoms either. Ranges are sorted and disjoint, and only
//! *overlapping* ranges are merged: distinct adjacent atoms stay separate, which
//! is what lets the wrap break between `2` and `)`.

use std::ops::Range;

use typ_edit_model::{BytePos, ByteRange};
use typst::syntax::SyntaxKind;

/// Folds the ordered math leaves collected by the fused visitor into the byte
/// ranges of math atoms, sorted and non-overlapping.
pub(super) fn atoms(leaves: Vec<Leaf>) -> Vec<ByteRange> {
    let mut atoms = Vec::with_capacity(leaves.len());
    let mut i = 0;
    while i < leaves.len() {
        let (kind, range, text) = (&leaves[i].kind, &leaves[i].range, &leaves[i].text);
        if is_ignored(*kind) {
            i += 1;
            continue;
        }

        // `consumed` is how many leaves the atom folds in: one, plus a glued
        // `(` for an identifier or a unit for a number.
        let mut end = range.end;
        let mut consumed = 1;
        if *kind == SyntaxKind::MathIdent {
            if let Some(paren) = following_left_paren(&leaves, i + 1) {
                end = leaves[paren].range.end;
                consumed = paren - i + 1;
            }
        } else if is_numeric(*kind, text)
            && let Some(unit) = following_adjacent_unit(&leaves, i + 1, end)
        {
            end = leaves[unit].range.end;
            consumed = unit - i + 1;
        }
        atoms.push(range.start..end);
        i += consumed;
    }

    atoms.sort_by_key(|range| range.start);
    merge_overlaps(&mut atoms);
    atoms
        .into_iter()
        .map(|range| BytePos(range.start)..BytePos(range.end))
        .collect()
}

/// One math leaf in document order, carrying its text for the glue tests.
///
/// Collected by the fused visitor (which owns the tree walk) and folded into
/// atoms by [`atoms`].
pub(super) struct Leaf {
    pub(super) kind: SyntaxKind,
    pub(super) range: Range<usize>,
    pub(super) text: String,
}

/// The index of the `(` that opens `leaves[from]`'s call, if any.
///
/// Only [`SyntaxKind::Space`] may sit between the name and the `(`; any other
/// leaf (a `\`, a delimiter, a value) ends the search.
fn following_left_paren(leaves: &[Leaf], from: usize) -> Option<usize> {
    let mut j = from;
    while matches!(leaves.get(j).map(|leaf| leaf.kind), Some(SyntaxKind::Space)) {
        j += 1;
    }
    let leaf = leaves.get(j)?;
    let is_paren = leaf.kind == SyntaxKind::LeftParen
        || (leaf.kind == SyntaxKind::MathText && leaf.text == "(");
    is_paren.then_some(j)
}

/// The index of a unit starting exactly at `end`.
fn following_adjacent_unit(leaves: &[Leaf], from: usize, end: usize) -> Option<usize> {
    let leaf = leaves.get(from)?;
    (leaf.range.start == end && is_unit(leaf.kind, &leaf.text)).then_some(from)
}

/// Whether the leaf is a number: a `Numeric` token, or math text beginning with
/// a digit (`3.14`).
fn is_numeric(kind: SyntaxKind, text: &str) -> bool {
    match kind {
        SyntaxKind::Numeric => true,
        SyntaxKind::MathText => text.chars().next().is_some_and(|c| c.is_numeric()),
        _ => false,
    }
}

/// Whether the leaf is a unit suffix the lexer would accept after a number.
fn is_unit(kind: SyntaxKind, text: &str) -> bool {
    match kind {
        SyntaxKind::MathIdent => matches!(
            text,
            "pt" | "mm" | "cm" | "in" | "deg" | "rad" | "em" | "fr"
        ),
        SyntaxKind::MathText => text == "%",
        _ => false,
    }
}

/// Whitespace and the code escape emit no atom.
fn is_ignored(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::Space | SyntaxKind::Hash)
}

/// Merges ranges that overlap; distinct adjacent atoms are preserved.
fn merge_overlaps(ranges: &mut Vec<Range<usize>>) {
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if range.start < last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    *ranges = merged;
}
