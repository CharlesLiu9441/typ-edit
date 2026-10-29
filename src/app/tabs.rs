use super::files::{format_on_save_applies, tree_entry_name};
use super::state::{Toast, ToastMsg};
use super::{ClosePrompt, EditorTab, TypEditApp, WriteOutcome};
use crate::shell::platform_ops;
use crate::theme::SemanticKind;
use std::path::{Path, PathBuf};
use std::time::Instant;
use typ_edit_editor::EditorState;
use typ_edit_i18n::{t, t_with};
use typ_edit_model::SpellRequest;
use typ_edit_widgets::tab_bar::{TabAction, TabBarOrientation, TabItem, move_item, moved_index};
use typ_edit_widgets::tui::{Tui, TuiBuilderLogic};

/// Clamps a byte offset into `text`, snapping down to the nearest UTF-8
/// boundary and past-the-end offsets onto the end.
pub(super) fn clamp_caret(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Index of the open tab whose path matches `path` exactly.
pub(super) fn find_open_tab(tabs: &[Option<PathBuf>], path: &Path) -> Option<usize> {
    tabs.iter().position(|tab| tab.as_deref() == Some(path))
}

/// Whether the active tab is an unnamed, empty, unmodified scratch buffer that
/// can adopt the next opened file instead of spawning a new tab.
pub(super) fn active_slot_reusable(
    tabs: &[Option<PathBuf>],
    active: usize,
    text_empty: bool,
    dirty: bool,
) -> bool {
    tabs.get(active).is_some_and(Option::is_none) && text_empty && !dirty
}

/// File name shown on the tab (or 未命名 for an unnamed scratch buffer).
pub(super) fn tab_label(tab: &EditorTab) -> String {
    tab.editor
        .path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| t("app.unnamed").into_owned())
}

/// Tab-bar label for the unsaved-changes prompt: name plus a bullet while dirty.
pub(super) fn tab_title(tab: &EditorTab) -> String {
    if tab.dirty {
        format!("{} ●", tab_label(tab))
    } else {
        tab_label(tab)
    }
}

/// Tab-bar label: file name, disambiguated with the parent directory when two
/// open tabs share the same name; 未命名 for an unnamed scratch buffer.
pub(super) fn tab_display_name(index: usize, tabs: &[Option<PathBuf>]) -> String {
    let Some(path) = tabs.get(index).and_then(Option::as_ref) else {
        return t("app.unnamed").into_owned();
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let collides = tabs.iter().enumerate().any(|(other_index, other)| {
        other_index != index
            && other
                .as_ref()
                .is_some_and(|p| p.file_name() == path.file_name())
    });
    if !collides {
        return name;
    }
    match path.parent().and_then(Path::file_name) {
        Some(parent) => format!("{}/{}", parent.to_string_lossy(), name),
        None => name,
    }
}

impl TypEditApp {
    /// The active tab's editor buffer.
    pub(super) fn editor(&self) -> &EditorState {
        &self.session.tabs[self.session.active_tab].editor
    }

    /// The active tab's editor buffer, mutably.
    pub(super) fn editor_mut(&mut self) -> &mut EditorState {
        &mut self.session.tabs[self.session.active_tab].editor
    }

    /// Switches the active tab and rebuilds the state that belongs to it.
    pub(super) fn activate_tab(&mut self, index: usize) {
        if index == self.session.active_tab || index >= self.session.tabs.len() {
            return;
        }
        self.session.set_active(index);
        self.on_tab_switched();
    }

    /// Drops state derived from the previous buffer: stale highlight/diagnostic
    /// ranges must not paint on the newly shown text.
    ///
    /// The compile is left to the edit debounce: [`Self::trigger_parse`] stamps
    /// `last_edit`, so compiling immediately would only be superseded (and
    /// redone) once the debounce elapsed, doubling the work.
    pub(super) fn on_tab_switched(&mut self) {
        // The previous tab's parse may still be running. `reset_for_new_document`
        // releases the in-flight flag so the new tab's request is sent now
        // instead of being deferred behind it; the old result is rejected by tab
        // id once it lands.
        self.compile.reset_for_new_document();
        // The previous buffer's spell ranges must not paint on this one. Orphan
        // any in-flight pass and request the new tab's issues right away, so the
        // first open does not have to wait for an edit.
        self.spell.reset_for_new_document();
        if self.active_plain_text() {
            // No parse will arrive to clear a gate armed by a Typst tab.
            self.compile.editor_gate.disarm();
        }
        self.trigger_parse();
        let enabled = self.settings.editor.spell_check_enabled;
        let plain = self.active_plain_text();
        self.spell.trigger_spell(
            &self.engine,
            &self.compile,
            &self.session.tabs[self.session.active_tab],
            enabled,
            plain,
        );
        let rows = self
            .compile
            .rebuild_diagnostics(&self.session, self.tree.root.as_deref());
        self.ui.error_rows = rows;
    }

    /// Closes `index`, asking about unsaved changes first when there are any.
    pub(super) fn close_tab(&mut self, index: usize) {
        if self.session.tabs.get(index).is_some_and(|tab| tab.dirty) {
            self.session.prompt_close(ClosePrompt::Tab(index));
        } else {
            self.remove_tab(index);
        }
    }

    /// Removes a tab; the last remaining one is reset to a fresh empty buffer.
    pub(super) fn remove_tab(&mut self, index: usize) {
        if index >= self.session.tabs.len() {
            return;
        }
        // A client's open-tab buffer is an overlay over the mirror. Before the
        // tab (and its text) disappears, queue the buffer so reopening the file
        // from the tree does not read a stale mirror copy.
        if self.collab.is_client()
            && let Some(root) = self.tree.root.as_deref()
            && let Some(path) = self.session.tabs[index].editor.path.as_deref()
            && path.starts_with(root)
        {
            let text = self.session.tabs[index].editor.shared_text();
            self.collab.queue_mirror_write_shared(
                path.to_path_buf(),
                text,
                std::time::Instant::now(),
            );
        }
        // The worker's per-tab lint cache would otherwise outlive the tab.
        let removed = self.session.tabs[index].id;
        if self
            .pending_save
            .as_ref()
            .is_some_and(|pending| pending.tab_id == removed)
        {
            self.pending_save = None;
        }
        self.external_changes.remove(&removed);
        self.engine
            .request_spell(SpellRequest::Forget { tab: removed });
        // Refreshes (or clears) the snapshot on the next debounce tick.
        self.recovery.note_change();
        if self.session.tabs.len() <= 1 {
            let id = self.session.alloc_tab_id();
            self.session.tabs[0] = EditorTab::new(id);
            self.session.set_active(0);
            self.on_tab_switched();
            return;
        }
        let was_active = index == self.session.active_tab;
        self.session.tabs.remove(index);
        if index < self.session.active_tab {
            self.session.set_active(self.session.active_tab - 1);
        } else if self.session.active_tab >= self.session.tabs.len() {
            self.session.set_active(self.session.tabs.len() - 1);
        }
        if was_active {
            self.on_tab_switched();
        }
    }

    /// Advances to the next/previous tab, wrapping around.
    pub(super) fn cycle_tab(&mut self, backwards: bool) {
        let len = self.session.tabs.len();
        if len < 2 {
            return;
        }
        let next = if backwards {
            (self.session.active_tab + len - 1) % len
        } else {
            (self.session.active_tab + 1) % len
        };
        self.activate_tab(next);
    }

    /// Toggles one pane; collapsing is refused when it would hide every pane.
    pub(super) fn toggle_pane(&mut self, index: usize) {
        if self.layout.popped[index] {
            // The popped pane is visible; the toggle hides it (close the window
            // and collapse the strip).
            self.layout.popped[index] = false;
            if self.layout.can_collapse() {
                self.layout.collapsed[index] = true;
            }
            return;
        }
        if self.layout.collapsed[index] {
            self.layout.collapsed[index] = false;
        } else if self.layout.can_collapse() {
            self.layout.collapsed[index] = true;
        }
    }

    /// Closes every tab in `indices`, unless one is dirty — then nothing is
    /// closed and the user is told to save or close them one by one.
    pub(super) fn close_clean_tabs(&mut self, indices: &[usize]) {
        if indices
            .iter()
            .any(|&index| self.session.tabs.get(index).is_some_and(|tab| tab.dirty))
        {
            self.toast_warning(t("toast.dirty_tabs"));
            return;
        }
        let mut ordered = indices.to_vec();
        ordered.sort_unstable();
        for &index in ordered.iter().rev() {
            self.remove_tab(index);
        }
    }

    /// Shows a neutral informational toast.
    pub(super) fn toast(&mut self, message: impl Into<String>) {
        self.toast_kind(SemanticKind::Info, message);
    }

    /// Shows a toast in an explicit semantic family.
    pub(super) fn toast_kind(&mut self, kind: SemanticKind, message: impl Into<String>) {
        self.ui.toast = Some(Toast {
            message: message.into(),
            kind,
            at: Instant::now(),
        });
    }

    /// Raises a deferred feedback message produced by an outcome struct.
    pub(super) fn raise_toast(&mut self, toast: ToastMsg) {
        self.toast_kind(toast.kind, toast.message);
    }

    /// Positive completion feedback (saved, installed, connected).
    pub(super) fn toast_success(&mut self, message: impl Into<String>) {
        self.toast_kind(SemanticKind::Success, message);
    }

    /// Recoverable warning feedback (skipped, disabled, disconnected).
    pub(super) fn toast_warning(&mut self, message: impl Into<String>) {
        self.toast_kind(SemanticKind::Warning, message);
    }

    /// Failure feedback (save/open/export/rename failures).
    pub(super) fn toast_error(&mut self, message: impl Into<String>) {
        self.toast_kind(SemanticKind::Error, message);
    }

    /// Writes tab `index` to `path` and adopts it as the tab's file.
    ///
    /// With 保存时自动格式化 enabled the buffer is first pretty-printed in
    /// memory, so the file and the editor both carry the formatted text.
    pub(super) fn write_tab(&mut self, index: usize, path: PathBuf) -> bool {
        if format_on_save_applies(self.settings.editor.format_on_save, &path)
            && let Some(formatted) =
                crate::format::format_source(self.session.tabs[index].editor.shared_text().as_str())
            && formatted != *self.session.tabs[index].editor.shared_text()
        {
            let caret = (index == self.session.active_tab)
                .then(|| clamp_caret(&formatted, self.compile.cursor_byte));
            // Apply in place so the format is an undoable edit; the tab is
            // marked clean once the write below succeeds.
            self.session.tabs[index].editor.replace_all_text(&formatted);
            if let Some(caret) = caret {
                self.session.tabs[index].editor.pending_jump = Some(caret);
                // The parse request reads the active tab with
                // `self.compile.cursor_byte`, so move it to the clamped
                // caret first (a non-active tab is parsed when activated).
                self.compile.set_cursor_byte(caret);
                self.trigger_parse();
            }
        }
        self.write_tab_now(index, path)
    }

    /// Writes an already-final buffer without running the formatter. Async
    /// formatted saves call this only after their formatter reply arrived.
    pub(super) fn write_tab_now(&mut self, index: usize, path: PathBuf) -> bool {
        let name = tree_entry_name(&path);
        let saved_rel = self
            .tree
            .root
            .as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .map(|rel| rel.to_string_lossy().replace('\\', "/"));
        match self.session.write_tab_to_path(index, path) {
            WriteOutcome::Saved => {
                // A client asks the server to write the authoritative copy too;
                // this one path covers Ctrl+S, save-as, save-all, format-on-save
                // and the close prompt (m1).
                if self.collab.is_client() {
                    // Flush the format-on-save edit first so the Save's
                    // `g_expected` includes it (NEW-3).
                    self.flush_tab_collab_edits(index);
                    if let Some(rel) = &saved_rel {
                        self.collab.save_file(rel);
                    }
                }
                self.toast_success(t_with("toast.saved", &[("name", name.as_str())]));
                self.git
                    .refresh(self.settings.git.git_enabled, self.tree.root.clone());
                true
            }
            WriteOutcome::Failed { path, error } => {
                log::error!("cannot save {}: {error}", path.display());
                self.toast_error(t_with("toast.save_failed", &[("err", &error.to_string())]));
                false
            }
        }
    }

    /// Save-as dialog for tab `index`; `None` when the user cancels.
    pub(super) fn prompt_save_path(&self, index: usize) -> Option<PathBuf> {
        let default_name = self.session.tabs[index]
            .editor
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.typ".to_owned());
        self.with_default_dialog_dir(
            rfd::FileDialog::new()
                .add_filter("Typst", &["typ"])
                .set_file_name(default_name),
        )
        .save_file()
    }

    /// Writes one tab to disk, prompting for a path when it has none yet.
    pub(super) fn save_tab(&mut self, index: usize) -> bool {
        if index >= self.session.tabs.len() {
            return false;
        }
        let path = match self.session.tabs[index].editor.path.clone() {
            Some(path) => path,
            None => match self.prompt_save_path(index) {
                Some(path) => path,
                None => return false,
            },
        };
        self.write_tab(index, path)
    }

    /// Saves every dirty tab (application close); reports whether all succeeded.
    pub(super) fn save_all_dirty(&mut self) -> bool {
        let mut ok = true;
        let mut failed = 0usize;
        for index in 0..self.session.tabs.len() {
            if self.session.tabs[index].dirty && !self.save_tab(index) {
                ok = false;
                failed += 1;
            }
        }
        if failed > 1 {
            self.toast_error(t_with(
                "toast.save_failed_many",
                &[("count", &failed.to_string())],
            ));
        }
        ok
    }

    /// Compact tab strip above the editor; hidden for the lone empty scratch tab.
    ///
    /// The strip is one tui leaf: the shared tab-bar widget keeps its internal
    /// layout (overflow scrollbar, drag reorder, context menu). This adapter
    /// projects the app's tabs into widget [`TabItem`]s and applies the single
    /// returned [`TabAction`] in the same frame. Applying a `Reorder` without
    /// delay is required by the widget's drag contract.
    pub(super) fn tab_bar(&mut self, tree: &mut Tui) {
        if self.session.tabs.len() == 1 && self.session.tabs[0].editor.path.is_none() {
            return;
        }
        let id = egui::Id::new("typ_edit_tab_bar");
        tree.id(id).ui(|ui| {
            let paths: Vec<Option<PathBuf>> = self
                .session
                .tabs
                .iter()
                .map(|tab| tab.editor.path.clone())
                .collect();
            let items: Vec<TabItem> = self
                .session
                .tabs
                .iter()
                .enumerate()
                .map(|(index, tab)| TabItem {
                    label: tab_display_name(index, &paths),
                    path: tab
                        .editor
                        .path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    dirty: tab.dirty,
                })
                .collect();

            let action = typ_edit_widgets::tab_bar::tab_bar(
                ui,
                "typ-edit-tab-bar",
                &items,
                self.session.active_tab,
                TabBarOrientation::Horizontal,
            );
            let Some(action) = action else {
                return;
            };
            match action {
                TabAction::Activate(index) => self.activate_tab(index),
                TabAction::Close(index) => self.close_tab(index),
                TabAction::CloseOthers(index) => {
                    let affected: Vec<usize> = (0..self.session.tabs.len())
                        .filter(|&other| other != index)
                        .collect();
                    self.close_clean_tabs(&affected);
                }
                TabAction::CloseAll => {
                    let affected: Vec<usize> = (0..self.session.tabs.len()).collect();
                    self.close_clean_tabs(&affected);
                }
                TabAction::CopyPath(index) => {
                    if let Some(path) = self.session.tabs[index].editor.path.as_ref() {
                        ui.ctx().copy_text(path.display().to_string());
                    }
                }
                TabAction::RevealInExplorer(index) => {
                    if let Some(path) = self.session.tabs[index].editor.path.as_ref()
                        && let Err(err) = platform_ops::reveal_in_explorer(path, false)
                    {
                        log::warn!("cannot reveal {}: {err}", path.display());
                    }
                }
                TabAction::Reorder { from, to } => {
                    move_item(&mut self.session.tabs, from, to);
                    self.session
                        .set_active(moved_index(self.session.active_tab, from, to));
                }
            }
        });
    }
}
