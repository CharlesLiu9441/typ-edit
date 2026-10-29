//! Extracts a [`SymbolNode`] from a single syntax-tree node.
//!
//! The walker in the parent module owns the frame stack and the traversal; this
//! module owns the identifier-context rule that decides whether a node *is* a
//! symbol and, if so, which kind and range.

use std::ops::Range;

use typ_edit_model::{BytePos, SymbolKind, SymbolNode};
use typst::syntax::{LinkedNode, SyntaxKind, ast};

/// The identifier context inherited while descending the tree. Entering a
/// marker node changes the context (restored on exit).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum IdentContext {
    /// A value reference site; plain identifiers are not definitions.
    Ref,
    /// A closure's name; an identifier here is a function.
    Func,
    /// A binding target; an identifier here is a variable.
    Var,
    /// A closure parameter list; identifiers and named parameters are variables.
    Params,
}

/// The symbol `node` carries in `context` at `fn_depth`, if any.
///
/// Only an `Ident`, a `Label` and a heading's own `Markup` become symbols; every
/// other node is structural and the walker dispatches on its kind instead. A
/// `Var` identifier is a key symbol only at the top level (`fn_depth == 0`); one
/// inside a function body is a local binding and is skipped.
pub(super) fn symbol(
    node: &LinkedNode,
    context: IdentContext,
    fn_depth: usize,
) -> Option<SymbolNode> {
    match node.kind() {
        SyntaxKind::Ident => {
            let kind = match context {
                IdentContext::Func => SymbolKind::Function,
                IdentContext::Params => SymbolKind::Parameter,
                IdentContext::Var if fn_depth == 0 => SymbolKind::Variable,
                IdentContext::Var | IdentContext::Ref => return None,
            };
            let name = node.get().cast::<ast::Ident>()?.get().to_string();
            Some(leaf(name, kind, node.range()))
        }
        SyntaxKind::Label => {
            // A label directly after `[`, `{`, `(`, `,` or `:` — or after a
            // keyword — is a code-context label (e.g. an array item), not a
            // document label.
            if let Some(prev) = node.prev_sibling_kind()
                && (matches!(
                    prev,
                    SyntaxKind::LeftBracket
                        | SyntaxKind::LeftBrace
                        | SyntaxKind::LeftParen
                        | SyntaxKind::Comma
                        | SyntaxKind::Colon
                ) || prev.is_keyword())
            {
                return None;
            }
            let name = node.get().cast::<ast::Label>()?.get().to_string();
            Some(leaf(name, SymbolKind::Label, node.range()))
        }
        SyntaxKind::Markup => {
            // Only a heading's own markup node becomes a heading symbol; the
            // name is its full text and the depth is the marker length.
            let parent = node.parent()?;
            if parent.kind() != SyntaxKind::Heading {
                return None;
            }
            let name = node.get().full_text();
            if name.is_empty() {
                return None;
            }
            let level = parent
                .get()
                .cast::<ast::Heading>()
                .map_or(1, |heading| heading.depth().get() as u16);
            Some(leaf(
                name.to_string(),
                SymbolKind::Heading(level),
                node.range(),
            ))
        }
        _ => None,
    }
}

/// Leaf tokens that are never entered: trivia, keywords and syntax errors.
pub(super) fn is_plain_token(kind: SyntaxKind) -> bool {
    kind.is_trivia() || kind.is_keyword() || kind.is_error()
}

/// Builds a symbol with no children yet.
fn leaf(name: String, kind: SymbolKind, range: Range<usize>) -> SymbolNode {
    SymbolNode {
        name,
        kind,
        range: BytePos(range.start)..BytePos(range.end),
        children: Vec::new(),
    }
}
