//! Typed positions and ranges, and the byte-vs-char conversion policy.
//!
//! Typst and the analysis/engine messages speak in **byte** offsets
//! ([`BytePos`] / [`ByteRange`]); the editor and its rope/buffer internals speak
//! in **char** indices ([`CharPos`] / [`CharRange`]). A [`CharCount`] is a
//! length in chars, never an index.
//!
//! The two spaces are deliberately distinct types so they cannot be mixed
//! accidentally:
//!
//! - Conversions (byte <-> char) happen ONLY at named boundaries, and each such
//!   boundary owns the mapping for one concrete text buffer.
//! - There are deliberately NO implicit `From<usize>` / `From<BytePos> for
//!   CharPos` impls.
//! - There is deliberately NO arithmetic between the spaces; you cannot add a
//!   [`BytePos`] and a [`CharPos`].
//!
//! Because the inner field is public and the types are plain newtypes, the
//! boundary layer can unwrap (`.0`) to do the one real conversion, then re-wrap.

use std::ops::Range;

/// A byte offset into a UTF-8 text. Produced by typst/analysis; only the
/// boundary layer converts it to char space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct BytePos(pub usize);

/// A char index into a rope/text buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CharPos(pub usize);

/// A count of chars (NOT a position — must never be used as an index).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CharCount(pub usize);

/// A half-open byte range `start..end`.
pub type ByteRange = Range<BytePos>;

/// A half-open char range `start..end`.
pub type CharRange = Range<CharPos>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtypes_compare_by_value() {
        assert_eq!(BytePos(3), BytePos(3));
        assert_ne!(BytePos(3), BytePos(4));
        assert_eq!(CharPos(3), CharPos(3));
        assert_ne!(CharPos(3), CharPos(4));
        assert_eq!(CharCount(3), CharCount(3));
        assert_ne!(CharCount(3), CharCount(4));
    }

    #[test]
    fn newtypes_order_by_inner_value() {
        assert!(BytePos(1) < BytePos(2));
        assert!(CharPos(1) < CharPos(2));
        assert!(CharCount(1) < CharCount(2));
    }

    #[test]
    fn byte_range_holds_typed_endpoints() {
        fn takes(_: ByteRange) {}
        let range: ByteRange = BytePos(0)..BytePos(5);
        assert_eq!(range.start, BytePos(0));
        assert_eq!(range.end, BytePos(5));
        takes(BytePos(1)..BytePos(2));
    }

    #[test]
    fn char_range_holds_typed_endpoints() {
        fn takes(_: CharRange) {}
        let range: CharRange = CharPos(0)..CharPos(5);
        assert_eq!(range.start, CharPos(0));
        assert_eq!(range.end, CharPos(5));
        takes(CharPos(1)..CharPos(2));
    }

    #[test]
    fn default_is_zero() {
        assert_eq!(BytePos::default(), BytePos(0));
        assert_eq!(CharPos::default(), CharPos(0));
        assert_eq!(CharCount::default(), CharCount(0));
    }

    #[test]
    fn newtypes_are_copy_send_sync() {
        fn assert_traits<T: Copy + Send + Sync>() {}
        assert_traits::<BytePos>();
        assert_traits::<CharPos>();
        assert_traits::<CharCount>();
    }
}
