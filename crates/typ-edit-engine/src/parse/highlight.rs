//! Syntax highlighting: maps the typst syntax tree to editor highlight spans.

use std::ops::Range;

use super::complete::DefinedNames;
use crate::stdlib::is_math_symbol;
use typ_edit_model::{BytePos, ByteRange, HighlightKind};
use typst::syntax::{LinkedNode, SyntaxKind, Tag, highlight};

/// Alignment-family constants (`alignment` values and their `x`/`y`
/// shorthands) that the upstream walker does not tag.
///
/// Typst's `highlight` only recognizes syntax *keywords* (`none`, `auto`,
/// `true`, `false`, ...). The alignment constructors are ordinary identifiers
/// bound in the global scope (`left`, `center`, `right`, `start`, `end`, `top`,
/// `horizon`, `bottom`), so without this table they render as plain variables.
///
/// The set is deliberately hard-coded and lexical rather than type- or
/// world-derived: the highlight walk is replayed incrementally over a reparsed
/// window and must stay a pure function of the tree, with no caret or
/// surrounding context. Because the rule matches an identifier leaf's text
/// alone, a user binding that reuses one of these names (`#let center = 1`) is
/// highlighted too; that false positive is accepted. `vertical` is intentionally
/// absent — it is an axis *string*, not an alignment constant, and `auto` is
/// already a syntax keyword.
const ALIGNMENT_CONSTANTS: &[&str] = &[
    "start", "left", "center", "right", "end", "top", "horizon", "bottom",
];

/// Collects `(byte range, kind)` highlight spans for the leaves of the tree.
///
/// Tags inherit down the tree so that e.g. the characters of a `Strong` node keep its
/// tag on the contained `Text` leaf (see the upstream `highlight_html` walker). Leaf
/// nodes without a tag and empty ranges are skipped.
pub(crate) fn highlights(
    root: &LinkedNode<'_>,
    defined: &DefinedNames,
) -> Vec<(Range<usize>, HighlightKind)> {
    let mut walk = HighlightWalk {
        window: None,
        defined,
        out: Vec::new(),
    };
    walk.visit(root, None);
    walk.out
}

/// Collects the highlight spans that fall inside `window`, using the same
/// semantics as [`highlights`] but visiting only the path from the root down to
/// the window and the window's own subtree.
///
/// The caller (the incremental parser) only ever passes a window whose start and
/// end coincide with node boundaries at the level the parser reparsed, so a node
/// is always either fully inside `window`, fully before it, or fully after it;
/// pruning the other two cases cannot drop a leaf that straddles the boundary.
/// The inherited [`Tag`] is still accumulated from the root down, and the
/// emitted spans come out in document order — identical to filtering the output
/// of a full [`highlights`] walk to the window.
pub(super) fn highlights_in(
    root: &LinkedNode<'_>,
    window: Range<usize>,
    defined: &DefinedNames,
) -> Vec<(Range<usize>, HighlightKind)> {
    let mut walk = HighlightWalk {
        window: Some(&window),
        defined,
        out: Vec::new(),
    };
    walk.visit(root, None);
    walk.out
}

/// Rewraps the untyped spans the walk emits into byte-space newtypes.
///
/// The walk itself stays in `Range<usize>` — the incremental splice does its
/// shift math in `usize` — and the typed contract begins at this boundary.
pub(super) fn typed(spans: Vec<(Range<usize>, HighlightKind)>) -> Vec<(ByteRange, HighlightKind)> {
    spans
        .into_iter()
        .map(|(range, kind)| (BytePos(range.start)..BytePos(range.end), kind))
        .collect()
}

/// Shared walk for [`highlights`] and [`highlights_in`].
///
/// `window` is `None` for a full walk; when `Some`, subtrees lying entirely
/// outside it are pruned, and only spans fully contained in it are emitted.
struct HighlightWalk<'a> {
    window: Option<&'a Range<usize>>,
    defined: &'a DefinedNames,
    out: Vec<(Range<usize>, HighlightKind)>,
}

impl HighlightWalk<'_> {
    fn visit(&mut self, node: &LinkedNode<'_>, inherited: Option<Tag>) {
        let range = node.range();
        if let Some(window) = self.window
            && (range.end <= window.start || range.start >= window.end)
        {
            return;
        }

        // A math field access (`eq.ast`, `plus.minus`) is one symbol token, not a
        // base name plus a field; a document-local binding of the head wins.
        if node.kind() == SyntaxKind::MathFieldAccess
            && !self
                .defined
                .contains(&leftmost_math_ident(node).unwrap_or_default())
            && is_math_symbol(&node_text(node))
        {
            if range.start < range.end && within(&range, self.window) {
                self.out.push((range, HighlightKind::MathSymbol));
            }
            return;
        }

        let tag = highlight(node).or(inherited);
        // A named argument's name (`name` in `name: value`) and a dictionary
        // literal's field name get their own category while the value keeps the
        // normal tags. Only call argument lists (`Args`) and dictionary literals
        // (`Dict`) qualify. A named pair always forces the parenthesized group
        // to `Dict`; named fields in patterns (`Destructuring`) and closure
        // parameters (`Params`) do not match.
        if node.kind() == SyntaxKind::Named
            && node
                .parent()
                .is_some_and(|parent| matches!(parent.kind(), SyntaxKind::Args | SyntaxKind::Dict))
        {
            if let Some(name) = node.children().next() {
                let range = name.range();
                if range.start < range.end && within(&range, self.window) {
                    self.out.push((range, HighlightKind::ParamName));
                }
            }
            for child in node.children().skip(1) {
                self.visit(&child, tag);
            }
            return;
        }
        if node.children().len() == 0 {
            let leaf_kind = math_symbol_kind(node, self.defined)
                .or_else(|| tag.map(kind))
                .or_else(|| fallback_kind(node, self.defined));
            if let Some(kind) = leaf_kind
                && range.start < range.end
                && within(&range, self.window)
            {
                self.out.push((range, kind));
            }
        } else {
            for child in node.children() {
                self.visit(&child, tag);
            }
        }
    }
}

/// A math identifier naming a Typst symbol, unless the document rebinds it.
fn math_symbol_kind(node: &LinkedNode<'_>, defined: &DefinedNames) -> Option<HighlightKind> {
    (node.kind() == SyntaxKind::MathIdent
        && !defined.contains(node.leaf_text().as_str())
        && is_math_symbol(node.leaf_text().as_str()))
    .then_some(HighlightKind::MathSymbol)
}

/// The dotted text of a subtree (`eq.ast`) built from its leaf texts.
fn node_text(node: &LinkedNode<'_>) -> String {
    if node.children().len() == 0 {
        return node.leaf_text().to_string();
    }
    let mut text = String::new();
    for child in node.children() {
        text.push_str(&node_text(&child));
    }
    text
}

/// The leftmost `MathIdent` in the subtree, i.e. a field access's base name.
fn leftmost_math_ident(node: &LinkedNode<'_>) -> Option<String> {
    if node.kind() == SyntaxKind::MathIdent {
        return Some(node.leaf_text().to_string());
    }
    node.children()
        .find_map(|child| leftmost_math_ident(&child))
}

/// The highlight of an identifier the upstream walker left untagged.
///
/// A document-local binding or reference wins over an alignment constant: a
/// `#let center = 1` makes `center` a variable, not the alignment value.
fn fallback_kind(node: &LinkedNode<'_>, defined: &DefinedNames) -> Option<HighlightKind> {
    if node.kind() != SyntaxKind::Ident {
        return None;
    }
    let text = node.leaf_text();
    if defined.contains(text.as_str()) {
        Some(HighlightKind::Interpolated)
    } else if ALIGNMENT_CONSTANTS.contains(&text.as_str()) {
        Some(HighlightKind::Keyword)
    } else {
        None
    }
}

/// Whether `range` is fully contained in `window` (`None` means the whole file).
fn within(range: &Range<usize>, window: Option<&Range<usize>>) -> bool {
    window.is_none_or(|window| range.start >= window.start && range.end <= window.end)
}

/// Maps every [`Tag`] to its UI-facing counterpart (the enums are kept in sync).
fn kind(tag: Tag) -> HighlightKind {
    match tag {
        Tag::Comment => HighlightKind::Comment,
        Tag::Punctuation => HighlightKind::Punctuation,
        Tag::Escape => HighlightKind::Escape,
        Tag::Strong => HighlightKind::Strong,
        Tag::Emph => HighlightKind::Emph,
        Tag::Link => HighlightKind::Link,
        Tag::Raw => HighlightKind::Raw,
        Tag::Label => HighlightKind::Label,
        Tag::Ref => HighlightKind::Ref,
        Tag::Heading => HighlightKind::Heading,
        Tag::ListMarker => HighlightKind::ListMarker,
        Tag::ListTerm => HighlightKind::ListTerm,
        Tag::MathDelimiter => HighlightKind::MathDelimiter,
        Tag::MathOperator => HighlightKind::MathOperator,
        Tag::MathGroupingParens => HighlightKind::MathGroupingParens,
        Tag::Keyword => HighlightKind::Keyword,
        Tag::Operator => HighlightKind::Operator,
        Tag::Number => HighlightKind::Number,
        Tag::String => HighlightKind::String,
        Tag::Function => HighlightKind::Function,
        Tag::Interpolated => HighlightKind::Interpolated,
        Tag::Error => HighlightKind::Error,
    }
}
