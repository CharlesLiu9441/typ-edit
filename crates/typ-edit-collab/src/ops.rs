//! Byte-offset text operations and change sets.
//!
//! A [`ChangeSet`] is everything a frame of user input produces: the file it
//! touches plus an ordered list of [`ChangeOp`]s. All offsets are **UTF-8 byte
//! offsets** (plan `§2` D10), never char indices, so they line up exactly with
//! the bytes both peers hold. Ops are applied in order; each op's `at` indexes
//! the text as it stands after the previous ops, which lets one frame carry
//! multi-cursor edits whose offsets naturally shift.
//!
//! [`validate`] and [`apply`] share one implementation, so a change set that
//! validates always applies and vice versa. Rejection is atomic: a change set is
//! never partially applied (plan `§5.3` R5).

use crate::error::RejectReason;
use serde::{Deserialize, Serialize};

/// One insert or delete against a text document, in UTF-8 byte offsets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeOp {
    /// Insert `text` at byte offset `at`.
    Insert { at: u64, text: String },
    /// Delete the bytes in `text` from byte offset `at`.
    ///
    /// The removed content is carried so the server can invert the op without
    /// reading the document, and so application can verify the peer held the
    /// same bytes (a mismatch is a hard fork).
    Delete { at: u64, text: String },
}

impl ChangeOp {
    /// The byte offset this op starts at.
    pub fn at(&self) -> u64 {
        match self {
            Self::Insert { at, .. } | Self::Delete { at, .. } => *at,
        }
    }

    /// The inverse op, occupying the same span in the same direction.
    pub fn inverse(&self) -> Self {
        match self {
            Self::Insert { at, text } => Self::Delete {
                at: *at,
                text: text.clone(),
            },
            Self::Delete { at, text } => Self::Insert {
                at: *at,
                text: text.clone(),
            },
        }
    }
}

/// A file-relative change: the path plus the ordered ops a frame produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    /// Root-relative path with forward slashes.
    pub file: String,
    /// Ops in application order.
    pub ops: Vec<ChangeOp>,
}

impl ChangeSet {
    /// Build a change set for `file`.
    pub fn new(file: impl Into<String>, ops: Vec<ChangeOp>) -> Self {
        Self {
            file: file.into(),
            ops,
        }
    }

    /// The inverse change set: reverse the op order, then invert each op.
    pub fn invert(&self) -> Self {
        Self {
            file: self.file.clone(),
            ops: invert_ops(&self.ops),
        }
    }
}

/// Reverse `ops` and invert each one.
///
/// Reversal is required because later ops were written against offsets that
/// already include the effect of earlier ones; undoing must peel them in the
/// opposite order.
pub fn invert_ops(ops: &[ChangeOp]) -> Vec<ChangeOp> {
    ops.iter().rev().map(ChangeOp::inverse).collect()
}

/// Validate a change set against `content` (plan `§5.3` R5).
///
/// Runs the exact application on a scratch copy and discards it, so success
/// guarantees [`apply`] will succeed and the original `content` is never
/// touched. The first offending op wins; nothing is partially applied.
pub fn validate(content: &str, ops: &[ChangeOp]) -> Result<(), RejectReason> {
    let mut scratch = content.to_owned();
    apply_in_place(&mut scratch, ops)
}

/// Apply a change set to `content`, returning the new text.
///
/// On any rejection the original `content` is unchanged and `Err` is returned.
pub fn apply(content: &str, ops: &[ChangeOp]) -> Result<String, RejectReason> {
    let mut text = content.to_owned();
    apply_in_place(&mut text, ops)?;
    Ok(text)
}

fn apply_in_place(text: &mut String, ops: &[ChangeOp]) -> Result<(), RejectReason> {
    for op in ops {
        match op {
            ChangeOp::Insert { at, text: insert } => {
                let at = offset_in_bounds(text, *at)?;
                text.insert_str(at, insert);
            }
            ChangeOp::Delete { at, text: removed } => {
                let at = offset_in_bounds(text, *at)?;
                let end = offset_in_bounds(text, at as u64 + removed.len() as u64)?;
                if !text.is_char_boundary(end) {
                    return Err(RejectReason::NotCharBoundary { at: end as u64 });
                }
                if &text[at..end] != removed.as_str() {
                    return Err(RejectReason::DeleteTextMismatch { at: at as u64 });
                }
                text.replace_range(at..end, "");
            }
        }
    }
    Ok(())
}

/// Convert `offset` to a `usize`, checking it is in bounds and on a character
/// boundary.
fn offset_in_bounds(text: &str, offset: u64) -> Result<usize, RejectReason> {
    let Ok(at) = usize::try_from(offset) else {
        return Err(RejectReason::OutOfBounds {
            at: offset,
            len: text.len() as u64,
        });
    };
    if at > text.len() {
        return Err(RejectReason::OutOfBounds {
            at: offset,
            len: text.len() as u64,
        });
    }
    if !text.is_char_boundary(at) {
        return Err(RejectReason::NotCharBoundary { at: offset });
    }
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(at: u64, text: &str) -> ChangeOp {
        ChangeOp::Insert {
            at,
            text: text.to_owned(),
        }
    }

    fn del(at: u64, text: &str) -> ChangeOp {
        ChangeOp::Delete {
            at,
            text: text.to_owned(),
        }
    }

    #[test]
    fn insert_and_delete_apply_in_order() {
        let ops = vec![ins(5, " world"), del(0, "hello")];
        let out = apply("hello", &ops).expect("valid ops");
        assert_eq!(out, " world");
    }

    #[test]
    fn sequential_ops_see_earlier_edits() {
        let ops = vec![ins(0, "ab"), ins(2, "cd")];
        assert_eq!(apply("XY", &ops).expect("valid"), "abcdXY");
    }

    #[test]
    fn delete_text_mismatch_is_rejected() {
        let ops = vec![del(0, "HELLO")];
        assert_eq!(
            apply("hello", &ops),
            Err(RejectReason::DeleteTextMismatch { at: 0 })
        );
    }

    #[test]
    fn out_of_bounds_is_rejected() {
        let ops = vec![ins(99, "x")];
        assert_eq!(
            apply("hi", &ops),
            Err(RejectReason::OutOfBounds { at: 99, len: 2 })
        );
    }

    #[test]
    fn cjk_multibyte_boundary_is_rejected() {
        let text = "\u{4f60}\u{597d}";
        assert_eq!(text.len(), 6);
        let ops = vec![ins(1, "x")];
        assert_eq!(
            apply(text, &ops),
            Err(RejectReason::NotCharBoundary { at: 1 })
        );
        let ok = vec![ins(3, "x")];
        assert_eq!(apply(text, &ok).expect("boundary ok"), "\u{4f60}x\u{597d}");
    }

    #[test]
    fn delete_span_must_end_on_boundary() {
        let text = "\u{4f60}\u{597d}";
        let ops = vec![del(0, "\u{4f60}\u{597d}")];
        assert_eq!(apply(text, &ops).expect("whole chars"), "");
    }

    #[test]
    fn multi_op_reject_leaves_content_unchanged() {
        let content = "hello";
        let ops = vec![ins(5, " world"), ins(99, "!")];
        assert!(apply(content, &ops).is_err());
        assert!(validate(content, &ops).is_err());
        assert_eq!(content, "hello");
    }

    #[test]
    fn validate_matches_apply() {
        let content = "ab\u{4f60}";
        let good = vec![ins(2, "Z"), del(2, "Z")];
        assert!(validate(content, &good).is_ok());
        assert!(apply(content, &good).is_ok());
        let bad = vec![del(2, "b")];
        assert_eq!(validate(content, &bad), apply(content, &bad).map(|_| ()));
    }

    #[test]
    fn invert_round_trips_content() {
        let content = "hello world";
        let ops = vec![ins(5, ","), del(6, " world")];
        let applied = apply(content, &ops).expect("apply");
        assert_eq!(applied, "hello,");
        let restored = apply(&applied, &invert_ops(&ops)).expect("invert");
        assert_eq!(restored, content);
    }

    #[test]
    fn changeset_invert_keeps_file_and_reverses() {
        let set = ChangeSet::new("a.typ", vec![ins(0, "x"), ins(1, "y")]);
        let inverse = set.invert();
        assert_eq!(inverse.file, "a.typ");
        assert_eq!(inverse.ops, vec![del(1, "y"), del(0, "x")]);
    }

    #[test]
    fn empty_ops_are_identity() {
        let ops: Vec<ChangeOp> = Vec::new();
        assert_eq!(apply("unchanged", &ops).expect("identity"), "unchanged");
        assert!(validate("unchanged", &ops).is_ok());
    }
}
