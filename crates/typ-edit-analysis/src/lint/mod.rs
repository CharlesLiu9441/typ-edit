//! Source-level syntax lints ported from tinymist's `tinymist-lint` crate.
//!
//! Only the pure syntax rules are ported (P3-3b): `break`/`continue` outside a
//! loop, `return` outside a function, `set`/`show` rules in a block whose value
//! is ineffective, and values implicitly discarded by a `return`. The
//! world-dependent rules (math unknown variables, variable fonts, unknown-font
//! suggestions) are deliberately left for a later pass.
//!
//! [`Diag`] carries only a byte range and a message, so tinymist's `hints` and
//! `trace` are folded into that message: a hint becomes a trailing
//! `\nHint: {hint}` line exactly as tinymist's own diagnostic renderer appends
//! it, and the break/continue trace becomes the same `while showing loop
//! element` / `while calling function` lines the trace renders. Every lint is a
//! warning; a missing node's children and spans are guarded rather than
//! panicked on.

mod dataflow;
mod visitor;
mod walk;

#[cfg(test)]
mod tests;

use std::ops::Range;
use std::path::PathBuf;

use typ_edit_model::{BytePos, Diag, Severity};
use typst::syntax::Source;
use typst::syntax::ast::{self, AstNode};

use walk::Linter;

/// Runs every pure syntax lint over `source` and returns its warnings.
///
/// The ranges are UTF-8 byte offsets into `source.text`; `path` is left empty
/// for the caller to fill in, like the parser's own diagnostics.
pub fn lint_source(source: &Source) -> Vec<Diag> {
    Linter::new(source).lint(source.root())
}

/// Builds a file-less warning diagnostic; the caller maps it to the open file.
fn warning(range: Range<usize>, message: String) -> Diag {
    Diag {
        path: PathBuf::new(),
        range: BytePos(range.start)..BytePos(range.end),
        message,
        severity: Severity::Warning,
    }
}

/// The full source text of a node or expression.
fn text<'a, T: AstNode<'a>>(node: T) -> String {
    node.to_untyped().clone().full_text().to_string()
}

/// A code block or a content block, iterated uniformly for rule ③.
#[derive(Clone, Copy)]
enum Block<'a> {
    Code(ast::Code<'a>),
    Markup(ast::Markup<'a>),
}

impl<'a> Block<'a> {
    /// Wraps the `CodeBlock`/`ContentBlock` expression `expr`, if it is one.
    fn from(expr: ast::Expr<'a>) -> Option<Self> {
        Some(match expr {
            ast::Expr::CodeBlock(block) => Block::Code(block.body()),
            ast::Expr::ContentBlock(block) => Block::Markup(block.body()),
            _ => return None,
        })
    }

    /// The block's expressions in source order.
    fn iter(self) -> impl Iterator<Item = ast::Expr<'a>> {
        let (code, markup) = match self {
            Block::Code(block) => (Some(block.exprs()), None),
            Block::Markup(block) => (None, Some(block.exprs())),
        };
        code.into_iter()
            .flatten()
            .chain(markup.into_iter().flatten())
    }
}

/// Where a buggy `set`/`show` block sits, which selects the hint's rewrite.
enum BuggyBlockLoc<'a> {
    Show(ast::ShowRule<'a>),
    IfTrue(ast::Conditional<'a>),
    IfFalse(ast::Conditional<'a>),
    While(ast::WhileLoop<'a>),
    For(ast::ForLoop<'a>),
}

impl BuggyBlockLoc<'_> {
    /// The `consider changing parent to ...` hint for the offending rule,
    /// matching tinymist's `BuggyBlockLoc::hint` text exactly.
    fn hint(&self, rule: ast::Expr<'_>) -> String {
        match self {
            BuggyBlockLoc::Show(parent) => {
                let selector = parent.selector().map(text).unwrap_or_default();
                if matches!(rule, ast::Expr::ShowRule(..)) {
                    format!(
                        "consider changing parent to `show {selector}: it => {{ {}; it }}`",
                        text(rule)
                    )
                } else {
                    format!(
                        "consider changing parent to `show {selector}: {}`",
                        text(rule)
                    )
                }
            }
            BuggyBlockLoc::IfTrue(cond) | BuggyBlockLoc::IfFalse(cond) => {
                let neg = if matches!(self, BuggyBlockLoc::IfTrue(..)) {
                    ""
                } else {
                    "not "
                };
                if let ast::Expr::ShowRule(show) = rule {
                    let selector = show.selector().map(text).unwrap_or_default();
                    format!(
                        "consider changing parent to `show {selector}: if {neg}({}) {{ .. }}`",
                        text(cond.condition())
                    )
                } else {
                    format!(
                        "consider changing parent to `{} if {neg}({})`",
                        text(rule),
                        text(cond.condition())
                    )
                }
            }
            BuggyBlockLoc::While(loop_) => format!(
                "consider changing parent to `show: it => if {} {{ {}; it }}`",
                text(loop_.condition()),
                text(rule)
            ),
            BuggyBlockLoc::For(loop_) => format!(
                "consider changing parent to `show: {}.fold(it => it, (style-it, {}) => it => {{ {}; style-it(it) }})`",
                text(loop_.iterable()),
                text(loop_.pattern()),
                text(rule)
            ),
        }
    }
}
