//! Results that carry compiled typst values; they live with the compiler, not
//! with the pure message contract in `typ-edit-model`.

use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::hover::{self, HoverInfo};
use crate::world::WorldState;
use typ_edit_model::Diag;
use typst::WorldExt;
use typst::layout::Point;
use typst::syntax::{Side, Source, Span};
use typst_layout::PagedDocument;

/// Result of a [`CompileRequest`](typ_edit_model::CompileRequest).
#[derive(Clone)]
pub struct CompileResult {
    pub generation: u64,
    pub document: Option<Arc<PagedDocument>>,
    /// Kept for source queries (jump_from_click / jump_from_cursor).
    pub(crate) world: Option<Arc<WorldState>>,
    pub diagnostics: Vec<Diag>,
    /// Fatal error text when no document could be produced.
    pub error: Option<String>,
    pub page_count: usize,
    /// Project-relative files the world read and did not find on disk. The app
    /// asks the collaboration server for them and recompiles; empty for local
    /// (non-collab) sessions.
    pub missing: Vec<String>,
}

impl CompileResult {
    /// Source of `path` as this compilation saw it, or `None` without a world.
    pub fn source_of(&self, path: &Path) -> Option<Source> {
        self.world.as_ref()?.source_of(path)
    }

    /// Maps a 0-based `page` and page-local `point` to a source file and byte offset.
    ///
    /// External URLs and page positions are not actionable and yield `None`.
    pub fn jump_at(&self, page: usize, point: Point) -> Option<(PathBuf, usize)> {
        let world = self.world.as_ref()?;
        let document = self.document.as_ref()?;
        let position = typst::introspection::PagedPosition {
            page: NonZeroUsize::new(page + 1)?,
            point,
        };
        match typst_ide::jump_from_click(&**world, &**document, &position) {
            Some(typst_ide::Jump::File(id, byte_offset)) => match world.path_of(id) {
                Some(path) => Some((path, byte_offset)),
                None => {
                    log::warn!("preview: cannot map {id:?} to a filesystem path");
                    None
                }
            },
            Some(typst_ide::Jump::Url(_) | typst_ide::Jump::Position(_)) | None => None,
        }
    }

    /// Semantic hover for `byte` in `path`, or `None` without a world/document.
    pub fn hover_info(&self, path: &Path, byte: usize) -> Option<HoverInfo> {
        let world = self.world.as_ref()?;
        let document = self.document.as_ref()?;
        let source = world.source_of(path)?;
        hover::hover_info(world, document, &source, byte)
    }

    /// Resolves a compiled-layout [`Span`] back to its source: the file it
    /// belongs to plus its byte range. `None` when there is no world, the span
    /// is detached (no [`FileId`](typst::syntax::FileId)), or the file has no
    /// filesystem path (for example a package source). Used by the debug frame
    /// tree to label and jump to individual layout nodes.
    pub fn span_source(&self, span: Span) -> Option<(PathBuf, Range<usize>)> {
        let world = self.world.as_ref()?;
        let id = span.id()?;
        let range = world.range(span)?;
        world.path_of(id).map(|path| (path, range))
    }

    /// Resolves the definition under `byte` in `path` to a file and byte offset.
    pub fn definition_at(&self, path: &Path, byte: usize) -> Option<(PathBuf, usize)> {
        let world = self.world.as_ref()?;
        let document = self.document.as_ref()?;
        let source = world.source_of(path)?;
        match typst_ide::definition(&**world, Some(&**document), &source, byte, Side::Before) {
            Some(typst_ide::Definition::Span(span)) => span
                .id()
                .and_then(|id| world.range(span).map(|range| (id, range.start))),
            Some(typst_ide::Definition::File(id)) => Some((id, 0)),
            Some(typst_ide::Definition::Std(_)) | None => None,
        }
        .and_then(|(id, offset)| world.path_of(id).map(|path| (path, offset)))
    }
}

/// Hands a freshly compiled document to the raster worker.
#[derive(Clone)]
pub struct PreviewDoc {
    pub generation: u64,
    pub document: Arc<PagedDocument>,
}
