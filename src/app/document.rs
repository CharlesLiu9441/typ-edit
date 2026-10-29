//! Document pipeline controller: the single owner of the parse/compile
//! generations, the current parsed/compiled document, the editor's first-parse
//! gate, and the diagnostics derived from them.
//!
//! Every mutation of this state goes through a method here; callers read the
//! `pub(in crate::app)` fields directly. Methods never take `&mut TypEditApp`:
//! foreign data (the session, the workspace root, a precomputed result) arrives
//! as discrete parameters, and app-level orchestration stays on `TypEditApp`.

use super::diag::{merged_diagnostics, with_path};
use super::errors::build_error_rows;
use super::gate::EditorGate;
use super::state::{DocResources, ErrorRow, SessionState};
use crate::shell::status::CompileState;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use typ_edit_engine::CompileResult;
use typ_edit_model::{Diag, ParseResult, Severity};

/// Inputs that determine the merged diagnostics and their rendered rows.
#[derive(Clone, PartialEq, Eq)]
struct DiagnosticKey {
    parse_generation: u64,
    compile_diags_generation: u64,
    buffer_epoch: u64,
    buffer_rev: u64,
    open_path: Option<PathBuf>,
    root: Option<PathBuf>,
}

/// Source/parse/compile pipeline: generations, the current document, per-buffer
/// diagnostics, and the editor's first-parse gate and caret memo.
#[derive(Default)]
pub(in crate::app) struct DocumentController {
    /// Current document (scenes fill in asynchronously).
    pub(in crate::app) doc: Option<DocResources>,
    /// Most recent compile result, successful or not. Member completions answer
    /// from its world's source, so an incomplete `it.` (whose compile yields no
    /// document) can still suggest members.
    pub(in crate::app) ide_result: Option<Arc<CompileResult>>,
    /// Highlight spans + syntax diagnostics of the open buffer.
    pub(in crate::app) parse: ParseResult,
    pub(in crate::app) parse_generation: u64,
    pub(in crate::app) compile_generation: u64,
    /// Edit generation captured when the latest compile request was sent.
    pub(in crate::app) requested_edit_generation: u64,
    /// Edit generation represented by the current compile result.
    pub(in crate::app) compiled_edit_generation: u64,
    pub(in crate::app) edit_generation: u64,
    /// Gates the editor pane behind the first parse of a freshly opened large
    /// buffer, avoiding a second full galley layout.
    pub(in crate::app) editor_gate: EditorGate,
    /// Whether a compile request has been sent and its result has not arrived yet.
    pub(in crate::app) compile_in_flight: bool,
    /// Diagnostics of the last compile, filtered to the open file.
    pub(in crate::app) editor_diags: Vec<Diag>,
    /// Parse (syntax) diagnostics merged with the compile diagnostics for the
    /// open file, de-duplicated and sorted. What the editor and error list show.
    pub(in crate::app) merged_diags: Vec<Diag>,
    /// Bumped only when the open-file compile diagnostics change.
    compile_diags_generation: u64,
    /// Rendered error rows and the exact inputs that produced them.
    diagnostic_rows_cache: Option<(DiagnosticKey, Vec<ErrorRow>)>,
    #[cfg(test)]
    pub(in crate::app) diagnostic_rebuild_count: u64,
    pub(in crate::app) compile_error: Option<String>,
    /// First error message of the last failed compile; while set, the preview keeps
    /// showing the previous good document with a failure banner.
    pub(in crate::app) preview_failure: Option<String>,
    pub(in crate::app) page_count: usize,
    /// Caret byte offset (updated after the editor pane is shown).
    pub(in crate::app) cursor_byte: usize,
    /// Byte boundary nearest the pointer while it is over the editor's text
    /// area (updated after the editor pane is shown). Drives drop insertion.
    pub(in crate::app) pointer_byte: Option<usize>,
    /// Current editor selections (byte ranges, one per caret, in list order).
    pub(in crate::app) selection: Vec<Range<usize>>,
    /// Pending debounced compile (set on every edit).
    pub(in crate::app) last_edit: Option<Instant>,
    /// Whether the parse worker holds a buffer snapshot; edits meanwhile only
    /// set `parse_dirty`, so no full-text clone is forced per keystroke.
    pub(in crate::app) parse_in_flight: bool,
    /// An edit arrived while a parse was in flight; re-request on completion.
    pub(in crate::app) parse_dirty: bool,
}

impl DocumentController {
    /// Clears every field derived from the previous buffer on a tab switch: the
    /// stale highlight/diagnostic ranges and the caret/flags that named it.
    pub(in crate::app) fn reset_for_new_document(&mut self) {
        self.parse = ParseResult::default();
        self.editor_diags.clear();
        self.merged_diags.clear();
        self.diagnostic_rows_cache = None;
        self.cursor_byte = 0;
        self.last_edit = None;
        self.parse_in_flight = false;
        self.parse_dirty = false;
        self.compiled_edit_generation = 0;
    }

    /// Records an edit: bumps the edit generation and stamps the debounce clock.
    pub(in crate::app) fn note_edit(&mut self, now: Instant) {
        self.edit_generation += 1;
        self.last_edit = Some(now);
    }

    /// Drops the pending debounced compile after it fires or is superseded.
    pub(in crate::app) fn clear_pending_edit(&mut self) {
        self.last_edit = None;
    }

    /// Marks the parse worker as holding a buffer snapshot.
    pub(in crate::app) fn begin_parse(&mut self) {
        self.parse_in_flight = true;
    }

    /// Queues another parse because an edit arrived while one was in flight.
    pub(in crate::app) fn mark_parse_dirty(&mut self) {
        self.parse_dirty = true;
    }

    /// Consumes the queued parse flag.
    pub(in crate::app) fn clear_parse_dirty(&mut self) {
        self.parse_dirty = false;
    }

    /// Clears the parse in-flight flag without an applied result (worker death),
    /// so the UI is never stuck waiting on a request that will never answer.
    pub(in crate::app) fn clear_parse_in_flight(&mut self) {
        self.parse_in_flight = false;
    }

    /// Applies an accepted parse: stores its generation and result and clears
    /// the in-flight flag. The caller rebuilds diagnostics afterwards.
    pub(in crate::app) fn apply_parse(&mut self, parse: ParseResult) {
        self.parse_generation = parse.generation;
        self.parse = parse;
        self.parse_in_flight = false;
    }

    /// Bumps the compile generation and marks a compile request in flight.
    pub(in crate::app) fn begin_compile(&mut self) {
        self.compile_generation += 1;
        self.compile_in_flight = true;
    }

    /// Clears the compile in-flight flag without an applied result (worker death).
    pub(in crate::app) fn clear_compile_in_flight(&mut self) {
        self.compile_in_flight = false;
    }

    /// Whether the compiled world includes the current buffer revision, so a
    /// completion query answered from it cannot show a stale world.
    pub(in crate::app) fn completion_world_is_current(&self) -> bool {
        self.compiled_edit_generation == self.edit_generation
    }

    /// Stores the compile metadata shared by the success and failure paths and
    /// clears the in-flight flag. `editor_diags` is filtered to the open file by
    /// the caller.
    pub(in crate::app) fn apply_compile_meta(
        &mut self,
        error: Option<String>,
        page_count: usize,
        editor_diags: Vec<Diag>,
    ) {
        self.compile_in_flight = false;
        self.compile_error = error;
        self.page_count = page_count;
        self.compiled_edit_generation = self.requested_edit_generation;
        if self.editor_diags != editor_diags {
            self.editor_diags = editor_diags;
            self.compile_diags_generation = self.compile_diags_generation.wrapping_add(1);
        }
    }

    /// Applies a successful compile: remembers the result and installs the new
    /// document, clearing any failure banner.
    pub(in crate::app) fn apply_compile_success(
        &mut self,
        result: Arc<CompileResult>,
        doc: DocResources,
    ) {
        let old_result = self.ide_result.replace(result);
        let old_doc = self.doc.replace(doc);
        if let Some(old) = old_result {
            super::background_drop::later(old);
        }
        if let Some(old) = old_doc {
            super::background_drop::later(old);
        }
        self.preview_failure = None;
    }

    /// Applies a failed compile: remembers the result (its world still answers
    /// member completions), keeps the previous document, and records the first
    /// failure message for the preview banner.
    pub(in crate::app) fn apply_compile_failure(
        &mut self,
        result: Arc<CompileResult>,
        message: String,
    ) {
        if let Some(old) = self.ide_result.replace(result) {
            super::background_drop::later(old);
        }
        self.preview_failure = Some(message);
    }

    /// Sets the caret byte offset reported by the editor.
    pub(in crate::app) fn set_cursor_byte(&mut self, byte: usize) {
        self.cursor_byte = byte;
    }

    /// Sets the byte boundary nearest the pointer while it is over the editor.
    pub(in crate::app) fn set_pointer_byte(&mut self, byte: Option<usize>) {
        self.pointer_byte = byte;
    }

    /// Replaces the editor's selection list.
    pub(in crate::app) fn set_selection(&mut self, ranges: Vec<Range<usize>>) {
        self.selection = ranges;
    }

    /// Rebuilds the merged diagnostic list (syntax + compile) and returns the
    /// error rows that render it. The open buffer's path and text come from
    /// `session`; `root` relativizes the displayed paths.
    ///
    /// Both the parse and the compile apply paths call this from the latest
    /// accepted state, and a tab switch calls it after clearing, so the editor
    /// and the error list never show diagnostics from a superseded buffer.
    pub(in crate::app) fn rebuild_diagnostics(
        &mut self,
        session: &SessionState,
        root: Option<&Path>,
    ) -> Vec<ErrorRow> {
        let editor = &session.tabs[session.active_tab].editor;
        let open_path = editor.path.clone();
        let key = DiagnosticKey {
            parse_generation: self.parse_generation,
            compile_diags_generation: self.compile_diags_generation,
            buffer_epoch: editor.buffer_epoch(),
            buffer_rev: editor.text_snapshot().rev(),
            open_path: open_path.clone(),
            root: root.map(Path::to_path_buf),
        };
        if let Some((cached_key, rows)) = &self.diagnostic_rows_cache
            && cached_key == &key
        {
            return rows.to_vec();
        }
        #[cfg(test)]
        {
            self.diagnostic_rebuild_count += 1;
        }
        let mut parse_diags = self.parse.diagnostics.clone();
        match open_path.as_deref() {
            Some(path) => with_path(&mut parse_diags, path),
            // No path to map syntax diagnostics to (plain-text buffer, or an
            // unnamed scratch): show only the compile diagnostics.
            None => parse_diags.clear(),
        }
        self.merged_diags = merged_diagnostics(parse_diags, &self.editor_diags);
        let rows = if self.merged_diags.is_empty() {
            Vec::new()
        } else {
            build_error_rows(
                &self.merged_diags,
                open_path.as_deref(),
                |byte| editor.line_column(byte),
                root,
            )
        };
        self.diagnostic_rows_cache = Some((key, rows.clone()));
        rows
    }

    /// Current compile state shown in the status bar.
    pub(in crate::app) fn compile_state(&self) -> CompileState {
        CompileState::resolve(
            self.compile_in_flight,
            self.doc.is_some(),
            self.compile_error_count(),
        )
    }

    /// Number of errors of the last compile: diagnostic errors, else the fatal error.
    pub(in crate::app) fn compile_error_count(&self) -> usize {
        let diagnostics = self
            .merged_diags
            .iter()
            .filter(|diag| diag.severity == Severity::Error)
            .count();
        if diagnostics > 0 {
            diagnostics
        } else {
            usize::from(self.compile_error.is_some())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::EditorTab;
    use typ_edit_editor::EditorState;
    use typ_edit_model::BytePos;

    fn session(text: &str) -> SessionState {
        let mut editor = EditorState::new();
        editor.set_text(text.to_owned());
        editor.path = Some(PathBuf::from("main.typ"));
        SessionState {
            tabs: vec![EditorTab {
                id: 0,
                editor,
                dirty: false,
            }],
            active_tab: 0,
            close_prompt: None,
            allow_close: false,
            next_tab_id: 1,
        }
    }

    fn diag(start: usize, end: usize) -> Diag {
        Diag {
            path: PathBuf::new(),
            range: BytePos(start)..BytePos(end),
            message: "probe".to_owned(),
            severity: Severity::Error,
        }
    }

    #[test]
    fn completion_world_gate_waits_for_the_compile_of_the_latest_edit() {
        let mut controller = DocumentController::default();
        assert!(
            controller.completion_world_is_current(),
            "nothing is pending"
        );

        controller.note_edit(Instant::now());
        controller.requested_edit_generation = controller.edit_generation;
        controller.begin_compile();
        assert!(!controller.completion_world_is_current());

        controller.apply_compile_meta(None, 1, Vec::new());
        assert!(controller.completion_world_is_current());

        controller.note_edit(Instant::now());
        assert!(
            !controller.completion_world_is_current(),
            "an edit after the compile must block stale-world completions"
        );
    }

    #[test]
    fn diagnostics_rebuild_only_when_an_input_changes() {
        let mut controller = DocumentController {
            parse_generation: 1,
            ..Default::default()
        };
        controller.parse.diagnostics = vec![diag(0, 3)];
        let mut session = session("abcdef");

        let first = controller.rebuild_diagnostics(&session, None);
        assert_eq!(controller.diagnostic_rebuild_count, 1);
        let second = controller.rebuild_diagnostics(&session, None);
        assert_eq!(
            controller.diagnostic_rebuild_count, 1,
            "an unchanged parse/buffer/compile input reuses the rows"
        );
        assert_eq!(first.len(), second.len());

        session.tabs[0].editor.set_text("abc".to_owned());
        let _ = controller.rebuild_diagnostics(&session, None);
        assert_eq!(
            controller.diagnostic_rebuild_count, 2,
            "a fresh buffer with rev 0 still invalidates the rows"
        );

        controller.apply_compile_meta(None, 1, vec![diag(0, 1)]);
        let _ = controller.rebuild_diagnostics(&session, None);
        assert_eq!(controller.diagnostic_rebuild_count, 3);
        controller.apply_compile_meta(None, 1, vec![diag(0, 1)]);
        let _ = controller.rebuild_diagnostics(&session, None);
        assert_eq!(
            controller.diagnostic_rebuild_count, 3,
            "identical compile diagnostics do not bump the generation"
        );
    }
}
