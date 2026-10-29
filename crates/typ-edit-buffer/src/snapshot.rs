//! O(1) value snapshots of a buffer's text.
//!
//! A [`TextSnapshot`] captures the buffer's text and revision without copying
//! the characters: [`ropey::Rope`] clones are O(1) structural clones, so
//! taking a snapshot and cloning it are both cheap. The captured text is
//! materialized explicitly with [`TextSnapshot::materialize`], meant to run on
//! the consuming thread.

use ropey::Rope;

use crate::buffer::TextBuffer;

/// Value snapshot of a buffer's text: an O(1) rope clone plus the revision it
/// captured.
///
/// Both construction ([`TextBuffer::snapshot`]) and [`Clone`] are O(1): a rope
/// clone shares its chunks copy-on-write. Use [`TextSnapshot::materialize`] to
/// obtain the full text; it allocates and is meant to run on the consuming
/// thread. A snapshot never exposes its rope.
#[derive(Clone)]
pub struct TextSnapshot {
    rope: Rope,
    rev: u64,
}

impl TextBuffer {
    /// Captures the current text and revision as an O(1) value snapshot.
    pub fn snapshot(&self) -> TextSnapshot {
        TextSnapshot {
            rope: self.rope.clone(),
            rev: self.rev,
        }
    }
}

impl TextSnapshot {
    /// Revision of the buffer at the moment this snapshot was taken.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Renders the full captured text.
    ///
    /// This is O(n) in the text length and allocates; call it on the thread
    /// that will consume the string.
    pub fn materialize(&self) -> String {
        self.rope.to_string()
    }
}

impl std::fmt::Debug for TextSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextSnapshot")
            .field("rev", &self.rev)
            .finish_non_exhaustive()
    }
}
