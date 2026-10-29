//! Parse request and result: highlighting, diagnostics, prose stats and IDE data.

use crate::completion::{CallParams, CompletionItem};
use crate::diag::Diag;
use crate::fold::FoldRange;
use crate::highlight::HighlightKind;
use crate::pos::{BytePos, ByteRange, CharCount};
use crate::prose::ProseSegment;
use crate::symbol::SymbolNode;
use crate::text::TextSnapshot;

/// Request: parse `text`, rebuild highlight spans, syntax diagnostics and counts.
#[derive(Clone, Debug)]
pub struct ParseRequest {
    pub generation: u64,
    /// Stable id of the tab whose buffer this request parses. The result is
    /// applied only while that tab is still active, so a parse that finishes
    /// after a tab switch is discarded instead of painting stale highlights.
    pub tab: u64,
    /// Snapshot of the buffer. The editor hands the parse worker a cheap handle
    /// (an O(1) clone) instead of copying the whole text on every edit; the
    /// worker materializes it on its own thread.
    pub text: TextSnapshot,
    /// Caret byte offset into `text` at edit time; used to resolve the call
    /// around the cursor for parameter completion.
    pub cursor: BytePos,
    /// Whether the parse should also extract prose segments for spell checking.
    /// The app sets it only while the spell setting is on and the buffer is a
    /// Typst source; otherwise [`ParseResult::prose`] stays empty.
    pub want_spell: bool,
}

/// Prose metrics of the parsed text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStats {
    /// Whitespace-delimited word count (CJK characters count individually).
    pub words: usize,
    /// Prose character count; excludes spaces, raw blocks, comments and escapes.
    pub chars: CharCount,
}

/// Layout and mode hints derived from the syntax tree.
#[derive(Clone, Debug, Default)]
pub struct LayoutHints {
    /// Byte ranges of code / math / raw contexts where markup helpers
    /// (auto-pairing) must not run.
    pub blocked_ranges: Vec<ByteRange>,
    /// Byte ranges of code mode — identifiers are evaluated there.
    pub code_ranges: Vec<ByteRange>,
    /// Byte ranges of math mode (`$...$`).
    pub math_ranges: Vec<ByteRange>,
    /// Byte ranges of unbreakable math atoms inside math mode (leaf-level):
    /// every math leaf is an atom, and only a function name plus its `(` and a
    /// number plus its unit are glued. A break may fall before any atom, so
    /// adjacent atoms stay separate. Sorted and non-overlapping; empty outside
    /// math.
    pub math_atoms: Vec<ByteRange>,
    /// Line-start byte offsets of list items (`-`, `+`, `1.`, terms), used for
    /// list-aware indentation.
    pub list_lines: Vec<BytePos>,
}

/// IDE-facing data: document-local completions and the call around the caret.
#[derive(Clone, Debug, Default)]
pub struct IdeData {
    /// Names defined in the document (`#let`, parameters, imports, labels).
    pub definitions: Vec<CompletionItem>,
    /// Named parameters of the call the cursor sits in, if any.
    pub call_params: Option<CallParams>,
}

/// Result of a [`ParseRequest`].
///
/// The parse pass produces several unrelated concerns; each is grouped into the
/// sub-struct that owns it so a new field widens one focused type instead of the
/// whole result. `highlights` and `diagnostics` stay top-level because they are
/// the two primary products the editor and the error list consume directly.
#[derive(Clone, Debug, Default)]
pub struct ParseResult {
    pub generation: u64,
    /// Stable id of the tab this result belongs to (see [`ParseRequest::tab`]);
    /// the app applies it only while that tab is still active.
    pub tab: u64,
    pub highlights: Vec<(ByteRange, HighlightKind)>,
    /// Byte ranges + messages; `path` is filled in by the caller (current file).
    pub diagnostics: Vec<Diag>,
    /// Prose word / char counts for the status bar.
    pub stats: TextStats,
    /// Mode ranges and list-line hints for the editor's markup helpers.
    pub layout: LayoutHints,
    /// Document definitions and call-parameter completion data.
    pub ide: IdeData,
    /// Multi-line fold candidates for the editor's folding gutter.
    pub folds: Vec<FoldRange>,
    /// Lexical symbol tree (document outline), nested and in source order.
    pub symbols: Vec<SymbolNode>,
    /// Prose runs for the spell worker; empty unless the request set
    /// [`ParseRequest::want_spell`].
    pub prose: Vec<ProseSegment>,
}
