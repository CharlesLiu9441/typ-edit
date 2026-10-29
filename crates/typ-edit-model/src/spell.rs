//! Spell-check request and result: byte-ranged issues with replacement suggestions.

use std::sync::Arc;

use crate::pos::ByteRange;
use crate::prose::ProseSegment;
use crate::text::TextSnapshot;

/// One misspelling: the byte range it covers, the word and its suggestions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpellIssue {
    /// BYTE range into the spell-checked text (matches Diag/fold/highlight ranges).
    pub range: ByteRange,
    pub word: String,
    pub suggestions: Vec<String>,
}

/// Message to the spell worker: either spell-check a tab's buffer or drop the
/// worker's cached results for a tab that has closed.
#[derive(Clone, Debug)]
pub enum SpellRequest {
    /// Spell-check `text`, rebuild the issue list.
    Check {
        generation: u64,
        /// Stable id of the tab whose buffer this request checks; the result is
        /// applied only while that tab is still active.
        tab: u64,
        /// Snapshot of the buffer, so the worker is handed a cheap handle
        /// instead of copying the whole text on every edit; the worker
        /// materializes it on its own thread.
        text: TextSnapshot,
        /// Prose runs the parse worker extracted from the same snapshot; the
        /// spell worker lints each one with its own parser. Shared (`Arc`)
        /// because the worker thread owns the request, and empty when the parse
        /// predates the spell feature.
        segments: Arc<Vec<ProseSegment>>,
    },
    /// Drop the cached results for `tab`; sent when the tab closes so its
    /// per-tab cache is not retained for the process lifetime.
    Forget { tab: u64 },
}

/// Result of a [`SpellRequest`].
#[derive(Clone, Debug, Default)]
pub struct SpellResult {
    pub generation: u64,
    pub tab: u64,
    pub issues: Vec<SpellIssue>,
}
