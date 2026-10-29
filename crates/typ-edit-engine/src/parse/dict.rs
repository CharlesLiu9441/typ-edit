//! Dictionary-key completion for direction-style value slots.
//!
//! When the caret sits in a fresh key slot of an anonymous dictionary literal
//! that is the value of a `margin` / `inset` / `padding` argument
//! (`#set page(margin: (|`), the curated keys of that dictionary are offered.

use super::complete::first_ident;
use typ_edit_model::{CharCount, CompletionItem, CompletionKind};
use typst::syntax::{LinkedNode, SyntaxKind};

/// Curated keys of a direction-style dictionary parameter.
///
/// Typst exposes no static, world-free description of the keys a `margin` /
/// `inset` / `padding` dictionary accepts, so the set is hard-coded: the four
/// sides plus the `x` / `y` shorthand pairs. Only these parameter names are
/// recognized; any other parameter's dictionary value falls back to its
/// ordinary value candidates.
const DIRECTION_KEYS: &[&str] = &["top", "bottom", "left", "right", "x", "y"];

/// The curated dictionary keys of `param`, if it is direction-style.
fn direction_keys(param: &str) -> Option<&'static [&'static str]> {
    matches!(param, "margin" | "inset" | "padding").then_some(DIRECTION_KEYS)
}

/// The anonymous dictionary literal that is the immediate value of `named` and
/// whose cursor position is a fresh key slot, if any.
///
/// `leaf` is the cursor's leaf. Only a group that is the named argument's
/// *direct* value qualifies, so a literal nested inside one of its keys
/// (`margin: (x: (top: 1pt)))`) does not inherit the outer keys.
pub(super) fn dict_group<'a>(
    leaf: &LinkedNode<'a>,
    named: &LinkedNode<'a>,
    cursor: usize,
) -> Option<LinkedNode<'a>> {
    let args = named.parent()?;
    let mut node = leaf.clone();
    let group = loop {
        let parent = node.parent()?.clone();
        if std::ptr::eq(parent.get(), args.get()) {
            return None;
        }
        if matches!(
            parent.kind(),
            SyntaxKind::Dict | SyntaxKind::Array | SyntaxKind::Parenthesized
        ) {
            break parent;
        }
        node = parent;
    };
    let is_named_value = group
        .parent()
        .is_some_and(|parent| std::ptr::eq(parent.get(), named.get()));
    (is_named_value && key_position(&group, cursor)).then_some(group)
}

/// Completion items for the keys of a direction-style dictionary, skipping the
/// keys already written in `group`. Each inserts `key: ` and leaves the caret
/// after the space, matching the parameter snippets.
pub(super) fn dict_key_items(param: &str, group: &LinkedNode<'_>) -> Option<Vec<CompletionItem>> {
    let keys = direction_keys(param)?;
    let existing: Vec<String> = group
        .children()
        .filter(|child| child.kind() == SyntaxKind::Named)
        .filter_map(|child| first_ident(&child))
        .collect();
    let items: Vec<CompletionItem> = keys
        .iter()
        .filter(|key| !existing.iter().any(|name| name.as_str() == **key))
        .map(|key| CompletionItem {
            name: (*key).to_owned(),
            kind: CompletionKind::Param,
            insert: format!("{key}: "),
            cursor_back: CharCount(0),
            docs: None,
        })
        .collect();
    (!items.is_empty()).then_some(items)
}

/// Whether `node` opens a parenthesized group. An unclosed `(` is reported as
/// an `Error` leaf carrying the bracket text, exactly as in `Args`.
fn is_open_paren(node: &LinkedNode<'_>) -> bool {
    node.kind() == SyntaxKind::LeftParen
        || (node.kind().is_error() && node.leaf_text().as_str() == "(")
}

/// Whether the cursor sits where a new dictionary key may be typed: right after
/// the opening `(` or a `,`, or inside a partially typed key before its colon.
/// A caret in a key's value region does not qualify.
fn key_position(group: &LinkedNode<'_>, cursor: usize) -> bool {
    let children: Vec<LinkedNode<'_>> = group.children().collect();
    let Some(index) = children
        .iter()
        .rposition(|child| child.range().start < cursor && !child.kind().is_trivia())
    else {
        return false;
    };
    let previous = &children[index];
    match previous.kind() {
        SyntaxKind::LeftParen | SyntaxKind::Comma => true,
        SyntaxKind::Ident => opens_key_slot(&children[..index]),
        SyntaxKind::Named | SyntaxKind::Keyed => cursor < key_end(previous),
        SyntaxKind::Error => is_open_paren(previous) || opens_key_slot(&children[..index]),
        _ => false,
    }
}

/// Whether the last non-trivia sibling before `prefix` is the opening `(` or a
/// comma (or there is none yet).
fn opens_key_slot(prefix: &[LinkedNode<'_>]) -> bool {
    prefix
        .iter()
        .rev()
        .find(|child| !child.kind().is_trivia())
        .is_none_or(|child| child.kind() == SyntaxKind::Comma || is_open_paren(child))
}

/// Byte offset of the colon separating a named pair's key from its value.
fn key_end(pair: &LinkedNode<'_>) -> usize {
    pair.children()
        .find(|child| child.kind() == SyntaxKind::Colon)
        .map_or(pair.range().end, |colon| colon.range().start)
}
