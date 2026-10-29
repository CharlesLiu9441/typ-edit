//! Code folding: maps the syntax tree to foldable byte ranges.
//!
//! A candidate covers a multi-line construct whose body the editor may
//! collapse. Ranges are byte offsets into `source.text()` (matching highlight
//! ranges) and only record the first and last logical line to hide.

use typ_edit_model::{BytePos, FoldKind, FoldRange};
use typst::syntax::{Lines, LinkedNode, Source, SyntaxKind, ast};

/// Collects fold candidates, sorted by `start` and deduplicated.
pub fn fold_ranges(source: &Source) -> Vec<FoldRange> {
    let mut walk = FoldWalk {
        text: source.text(),
        lines: source.lines(),
        headings: Vec::new(),
        out: Vec::new(),
    };
    walk.visit(&LinkedNode::new(source.root()));

    // A heading section extends to the next heading of the same or a shallower
    // level. A monotonic stack (levels strictly increasing) resolves every
    // boundary in one pass: an arriving heading closes all still-open sections
    // whose level is at least its own, then stays open itself.
    let text_len = walk.text.len();
    let headings = std::mem::take(&mut walk.headings);
    let mut open: Vec<(usize, usize)> = Vec::new();
    for &(start, level) in &headings {
        while let Some(&(_, open_level)) = open.last() {
            if open_level < level {
                break;
            }
            let Some((open_start, _)) = open.pop() else {
                break;
            };
            walk.push(open_start, start, FoldKind::Heading);
        }
        open.push((start, level));
    }
    for (start, _) in open {
        walk.push(start, text_len, FoldKind::Heading);
    }

    let mut out = walk.out;
    out.sort_by_key(|range| (range.start, range.end));
    out.dedup();
    out
}

/// Recursive tree walk that records headings and multi-line constructs.
struct FoldWalk<'a> {
    text: &'a str,
    lines: &'a Lines<String>,
    /// `(byte start, level)` of every heading in document order.
    headings: Vec<(usize, usize)>,
    out: Vec<FoldRange>,
}

impl FoldWalk<'_> {
    fn visit(&mut self, node: &LinkedNode<'_>) {
        let range = node.range();
        match node.kind() {
            SyntaxKind::Heading => {
                if let Some(heading) = node.get().cast::<ast::Heading>() {
                    self.headings.push((range.start, heading.depth().get()));
                }
            }
            SyntaxKind::BlockComment
            | SyntaxKind::ListItem
            | SyntaxKind::EnumItem
            | SyntaxKind::TermItem
            | SyntaxKind::Raw
            | SyntaxKind::Equation => {
                let kind = match node.kind() {
                    SyntaxKind::BlockComment => FoldKind::Comment,
                    SyntaxKind::Raw => FoldKind::Raw,
                    SyntaxKind::Equation => FoldKind::Equation,
                    _ => FoldKind::List,
                };
                self.push(range.start, range.end, kind);
            }
            SyntaxKind::CodeBlock
            | SyntaxKind::ContentBlock
            | SyntaxKind::Parenthesized
            | SyntaxKind::Array
            | SyntaxKind::Dict
            | SyntaxKind::Args => {
                // Hide only the inner lines, so the closing delimiter line
                // stays visible. An error node without a closer is skipped.
                if let Some(inner_end) = closing_delimiter_start(node) {
                    self.push(range.start, inner_end, FoldKind::Block);
                }
            }
            _ => {}
        }

        for child in node.children() {
            self.visit(&child);
        }
    }

    /// Records `start..trimmed(boundary)` when it spans more than one line.
    fn push(&mut self, start: usize, boundary: usize, kind: FoldKind) {
        let end = trim_trailing(self.text, boundary);
        if end <= start {
            return;
        }
        if self.lines.byte_to_line(start) == self.lines.byte_to_line(end) {
            return;
        }
        self.out.push(FoldRange {
            start: BytePos(start),
            end: BytePos(end),
            kind,
        });
    }
}

/// The exclusive end of `text[..boundary]` after trimming trailing whitespace,
/// so the returned offset stays on the construct's last content line.
fn trim_trailing(text: &str, boundary: usize) -> usize {
    text[..boundary.min(text.len())].trim_end().len()
}

/// The byte offset of `node`'s closing delimiter, if the last child is one.
fn closing_delimiter_start(node: &LinkedNode<'_>) -> Option<usize> {
    let last = node.children().next_back()?;
    matches!(
        last.kind(),
        SyntaxKind::RightBrace | SyntaxKind::RightBracket | SyntaxKind::RightParen
    )
    .then(|| last.offset())
}
