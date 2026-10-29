//! Reverse dataflow pass: a `return` implicitly discards the nearest preceding
//! value-producing statement.
//!
//! This is tinymist's `LateFuncLinter` (`tinymist-lint/src/lib.rs`) ported
//! verbatim: the function body is scanned in reverse, so the state describes
//! the *terminator that follows* the current position. `break`/`continue`
//! erase that state, `if`/`else` branches are joined by AND, and `warned` keeps
//! only the single nearest expresson per control-flow suffix.

use typst::syntax::ast::{self, AstNode};

use super::text;
use super::visitor::{DataFlowVisitor, ExprsOnce, ExprsUntyped};
use super::walk::Linter;

/// What the statement following the current position does to the block value.
#[derive(Clone, Default)]
struct ReturnBlockInfo {
    /// A following `return <value>` discards any preceding value.
    return_value: bool,
    /// A following bare `return` only makes preceding `set`/`show` pointless.
    return_none: bool,
    /// Whether this suffix already warned, so only the nearest expression does.
    warned: bool,
}

impl ReturnBlockInfo {
    fn merge(self, other: Self) -> Self {
        Self {
            return_value: self.return_value && other.return_value,
            return_none: self.return_none && other.return_none,
            warned: self.warned && other.warned,
        }
    }
}

/// Whether the current expression is a statement in a block, a nested block used
/// as a value, or an operand; only `Block` positions can discard a value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExprContext {
    Block,
    BlockExpr,
    Expr,
}

/// Reverse pass over one function body. Built by [`Linter::late_func_return`].
///
/// [`Linter::late_func_return`]: super::walk::Linter
pub(super) struct LateFuncLinter<'a, 'b> {
    linter: &'a mut Linter<'b>,
    return_block_info: Option<ReturnBlockInfo>,
    expr_context: ExprContext,
}

impl<'a, 'b> LateFuncLinter<'a, 'b> {
    /// Starts the pass at the body root in block context.
    pub(super) fn new(linter: &'a mut Linter<'b>) -> Self {
        Self {
            linter,
            return_block_info: None,
            expr_context: ExprContext::Block,
        }
    }

    /// Runs `f` with `ctx`, downgrading a nested block to `BlockExpr` when we
    /// are already outside block context.
    fn expr_ctx<F: FnOnce(&mut Self) -> Option<()>>(
        &mut self,
        ctx: ExprContext,
        f: F,
    ) -> Option<()> {
        let ctx = match ctx {
            ExprContext::Block if self.expr_context != ExprContext::Block => ExprContext::BlockExpr,
            other => other,
        };
        let old = std::mem::replace(&mut self.expr_context, ctx);
        let result = f(self);
        self.expr_context = old;
        result
    }

    /// Merges the `if` branch state `parent` into the `else` branch state.
    fn join(&mut self, parent: Option<ReturnBlockInfo>) {
        if let Some(parent) = parent {
            match &mut self.return_block_info {
                Some(info) => {
                    if info.return_value == parent.return_value {
                        return;
                    }
                    *info = parent.merge(std::mem::take(info));
                }
                info @ None => *info = Some(parent),
            }
        }
    }
}

impl DataFlowVisitor for LateFuncLinter<'_, '_> {
    fn exprs<'a>(&mut self, exprs: impl DoubleEndedIterator<Item = ast::Expr<'a>>) -> Option<()> {
        for expr in exprs.rev() {
            self.expr(expr);
        }
        Some(())
    }

    fn block<'a>(&mut self, exprs: impl DoubleEndedIterator<Item = ast::Expr<'a>>) -> Option<()> {
        self.expr_ctx(ExprContext::Block, |this| this.exprs(exprs))
    }

    fn loop_break(&mut self, _expr: ast::LoopBreak<'_>) -> Option<()> {
        self.return_block_info = Some(ReturnBlockInfo::default());
        Some(())
    }

    fn loop_continue(&mut self, _expr: ast::LoopContinue<'_>) -> Option<()> {
        self.return_block_info = Some(ReturnBlockInfo::default());
        Some(())
    }

    fn func_return(&mut self, expr: ast::FuncReturn<'_>) -> Option<()> {
        self.return_block_info = Some(ReturnBlockInfo {
            return_value: expr.body().is_some(),
            return_none: expr.body().is_none(),
            warned: false,
        });
        Some(())
    }

    /// A nested closure's body is a separate function; only its name and
    /// parameters are scanned for other discarded values.
    fn closure(&mut self, expr: ast::Closure<'_>) -> Option<()> {
        let ident = expr.name().map(ast::Expr::Ident).into_iter();
        let params = expr.params().to_untyped().exprs();
        self.exprs(ident.chain(params))
    }

    fn contextual(&mut self, _expr: ast::Contextual<'_>) -> Option<()> {
        Some(())
    }

    fn field_access(&mut self, _expr: ast::FieldAccess<'_>) -> Option<()> {
        Some(())
    }

    fn unary(&mut self, expr: ast::Unary<'_>) -> Option<()> {
        self.expr_ctx(ExprContext::Expr, |this| this.expr(expr.expr()))
    }

    fn binary(&mut self, expr: ast::Binary<'_>) -> Option<()> {
        self.expr_ctx(ExprContext::Expr, |this| {
            this.exprs([expr.lhs(), expr.rhs()].into_iter())
        })
    }

    fn equation(&mut self, expr: ast::Equation<'_>) -> Option<()> {
        self.value(ast::Expr::Equation(expr))
    }

    fn array(&mut self, expr: ast::Array<'_>) -> Option<()> {
        self.value(ast::Expr::Array(expr))
    }

    fn dict(&mut self, expr: ast::Dict<'_>) -> Option<()> {
        self.value(ast::Expr::Dict(expr))
    }

    fn include(&mut self, expr: ast::ModuleInclude<'_>) -> Option<()> {
        self.value(ast::Expr::ModuleInclude(expr))
    }

    fn func_call(&mut self, _expr: ast::FuncCall<'_>) -> Option<()> {
        Some(())
    }

    fn let_binding(&mut self, _expr: ast::LetBinding<'_>) -> Option<()> {
        Some(())
    }

    fn destruct_assign(&mut self, _expr: ast::DestructAssignment<'_>) -> Option<()> {
        Some(())
    }

    /// Each `if`/`else` branch is analyzed independently before the states are
    /// merged, so both branches can warn but an outer statement does not.
    fn conditional(&mut self, expr: ast::Conditional<'_>) -> Option<()> {
        let if_body = expr.if_body();
        let else_body = expr.else_body();

        let parent = self.return_block_info.clone();
        self.exprs(if_body.once());
        let if_branch = std::mem::replace(&mut self.return_block_info, parent.clone());
        self.exprs(else_body.into_iter());
        self.join(if_branch);

        Some(())
    }

    fn value(&mut self, expr: ast::Expr) -> Option<()> {
        match self.expr_context {
            ExprContext::Block => {}
            ExprContext::BlockExpr | ExprContext::Expr => return None,
        }

        let ri = self.return_block_info.as_mut()?;
        if ri.warned {
            return None;
        }
        if matches!(expr, ast::Expr::None(..)) || expr.to_untyped().kind().is_trivia() {
            return None;
        }

        let show_set = matches!(expr, ast::Expr::ShowRule(..) | ast::Expr::SetRule(..));
        let discarded = ri.return_value || (ri.return_none && show_set);
        if !discarded {
            return None;
        }

        ri.warned = true;
        let kind = expr.to_untyped().kind().name();
        let mut message = format!("This {kind} is implicitly discarded by function return");
        if ri.return_value && !show_set && expr.hash() {
            message.push_str(
                "\nHint: consider ignoring the value explicitly using underscore: `let _ = ",
            );
            message.push_str(&text(expr));
            message.push('`');
        }
        self.linter.push_warning(expr.span(), message);
        Some(())
    }

    fn show(&mut self, expr: ast::ShowRule<'_>) -> Option<()> {
        self.value(ast::Expr::ShowRule(expr))
    }

    fn set(&mut self, expr: ast::SetRule<'_>) -> Option<()> {
        self.value(ast::Expr::SetRule(expr))
    }

    fn for_loop(&mut self, expr: ast::ForLoop<'_>) -> Option<()> {
        self.expr(expr.body())
    }

    fn while_loop(&mut self, expr: ast::WhileLoop<'_>) -> Option<()> {
        self.expr(expr.body())
    }
}
