//! First-parse gate for freshly opened large buffers.
//!
//! Opening a large file lays the whole buffer out twice: once immediately with
//! no highlights, then again when the parse result replaces them with real
//! spans. The gate holds the `TextEdit` back until the first parse lands, so
//! the pane builds one highlighted galley instead of an un-highlighted one plus
//! a second full relayout. Small buffers, where the extra layout is cheap,
//! open immediately.

/// Buffer size above which a freshly opened file waits for its first parse.
pub(super) const LARGE_OPEN_GATE_BYTES: usize = 256 * 1024;

/// Tracks the parse result a freshly opened large buffer is waiting for.
#[derive(Default)]
pub(super) struct EditorGate {
    pending: Option<u64>,
    focus_pending: bool,
}

impl EditorGate {
    /// Arms the gate for `generation` when `text_len` is a large buffer, and
    /// disarms it for small ones.
    pub(super) fn arm(&mut self, generation: u64, text_len: usize) {
        self.pending = (text_len > LARGE_OPEN_GATE_BYTES).then_some(generation);
    }

    /// Clears a pending gate without a parse, for tabs that never parse (plain
    /// text) so the placeholder cannot stick.
    pub(super) fn disarm(&mut self) {
        self.pending = None;
    }

    /// Whether the pane must show the loading placeholder for a `text_len`-byte
    /// buffer instead of the `TextEdit`.
    pub(super) fn is_pending(&self, text_len: usize) -> bool {
        self.pending.is_some() && text_len > LARGE_OPEN_GATE_BYTES
    }

    /// Records a parse at `parse_generation`, clearing the gate when that parse
    /// is the awaited one or newer. Returns whether it just cleared, which arms
    /// a focus request so the editor reclaims what the placeholder interrupted.
    ///
    /// The caller only reports a parse that matched the active tab, so a stale
    /// result from a switched-away tab can never clear the gate.
    pub(super) fn observe(&mut self, parse_generation: u64) -> bool {
        if self.pending.is_some_and(|gate| parse_generation >= gate) {
            self.pending = None;
            self.focus_pending = true;
            true
        } else {
            false
        }
    }

    /// Arms a focus request independent of the parse gate, so a preview
    /// jump-to-source that moved the caret hands the keyboard back to the
    /// editor on the next frame.
    pub(super) fn request_focus(&mut self) {
        self.focus_pending = true;
    }

    /// Takes the focus request armed when the gate last cleared.
    pub(super) fn take_focus(&mut self) -> bool {
        std::mem::take(&mut self.focus_pending)
    }
}
