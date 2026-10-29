//! Prose segments for the spell worker.
//!
//! The parse worker extracts one [`ProseSegment`] per contiguous run of markup
//! prose; the spell worker lints each segment with its own tokenizer and marks
//! the inline non-prose spans (`holes`) as unlintable. Byte offsets match
//! [`crate::Diag`], fold and highlight ranges.

use crate::pos::ByteRange;

/// One run of prose in typst source; byte offsets into the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProseSegment {
    /// Full byte span of the run (may include inline markup markers like `*`).
    pub range: ByteRange,
    /// Byte spans of inline non-prose elements inside `range` (math, raw, refs,
    /// `#expr`); the spell classifier tags these `Unlintable`. Sorted and
    /// non-overlapping.
    pub holes: Vec<ByteRange>,
}
