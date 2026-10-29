//! Cursor classification: a small, dot-access-free subset of tinymist's
//! `classify_syntax` (`crates/tinymist-analysis/src/syntax/matcher.rs`).
//!
//! The cursor is a UTF-8 byte offset (the editor reports the caret-side byte of
//! the glyph under the pointer). [`classify`] resolves that byte to a leaf, walks
//! to the enclosing expression and reports the subset of classes this crate
//! acts on. `classified_expr` exposes the adjusted expression node itself so a
//! caller (rename) can recognise a field access before mapping to the enum.
//!
//! Ported faithfully but deliberately smaller than tinymist:
//! - the `+1`-byte end correction selects the leaf preceding a delimiter,
//! - `Ident`/`MathIdent` on the right of a dot are promoted to the whole
//!   `FieldAccess` (so rename can refuse them),
//! - the callee of a call/set rule is reported as [`SyntaxClass::Callee`].
//!
//! Out of scope (v1, no behaviour for these): incomplete dot access (`a.`),
//! bare `@`, ref suffix colons, math-text identifiers and error labels.

use typst::syntax::ast::{self, AstNode};
use typst::syntax::{LinkedNode, Side, Source, SyntaxKind};

/// The subset of cursor classifications this crate acts on.
///
/// `Other` covers every expression the subset does not act on (text, math,
/// field access, ...); callers treat it as "not renameable".
pub enum SyntaxClass<'a> {
    /// A variable access expression: an `Ident`/`MathIdent`.
    VarAccess(LinkedNode<'a>),
    /// The callee/target of a call or set rule: `f` in `f(x)` / `set f(..)`.
    Callee(LinkedNode<'a>),
    /// A label expression: `<intro>`.
    Label(LinkedNode<'a>),
    /// A reference expression: `@intro`.
    Ref(LinkedNode<'a>),
    /// A string path inside a module import: `"utils.typ"`.
    ImportPath(LinkedNode<'a>),
    /// A classifiable expression this subset does not act on.
    Other,
}

/// Resolves the expression under `byte`, or `None` when the cursor is not on an
/// expression at all.
///
/// The returned node is the *adjusted* expression: parenthesized chains are
/// unwrapped and the field of a field access is promoted to the whole access.
pub fn classified_expr(source: &Source, byte: usize) -> Option<LinkedNode<'_>> {
    let root = LinkedNode::new(source.root());
    let cursor = byte.min(source.text().len());
    let mut node = root.leaf_at(cursor, Side::Before)?;

    // The editor reports the token's end byte. When that byte sits one past a
    // mark that abuts the previous leaf, attribute it to the previous leaf.
    if cursor == node.offset() + 1
        && is_mark(node.kind())
        && let Some(prev) = node.prev_leaf()
        && prev.range().end == node.offset()
    {
        node = prev;
    }

    // Move to the first ancestor that is an expression.
    while !node.is::<ast::Expr>() {
        node = node.parent()?.clone();
    }

    // Unwrap parenthesized chains: `(x)` classifies as `x`.
    while let Some(paren) = node.cast::<ast::Parenthesized>() {
        let inner = paren.expr().span();
        node = node.find(inner)?;
    }

    // Promote the field of a field access to the whole access expression.
    if let Some(parent) = node.parent() {
        let is_field = parent
            .cast::<ast::FieldAccess>()
            .is_some_and(|access| access.field().span() == node.span())
            || parent
                .cast::<ast::MathFieldAccess>()
                .is_some_and(|access| access.field().span() == node.span());
        if is_field {
            node = parent.clone();
        }
    }

    Some(node)
}

/// Classifies the expression under `byte` into the actionable subset.
pub fn classify(source: &Source, byte: usize) -> Option<SyntaxClass<'_>> {
    let node = classified_expr(source, byte)?;

    // A callee/target is classified by its parent call or set rule.
    if let Some(parent) = node.parent() {
        if let Some(call) = parent.cast::<ast::FuncCall>()
            && call.callee().span() == node.span()
        {
            return Some(SyntaxClass::Callee(parent.find(call.callee().span())?));
        }
        if let Some(call) = parent.cast::<ast::MathCall>() {
            let callee = call.callee().to_untyped().span();
            if callee == node.span() {
                return Some(SyntaxClass::Callee(parent.find(callee)?));
            }
        }
        if let Some(set) = parent.cast::<ast::SetRule>()
            && set.target().span() == node.span()
        {
            return Some(SyntaxClass::Callee(parent.find(set.target().span())?));
        }
    }

    let expr = node.cast::<ast::Expr>()?;
    Some(match expr {
        ast::Expr::Label(_) => SyntaxClass::Label(node),
        ast::Expr::Ref(_) => SyntaxClass::Ref(node),
        ast::Expr::FuncCall(call) => SyntaxClass::Callee(node.find(call.callee().span())?),
        ast::Expr::MathCall(call) => {
            SyntaxClass::Callee(node.find(call.callee().to_untyped().span())?)
        }
        ast::Expr::SetRule(set) => SyntaxClass::Callee(node.find(set.target().span())?),
        ast::Expr::Ident(_) | ast::Expr::MathIdent(_) => SyntaxClass::VarAccess(node),
        ast::Expr::Str(_) => {
            if node
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::ModuleImport)
            {
                SyntaxClass::ImportPath(node)
            } else {
                SyntaxClass::Other
            }
        }
        _ => SyntaxClass::Other,
    })
}

/// Checks whether a leaf is a short delimiter/keyword token the end correction
/// should step over. Ported verbatim from tinymist's `is_mark`.
fn is_mark(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        MathAlignPoint
            | Plus
            | Minus
            | Dot
            | Dots
            | Arrow
            | Not
            | And
            | Or
            | Eq
            | EqEq
            | ExclEq
            | Lt
            | LtEq
            | Gt
            | GtEq
            | PlusEq
            | HyphEq
            | StarEq
            | SlashEq
            | LeftBrace
            | RightBrace
            | LeftBracket
            | RightBracket
            | LeftParen
            | RightParen
            | Slash
            | Hat
            | Comma
            | Semicolon
            | Colon
            | Hash
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_at(text: &str, byte: usize) -> Option<(&'static str, std::ops::Range<usize>)> {
        let source = Source::detached(text);
        Some(match classify(&source, byte)? {
            SyntaxClass::VarAccess(node) => ("var", node.range()),
            SyntaxClass::Callee(node) => ("callee", node.range()),
            SyntaxClass::Label(node) => ("label", node.range()),
            SyntaxClass::Ref(node) => ("ref", node.range()),
            SyntaxClass::ImportPath(node) => ("import", node.range()),
            SyntaxClass::Other => ("other", 0..0),
        })
    }

    #[test]
    fn ident_in_the_middle_and_at_its_end_classify_as_var_access() {
        let text = "#let foo = 1";
        // Cursor on the middle of `foo` still resolves the whole ident.
        assert_eq!(class_at(text, 7), Some(("var", 5..8)));
        // The token's end byte resolves to the same ident.
        assert_eq!(class_at(text, 8), Some(("var", 5..8)));
    }

    #[test]
    fn end_correction_resolves_the_leaf_before_a_mark() {
        // `)` ends at byte 5; the correction attributes it to `x`.
        let text = "#f(x)";
        let source = Source::detached(text);
        let classified = classified_expr(&source, 5).expect("expr");
        assert_eq!(classified.kind(), SyntaxKind::Ident);
        assert_eq!(classified.range(), 3..4);
    }

    #[test]
    fn field_access_is_promoted_to_the_whole_access() {
        let text = "#foo.bar";
        let source = Source::detached(text);
        // The field `bar`...
        let classified = classified_expr(&source, 8).expect("expr");
        assert_eq!(classified.kind(), SyntaxKind::FieldAccess);
        assert_eq!(classified.range(), 1..8);
        // ...is reported as `Other` by the subset.
        assert_eq!(class_at(text, 8), Some(("other", 0..0)));
        // The target `foo` stays a plain var access.
        assert_eq!(class_at(text, 4), Some(("var", 1..4)));
    }

    #[test]
    fn labels_and_refs_classify() {
        let text = "<a> and @a";
        assert_eq!(class_at(text, 3), Some(("label", 0..3)));
        assert_eq!(class_at(text, 10), Some(("ref", 8..10)));
    }

    #[test]
    fn callees_classify_in_markup_and_code() {
        assert_eq!(class_at("#f(x)", 2), Some(("callee", 1..2)));
        assert_eq!(class_at("#{ f(x) }", 4), Some(("callee", 3..4)));
    }

    #[test]
    fn import_path_classifies() {
        assert_eq!(class_at("#import \"x\": y", 11), Some(("import", 8..11)));
    }

    #[test]
    fn unrelated_text_is_other_and_a_hash_alone_is_none() {
        assert!(matches!(
            classify(&Source::detached("plain text"), 4),
            Some(SyntaxClass::Other)
        ));
        assert!(classify(&Source::detached("#"), 1).is_none());
    }
}
