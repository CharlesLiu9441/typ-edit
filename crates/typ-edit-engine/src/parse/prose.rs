//! Prose segments: the byte runs a spell worker lints.
//!
//! A recursive walk of the syntax tree emits one [`ProseSegment`] per contiguous
//! run of markup prose. A segment's `range` is the full byte span of the run —
//! inline markup markers (`*`, `_`, backticks around a hole, `#`) may sit inside
//! it and are harmlessly treated as punctuation by the spell classifier — and
//! its `holes` are the spans of the inline non-prose elements the classifier
//! must tag unlintable.
//!
//! Run rules (v1, pinned by the tests):
//! - Text and inter-word spaces are prose. Transparent wrappers recurse and keep
//!   the run going: `Strong`, `Emph`, `ContentBlock`, and any other container
//!   whose children are markup.
//! - Headings, list/enum/term items and their marker prefixes start a fresh run
//!   and exclude the marker (`= `, `- `, `/ `, `1. `) from the range.
//! - A run breaks at a `Parbreak` (blank line), a block construct (display
//!   `Equation` or `Raw`, `CodeBlock`, comments) and at the end of the enclosing
//!   block, item or heading.
//! - Inline `Equation`, inline `Raw`, `Ref` and `Label` are holes.
//! - A `#` expression is a hole. When it carries a single content-block argument
//!   (`#link("url")[text]`, `#text[x]`, `#underline[x]`), the call part is the
//!   hole and the content block continues the run, so its prose stays lintable;
//!   with zero or several content blocks (`#f(1)`, `#f([a], [b])`) the whole
//!   expression is one hole. Block-level escapes (`#let`, `#set`, `#show`, `#if`,
//!   loops, imports) break the run and contribute no prose.
//! - Empty runs (no non-whitespace prose) are dropped; a CJK-only run is kept
//!   because CJK filtering belongs to the spell worker.

use std::ops::Range;

use typ_edit_model::{BytePos, ProseSegment};
use typst::syntax::{LinkedNode, Source, SyntaxKind, ast};

/// Collects the prose segments of `source`, sorted by start.
pub fn prose_segments(source: &Source) -> Vec<ProseSegment> {
    let mut walk = ProseWalk {
        text: source.text(),
        run: None,
        out: Vec::new(),
    };
    walk.visit(&LinkedNode::new(source.root()));
    walk.flush();
    walk.out
}

/// Prose bytes being accumulated; holes attach only while a run is active.
struct Run {
    start: usize,
    end: usize,
    holes: Vec<Range<usize>>,
    has_prose: bool,
}

struct ProseWalk<'a> {
    text: &'a str,
    run: Option<Run>,
    out: Vec<ProseSegment>,
}

impl ProseWalk<'_> {
    fn visit(&mut self, node: &LinkedNode<'_>) {
        match node.kind() {
            SyntaxKind::Markup => self.walk_children(node),
            SyntaxKind::Strong | SyntaxKind::Emph | SyntaxKind::ContentBlock => {
                self.visit_transparent(node)
            }
            SyntaxKind::Text => {
                self.extend(node.range());
                if node.leaf_text().chars().any(|c| !c.is_whitespace()) {
                    self.mark_prose();
                }
            }
            SyntaxKind::Space
            | SyntaxKind::SmartQuote
            | SyntaxKind::Shorthand
            | SyntaxKind::Linebreak
            | SyntaxKind::Escape => self.extend(node.range()),
            SyntaxKind::Equation => {
                if node
                    .get()
                    .cast::<ast::Equation>()
                    .is_some_and(ast::Equation::block)
                {
                    self.flush();
                } else {
                    self.hole(node.range());
                }
            }
            SyntaxKind::Raw => {
                if node.get().cast::<ast::Raw>().is_some_and(ast::Raw::block) {
                    self.flush();
                } else {
                    self.hole(node.range());
                }
            }
            SyntaxKind::Ref | SyntaxKind::Label => self.hole(node.range()),
            SyntaxKind::Parbreak
            | SyntaxKind::LineComment
            | SyntaxKind::BlockComment
            | SyntaxKind::CodeBlock => self.flush(),
            SyntaxKind::Heading => self.visit_heading(node),
            SyntaxKind::ListItem | SyntaxKind::EnumItem | SyntaxKind::TermItem => {
                self.visit_list_item(node)
            }
            _ => {
                // Catch-all: a container whose children are markup keeps the run
                // going; a leaf contributes nothing.
                for child in node.children() {
                    self.visit(&child);
                }
            }
        }
    }

    /// Adds every inline child, pairing a `#` with the expression that follows.
    fn walk_children(&mut self, parent: &LinkedNode<'_>) {
        let mut children = parent.children().peekable();
        while let Some(child) = children.next() {
            if child.kind() != SyntaxKind::Hash {
                self.visit(&child);
                continue;
            }
            match children.peek().cloned() {
                Some(operand) if !is_markup_kind(operand.kind()) => {
                    self.hash_expression(&child, &operand);
                    children.next();
                }
                // A bare `#` (or one followed by markup) contributes nothing.
                _ => {}
            }
        }
    }

    /// Extends the run over a transparent wrapper and recurses into its markup.
    ///
    /// The wrapper's own range contributes to the span (so emphasis markers stay
    /// inside the segment) while only its `Markup` child yields prose; the
    /// delimiters (`*`, `_`, `[`, `]`) are never holes.
    fn visit_transparent(&mut self, node: &LinkedNode<'_>) {
        self.extend(node.range());
        self.walk_markup_child(node);
    }

    fn walk_markup_child(&mut self, node: &LinkedNode<'_>) {
        for child in node.children() {
            if child.kind() == SyntaxKind::Markup {
                self.walk_children(&child);
            }
        }
    }

    /// A heading is its own run; the marker and the space after it are excluded.
    fn visit_heading(&mut self, node: &LinkedNode<'_>) {
        self.flush();
        self.walk_markup_child(node);
        self.flush();
    }

    /// A list/enum/term item is its own run, with its marker excluded.
    fn visit_list_item(&mut self, node: &LinkedNode<'_>) {
        self.flush();
        for child in node.children() {
            match child.kind() {
                SyntaxKind::ListMarker | SyntaxKind::EnumMarker | SyntaxKind::TermMarker => {}
                SyntaxKind::Markup => self.walk_children(&child),
                _ => self.visit(&child),
            }
        }
        self.flush();
    }

    /// Handles a `#` escape and the expression that follows it.
    fn hash_expression(&mut self, hash: &LinkedNode<'_>, expression: &LinkedNode<'_>) {
        if is_block_expression(expression.kind()) {
            self.flush();
            return;
        }
        match body_content_block(expression) {
            Some(body) => {
                self.hole(hash.range().start..body.range().start);
                self.visit_transparent(&body);
                if body.range().end < expression.range().end {
                    self.hole(body.range().end..expression.range().end);
                }
            }
            None => self.hole(hash.range().start..expression.range().end),
        }
    }

    fn extend(&mut self, range: Range<usize>) {
        match &mut self.run {
            Some(run) => {
                run.start = run.start.min(range.start);
                run.end = run.end.max(range.end);
            }
            None => {
                self.run = Some(Run {
                    start: range.start,
                    end: range.end,
                    holes: Vec::new(),
                    has_prose: false,
                });
            }
        }
    }

    fn hole(&mut self, range: Range<usize>) {
        if let Some(run) = &mut self.run {
            run.start = run.start.min(range.start);
            run.end = run.end.max(range.end);
            run.holes.push(range);
        }
    }

    fn mark_prose(&mut self) {
        if let Some(run) = &mut self.run {
            run.has_prose = true;
        }
    }

    /// Emits the current run trimmed of leading/trailing whitespace, dropping it
    /// when it carries no prose.
    fn flush(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        if !run.has_prose {
            return;
        }
        let slice = &self.text[run.start..run.end];
        let start = run.start + (slice.len() - slice.trim_start().len());
        let end = run.start + slice.trim_end().len();
        if start >= end {
            return;
        }
        let holes = merge_holes(run.holes, start..end);
        self.out.push(ProseSegment {
            range: BytePos(start)..BytePos(end),
            holes: holes
                .into_iter()
                .map(|hole| BytePos(hole.start)..BytePos(hole.end))
                .collect(),
        });
    }
}

/// The single content-block argument of an inline expression, if it has one.
///
/// `#link("url")[text]` and `#text[x]` qualify; `#f([a], [b])` (two) and
/// `#f(1)` (none) do not, so both fall back to "the whole expression is a hole".
fn body_content_block<'a>(expression: &LinkedNode<'a>) -> Option<LinkedNode<'a>> {
    if expression.kind() == SyntaxKind::ContentBlock {
        return Some(expression.clone());
    }
    let args = expression
        .children()
        .find(|child| child.kind() == SyntaxKind::Args)?;
    let mut blocks = args
        .children()
        .filter(|child| child.kind() == SyntaxKind::ContentBlock);
    let first = blocks.next()?;
    blocks.next().is_none().then_some(first)
}

/// Expression kinds that are block-level at markup: they break the prose run.
fn is_block_expression(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LetBinding
            | SyntaxKind::SetRule
            | SyntaxKind::ShowRule
            | SyntaxKind::Contextual
            | SyntaxKind::Conditional
            | SyntaxKind::WhileLoop
            | SyntaxKind::ForLoop
            | SyntaxKind::ModuleImport
            | SyntaxKind::ModuleInclude
            | SyntaxKind::CodeBlock
            | SyntaxKind::Destructuring
            | SyntaxKind::DestructAssignment
            | SyntaxKind::LoopBreak
            | SyntaxKind::LoopContinue
            | SyntaxKind::FuncReturn
    )
}

/// Markup kinds that never follow a `#` as its operand; every other kind is
/// treated as an expression.
fn is_markup_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Markup
            | SyntaxKind::Text
            | SyntaxKind::Space
            | SyntaxKind::Parbreak
            | SyntaxKind::Strong
            | SyntaxKind::Emph
            | SyntaxKind::Equation
            | SyntaxKind::Raw
            | SyntaxKind::Ref
            | SyntaxKind::Label
            | SyntaxKind::SmartQuote
            | SyntaxKind::Shorthand
            | SyntaxKind::Linebreak
            | SyntaxKind::Escape
            | SyntaxKind::Heading
            | SyntaxKind::ListItem
            | SyntaxKind::EnumItem
            | SyntaxKind::TermItem
            | SyntaxKind::LineComment
            | SyntaxKind::BlockComment
    )
}

/// Clips holes to `bounds`, dropping empties, then sorts and merges overlapping
/// or adjacent spans.
fn merge_holes(mut holes: Vec<Range<usize>>, bounds: Range<usize>) -> Vec<Range<usize>> {
    holes.retain_mut(|hole| {
        hole.start = hole.start.max(bounds.start);
        hole.end = hole.end.min(bounds.end);
        hole.start < hole.end
    });
    holes.sort_by_key(|hole| hole.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(holes.len());
    for hole in holes {
        match merged.last_mut() {
            Some(last) if hole.start <= last.end => last.end = last.end.max(hole.end),
            _ => merged.push(hole),
        }
    }
    merged
}
