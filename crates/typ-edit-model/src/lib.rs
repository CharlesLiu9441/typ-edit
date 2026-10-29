//! Pure cross-thread contract types shared by the UI thread and the background workers.
//!
//! PRE-1.0 CONTRACT: signatures here are consumed by the `typ-edit` binary,
//! `typ-edit-engine` and `typ-edit-editor`. The contract is deliberately refined
//! before 1.0: each concern is grouped into a focused sub-struct (parse stats,
//! layout hints, IDE data) so a new field widens one small type instead of a
//! single "everything" struct. The crate root re-exports every type, so these
//! grouped shapes do not churn the public paths callers depend on. Nothing in
//! this crate carries a typst value, which is why it has zero dependencies.
//! Results that do carry compiled typst values (`CompileResult`, `PreviewDoc`)
//! live with the compiler in `typ-edit-engine::results`.

mod compile;
mod completion;
mod diag;
mod fold;
mod highlight;
mod ide;
mod parse;
mod pos;
mod prose;
mod raster;
mod search;
mod spell;
mod symbol;
mod text;
mod unicode;

pub use compile::CompileRequest;
pub use completion::{CallParams, CompletionItem, CompletionKind, ValueSlot};
pub use diag::{Diag, Severity};
pub use fold::{FoldKind, FoldRange};
pub use highlight::HighlightKind;
pub use ide::{
    FileEdits, IdeQuery, IdeRequest, IdeResponse, IdeResult, PrepareRename, ReferenceLocation,
    TextEdit,
};
pub use parse::{IdeData, LayoutHints, ParseRequest, ParseResult, TextStats};
pub use pos::{BytePos, ByteRange, CharCount, CharPos, CharRange};
pub use prose::ProseSegment;
pub use raster::{RasterRequest, RasterResult};
pub use search::{
    FileMatches, ProjectSearchRequest, ProjectSearchResult, SearchMatch, SearchOptions,
};
pub use spell::{SpellIssue, SpellRequest, SpellResult};
pub use symbol::{SymbolKind, SymbolNode};
pub use text::{TextSnapshot, TextSource};
pub use unicode::is_cjk;
