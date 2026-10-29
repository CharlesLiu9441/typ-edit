//! Forward syntax walk: `break`/`continue` outside a loop, `return` outside a
//! function, and `set`/`show` rules in a block whose value is ineffective.
//!
//! Ported from tinymist's `Linter` (`tinymist-lint/src/lib.rs`) with the
//! world-dependent rules omitted. The loop/function context is a single
//! save/restore slot each: entering a loop sets `in_loop`, and entering a
//! closure or `#context` body starts a fresh function (remembering whether it
//! was nested in a loop) and clears `in_loop`, so a `break` in a closure that
//! *looks* like it is inside a loop is still flagged.

use std::ops::Range;

use typ_edit_model::Diag;
use typst::syntax::ast::{self, AstNode};
use typst::syntax::{Source, Span, SpanKind, SyntaxNode};

use super::dataflow::LateFuncLinter;
use super::visitor::{DataFlowVisitor, ExprsUntyped, is_show_set};
use super::{Block, BuggyBlockLoc, warning};

/// One function scope. `parent_loop` is the loop state outside the function, so
/// a `break`/`continue` inside a closure nested in a loop can cite it.
#[derive(Clone, Copy)]
struct FuncInfo {
    parent_loop: bool,
    /// Whether this function's body contains an explicit `return`; only then is
    /// the reverse value-discard pass worth running.
    has_return: bool,
}

/// Forward pass: the loop/function context walker for rules ①②③.
pub(super) struct Linter<'a> {
    /// The source the tree belongs to, needed to map numbered spans to bytes.
    source: &'a Source,
    diags: Vec<Diag>,
    in_loop: bool,
    func_info: Option<FuncInfo>,
}

impl<'a> Linter<'a> {
    pub(super) fn new(source: &'a Source) -> Self {
        Self {
            source,
            diags: Vec::new(),
            in_loop: false,
            func_info: None,
        }
    }

    /// Lints the whole document rooted at `root` and returns the warnings.
    pub(super) fn lint(mut self, root: &SyntaxNode) -> Vec<Diag> {
        if let Some(markup) = root.cast::<ast::Markup>() {
            self.exprs(markup.exprs());
        } else if let Some(expr) = root.cast() {
            self.expr(expr);
        }
        self.diags
    }

    /// Records a warning at `span`, skipping detached or foreign spans.
    pub(super) fn push_warning(&mut self, span: Span, message: String) {
        if let Some(range) = node_range(self.source, span) {
            self.diags.push(warning(range, message));
        }
    }

    /// Runs `f` with a loop context, restoring the previous context afterwards.
    fn with_loop<F: FnOnce(&mut Self)>(&mut self, f: F) {
        let old = self.in_loop;
        self.in_loop = true;
        f(self);
        self.in_loop = old;
    }

    /// Runs `f` in a fresh function context, then restores the loop and function
    /// state. A `break`/`continue` in the body starts from a non-loop context.
    fn with_func<F: FnOnce(&mut Self)>(&mut self, f: F) {
        let old_func = self.func_info.take();
        let parent_loop = self.in_loop;
        self.func_info = Some(FuncInfo {
            parent_loop,
            has_return: false,
        });
        self.in_loop = false;
        f(self);
        self.in_loop = self.func_info.take().expect("function info").parent_loop;
        self.func_info = old_func;
    }

    /// Warns for each `set`/`show` rule when `expr` is a block used as a value
    /// but consists only of such rules (rule ③).
    fn buggy_block_expr(&mut self, expr: ast::Expr, loc: &BuggyBlockLoc) {
        let Some(block) = Block::from(expr) else {
            return;
        };
        if !only_show(block) {
            return;
        }
        let mut first = true;
        for rule in block.iter() {
            let message = match rule {
                ast::Expr::SetRule(..) => "This set statement doesn't take effect.",
                ast::Expr::ShowRule(..) => "This show statement doesn't take effect.",
                _ => continue,
            };
            let mut message = message.to_owned();
            if first {
                first = false;
                message.push_str("\nHint: ");
                message.push_str(&loc.hint(rule));
            }
            self.push_warning(rule.span(), message);
        }
    }

    fn bad_branch(&mut self, span: Span, name: &str) {
        let mut message = format!("`{name}` statement in a non-loop context");
        // tinymist attaches a `Show("loop")`/`Call(None)` trace here; `Diag` has
        // no trace field, so the rendered trace lines are folded in instead.
        if self.func_info.as_ref().is_some_and(|info| info.parent_loop) {
            message.push_str("\nwhile showing loop element\nwhile calling function");
        }
        self.push_warning(span, message);
    }

    /// Runs the reverse value-discard pass over a function body when it returns.
    fn late_func_return(&mut self, body: ast::Expr) {
        if !self.func_info.as_ref().is_some_and(|info| info.has_return) {
            return;
        }
        let mut late = LateFuncLinter::new(self);
        late.expr(body);
    }
}

/// Maps a syntax-tree span to its byte range in `source`, or `None` when the
/// span is detached or belongs to another file.
fn node_range(source: &Source, span: Span) -> Option<Range<usize>> {
    match span.get() {
        SpanKind::Number { id, num } if id == source.id() => source.range(num, None),
        SpanKind::Range { id, range } if id == source.id() => Some(range),
        _ => None,
    }
}

/// Whether a block is made up only of `set`/`show` rules (plus trivia and an
/// optional trailing `break`/`continue`). Such a block produces no value, so
/// the rules "don't take effect" when the block is used as a value.
fn only_show(block: Block) -> bool {
    let mut has_set = false;
    for expr in block.iter() {
        if is_show_set(expr) {
            has_set = true;
        } else if matches!(expr, ast::Expr::LoopBreak(..) | ast::Expr::LoopContinue(..)) {
            return has_set;
        } else if !expr.to_untyped().kind().is_trivia() {
            return false;
        }
    }
    has_set
}

impl DataFlowVisitor for Linter<'_> {
    fn show(&mut self, expr: ast::ShowRule<'_>) -> Option<()> {
        if let Some(selector) = expr.selector() {
            self.expr(selector);
        }
        let transform = expr.transform();
        self.buggy_block_expr(transform, &BuggyBlockLoc::Show(expr));
        self.expr(transform)
    }

    fn conditional(&mut self, expr: ast::Conditional<'_>) -> Option<()> {
        self.expr(expr.condition());

        let if_body = expr.if_body();
        self.buggy_block_expr(if_body, &BuggyBlockLoc::IfTrue(expr));
        self.expr(if_body);

        if let Some(else_body) = expr.else_body() {
            self.buggy_block_expr(else_body, &BuggyBlockLoc::IfFalse(expr));
            self.expr(else_body);
        }

        Some(())
    }

    fn while_loop(&mut self, expr: ast::WhileLoop<'_>) -> Option<()> {
        self.with_loop(|this| {
            this.expr(expr.condition());
            let body = expr.body();
            this.buggy_block_expr(body, &BuggyBlockLoc::While(expr));
            this.expr(body);
        });
        Some(())
    }

    fn for_loop(&mut self, expr: ast::ForLoop<'_>) -> Option<()> {
        self.with_loop(|this| {
            this.expr(expr.iterable());
            let body = expr.body();
            this.buggy_block_expr(body, &BuggyBlockLoc::For(expr));
            this.expr(body);
        });
        Some(())
    }

    fn closure(&mut self, expr: ast::Closure<'_>) -> Option<()> {
        self.with_func(|this| {
            this.exprs(expr.params().to_untyped().exprs());
            let body = expr.body();
            this.expr(body);
            this.late_func_return(body);
        });
        Some(())
    }

    fn contextual(&mut self, expr: ast::Contextual<'_>) -> Option<()> {
        self.with_func(|this| {
            let body = expr.body();
            this.expr(body);
            this.late_func_return(body);
        });
        Some(())
    }

    fn loop_break(&mut self, expr: ast::LoopBreak<'_>) -> Option<()> {
        if !self.in_loop {
            self.bad_branch(expr.span(), "break");
        }
        Some(())
    }

    fn loop_continue(&mut self, expr: ast::LoopContinue<'_>) -> Option<()> {
        if !self.in_loop {
            self.bad_branch(expr.span(), "continue");
        }
        Some(())
    }

    fn func_return(&mut self, expr: ast::FuncReturn<'_>) -> Option<()> {
        match &mut self.func_info {
            Some(info) => info.has_return = true,
            None => self.push_warning(
                expr.span(),
                "`return` statement in a non-function context".to_owned(),
            ),
        }
        Some(())
    }
}
