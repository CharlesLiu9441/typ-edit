//! Preview compilation request.

use std::path::PathBuf;

use crate::text::TextSnapshot;

/// Request: compile the preview target.
#[derive(Clone, Debug)]
pub struct CompileRequest {
    pub generation: u64,
    /// Project root directory (all project files are addressed relative to it).
    pub root: PathBuf,
    /// Absolute path of the preview target (main file or currently open file).
    pub main: PathBuf,
    /// In-memory overrides (absolute path -> contents) for unsaved editor
    /// buffers. The snapshot is an O(1) handle; the compile worker materializes
    /// it on its own thread instead of making the UI copy the document.
    pub overlay: Vec<(PathBuf, TextSnapshot)>,
}
