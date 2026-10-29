//! Path normalization shared by every git operation.
//!
//! Git paths are forward-slashed; the app runs on Windows where `Path` may carry
//! backslashes, so every comparison key is normalized before use.

use std::path::{Path, PathBuf};

/// A git path (forward slashes) as a platform [`PathBuf`].
pub(crate) fn to_path(path: &str) -> PathBuf {
    PathBuf::from(path.replace('\\', "/"))
}

/// A [`Path`] as a forward-slashed comparison/`git2` key.
pub(crate) fn key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
