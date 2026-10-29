//! Lexical symbol tree (document outline).
//!
//! This is a port of tinymist's `get_lexical_hierarchy` in `Symbol` scope: a
//! recursive walk of the syntax tree that emits key symbols — headings,
//! functions, parameters and labels, plus top-level variables. All ranges are
//! UTF-8 byte offsets into [`Source::text`], matching the highlight and fold
//! ranges. Only the `Symbol` mode is implemented; tinymist's `Braced` mode
//! (`Block`/`CommentGroup`) and its value / label *references* are out of scope.
//!
//! The outline shows key symbols only: a variable binding or closure parameter
//! inside a function body is a local and is not displayed, and anonymous
//! closures (and named closures nested in a function body) are not displayed
//! either. Only their labels survive, because labels are key symbols wherever
//! they appear.
//!
//! The walk keeps a stack of open frames. A symbol opens a frame; closing it
//! appends the symbol to the new top frame's children. Heading frames stay open
//! after their subtree so the rest of the section nests under them until a
//! heading at the same or a shallower depth closes them. Function (closure)
//! frames wrap their parameters and body so both nest under the function.

mod extract;

use typ_edit_model::{BytePos, SymbolKind, SymbolNode};
use typst::syntax::{LinkedNode, Source, SyntaxKind, ast};

use extract::{IdentContext, is_plain_token};

/// Collects the document's lexical symbol tree, in source order.
pub fn symbols(source: &Source) -> Vec<SymbolNode> {
    let root = LinkedNode::new(source.root());
    let mut walker = LexicalWalker::new();
    walker.check_node(&root);

    // Heading frames deliberately stay open while walking; close every frame
    // still on top of the synthetic root, which appends it to its parent.
    while walker.stack.len() > 1 {
        walker.finish();
    }
    walker
        .stack
        .pop()
        .map(|frame| frame.children)
        .unwrap_or_default()
}

/// One open level of the outline while walking.
struct Frame {
    /// The symbol this frame emits, or `None` for the synthetic root frame.
    symbol: Option<SymbolNode>,
    /// Finished symbols are appended here.
    children: Vec<SymbolNode>,
    /// Heading depth, `0` for non-heading symbols and `-1` for the root.
    level: i16,
}

/// Recursive outline walker holding the open frames and the identifier context.
struct LexicalWalker {
    stack: Vec<Frame>,
    ident_context: IdentContext,
    /// Number of enclosing closure bodies currently being traversed. `0` is the
    /// top level; anything deeper means a local binding or a nested closure.
    fn_depth: usize,
}

impl LexicalWalker {
    fn new() -> Self {
        Self {
            stack: vec![Frame {
                symbol: None,
                children: Vec::new(),
                level: -1,
            }],
            ident_context: IdentContext::Ref,
            fn_depth: 0,
        }
    }

    /// Closes the top frame, appending its finished symbol to the new top.
    fn finish(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let Some(mut symbol) = frame.symbol else {
            return;
        };
        symbol.children = frame.children;
        if let Some(parent) = self.stack.last_mut() {
            parent.children.push(symbol);
        }
    }

    /// Opens a frame for `symbol`. The frame's level drives heading nesting.
    fn push_symbol(&mut self, symbol: SymbolNode) {
        let level = match symbol.kind {
            SymbolKind::Heading(level) => level as i16,
            _ => 0,
        };
        self.stack.push(Frame {
            symbol: Some(symbol),
            children: Vec::new(),
            level,
        });
    }

    /// Closes open heading frames that are at least as deep as `level`, so a
    /// new heading becomes a sibling of equal-or-shallower headings.
    fn pop_headings(&mut self, level: i16) {
        while self.stack.len() > 1 && self.stack.last().is_some_and(|frame| frame.level >= level) {
            self.finish();
        }
    }

    /// Applies the identifier context a node establishes for its subtree.
    fn enter_node(&mut self, node: &LinkedNode) {
        match node.kind() {
            SyntaxKind::RefMarker | SyntaxKind::LetBinding => {
                self.ident_context = IdentContext::Ref;
            }
            SyntaxKind::Closure => self.ident_context = IdentContext::Func,
            SyntaxKind::Params => self.ident_context = IdentContext::Params,
            _ => {}
        }
    }

    /// Visits `node` with an explicit context, restoring the previous one after.
    fn check_node_with(&mut self, node: &LinkedNode, context: IdentContext) {
        let saved = self.ident_context;
        self.ident_context = context;
        self.check_node(node);
        self.ident_context = saved;
    }

    fn check_nodes(&mut self, node: &LinkedNode) {
        for child in node.children() {
            self.check_node(&child);
        }
    }

    /// Visits `node`: emits its own symbol, or dispatches on its kind.
    fn check_node(&mut self, node: &LinkedNode) {
        let symbol = extract::symbol(node, self.ident_context, self.fn_depth);
        let checkpoint = self.ident_context;
        self.enter_node(node);

        match symbol {
            Some(symbol) => {
                let is_heading = matches!(symbol.kind, SymbolKind::Heading(_));
                if let SymbolKind::Heading(level) = symbol.kind {
                    self.pop_headings(level as i16);
                }
                self.push_symbol(symbol);
                self.check_nodes(node);
                // A heading frame stays open so its section content nests under
                // it; every other symbol closes right after its subtree.
                if !is_heading {
                    self.finish();
                }
            }
            None => self.check_non_symbol(node),
        }

        self.ident_context = checkpoint;
    }

    /// Dispatches a node that carries no symbol of its own.
    fn check_non_symbol(&mut self, node: &LinkedNode) {
        match node.kind() {
            SyntaxKind::LetBinding => self.check_let_binding(node),
            SyntaxKind::ForLoop => self.check_for_loop(node),
            SyntaxKind::Closure => self.check_closure(node),
            SyntaxKind::Named => self.check_named(node),
            SyntaxKind::FieldAccess => self.check_first_expr(node),
            // Leaf tokens carry no definitions and are not entered. Comments are
            // trivia, so they are skipped here without a comment-group pass.
            kind if is_plain_token(kind) => {}
            _ => self.check_nodes(node),
        }
    }

    /// `let` binding: a closure pattern is a function definition; otherwise the
    /// pattern binds a variable and the trailing expression is the body.
    fn check_let_binding(&mut self, node: &LinkedNode) {
        let pattern = node
            .children()
            .find(|child| child.get().is::<ast::Pattern>());

        let closure_pattern = pattern.as_ref().is_some_and(|pattern| {
            matches!(
                pattern.get().cast::<ast::Pattern>(),
                Some(ast::Pattern::Normal(ast::Expr::Closure(_)))
            )
        });
        if closure_pattern {
            // The closure branch resolves the function name, parameters and body.
            if let Some(pattern) = pattern {
                self.check_node_with(&pattern, IdentContext::Ref);
            }
            return;
        }

        let name_offset = pattern.as_ref().map(|pattern| pattern.offset());
        if let Some(pattern) = pattern {
            self.check_node_with(&pattern, IdentContext::Var);
        }
        // The body is the last expression, guarded so a malformed binding whose
        // only expression is the pattern is not checked twice.
        self.check_last_expr(node, name_offset);
    }

    /// `for x in xs { .. }`: the iterable is a reference site, the pattern binds
    /// a variable and the body is walked for nested definitions last.
    fn check_for_loop(&mut self, node: &LinkedNode) {
        let pattern = node
            .children()
            .find(|child| child.get().is::<ast::Pattern>());
        let iterable = node
            .children()
            .skip_while(|child| child.kind() != SyntaxKind::In)
            .find(|child| child.get().is::<ast::Expr>());

        let iterable_offset = iterable.as_ref().map(|iterable| iterable.offset());
        if let Some(iterable) = iterable {
            self.check_node_with(&iterable, IdentContext::Ref);
        }
        if let Some(pattern) = pattern {
            self.check_node_with(&pattern, IdentContext::Var);
        }
        self.check_last_expr(node, iterable_offset);
    }

    /// Closure: `f(x) = ..` names a function at the top level, `(x) => ..` is
    /// anonymous. A key function's symbol spans the whole closure and its
    /// parameters nest under it as [`SymbolKind::Parameter`]. Anonymous closures
    /// and named closures nested inside another function body are not key
    /// symbols: their symbol and parameters are skipped, but their body is still
    /// traversed (deeper) so labels inside it survive.
    fn check_closure(&mut self, node: &LinkedNode) {
        let name = node
            .children()
            .next()
            .filter(|child| child.kind() == SyntaxKind::Ident)
            .and_then(|child| child.get().cast::<ast::Ident>())
            .map(|ident| ident.get().to_string());
        let key = self.fn_depth == 0 && name.is_some();

        if key && let Some(name) = name {
            let range = node.range();
            self.push_symbol(SymbolNode {
                name,
                kind: SymbolKind::Function,
                range: BytePos(range.start)..BytePos(range.end),
                children: Vec::new(),
            });
            if let Some(params) = node
                .children()
                .find(|child| child.kind() == SyntaxKind::Params)
            {
                self.check_node_with(&params, IdentContext::Params);
            }
        }

        self.fn_depth += 1;
        self.check_last_expr(node, None);
        self.fn_depth -= 1;

        if key {
            self.finish();
        }
    }

    /// A named pair (`name: value`): the value may hold nested definitions; the
    /// name is a [`SymbolKind::Parameter`] only inside a parameter list.
    fn check_named(&mut self, node: &LinkedNode) {
        self.check_last_expr(node, None);
        if self.ident_context == IdentContext::Params
            && let Some(ident) = node
                .children()
                .find(|child| child.kind() == SyntaxKind::Ident)
        {
            self.check_node_with(&ident, IdentContext::Params);
        }
    }

    /// Visits the first expression child as a reference site (nested defs only).
    fn check_first_expr(&mut self, node: &LinkedNode) {
        if let Some(target) = node.children().find(|child| child.get().is::<ast::Expr>()) {
            self.check_node_with(&target, IdentContext::Ref);
        }
    }

    /// Visits the last expression child (the body) as a reference site, unless
    /// it starts at or before `after_offset`.
    fn check_last_expr(&mut self, node: &LinkedNode, after_offset: Option<usize>) {
        if let Some(body) = node
            .children()
            .rev()
            .find(|child| child.get().is::<ast::Expr>())
        {
            if after_offset.is_some_and(|offset| offset >= body.offset()) {
                return;
            }
            self.check_node_with(&body, IdentContext::Ref);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_outline(text: &str) -> Vec<SymbolNode> {
        symbols(&Source::detached(text))
    }

    /// The single symbol named `name` in a list, panicking otherwise.
    #[track_caller]
    fn named<'a>(symbols: &'a [SymbolNode], name: &str) -> &'a SymbolNode {
        let mut found = symbols.iter().filter(|symbol| symbol.name == name);
        let symbol = found
            .next()
            .unwrap_or_else(|| panic!("no symbol named {name:?} in {symbols:?}"));
        assert!(found.next().is_none(), "duplicate symbol {name:?}");
        symbol
    }

    #[test]
    fn headings_nest_by_level_and_keep_source_order() {
        let text = "= A\n== B\n= C";
        let outline = parse_outline(text);
        assert_eq!(outline.len(), 2, "{outline:?}");

        let a = &outline[0];
        assert_eq!(a.name, "A");
        assert_eq!(a.kind, SymbolKind::Heading(1));
        assert_eq!(
            a.range,
            BytePos(2)..BytePos(3),
            "heading name is the markup bytes only"
        );

        let b = &a.children[0];
        assert_eq!(b.name, "B");
        assert_eq!(b.kind, SymbolKind::Heading(2));
        assert_eq!(b.range, BytePos(7)..BytePos(8));
        assert!(b.children.is_empty());

        let c = &outline[1];
        assert_eq!(c.name, "C");
        assert_eq!(c.kind, SymbolKind::Heading(1));
        assert_eq!(c.range, BytePos(11)..BytePos(12));
        assert!(c.children.is_empty());
    }

    #[test]
    fn let_binding_is_a_variable_and_a_bare_reference_emits_nothing() {
        let text = "#let x = 1\n#x";
        let outline = parse_outline(text);
        assert_eq!(outline.len(), 1, "{outline:?}");
        assert_eq!(outline[0].name, "x");
        assert_eq!(outline[0].kind, SymbolKind::Variable);
        assert_eq!(
            outline[0].range,
            BytePos(5)..BytePos(6),
            "range is the identifier bytes only"
        );
        assert!(outline[0].children.is_empty());
    }

    #[test]
    fn named_function_takes_the_closure_range_and_owns_its_params() {
        let text = "#let f(a) = a";
        let outline = parse_outline(text);
        assert_eq!(outline.len(), 1, "{outline:?}");

        let function = &outline[0];
        assert_eq!(function.name, "f");
        assert_eq!(function.kind, SymbolKind::Function);
        assert_eq!(
            function.range,
            BytePos(text.find('f').unwrap())..BytePos(text.len()),
            "the function symbol spans the whole closure"
        );

        let param = named(&function.children, "a");
        assert_eq!(param.kind, SymbolKind::Parameter);
        assert_eq!(param.range, BytePos(7)..BytePos(8));
        assert!(param.children.is_empty());
    }

    #[test]
    fn function_parameters_include_positional_and_named_with_defaults() {
        let text = "#let f(a, b: 1) = a";
        let outline = parse_outline(text);
        assert_eq!(outline.len(), 1, "{outline:?}");

        let function = &outline[0];
        assert_eq!(function.kind, SymbolKind::Function);
        let names: Vec<&str> = function
            .children
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(
            function
                .children
                .iter()
                .all(|symbol| symbol.kind == SymbolKind::Parameter)
        );
        assert_eq!(
            named(&function.children, "b").range,
            BytePos(10)..BytePos(11)
        );
    }

    #[test]
    fn anonymous_closure_under_a_let_binds_only_the_variable() {
        let text = "#let g = (x) => x";
        let outline = parse_outline(text);
        assert_eq!(outline.len(), 1, "{outline:?}");
        assert_eq!(outline[0].name, "g");
        assert_eq!(outline[0].kind, SymbolKind::Variable);
        assert!(
            outline[0].children.is_empty(),
            "the anonymous closure and its parameter are skipped"
        );
    }

    #[test]
    fn local_bindings_inside_a_function_body_are_skipped() {
        let text = "#let f() = {\n  let inner = 1\n  for y in (1, 2) { y }\n}";
        let outline = parse_outline(text);
        assert_eq!(outline.len(), 1, "{outline:?}");
        let function = named(&outline, "f");
        assert!(function.children.is_empty(), "{function:?}");
    }

    #[test]
    fn nested_named_closure_inside_a_function_is_skipped() {
        let text = "#let f() = {\n  let g(x) = x\n  g(1)\n}";
        let outline = parse_outline(text);
        assert_eq!(outline.len(), 1, "{outline:?}");
        let function = named(&outline, "f");
        assert!(
            function.children.is_empty(),
            "the nested closure and its parameter are skipped: {function:?}"
        );
    }

    #[test]
    fn label_inside_a_function_body_is_kept() {
        let text = "#let f() = [body <inner>]";
        let outline = parse_outline(text);
        let function = named(&outline, "f");
        let label = named(&function.children, "inner");
        assert_eq!(label.kind, SymbolKind::Label);
    }

    #[test]
    fn top_level_for_loop_and_destructuring_patterns_bind_variables() {
        let outline = parse_outline("#for x in (1, 2) [a]");
        assert_eq!(named(&outline, "x").kind, SymbolKind::Variable);

        let outline = parse_outline("#let (a, b) = (1, 2)");
        let names: Vec<&str> = outline.iter().map(|symbol| symbol.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(
            outline
                .iter()
                .all(|symbol| symbol.kind == SymbolKind::Variable)
        );
    }

    #[test]
    fn markup_label_is_kept_and_code_context_labels_are_skipped() {
        let outline = parse_outline("text <foo>");
        let label = named(&outline, "foo");
        assert_eq!(label.kind, SymbolKind::Label);
        assert_eq!(
            label.range,
            BytePos(5)..BytePos(10),
            "range keeps the angle brackets"
        );

        // A label after `(` or `,` inside code is an array item, not a document
        // label.
        assert!(parse_outline("#(x, <foo>)").is_empty());
        assert!(parse_outline("#(<foo>, <bar>)").is_empty());
    }

    #[test]
    fn empty_source_and_empty_heading_body_yield_nothing() {
        assert!(parse_outline("").is_empty());
        assert!(parse_outline("plain prose only").is_empty());
        // A heading whose body markup is empty must not emit a symbol.
        assert!(parse_outline("=\n").is_empty());
    }

    #[test]
    fn siblings_keep_source_order_and_nested_let_is_included() {
        let outline = parse_outline("#let a = 1\n#let b = 2");
        let names: Vec<&str> = outline.iter().map(|symbol| symbol.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);

        // A `let` inside a code block is a top-level variable of the enclosing
        // frame (there is no block symbol in Symbol mode).
        let outline = parse_outline("#{\n  let y = 1\n}");
        assert_eq!(outline.len(), 1, "{outline:?}");
        assert_eq!(outline[0].name, "y");
        assert_eq!(outline[0].kind, SymbolKind::Variable);
    }
}
