use super::errors::first_error_message;
use super::search::{LeftView, search_due};
use super::spell::{spell_due, spell_toggled_on};
use super::{DocResources, TypEditApp};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use typ_edit_engine::accept::{accepts_latest, accepts_tab_latest};
use typ_edit_engine::{EngineHealth, Pipeline, PreviewDoc};
use typ_edit_i18n::t_with;
use typ_edit_model::{BytePos, CompileRequest, ParseRequest, TextSnapshot};

/// Idle time after an edit before the debounced compile is requested.
const COMPILE_DEBOUNCE: Duration = Duration::from_millis(200);

impl TypEditApp {
    pub(super) fn trigger_parse(&mut self) {
        // Plain-text tabs have no Typst parse; never send a snapshot for them.
        if self.active_plain_text() {
            return;
        }
        self.compile.note_edit(Instant::now());
        // This edit invalidates the shown spell issues; the spell debounce below
        // re-runs the pass once typing settles.
        if self.settings.editor.spell_check_enabled {
            self.spell.mark_dirty();
        }
        // While the worker holds a snapshot, sending another request would
        // force a whole-buffer clone on the next edit; queue one follow-up
        // instead and send it once the parse completes.
        if self.compile.parse_in_flight {
            self.compile.mark_parse_dirty();
            return;
        }
        self.compile.begin_parse();
        let text = self.editor().text_snapshot();
        let tab = self.session.tabs[self.session.active_tab].id;
        self.engine.request_parse(ParseRequest {
            generation: self.compile.edit_generation,
            tab,
            text,
            cursor: BytePos(self.compile.cursor_byte),
            want_spell: self.settings.editor.spell_check_enabled && !self.active_plain_text(),
        });
    }

    pub(super) fn trigger_compile(&mut self) {
        // A compile reads closed files from disk, so any queued remote-edit
        // mirror write must land before the worker starts.
        self.flush_mirror_writes(true);
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let Some(main) = self.preview_target() else {
            return;
        };
        self.compile.begin_compile();
        self.compile.requested_edit_generation = self.compile.edit_generation;
        // Overlay the active buffer plus every other tab with unsaved changes or
        // an unresolved disk conflict, so the preview sees in-memory edits
        // regardless of which tab is shown and never adopts an external copy
        // before the user chooses it.
        let overlay: Vec<(PathBuf, TextSnapshot)> = self
            .session
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                let path = tab.editor.path.as_ref()?;
                (index == self.session.active_tab
                    || tab.dirty
                    || self.external_changes.contains_key(&tab.id))
                .then(|| (path.clone(), tab.editor.text_snapshot()))
            })
            .collect();
        self.engine.request_compile(CompileRequest {
            generation: self.compile.compile_generation,
            root,
            main,
            overlay,
        });
    }

    /// Routes one worker health event into a toast and pipeline recovery.
    ///
    /// A restarted worker is retriggered so its lost request is replayed; a
    /// stopped worker is recorded as degraded and only has its bookkeeping
    /// cleared (its feature stays unavailable); a one-shot task panic is reported
    /// without touching the live worker.
    fn handle_engine_health(&mut self, event: EngineHealth) {
        match event {
            EngineHealth::WorkerRestarted { pipeline, .. } => {
                self.toast_warning(t_with(
                    "toast.worker_restarted",
                    &[("pipeline", pipeline.name())],
                ));
                self.recover_pipeline(pipeline, true);
            }
            EngineHealth::WorkerStopped { pipeline, .. } => {
                self.toast_error(t_with(
                    "toast.worker_stopped",
                    &[("pipeline", pipeline.name())],
                ));
                if self.ui.worker_degraded.is_none() {
                    self.ui.worker_degraded = Some(pipeline.name().to_owned());
                }
                self.recover_pipeline(pipeline, false);
            }
            EngineHealth::TaskPanicked { pipeline, .. } => {
                self.toast_error(t_with(
                    "toast.task_panicked",
                    &[("pipeline", pipeline.name())],
                ));
            }
        }
    }

    /// Clears a pipeline's in-flight bookkeeping after a worker panic, so the UI
    /// is never stuck waiting on a request that will never answer, and, for a
    /// restart, replays it. A stopped worker is not retriggered.
    fn recover_pipeline(&mut self, pipeline: Pipeline, retrigger: bool) {
        match pipeline {
            Pipeline::Parse => {
                self.compile.clear_parse_in_flight();
                if retrigger {
                    self.trigger_parse();
                }
            }
            Pipeline::Spell => {
                self.spell.clear_in_flight();
                if retrigger {
                    let enabled = self.settings.editor.spell_check_enabled;
                    let plain = self.active_plain_text();
                    self.spell.trigger_spell(
                        &self.engine,
                        &self.compile,
                        &self.session.tabs[self.session.active_tab],
                        enabled,
                        plain,
                    );
                }
            }
            Pipeline::Search => {
                self.search.clear_in_flight();
                if retrigger && self.left_view == LeftView::Search && !self.search.query.is_empty()
                {
                    self.search.mark_dirty();
                }
            }
            Pipeline::Compile => {
                self.compile.clear_compile_in_flight();
                if retrigger {
                    self.trigger_compile();
                }
            }
            // Raster has no in-flight flag; the next frame re-requests.
            Pipeline::Raster => {}
            // A panicked IDE worker must not leave a stuck overlay or a
            // permanently in-flight completion; clearing `in_flight` lets the
            // next pump resend the pending completion.
            Pipeline::Ide => {
                self.ide.clear_pending();
                self.ide.clear_completions_in_flight();
            }
        }
    }

    pub(super) fn pump(&mut self, ctx: &egui::Context) {
        while let Some(reply) = self.format_worker.poll() {
            self.apply_format_reply(reply);
        }
        self.poll_project_watcher(ctx);
        self.git.drain_and_apply();
        while let Ok(report) = self.export.rx.try_recv() {
            // Exactly one result per spawn: release the slot on success,
            // failure and cancellation alike.
            self.export.finish_export();
            if let Some(report) = report {
                if report.ok {
                    self.toast_success(report.message);
                } else {
                    self.toast_error(report.message);
                }
            }
        }

        let events = self.engine.drain();

        // Health first: a restart/stop must reset the in-flight flags the result
        // loops below rely on before their (possibly lost) results are handled.
        for event in events.health {
            self.handle_engine_health(event);
        }

        let active_tab = self.session.tabs[self.session.active_tab].id;
        let mut applied_parse = false;
        for parse in events.parses {
            if !accepts_tab_latest(
                active_tab,
                self.compile.parse_generation,
                parse.tab,
                parse.generation,
            ) {
                continue;
            }
            self.compile.apply_parse(parse);
            applied_parse = true;
        }

        if applied_parse {
            self.compile
                .editor_gate
                .observe(self.compile.parse_generation);
            let rows = self
                .compile
                .rebuild_diagnostics(&self.session, self.tree.root.as_deref());
            self.ui.error_rows = rows;
            if self.compile.parse_dirty {
                self.compile.clear_parse_dirty();
                self.trigger_parse();
            }
        }

        // Spell results use the parse acceptance rule: active tab, at least the
        // applied generation. A stale pass is dropped so its byte ranges can
        // never paint on another buffer.
        let spell_enabled = self.settings.editor.spell_check_enabled && !self.active_plain_text();
        if !spell_enabled {
            // Once the feature is off (or a plain-text tab is shown) the underlines
            // must go; the guard makes this run at most once per state change.
            if self.spell.in_flight
                || self.spell.dirty
                || !self.spell.issues.is_empty()
                || self.spell.was_enabled
            {
                self.spell.reset_for_new_document();
            }
        } else {
            for spell in events.spells {
                if accepts_tab_latest(
                    active_tab,
                    self.spell.generation,
                    spell.tab,
                    spell.generation,
                ) {
                    self.spell.apply_result(spell.generation, spell.issues);
                }
            }
            // Toggling the feature on (or showing the first Typst tab) must
            // request a pass even without an edit. The applied parse was sent
            // while the feature was off, so its `prose` is empty; re-parse once
            // with `want_spell` to fill the segments the pass needs.
            if spell_toggled_on(spell_enabled, self.spell.was_enabled) {
                self.spell.mark_dirty();
                self.trigger_parse();
            }
        }
        self.spell.note_enabled(spell_enabled);

        // Project search: a workspace change invalidates the stored result set
        // and drops any late result by bumping the applied generation past it.
        if self.tree.root != self.search.root {
            self.search.reset_for_workspace(self.tree.root.clone());
            if self.left_view == LeftView::Search && !self.search.query.is_empty() {
                self.search.mark_dirty();
            }
        }
        for search in events.searches {
            if accepts_latest(self.search.applied_generation, search.generation) {
                self.search.apply_result(search);
            }
        }

        // Debounced search request. An empty query clears the result and never
        // fires; a valid root/query waits out the debounce, repainting until the
        // idle deadline so the request runs without another input event. A
        // find-references result also lives with an empty query, so it is left
        // intact until the query is edited (which clears `reference_label`).
        if self.search.query.is_empty() {
            if self.search.reference_label.is_none() && self.search.result.is_some() {
                self.search.clear_result();
            }
            self.search.clear_dirty();
        } else {
            let ready = self.tree.root.is_some();
            let idle = self.search.last_change.map(|changed| changed.elapsed());
            if ready && search_due(true, self.search.dirty, self.search.in_flight, idle) {
                let root = self.tree.root.clone();
                self.search.trigger_search(root, &self.engine);
            } else if ready && self.search.dirty && !self.search.in_flight {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }

        for compile in events.compiles {
            if !accepts_latest(self.compile.compile_generation, compile.generation) {
                continue;
            }
            let open_path = self.editor().path.clone();
            let editor_diags = compile
                .diagnostics
                .iter()
                .filter(|d| Some(d.path.as_path()) == open_path.as_deref())
                .cloned()
                .collect();
            self.compile.apply_compile_meta(
                compile.error.clone(),
                compile.page_count,
                editor_diags,
            );
            self.request_missing_assets(&compile.missing);
            let rows = self
                .compile
                .rebuild_diagnostics(&self.session, self.tree.root.as_deref());
            self.ui.error_rows = rows;

            let result = Arc::new(compile);
            if let Some(document) = result.document.clone() {
                self.engine.set_preview_doc(PreviewDoc {
                    generation: result.generation,
                    document: document.clone(),
                });
                let page_sizes: Vec<(f32, f32)> = document
                    .pages()
                    .iter()
                    .map(|page| {
                        let size = page.frame.size();
                        (size.x.to_pt() as f32, size.y.to_pt() as f32)
                    })
                    .collect();
                let doc = DocResources {
                    generation: result.generation,
                    document,
                    result: Arc::clone(&result),
                    page_sizes,
                };
                self.compile.apply_compile_success(result, doc);
            } else {
                // Keep the previous document/result pair (stored together, so their
                // pairing stays consistent) and remember why the compile failed.
                // The failed result's world still answers member completions.
                let message = first_error_message(&self.ui.error_rows, &self.compile.compile_error);
                self.compile.apply_compile_failure(result, message);
            }
            let path = self.editor().path.clone();
            self.preview.follow_after_compile(
                &self.compile,
                &self.settings,
                self.layout.collapsed[crate::shell::layout::Pane::Preview as usize],
                path.as_deref(),
            );
        }
        // A compile that now matches the live buffer can answer a pending member
        // request (the `.` typed this frame only reaches the world here).
        let root = self.tree.root.clone();
        self.ide
            .send_pending_completions(&self.engine, &self.compile, &self.session, root);

        // IDE answers are routed after compiles so the rename flow sees the
        // latest document generation.
        for ide in events.ide {
            let root = self.tree.root.clone();
            let outcome = self.ide.handle_ide_result(
                ide,
                &self.compile,
                &mut self.search,
                &mut self.hover,
                &self.session,
                root,
                &self.engine,
                ctx,
            );
            for toast in outcome.toasts {
                self.raise_toast(toast);
            }
            if let Some(edits) = outcome.rename_edits {
                self.apply_symbol_rename(edits);
            }
            if let Some(view) = outcome.left_view {
                self.left_view = view;
            }
        }

        let mut reference_outcome = super::ide::IdeOutcome::default();
        self.ide
            .poll_reference_previews(&self.compile, &mut self.search, &mut reference_outcome);
        for toast in reference_outcome.toasts {
            self.raise_toast(toast);
        }
        if let Some(view) = reference_outcome.left_view {
            self.left_view = view;
        }

        for raster in events.raster {
            self.preview.apply_result(ctx, raster);
        }

        // Debounced spell pass after edits. It waits longer than the compile so
        // the linter runs once typing has settled; repaint until the idle
        // deadline so the pass fires without another input event.
        let spell_idle = self.compile.last_edit.map(|last| last.elapsed());
        if spell_due(
            self.settings.editor.spell_check_enabled,
            self.active_plain_text(),
            self.spell.dirty,
            self.spell.in_flight,
            spell_idle,
        ) {
            let enabled = self.settings.editor.spell_check_enabled;
            let plain = self.active_plain_text();
            self.spell.trigger_spell(
                &self.engine,
                &self.compile,
                &self.session.tabs[self.session.active_tab],
                enabled,
                plain,
            );
        } else if self.settings.editor.spell_check_enabled
            && !self.active_plain_text()
            && self.spell.dirty
            && !self.spell.in_flight
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        // Debounced compile after edits. While a large buffer is still waiting
        // for its first parse the compile is held back too: its CPU would
        // contend with the one galley layout the open has to do.
        if let Some(last) = self.compile.last_edit {
            if self
                .compile
                .editor_gate
                .is_pending(self.editor().len_bytes())
            {
                ctx.request_repaint_after(Duration::from_millis(50));
            } else if last.elapsed() >= COMPILE_DEBOUNCE {
                self.compile.clear_pending_edit();
                self.trigger_compile();
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }

        let path = self.editor().path.clone();
        self.preview.evaluate_auto_follow(
            ctx,
            &self.compile,
            &self.settings,
            self.layout.collapsed[crate::shell::layout::Pane::Preview as usize],
            path.as_deref(),
        );
    }
}
