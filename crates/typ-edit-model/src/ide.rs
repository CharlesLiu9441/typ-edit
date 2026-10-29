//! IDE wire types: reference locations, rename edits and the engine-worker
//! query/response envelope. Pure data only — no typst values cross this
//! boundary, so the engine can move them across threads.

use std::path::PathBuf;

use crate::CompletionItem;
use crate::pos::{BytePos, ByteRange};

/// One reference location: byte range inside `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceLocation {
    pub path: PathBuf,
    pub range: ByteRange,
}

/// A single text replacement (byte range) in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: ByteRange,
    pub new_text: String,
}

/// All edits for one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdits {
    pub path: PathBuf,
    pub edits: Vec<TextEdit>,
}

/// What a rename would change at the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrepareRename {
    pub range: ByteRange,
    pub placeholder: String,
    pub is_label: bool,
}

/// A query routed to the engine ide worker.
#[derive(Clone, Debug)]
pub enum IdeQuery {
    References {
        path: PathBuf,
        byte: BytePos,
    },
    PrepareRename {
        path: PathBuf,
        byte: BytePos,
    },
    Rename {
        path: PathBuf,
        byte: BytePos,
        new_name: String,
    },
    /// Member/context completions at `byte` (the editor caret byte), answered
    /// from the compiled world by `typst_ide::autocomplete`.
    Complete {
        path: PathBuf,
        byte: BytePos,
    },
}

/// The worker's answer to one [`IdeQuery`].
#[derive(Clone, Debug)]
pub enum IdeResponse {
    /// References for a [`IdeQuery::References`], sorted by path then start.
    References(Vec<ReferenceLocation>),
    /// Rename information, or `None` when the cursor is not renameable.
    PrepareRename(Option<PrepareRename>),
    /// Grouped rename edits, or `None` when the cursor is not renameable.
    Rename(Option<Vec<FileEdits>>),
    /// World-backed completions for a [`IdeQuery::Complete`], in the order the
    /// compiler produced them (empty when there is nothing to suggest).
    Completions(Vec<CompletionItem>),
}

/// A request routed to the engine ide worker.
#[derive(Clone, Debug)]
pub struct IdeRequest {
    pub generation: u64,
    pub root: PathBuf,
    pub query: IdeQuery,
}

/// The worker's answer to one [`IdeRequest`].
#[derive(Clone, Debug)]
pub struct IdeResult {
    pub generation: u64,
    pub response: IdeResponse,
}
