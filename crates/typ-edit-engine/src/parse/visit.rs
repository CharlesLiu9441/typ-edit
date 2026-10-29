//! The fused parse visitor.
//!
//! Every engine-local, full-tree derivation the parse worker needs — prose
//! stats, syntax-mode ranges, math atoms, list-item line starts and completion
//! definitions — is produced by ONE pre-order walk of the syntax tree instead
//! of one walk per output. Fusing the walks removes the repeated tree traversal
//! and `node_mode` computation that dominated edit cost; each output is
//! post-processed here exactly as its standalone walk did, so the final vectors
//! are byte-identical.
//!
//! Kept out of the fusion on purpose:
//! - `call_params` is cursor-local: it walks from the caret leaf down and up,
//!   not the whole tree, so it stays a separate pass.
//! - `highlight` is already updated incrementally by the incremental parser.
//! - `fold_ranges`, `prose_segments` and the diagnostics operate on a `Source`,
//!   not the `LinkedNode` tree.
//! - `lexical::symbols` lives in `typ-edit-analysis`; moving it here would cross
//!   a crate boundary, so it stays a separate pass.

use std::ops::Range;

use typ_edit_model::{BytePos, ByteRange, CompletionItem};
use typst::syntax::{LinkedNode, SyntaxKind};

use super::complete::{collect_definition, finish_definitions};
use super::diag::count_words;
use super::math_atoms::{self, Leaf};
use super::{Mode, node_mode};

/// The complete output of one [`visit`] walk over a document tree.
pub(crate) struct Fused {
    pub(crate) chars: usize,
    pub(crate) words: usize,
    pub(crate) code_ranges: Vec<ByteRange>,
    pub(crate) math_ranges: Vec<ByteRange>,
    pub(crate) blocked_ranges: Vec<ByteRange>,
    pub(crate) math_atoms: Vec<ByteRange>,
    pub(crate) list_lines: Vec<BytePos>,
    pub(crate) definitions: Vec<CompletionItem>,
}

/// One pre-order walk producing every engine-local full-tree derivation.
///
/// `text` is the document text the tree was parsed from; it is used only to
/// resolve the collected list-item byte offsets to their line starts.
pub(crate) fn visit(root: &LinkedNode<'_>, text: &str) -> Fused {
    let mut acc = Accumulator::default();
    walk(root, Mode::Markup, false, &mut acc);

    // Mode ranges: sort each mode's leaf ranges by start and merge overlapping
    // or adjacent spans, then `blocked` is the union of code, math and raw.
    let mut code = std::mem::take(&mut acc.code);
    let mut math = std::mem::take(&mut acc.math);
    let mut raw = std::mem::take(&mut acc.raw);
    for ranges in [&mut code, &mut math, &mut raw] {
        ranges.sort_by_key(|range| range.start);
        merge_ranges(ranges);
    }
    let mut blocked: Vec<Range<usize>> = code.iter().chain(&math).chain(&raw).cloned().collect();
    blocked.sort_by_key(|range| range.start);
    merge_ranges(&mut blocked);

    Fused {
        chars: acc.chars,
        words: acc.words,
        code_ranges: typed(code),
        math_ranges: typed(math),
        blocked_ranges: typed(blocked),
        math_atoms: math_atoms::atoms(acc.math_leaves),
        list_lines: finish_list_lines(acc.list_offsets, text),
        definitions: finish_definitions(acc.definitions),
    }
}

/// Mutable accumulators threaded through the single walk.
#[derive(Default)]
struct Accumulator {
    chars: usize,
    words: usize,
    code: Vec<Range<usize>>,
    math: Vec<Range<usize>>,
    raw: Vec<Range<usize>>,
    math_leaves: Vec<Leaf>,
    list_offsets: Vec<usize>,
    definitions: Vec<CompletionItem>,
}

/// Visits `node` in the pre-order every standalone walk used: the node's own
/// contribution first, then its children in order.
fn walk(node: &LinkedNode<'_>, parent: Mode, in_math: bool, acc: &mut Accumulator) {
    let mode = node_mode(node, parent);
    let in_math = in_math || node.kind() == SyntaxKind::Math;

    // Entry-time contributions, in pre-order.
    collect_definition(node, &mut acc.definitions);
    if matches!(
        node.kind(),
        SyntaxKind::ListItem | SyntaxKind::EnumItem | SyntaxKind::TermItem
    ) {
        acc.list_offsets.push(node.offset());
    }

    let mut has_children = false;
    for child in node.children() {
        has_children = true;
        walk(&child, mode, in_math, acc);
    }
    if has_children {
        return;
    }

    // Leaf-only contributions. The stats guard counts `Text`/`MathText` leaves;
    // the mode walk records every non-empty leaf by its syntax mode; the math
    // collection records non-empty leaves that sit in math.
    let range = node.range();
    if matches!(node.kind(), SyntaxKind::Text | SyntaxKind::MathText) {
        let text = node.leaf_text();
        acc.chars += text.chars().count();
        acc.words += count_words(text);
    }
    if range.start >= range.end {
        return;
    }
    match mode {
        Mode::Code => acc.code.push(range.clone()),
        Mode::Math => acc.math.push(range.clone()),
        Mode::Raw => acc.raw.push(range.clone()),
        Mode::Markup => {}
    }
    if in_math && mode == Mode::Math {
        acc.math_leaves.push(Leaf {
            kind: node.kind(),
            range,
            text: node.leaf_text().as_str().to_owned(),
        });
    }
}

/// Rewraps untyped ranges into byte-space newtypes.
fn typed(ranges: Vec<Range<usize>>) -> Vec<ByteRange> {
    ranges
        .into_iter()
        .map(|range| BytePos(range.start)..BytePos(range.end))
        .collect()
}

/// Merges overlapping or adjacent ranges from a start-sorted list.
fn merge_ranges(ranges: &mut Vec<Range<usize>>) {
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    *ranges = merged;
}

/// Resolves the collected item offsets to their line-start byte offsets,
/// deduplicated and sorted.
///
/// Every offset is resolved in one forward pass instead of scanning back to the
/// previous newline per item: `line_start` tracks where the current line begins
/// and the sorted offsets are drained as the cursor reaches them. An offset
/// sitting exactly on a `\n` still belongs to the line that starts at
/// `line_start`, so targets are resolved before the newline resets it.
fn finish_list_lines(mut offsets: Vec<usize>, text: &str) -> Vec<BytePos> {
    offsets.sort_unstable();
    offsets.dedup();

    let mut out: Vec<BytePos> = Vec::with_capacity(offsets.len());
    let mut next = 0;
    let mut line_start = 0;
    for (i, byte) in text.bytes().enumerate() {
        while offsets.get(next).is_some_and(|&offset| offset <= i) {
            out.push(BytePos(line_start));
            next += 1;
        }
        if byte == b'\n' {
            line_start = i + 1;
        }
    }
    // Defensive: the tree never yields offsets past the end, but clamp any that
    // do to the start of the final line.
    for _ in next..offsets.len() {
        out.push(BytePos(line_start));
    }

    // `out` is non-decreasing by construction; only adjacent duplicates from two
    // items on one line can remain.
    out.dedup();
    out
}
