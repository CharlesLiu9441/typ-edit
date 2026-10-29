//! Foldable syntax ranges for code folding.

use crate::pos::BytePos;

/// Which construct a fold candidate covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldKind {
    Heading,
    Block,
    Comment,
    List,
    Raw,
    Equation,
}

/// A foldable byte range in the source. When collapsed, the editor hides the
/// logical lines strictly after `start`'s line and up to and including `end`'s
/// line: `(line_of(start), line_of(end)]`. `start < end` and the two must lie
/// on different lines (single-line constructs produce no candidate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoldRange {
    pub start: BytePos,
    pub end: BytePos,
    pub kind: FoldKind,
}
