//! Diagnostics: severity and the file-mapped diagnostic value.

use std::path::PathBuf;

use crate::pos::ByteRange;

/// Severity of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A diagnostic mapped to a file and a byte range within it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub path: PathBuf,
    pub range: ByteRange,
    pub message: String,
    pub severity: Severity,
}
