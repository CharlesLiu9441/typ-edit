//! The expression-walking trait shared by the syntax lint passes.
//!
//! This is tinymist's `DataFlowVisitor` (`tinymist-lint/src/lib.rs`) trimmed to
//! what the pure syntax rules need: a recursive dispatch over [`ast::Expr`] that
//! lets each pass override only the variants it cares about. [`expr`] visits a
//! node; the default methods recurse through every child, and an override
//! returns `None` to prune a subtree.
//!
//! [`expr`]: DataFlowVisitor::expr

use typst::syntax::SyntaxNode;
use typst::syntax::ast::{self, AstNode};

/// Iterates the direct expression children of an untyped node.
pub(super) trait ExprsUntyped {
    fn exprs(&self) -> impl DoubleEndedIterator<Item = ast::Expr<'_>>;
}

impl ExprsUntyped for ast::Expr<'_> {
    fn exprs(&self) -> impl DoubleEndedIterator<Item = ast::Expr<'_>> {
        self.to_untyped().exprs()
    }
}

impl ExprsUntyped for SyntaxNode {
    fn exprs(&self) -> impl DoubleEndedIterator<Item = ast::Expr<'_>> {
        self.children().filter_map(SyntaxNode::cast)
    }
}

/// Yields a single expression as a double-ended iterator, so single children
/// can be chained into the same iteration the multi-child visitors use.
pub(super) trait ExprsOnce<'a> {
    fn once(self) -> impl DoubleEndedIterator<Item = ast::Expr<'a>>;
}

impl<'a> ExprsOnce<'a> for ast::Expr<'a> {
    fn once(self) -> impl DoubleEndedIterator<Item = ast::Expr<'a>> {
        std::iter::once(self)
    }
}

/// Whether `expr` is a `set`/`show` rule (a statement with no value).
pub(super) fn is_show_set(expr: ast::Expr) -> bool {
    matches!(expr, ast::Expr::SetRule(..) | ast::Expr::ShowRule(..))
}

/// Recursive expression visitor.
///
/// Defaults walk every child expression; a pass overrides the variants it needs
/// and returns `None` to stop that subtree.
pub(super) trait DataFlowVisitor {
    fn expr(&mut self, expr: ast::Expr) -> Option<()> {
        match expr {
            ast::Expr::Parenthesized(expr) => self.expr(expr.expr()),
            ast::Expr::CodeBlock(expr) => self.block(expr.body().exprs()),
            ast::Expr::ContentBlock(expr) => self.block(expr.body().exprs()),
            ast::Expr::Math(expr) => self.exprs(expr.exprs()),

            ast::Expr::Text(..) => self.value(expr),
            ast::Expr::Space(..) => self.value(expr),
            ast::Expr::Linebreak(..) => self.value(expr),
            ast::Expr::Parbreak(..) => self.value(expr),
            ast::Expr::Escape(..) => self.value(expr),
            ast::Expr::Shorthand(..) => self.value(expr),
            ast::Expr::SmartQuote(..) => self.value(expr),
            ast::Expr::Raw(..) => self.value(expr),
            ast::Expr::Link(..) => self.value(expr),

            ast::Expr::Label(..) => self.value(expr),
            ast::Expr::Ref(..) => self.value(expr),
            ast::Expr::None(..) => self.value(expr),
            ast::Expr::Auto(..) => self.value(expr),
            ast::Expr::Bool(..) => self.value(expr),
            ast::Expr::Int(..) => self.value(expr),
            ast::Expr::Float(..) => self.value(expr),
            ast::Expr::Numeric(..) => self.value(expr),
            ast::Expr::Str(..) => self.value(expr),
            ast::Expr::MathText(..) => self.value(expr),
            ast::Expr::MathShorthand(..) => self.value(expr),
            ast::Expr::MathAlignPoint(..) => self.value(expr),
            ast::Expr::MathPrimes(..) => self.value(expr),
            ast::Expr::MathRoot(..) => self.value(expr),

            ast::Expr::Strong(content) => self.exprs(content.body().exprs()),
            ast::Expr::Emph(content) => self.exprs(content.body().exprs()),
            ast::Expr::Heading(content) => self.exprs(content.body().exprs()),
            ast::Expr::ListItem(content) => self.exprs(content.body().exprs()),
            ast::Expr::EnumItem(content) => self.exprs(content.body().exprs()),
            ast::Expr::TermItem(content) => {
                self.exprs(content.term().exprs().chain(content.description().exprs()))
            }
            ast::Expr::MathDelimited(content) => self.exprs(content.body().exprs()),
            ast::Expr::MathAttach(..) | ast::Expr::MathFrac(..) => self.exprs(expr.exprs()),
            ast::Expr::MathFieldAccess(expr) => self.math_field_access(expr),
            ast::Expr::MathCall(expr) => self.math_call(expr),

            ast::Expr::Ident(expr) => self.ident(expr),
            ast::Expr::MathIdent(expr) => self.math_ident(expr),
            ast::Expr::Equation(expr) => self.equation(expr),
            ast::Expr::Array(expr) => self.array(expr),
            ast::Expr::Dict(expr) => self.dict(expr),
            ast::Expr::Unary(expr) => self.unary(expr),
            ast::Expr::Binary(expr) => self.binary(expr),
            ast::Expr::FieldAccess(expr) => self.field_access(expr),
            ast::Expr::FuncCall(expr) => self.func_call(expr),
            ast::Expr::Closure(expr) => self.closure(expr),
            ast::Expr::LetBinding(expr) => self.let_binding(expr),
            ast::Expr::DestructAssignment(expr) => self.destruct_assign(expr),
            ast::Expr::SetRule(expr) => self.set(expr),
            ast::Expr::ShowRule(expr) => self.show(expr),
            ast::Expr::Contextual(expr) => self.contextual(expr),
            ast::Expr::Conditional(expr) => self.conditional(expr),
            ast::Expr::WhileLoop(expr) => self.while_loop(expr),
            ast::Expr::ForLoop(expr) => self.for_loop(expr),
            ast::Expr::ModuleImport(expr) => self.import(expr),
            ast::Expr::ModuleInclude(expr) => self.include(expr),
            ast::Expr::LoopBreak(expr) => self.loop_break(expr),
            ast::Expr::LoopContinue(expr) => self.loop_continue(expr),
            ast::Expr::FuncReturn(expr) => self.func_return(expr),
        }
    }

    fn exprs<'a>(&mut self, exprs: impl DoubleEndedIterator<Item = ast::Expr<'a>>) -> Option<()> {
        for expr in exprs {
            self.expr(expr);
        }
        Some(())
    }

    fn block<'a>(&mut self, exprs: impl DoubleEndedIterator<Item = ast::Expr<'a>>) -> Option<()> {
        self.exprs(exprs)
    }

    fn value(&mut self, _expr: ast::Expr) -> Option<()> {
        Some(())
    }

    fn ident(&mut self, _expr: ast::Ident<'_>) -> Option<()> {
        Some(())
    }

    fn math_ident(&mut self, _expr: ast::MathIdent<'_>) -> Option<()> {
        Some(())
    }

    fn import(&mut self, _expr: ast::ModuleImport<'_>) -> Option<()> {
        Some(())
    }

    fn include(&mut self, _expr: ast::ModuleInclude<'_>) -> Option<()> {
        Some(())
    }

    fn equation(&mut self, expr: ast::Equation<'_>) -> Option<()> {
        self.exprs(expr.body().exprs())
    }

    fn array(&mut self, expr: ast::Array<'_>) -> Option<()> {
        self.exprs(expr.to_untyped().exprs())
    }

    fn dict(&mut self, expr: ast::Dict<'_>) -> Option<()> {
        self.exprs(expr.to_untyped().exprs())
    }

    fn unary(&mut self, expr: ast::Unary<'_>) -> Option<()> {
        self.expr(expr.expr())
    }

    fn binary(&mut self, expr: ast::Binary<'_>) -> Option<()> {
        self.exprs([expr.lhs(), expr.rhs()].into_iter())
    }

    fn field_access(&mut self, expr: ast::FieldAccess<'_>) -> Option<()> {
        self.expr(expr.target())
    }

    fn math_access(&mut self, access: ast::MathAccess<'_>) -> Option<()> {
        match access {
            ast::MathAccess::MathIdent(expr) => self.math_ident(expr),
            ast::MathAccess::MathFieldAccess(expr) => self.math_field_access(expr),
        }
    }

    fn math_field_access(&mut self, expr: ast::MathFieldAccess<'_>) -> Option<()> {
        self.math_access(expr.target())
    }

    fn math_call(&mut self, expr: ast::MathCall<'_>) -> Option<()> {
        self.exprs(expr.args().to_untyped().exprs())?;
        self.math_access(expr.callee())
    }

    fn func_call(&mut self, expr: ast::FuncCall<'_>) -> Option<()> {
        self.exprs(expr.args().to_untyped().exprs().chain(expr.callee().once()))
    }

    fn closure(&mut self, expr: ast::Closure<'_>) -> Option<()> {
        let ident = expr.name().map(ast::Expr::Ident).into_iter();
        let params = expr.params().to_untyped().exprs();
        let body = expr.body().once();
        self.exprs(ident.chain(params).chain(body))
    }

    fn let_binding(&mut self, expr: ast::LetBinding<'_>) -> Option<()> {
        self.expr(expr.init()?)
    }

    fn destruct_assign(&mut self, expr: ast::DestructAssignment<'_>) -> Option<()> {
        self.expr(expr.value())
    }

    fn set(&mut self, expr: ast::SetRule<'_>) -> Option<()> {
        let cond = expr.condition().into_iter();
        let args = expr.args().to_untyped().exprs();
        self.exprs(cond.chain(args).chain(expr.target().once()))
    }

    fn show(&mut self, expr: ast::ShowRule<'_>) -> Option<()> {
        let selector = expr.selector().into_iter();
        let transform = expr.transform();
        self.exprs(selector.chain(transform.once()))
    }

    fn contextual(&mut self, expr: ast::Contextual<'_>) -> Option<()> {
        self.expr(expr.body())
    }

    fn conditional(&mut self, expr: ast::Conditional<'_>) -> Option<()> {
        let cond = expr.condition().once();
        let if_body = expr.if_body().once();
        let else_body = expr.else_body().into_iter();
        self.exprs(cond.chain(if_body).chain(else_body))
    }

    fn while_loop(&mut self, expr: ast::WhileLoop<'_>) -> Option<()> {
        let cond = expr.condition().once();
        let body = expr.body().once();
        self.exprs(cond.chain(body))
    }

    fn for_loop(&mut self, expr: ast::ForLoop<'_>) -> Option<()> {
        let iterable = expr.iterable().once();
        let body = expr.body().once();
        self.exprs(iterable.chain(body))
    }

    fn loop_break(&mut self, _expr: ast::LoopBreak<'_>) -> Option<()> {
        Some(())
    }

    fn loop_continue(&mut self, _expr: ast::LoopContinue<'_>) -> Option<()> {
        Some(())
    }

    fn func_return(&mut self, expr: ast::FuncReturn<'_>) -> Option<()> {
        self.expr(expr.body()?)
    }
}
