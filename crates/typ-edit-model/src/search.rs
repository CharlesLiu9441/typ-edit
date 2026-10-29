//! Project-wide search request and result: line-based, byte-ranged matches.

use std::path::PathBuf;

use crate::pos::{BytePos, CharRange};

/// Matching toggles for a project-wide query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub regex: bool,
    pub whole_word: bool,
}

/// Request: search `root` recursively for `query` under `options`.
#[derive(Clone, Debug, Default)]
pub struct ProjectSearchRequest {
    pub generation: u64,
    pub root: PathBuf,
    pub query: String,
    pub options: SearchOptions,
    /// Simple include globs (comma-separated in the UI, split by the app).
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// One match: a 0-based line, the byte range in the file, and a line preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    /// 0-based line number.
    pub line: usize,
    /// Match start BYTE offset in the file.
    pub byte: BytePos,
    pub byte_len: usize,
    /// The line's text, truncated to at most 240 chars around the match.
    pub preview: String,
    /// CHAR range of the match inside `preview`.
    pub preview_match: CharRange,
}

/// Every match in one file. Files are reported sorted by `relative`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatches {
    pub path: PathBuf,
    /// Root-relative path with forward slashes (display + glob target).
    pub relative: String,
    pub matches: Vec<SearchMatch>,
}

/// Result of a [`ProjectSearchRequest`].
#[derive(Clone, Debug, Default)]
pub struct ProjectSearchResult {
    pub generation: u64,
    pub files: Vec<FileMatches>,
    pub total_matches: usize,
    pub truncated: bool,
    /// Invalid regex, unreadable root, etc. A search that merely finds nothing
    /// leaves this `None`.
    pub error: Option<String>,
}
