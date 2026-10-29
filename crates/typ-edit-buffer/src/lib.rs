//! Rope-backed text buffer with an incremental line index, grouped undo/redo
//! and reactive edit hooks.
//!
//! [`TextBuffer`] stores text in a [`ropey::Rope`] and tracks every `\n` in a
//! balanced FHQ treap ([`treap`]) so line queries and suffix shifts stay
//! logarithmic. All public positions and ranges are **char** indices (egui is
//! char-indexed); byte conversion is available for integrations such as typst.
//!
//! Edits are recorded as [`Operation`]s. [`Operation::inverse`] simply swaps
//! Insert and Delete while keeping `at` and `text`, because the edited span
//! occupies the same char range in both directions. Undo/redo grouping, the
//! treap lazy invariant and the hook borrow trick are documented at their
//! definitions.

mod buffer;
mod edits;
mod operation;
mod snapshot;
mod treap;

pub use buffer::{EditHook, TextBuffer};
pub use operation::Operation;
pub use snapshot::TextSnapshot;
