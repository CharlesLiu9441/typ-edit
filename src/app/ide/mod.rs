//! Inline rename (F2) and find-references (Shift+F12): both drive the engine
//! ide worker against the compiled world. Rename rewrites the reference set
//! across open tabs and on-disk files; find-references converts the same
//! reference set into the search view's result shape.

mod references;
mod ui;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::document::DocumentController;
use super::search::{LeftView, SearchState};
use super::state::{HoverState, SessionState, ToastMsg};
use references::{ReferencePreviewRequest, ReferencePreviewWorker, ident_at_editor};
use typ_edit_editor::{EditorState, floor_char_boundary};
use typ_edit_engine::Engine;
use typ_edit_engine::accept::accepts_exact_current;
use typ_edit_i18n::t;
use typ_edit_model::{
    BytePos, CompletionItem, FileEdits, IdeQuery, IdeResponse, IdeResult, PrepareRename,
    ProjectSearchResult, ReferenceLocation, TextEdit,
};

/// IDE overlay state.
#[derive(Default)]
pub(super) struct IdeState {
    pub(super) rename: Option<RenameSession>,
    /// Label of an in-flight find-references query (the identifier under the
    /// cursor when it was requested), or `None` when none is pending. A late
    /// [`IdeResponse::References`] is routed only while this is set.
    pub(super) references_pending: Option<String>,
    /// Member-completion position waiting for an engine answer, or `None` when
    /// none is pending. The query is sent only once the compiled world matches
    /// the live buffer, so a dot typed this frame is answered by the compile
    /// that actually contains it.
    pub(super) completions_pending: Option<PendingCompletions>,
    /// Off-UI reference preview formatting; created on the first query.
    reference_preview: Option<ReferencePreviewWorker>,
    /// Query currently being formatted by the reference worker.
    reference_pending: Option<PendingReferencePreview>,
    /// Monotonic id so a stale reference preview can never consume a newer label.
    reference_seq: u64,
}

/// One reference response waiting for the preview worker.
struct PendingReferencePreview {
    seq: u64,
    generation: u64,
    label: String,
}

/// A member-completion position waiting for (or awaiting) an engine answer.
#[derive(Clone, Debug)]
pub(super) struct PendingCompletions {
    /// File the request belongs to.
    pub(super) path: PathBuf,
    /// Editor caret byte the members are for.
    pub(super) byte: usize,
    /// Whether the query is currently with the engine.
    pub(super) in_flight: bool,
}

/// One in-flight rename: its target, the overlay anchor and the current phase.
pub(super) struct RenameSession {
    pub(super) path: PathBuf,
    pub(super) byte: usize,
    pub(super) anchor: Option<egui::Pos2>,
    /// The prepare result once known (`None` while `Preparing`).
    pub(super) prepare: Option<PrepareRename>,
    /// Current field text, preserved across phases.
    pub(super) input: String,
    /// One-shot request to focus the field.
    pub(super) focus_pending: bool,
    pub(super) phase: RenamePhase,
}

/// Which overlay a rename session is showing.
pub(super) enum RenamePhase {
    /// The prepare query is in flight.
    Preparing,
    /// The name field is open.
    Editing,
    /// A label rename waits for confirmation before it is applied.
    ConfirmingLabel { edits: Vec<FileEdits> },
}

/// Whether a stored world-completion list is for a different caret byte than
/// `caret` and must therefore be dropped (it is only valid at its own byte).
pub(super) fn completions_moved_away(byte: usize, caret: usize) -> bool {
    byte != caret
}

/// App-only side effects produced by routing one IDE worker answer: toasts for
/// the caller to raise, a left-view switch, and a rename's grouped edits for the
/// caller to apply (open tabs, disk, recompile).
#[derive(Default)]
pub(in crate::app) struct IdeOutcome {
    pub(in crate::app) toasts: Vec<ToastMsg>,
    pub(in crate::app) left_view: Option<LeftView>,
    pub(in crate::app) rename_edits: Option<Vec<FileEdits>>,
}

/// Where a rename request came from.
pub(in crate::app) struct RenameTarget {
    pub path: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub plain_text: bool,
}

/// The immutable half of a references result routing pass.
struct ReferenceLookup<'a> {
    locations: Vec<ReferenceLocation>,
    generation: u64,
    doc: &'a DocumentController,
    session: &'a SessionState,
    root: Option<&'a Path>,
}

impl IdeState {
    /// Clears the rename overlay and the pending-references marker.
    pub(in crate::app) fn clear_pending(&mut self) {
        self.rename = None;
        self.references_pending = None;
    }

    /// Drops the rename overlay without touching the references marker.
    pub(in crate::app) fn clear_rename(&mut self) {
        self.rename = None;
    }

    /// Drops the pending member-completion request.
    pub(in crate::app) fn clear_completions_pending(&mut self) {
        self.completions_pending = None;
    }

    /// Clears the in-flight flag of a pending completion (IDE worker panic), so
    /// the next pump resends it.
    pub(in crate::app) fn clear_completions_in_flight(&mut self) {
        if let Some(pending) = &mut self.completions_pending {
            pending.in_flight = false;
        }
    }

    /// Starts a rename at `byte`, anchored at `anchor` when the editor reported
    /// its caret rect. A no-op for plain-text tabs, an unnamed scratch buffer, a
    /// missing document or a missing project root.
    pub(in crate::app) fn request_rename(
        &mut self,
        byte: usize,
        anchor: Option<egui::Pos2>,
        engine: &Engine,
        doc: &DocumentController,
        target: RenameTarget,
    ) {
        let RenameTarget {
            path,
            root,
            plain_text,
        } = target;
        if plain_text {
            return;
        }
        let Some(path) = path else {
            return;
        };
        let (Some(root), Some(resources)) = (root, doc.doc.as_ref()) else {
            return;
        };
        let result = Arc::clone(&resources.result);
        self.rename = Some(RenameSession {
            path: path.clone(),
            byte,
            anchor,
            prepare: None,
            input: String::new(),
            focus_pending: true,
            phase: RenamePhase::Preparing,
        });
        engine.request_ide(
            result,
            root,
            IdeQuery::PrepareRename {
                path,
                byte: BytePos(byte),
            },
        );
    }

    /// Requests the references of the identifier under the cursor and shows
    /// them in the search view. Returns a `references.none` toast for the caller
    /// to raise when there is no source to query (plain-text tab, unnamed
    /// buffer, missing document or root).
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn request_references(
        &mut self,
        engine: &Engine,
        doc: &DocumentController,
        path: Option<PathBuf>,
        root: Option<PathBuf>,
        cursor_byte: usize,
        editor: &EditorState,
        plain_text: bool,
    ) -> Option<ToastMsg> {
        if plain_text {
            return Some(ToastMsg::warning(t("references.none")));
        }
        let Some(path) = path else {
            return Some(ToastMsg::warning(t("references.none")));
        };
        let (Some(root), Some(resources)) = (root, doc.doc.as_ref()) else {
            return Some(ToastMsg::warning(t("references.none")));
        };
        let label = ident_at_editor(editor, cursor_byte).unwrap_or_default();
        let result = Arc::clone(&resources.result);
        self.references_pending = Some(label);
        engine.request_ide(
            result,
            root,
            IdeQuery::References {
                path,
                byte: BytePos(cursor_byte),
            },
        );
        None
    }

    /// Requests world-backed member completions at `byte`. A no-op for plain-text
    /// tabs, an unnamed scratch buffer, a missing document or a missing project
    /// root; the editor's `world_completions` is populated when the answer lands.
    pub(in crate::app) fn request_completions(
        &mut self,
        byte: usize,
        engine: &Engine,
        doc: &DocumentController,
        session: &SessionState,
        root: Option<PathBuf>,
        plain_text: bool,
    ) {
        if plain_text {
            return;
        }
        let active = &session.tabs[session.active_tab].editor;
        let Some(path) = active.path.clone() else {
            return;
        };
        self.completions_pending = Some(PendingCompletions {
            path,
            byte,
            in_flight: false,
        });
        self.send_pending_completions(engine, doc, session, root);
    }

    /// Sends the pending completion query once the compiled world matches the
    /// live buffer, so the dot typed this frame is answered by the compile that
    /// contains it (and an incomplete `it.`, whose compile has no document, by
    /// the world's source). A no-op when nothing is pending, a query is in
    /// flight, the tab changed, or the compile predates the edit.
    pub(in crate::app) fn send_pending_completions(
        &mut self,
        engine: &Engine,
        doc: &DocumentController,
        session: &SessionState,
        root: Option<PathBuf>,
    ) {
        let Some((path, byte, in_flight)) = self
            .completions_pending
            .as_ref()
            .map(|pending| (pending.path.clone(), pending.byte, pending.in_flight))
        else {
            return;
        };
        if in_flight {
            return;
        }
        let active = &session.tabs[session.active_tab].editor;
        if active.path.as_deref() != Some(path.as_path()) {
            self.completions_pending = None;
            return;
        }
        let Some(result) = doc.ide_result.as_ref() else {
            return;
        };
        if !doc.completion_world_is_current() {
            return;
        }
        let Some(root) = root else {
            return;
        };
        if let Some(pending) = self.completions_pending.as_mut() {
            pending.in_flight = true;
        }
        engine.request_ide(
            Arc::clone(result),
            root,
            IdeQuery::Complete {
                path,
                byte: BytePos(byte),
            },
        );
    }

    /// Routes one IDE worker answer into the rename or references flow,
    /// dropping answers from a stale generation (a newer document was compiled
    /// since the query). App-only effects are returned in [`IdeOutcome`].
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn handle_ide_result(
        &mut self,
        result: IdeResult,
        doc: &DocumentController,
        search: &mut SearchState,
        hover: &mut HoverState,
        session: &SessionState,
        root: Option<PathBuf>,
        engine: &Engine,
        ctx: &egui::Context,
    ) -> IdeOutcome {
        let generation = result.generation;
        let mut outcome = IdeOutcome::default();
        match result.response {
            IdeResponse::References(locations) => {
                self.route_references(
                    ReferenceLookup {
                        locations,
                        generation,
                        doc,
                        session,
                        root: root.as_deref(),
                    },
                    search,
                    &mut outcome,
                    ctx,
                );
            }
            IdeResponse::Completions(items) => {
                self.apply_completions(items, generation, doc, hover, engine, session, root);
            }
            response => {
                let current = doc.doc.as_ref().map(|doc| doc.generation);
                if !accepts_exact_current(current, generation) {
                    return outcome;
                }
                let Some(mut session) = self.rename.take() else {
                    return outcome;
                };
                match response {
                    IdeResponse::PrepareRename(prepared) => {
                        if !matches!(session.phase, RenamePhase::Preparing) {
                            self.rename = Some(session);
                            return outcome;
                        }
                        match prepared {
                            Some(prepare) => {
                                session.input = prepare.placeholder.clone();
                                session.prepare = Some(prepare);
                                session.phase = RenamePhase::Editing;
                                self.rename = Some(session);
                            }
                            None => outcome
                                .toasts
                                .push(ToastMsg::warning(t("rename.unavailable"))),
                        }
                    }
                    IdeResponse::Rename(edits) => match edits {
                        Some(edits) => {
                            if !matches!(
                                session.phase,
                                RenamePhase::Editing | RenamePhase::ConfirmingLabel { .. }
                            ) {
                                self.rename = Some(session);
                                return outcome;
                            }
                            let confirmed =
                                matches!(session.phase, RenamePhase::ConfirmingLabel { .. });
                            let is_label = session.prepare.as_ref().is_some_and(|p| p.is_label);
                            if is_label && !confirmed {
                                session.phase = RenamePhase::ConfirmingLabel { edits };
                                self.rename = Some(session);
                            } else {
                                outcome.rename_edits = Some(edits);
                            }
                        }
                        None => outcome.toasts.push(ToastMsg::error(t("rename.failed"))),
                    },
                    // References and completions are handled above.
                    IdeResponse::References(_) | IdeResponse::Completions(_) => {
                        self.rename = Some(session);
                    }
                }
            }
        }
        outcome
    }

    /// Converts a find-references answer into the search view, reading each
    /// file's text from its open tab first and from disk otherwise. A stale
    /// answer (a newer document compiled since the query) is dropped while the
    /// pending marker stays so the current query's answer can still land.
    ///
    /// Nothing found still switches to the Search view, showing the reference
    /// header with the empty-results state, and queues the `references.none`
    /// toast.
    fn route_references(
        &mut self,
        lookup: ReferenceLookup<'_>,
        _search: &mut SearchState,
        outcome: &mut IdeOutcome,
        ctx: &egui::Context,
    ) {
        let ReferenceLookup {
            locations,
            generation,
            doc,
            session,
            root,
        } = lookup;
        if self.references_pending.is_none() {
            return;
        }
        if !accepts_exact_current(doc.doc.as_ref().map(|doc| doc.generation), generation) {
            return;
        }
        let label = self.references_pending.take().unwrap_or_default();
        let open: Vec<(PathBuf, typ_edit_model::TextSnapshot)> = session
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.editor
                    .path
                    .clone()
                    .map(|path| (path, tab.editor.text_snapshot()))
            })
            .collect();
        let seq = self.reference_seq.wrapping_add(1);
        self.reference_seq = seq;
        let worker = self
            .reference_preview
            .get_or_insert_with(ReferencePreviewWorker::spawn);
        let request = ReferencePreviewRequest {
            seq,
            locations,
            root: root.map(Path::to_path_buf),
            open,
            generation,
            ctx: ctx.clone(),
        };
        if worker.tx.send(request).is_ok() {
            self.reference_pending = Some(PendingReferencePreview {
                seq,
                generation,
                label,
            });
            outcome.left_view = Some(LeftView::Search);
        } else {
            outcome.left_view = Some(LeftView::Search);
            outcome.toasts.push(ToastMsg::warning(t("references.none")));
        }
    }

    /// Applies a completed reference preview. Only the newest query is used; a
    /// reply from a superseded query is dropped without touching its label.
    pub(in crate::app) fn poll_reference_previews(
        &mut self,
        doc: &DocumentController,
        search: &mut SearchState,
        outcome: &mut IdeOutcome,
    ) {
        let mut replies = Vec::new();
        if let Some(worker) = &self.reference_preview {
            while let Ok(reply) = worker.rx.try_recv() {
                replies.push(reply);
            }
        }
        for reply in replies {
            let Some(pending) = self.reference_pending.as_ref() else {
                continue;
            };
            if pending.seq != reply.seq {
                continue;
            }
            if !accepts_exact_current(doc.doc.as_ref().map(|doc| doc.generation), reply.generation)
            {
                continue;
            }
            let pending = self
                .reference_pending
                .take()
                .expect("pending reference preview");
            match reply.result {
                Ok(result) => search.apply_references(result, pending.label),
                Err(_) => {
                    outcome.toasts.push(ToastMsg::warning(t("references.none")));
                    search.apply_references(
                        ProjectSearchResult {
                            generation: pending.generation,
                            ..ProjectSearchResult::default()
                        },
                        pending.label,
                    );
                }
            }
            outcome.left_view = Some(LeftView::Search);
        }
    }

    /// Stores a world-completion answer for the caret byte it was requested at.
    ///
    /// A stale generation is dropped without clearing the pending request, so
    /// the retry that follows a newer compile can still land; the byte stored
    /// with the list is always the requested one, keeping the pair consistent.
    #[allow(clippy::too_many_arguments)]
    fn apply_completions(
        &mut self,
        items: Vec<CompletionItem>,
        generation: u64,
        doc: &DocumentController,
        hover: &mut HoverState,
        engine: &Engine,
        session: &SessionState,
        root: Option<PathBuf>,
    ) {
        let Some((byte, in_flight)) = self
            .completions_pending
            .as_ref()
            .map(|pending| (pending.byte, pending.in_flight))
        else {
            return;
        };
        if !in_flight {
            return;
        }
        let current = doc.ide_result.as_ref().map(|result| result.generation);
        if !accepts_exact_current(current, generation) {
            if let Some(pending) = self.completions_pending.as_mut() {
                pending.in_flight = false;
            }
            self.send_pending_completions(engine, doc, session, root);
            return;
        }
        self.completions_pending = None;
        hover.set_world_completions(byte, items);
    }

    /// Sends the rename query for the current overlay text.
    pub(in crate::app) fn send_rename(
        &mut self,
        session: &RenameSession,
        engine: &Engine,
        doc: &DocumentController,
        root: Option<PathBuf>,
    ) {
        let Some(doc) = doc.doc.as_ref() else {
            return;
        };
        let Some(root) = root else {
            return;
        };
        let result = Arc::clone(&doc.result);
        engine.request_ide(
            result,
            root,
            IdeQuery::Rename {
                path: session.path.clone(),
                byte: BytePos(session.byte),
                new_name: session.input.clone(),
            },
        );
    }
}

/// `(total edits, distinct files)` for a rename's grouped edits.
pub(super) fn edit_counts(edits: &[FileEdits]) -> (usize, usize) {
    let count = edits.iter().map(|file| file.edits.len()).sum();
    let mut paths: Vec<&Path> = edits.iter().map(|file| file.path.as_path()).collect();
    paths.sort_unstable();
    paths.dedup();
    (count, paths.len())
}

/// Applies `edits` to `text` back-to-front, skipping empty, invalid and
/// overlapping ranges. Pure, so the on-disk rewrite path is exercised without a
/// filesystem.
pub(super) fn apply_edits_to_text(text: &str, edits: &[TextEdit]) -> String {
    let mut ordered: Vec<(Range<usize>, &str)> = edits
        .iter()
        .filter_map(|edit| normalize_edit(text, edit))
        .collect();
    if ordered.is_empty() {
        return text.to_owned();
    }
    ordered.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_owned();
    let mut min_start: Option<usize> = None;
    for (range, replacement) in ordered {
        if min_start.is_some_and(|start| range.end > start) {
            continue;
        }
        min_start = Some(range.start);
        out.replace_range(range, replacement);
    }
    out
}

/// Rewrites `path` with `edits` applied, propagating a read or write failure.
pub(super) fn rewrite_file(path: &Path, edits: &[TextEdit]) -> std::io::Result<()> {
    let text = crate::platform::fs_ops::read_to_string(path)?;
    let new_text = apply_edits_to_text(&text, edits);
    if new_text == text {
        return Ok(());
    }
    crate::platform::fs_ops::write_atomic(path, new_text.as_bytes())
}

/// Clamps `edit` to `text`, snapping to char boundaries, or `None` when the
/// range is inverted, starts past the end, or replaces nothing with nothing.
fn normalize_edit<'a>(text: &str, edit: &'a TextEdit) -> Option<(Range<usize>, &'a str)> {
    if edit.range.start.0 > edit.range.end.0 || edit.range.start.0 > text.len() {
        return None;
    }
    let start = floor_char_boundary(text, edit.range.start.0);
    let end = floor_char_boundary(text, edit.range.end.0.min(text.len()));
    if start == end && edit.new_text.is_empty() {
        return None;
    }
    Some((start..end, edit.new_text.as_str()))
}
