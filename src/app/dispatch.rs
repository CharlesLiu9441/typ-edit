//! App-side command dispatch and keybinding rebuild.
//!
//! The command registry and keybinding data live at the crate root
//! (`crate::commands`, `crate::keybindings`) so shell widgets can read them
//! without depending on the app. The glue that calls app-private handlers and
//! fields stays here, inside `crate::app`.

use super::TypEditApp;
use super::collab::{CollabController, SessionKind};
use super::editor_pane::blank_scratch;
use super::git_ui::pick_hash;
use super::ide::{edit_counts, rewrite_file};
use super::search::LeftView;
use super::state::SessionState;
use super::tabs::find_open_tab;
use crate::commands::CommandId;
use crate::export::{self, ExportFormat};
use crate::keybindings::KeyBindings;
use crate::shell::layout::{ColumnLayout, Pane};
use crate::shell::tree::{TreeAction, TreeIconStyle};
use std::path::PathBuf;
use typ_edit_editor::EditorCommand;
use typ_edit_i18n::{t, t_with};
use typ_edit_model::FileEdits;
use typ_edit_preview::PreviewAction;
use typ_edit_widgets::tab_bar::tab_switch;
use typ_edit_widgets::tui::{self, Tui, TuiBuilderLogic};

/// Read-only view of the app state the command-enablement predicates need.
///
/// Kept narrow (session + layout) on purpose: a shell method that only holds
/// `&mut UiState` cannot borrow the whole [`TypEditApp`], so the registry's
/// `enabled` predicates take this view instead.
pub(crate) struct CommandState<'a> {
    pub(in crate::app) session: &'a SessionState,
    pub(in crate::app) layout: &'a ColumnLayout,
    pub(in crate::app) collab: &'a CollabController,
    pub(in crate::app) has_root: bool,
}

/// Whether 开启局域网协作 is available: a workspace is open and no session is
/// already running.
pub(crate) fn collab_can_share(state: &CommandState<'_>) -> bool {
    collab_share_enabled(state.collab.kind(), state.has_root)
}

/// Whether 加入局域网协作 is available: no session is already running.
pub(crate) fn collab_can_join(state: &CommandState<'_>) -> bool {
    collab_join_enabled(state.collab.kind())
}

/// Whether 已连接用户 is available: hosting a server.
pub(crate) fn collab_can_users(state: &CommandState<'_>) -> bool {
    collab_server_enabled(state.collab.kind())
}

/// Whether 关闭服务端 is available: hosting a server.
pub(crate) fn collab_can_stop(state: &CommandState<'_>) -> bool {
    collab_server_enabled(state.collab.kind())
}

/// Whether 断开连接 is available: joined to a server.
pub(crate) fn collab_can_disconnect(state: &CommandState<'_>) -> bool {
    collab_client_enabled(state.collab.kind())
}

/// Pure predicate behind [`collab_can_share`]: a workspace must be open and no
/// session active.
pub(in crate::app) fn collab_share_enabled(kind: SessionKind, has_root: bool) -> bool {
    has_root && kind == SessionKind::Off
}

/// Pure predicate behind [`collab_can_join`].
pub(in crate::app) fn collab_join_enabled(kind: SessionKind) -> bool {
    kind == SessionKind::Off
}

/// Pure predicate behind [`collab_can_users`] / [`collab_can_stop`].
pub(in crate::app) fn collab_server_enabled(kind: SessionKind) -> bool {
    kind == SessionKind::Server
}

/// Pure predicate behind [`collab_can_disconnect`].
pub(in crate::app) fn collab_client_enabled(kind: SessionKind) -> bool {
    kind == SessionKind::Client
}

/// Whether the editor can act on a document-editing command: not the pristine
/// scratch buffer, and the editor pane visible.
pub(crate) fn editor_editable(state: &CommandState<'_>) -> bool {
    !blank_scratch(state.session) && !state.layout.collapsed[Pane::Editor as usize]
}

/// Whether 撤销 is available: in collab mode the server-global stack decides;
/// otherwise the editor pane is visible and the buffer has an undoable group.
pub(crate) fn editor_can_undo(state: &CommandState<'_>) -> bool {
    if state.collab.is_active() {
        return state.collab.history_state().0;
    }
    !state.layout.collapsed[Pane::Editor as usize]
        && state.session.tabs[state.session.active_tab]
            .editor
            .can_undo()
}

/// Whether 重做 is available: in collab mode the server-global stack decides;
/// otherwise the editor pane is visible and the buffer has a redoable group.
pub(crate) fn editor_can_redo(state: &CommandState<'_>) -> bool {
    if state.collab.is_active() {
        return state.collab.history_state().1;
    }
    !state.layout.collapsed[Pane::Editor as usize]
        && state.session.tabs[state.session.active_tab]
            .editor
            .can_redo()
}

impl TypEditApp {
    /// A narrow command-enablement view over this app's session and layout.
    pub(super) fn command_state(&self) -> CommandState<'_> {
        CommandState {
            session: &self.session,
            layout: &self.layout,
            collab: &self.collab,
            has_root: self.tree.root.is_some(),
        }
    }

    /// Runs one command, delegating to the same handlers the menus used before
    /// the registry existed.
    pub(super) fn run_command(&mut self, id: CommandId, ctx: &egui::Context) {
        match id {
            CommandId::OpenFolder => self.pick_and_open_folder(),
            CommandId::OpenFile => self.pick_and_open_file(),
            CommandId::NewProject => self.new_project.open_fresh(),
            CommandId::Save => self.save_file(),
            CommandId::SaveAs => self.save_as(),
            CommandId::Recompile => self.trigger_compile(),
            CommandId::Exit => {
                // The close must hit the app's root viewport even when the
                // shortcut was pressed in a popped pane's window.
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close)
            }
            CommandId::Cut => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().queue_command(EditorCommand::Cut);
                }
            }
            CommandId::Copy => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().queue_command(EditorCommand::Copy);
                }
            }
            CommandId::Paste => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().queue_command(EditorCommand::Paste);
                }
            }
            CommandId::SelectAll => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().queue_command(EditorCommand::SelectAll);
                }
            }
            CommandId::Undo => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().queue_command(EditorCommand::Undo);
                }
            }
            CommandId::Redo => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().queue_command(EditorCommand::Redo);
                }
            }
            CommandId::Find => self.open_find_or_recover(),
            CommandId::FormatDocument => {
                if editor_editable(&self.command_state()) {
                    self.format_document();
                }
            }
            CommandId::CloseTab => {
                let index = self.session.active_tab;
                self.close_tab(index);
            }
            CommandId::PreviousTab => self.cycle_tab(true),
            CommandId::NextTab => self.cycle_tab(false),
            CommandId::ToggleTree => self.toggle_pane(0),
            CommandId::ToggleEditor => self.toggle_pane(1),
            CommandId::TogglePreview => self.toggle_pane(2),
            CommandId::FoldAtCaret => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().fold_at_caret();
                }
            }
            CommandId::UnfoldAtCaret => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().unfold_at_caret();
                }
            }
            CommandId::FoldAll => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().fold_all();
                }
            }
            CommandId::UnfoldAll => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().unfold_all();
                }
            }
            CommandId::ToggleBookmark => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().toggle_bookmark();
                }
            }
            CommandId::NextBookmark => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().next_bookmark();
                }
            }
            CommandId::PreviousBookmark => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().prev_bookmark();
                }
            }
            CommandId::ClearBookmarks => {
                if editor_editable(&self.command_state()) {
                    self.editor_mut().clear_bookmarks();
                }
            }
            CommandId::ResetView => self.preview.reset_view(),
            CommandId::ZoomIn => self.preview.zoom_step(1.25),
            CommandId::ZoomOut => self.preview.zoom_step(1.0 / 1.25),
            CommandId::OpenSettings => self.ui.settings_open = true,
            CommandId::ShowShortcuts => self.ui.shortcuts_open = true,
            CommandId::ShowCommandPalette => self.ui.toggle_command_palette(),
            CommandId::OpenTemplateGallery => self.gallery.open_window(ctx),
            CommandId::NextSearchResult => {
                if let Some((path, byte, byte_len)) = self.search.step(1) {
                    self.open_search_match(path, byte, byte_len);
                }
            }
            CommandId::PreviousSearchResult => {
                if let Some((path, byte, byte_len)) = self.search.step(-1) {
                    self.open_search_match(path, byte, byte_len);
                }
            }
            CommandId::FindReferences => {
                let plain = self.active_plain_text();
                let path = self.editor().path.clone();
                let root = self.tree.root.clone();
                let cursor = self.compile.cursor_byte;
                let editor = &self.session.tabs[self.session.active_tab].editor;
                if let Some(msg) = self.ide.request_references(
                    &self.engine,
                    &self.compile,
                    path,
                    root,
                    cursor,
                    editor,
                    plain,
                ) {
                    self.raise_toast(msg);
                }
            }
            CommandId::GitRefresh => self.run_git_refresh(),
            CommandId::GitFileHistory => self.run_git_file_history(),
            CommandId::GitCopyHash => self.run_git_copy_hash(ctx),
            CommandId::CollabShare => self.collab.open_share_dialog(&self.settings),
            CommandId::CollabJoin => self.join_from_menu(),
            CommandId::CollabUsers => self.collab.open_users_dialog(),
            CommandId::CollabStopServer => self.collab.request_stop(),
            CommandId::CollabDisconnect => self.collab.request_disconnect(),
        }
    }

    /// Opens the join panel from the menu, refusing while unsaved buffers exist.
    fn join_from_menu(&mut self) {
        if self.session.tabs.iter().any(|tab| tab.dirty) {
            self.toast_warning(t("collab.toast.saved_dirty").into_owned());
            return;
        }
        self.collab.open_join_dialog(&self.settings);
    }

    /// Routes one preview action to the app subsystems it commands.
    pub(super) fn handle_preview_action(&mut self, action: PreviewAction) {
        match action {
            PreviewAction::None => {}
            PreviewAction::JumpToSource { path, byte_offset } => {
                if self.editor().path.as_deref() != Some(path.as_path()) {
                    self.open_file(path);
                }
                self.editor_mut().jump_to_byte(byte_offset);
                self.compile.editor_gate.request_focus();
            }
            // The widget reports a 0-based page + page-local point; resolution may
            // yield nothing (external URL / page position), in which case only this
            // frame's raster scheduling was suppressed and the next frame re-issues.
            PreviewAction::JumpAt { page, point_pt } => {
                if let Some((path, byte_offset)) = self.resolve_jump_at(page, point_pt) {
                    if self.editor().path.as_deref() != Some(path.as_path()) {
                        self.open_file(path);
                    }
                    self.editor_mut().jump_to_byte(byte_offset);
                    self.compile.editor_gate.request_focus();
                }
            }
            PreviewAction::RequestRaster(request) => {
                self.engine.request_raster(request);
            }
            PreviewAction::ToggleAutoScroll => {
                let on = !self.settings.preview.auto_scroll_preview;
                self.settings.preview.auto_scroll_preview = on;
                self.mark_settings_dirty();
                self.preview.clear_follow_pending();
                if on {
                    // Immediate feedback with the current cursor.
                    let path = self.editor().path.clone();
                    let byte = self.compile.cursor_byte;
                    self.preview
                        .follow_cursor_in_preview(&self.compile, path.as_deref(), byte);
                }
            }
        }
    }

    /// Maps a widget `JumpAt` (0-based page + page-local pt) to a source position.
    fn resolve_jump_at(&self, page: usize, point_pt: (f32, f32)) -> Option<(PathBuf, usize)> {
        let doc = self.compile.doc.as_ref()?;
        let point = typst::layout::Point::new(
            typst::layout::Abs::pt(point_pt.0 as f64),
            typst::layout::Abs::pt(point_pt.1 as f64),
        );
        doc.result.jump_at(page, point)
    }

    /// Opens the definition under a Ctrl/Cmd+click (switch to an open tab or a
    /// new one) and puts the caret on the target offset.
    pub(super) fn jump_to_definition(&mut self, byte: usize) {
        let Some(doc) = self.compile.doc.as_ref() else {
            return;
        };
        let Some(path) = self.editor().path.clone() else {
            return;
        };
        let Some((path, offset)) = doc.result.definition_at(&path, byte) else {
            log::debug!("ctrl+click: no definition at byte {byte}");
            return;
        };
        self.open_file(path);
        self.editor_mut().jump_to_byte(offset);
    }

    pub(super) fn start_export(&mut self, format: ExportFormat, ctx: egui::Context) {
        let Some(doc) = self.compile.doc.as_ref() else {
            self.toast_warning(t("toast.no_export_doc"));
            return;
        };
        let document = doc.document.clone();
        if !self.export.begin_export() {
            self.toast_warning(t("toast.export_busy"));
            return;
        }
        let stem = self
            .preview_target()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "document".to_owned());
        export::spawn_export(
            document,
            stem,
            format,
            self.settings.export.png_dpi,
            self.tree.root.clone(),
            ctx,
            self.export.tx.clone(),
        );
        self.toast(t_with("toast.exporting", &[("format", format.label())]));
    }

    /// Rebuilds the runtime bindings after the settings changed.
    pub(super) fn rebuild_bindings(&mut self) {
        self.bindings = KeyBindings::from_settings(&self.settings);
        for (combo, ids) in self.bindings.conflicts() {
            log::warn!("shortcut conflict: {} is bound to {ids:?}", combo.label());
        }
    }

    /// `git.refresh`: re-read HEAD, branch and working-tree status.
    pub(super) fn run_git_refresh(&mut self) {
        if !self.settings.git.git_enabled {
            self.toast_warning(t("git.disabled_hint"));
            return;
        }
        self.git
            .refresh(self.settings.git.git_enabled, self.tree.root.clone());
    }

    /// `git.file_history`: switch the left pane to 历史 and request the log.
    pub(super) fn run_git_file_history(&mut self) {
        self.left_view = LeftView::History;
        if self.settings.git.git_enabled {
            self.git.request_history_log(&self.session);
        }
    }

    /// `git.copy_hash`: copy the selected history commit, else the HEAD short id.
    pub(super) fn run_git_copy_hash(&mut self, ctx: &egui::Context) {
        if !self.settings.git.git_enabled {
            self.toast_warning(t("git.disabled_hint"));
            return;
        }
        let selected = self
            .git
            .active_repo_path(&self.session)
            .filter(|relative| self.git.log_path.as_deref() == Some(relative.as_path()))
            .and_then(|_| pick_hash(&self.git.log, self.git_ui.history_selected, ""))
            .map(str::to_owned);
        let hash = selected
            .or_else(|| pick_hash(&[], 0, &self.git.snapshot.head_short).map(str::to_owned));
        match hash {
            Some(hash) => {
                ctx.copy_text(hash.clone());
                self.toast_success(t_with("git.copy_hash.copied", &[("hash", &hash)]));
            }
            None => self.toast_warning(t("git.copy_hash.none")),
        }
    }
}

/// The left-pane switcher entries: History is present only with Git enabled.
fn left_view_entries(git_enabled: bool) -> Vec<(LeftView, std::borrow::Cow<'static, str>)> {
    let mut entries = vec![
        (LeftView::Project, t("left.project")),
        (LeftView::Search, t("left.search")),
        (LeftView::Outline, t("left.outline")),
    ];
    if git_enabled {
        entries.push((LeftView::History, t("left.history")));
    }
    entries
}

impl TypEditApp {
    /// The left pane body: the 项目/搜索/大纲 switcher over the project tree,
    /// the search view or the document outline. Returns the tree action so the
    /// frame caller is unchanged.
    ///
    /// The switcher is one leaf hosting the shared segmented widget (its
    /// internal strip is widget-internal); the views add their own tui children
    /// to the pane body below a 2pt spacer.
    pub(super) fn left_pane(&mut self, tree: &mut Tui) -> TreeAction {
        // History is only meaningful while Git integration is enabled. If it
        // was the active view when Git was switched off, fall back to Project.
        let git_enabled = self.settings.git.git_enabled;
        if self.left_view == LeftView::History && !git_enabled {
            self.left_view = LeftView::Project;
        }
        let entries = left_view_entries(git_enabled);
        let labels: Vec<&str> = entries.iter().map(|(_, label)| label.as_ref()).collect();
        let active = entries
            .iter()
            .position(|(view, _)| *view == self.left_view)
            .unwrap_or(0);
        let id = egui::Id::new("typ_edit_left_pane");
        tree.id(id.with("switcher")).ui(|ui| {
            if let Some(index) = tab_switch(ui, ("left-view",), &labels, active) {
                let view = entries[index].0;
                self.left_view = view;
                match view {
                    LeftView::History => {
                        self.git.request_history_log(&self.session);
                    }
                    LeftView::Outline => self.outline.request_focus(),
                    LeftView::Search => {
                        self.search.request_focus();
                        // A non-empty query with no fresh result (e.g. after a
                        // restart of the view) must fire without waiting for
                        // another keystroke.
                        if !self.search.query.is_empty() && self.search.result.is_none() {
                            self.search.mark_dirty();
                        }
                    }
                    LeftView::Project => {}
                }
            }
        });
        // Equivalent of the former `ui.add_space(2.0)`.
        tree.style(tui::fixed(2.0))
            .id(id.with("switcher_gap"))
            .add_empty();
        match self.left_view {
            LeftView::Search => {
                if let Some((path, byte, byte_len)) =
                    self.search.view(tree, self.tree.root.is_some())
                {
                    self.open_search_match(path, byte, byte_len);
                }
                TreeAction::None
            }
            LeftView::Outline => {
                if let Some(range) = self.outline.view(
                    tree,
                    &self.compile.parse.symbols,
                    self.compile.parse.generation,
                    self.compile.cursor_byte,
                ) {
                    self.editor_mut().select_byte_range(range.start, range.end);
                    self.compile.editor_gate.request_focus();
                }
                TreeAction::None
            }
            LeftView::History => {
                // Existing egui view (git history rows) as one flexible leaf.
                tree.style(tui::flexible()).id(id.with("history")).ui(|ui| {
                    self.git_ui.history_view(
                        ui,
                        &mut self.git,
                        &self.session,
                        self.settings.git.git_enabled,
                    );
                });
                TreeAction::None
            }
            LeftView::Project => {
                let snapshot = std::sync::Arc::clone(&self.git.snapshot);
                let repo_root = self.git.repo_root.clone();
                let git = match repo_root.as_deref() {
                    Some(root)
                        if crate::shell::tree::badges_available(
                            self.settings.git.git_enabled,
                            true,
                        ) =>
                    {
                        Some(crate::shell::tree::GitTreeStatus {
                            repo_root: root,
                            statuses: &snapshot.statuses,
                        })
                    }
                    _ => None,
                };
                self.tree.show(
                    tree,
                    TreeIconStyle {
                        folder_filled: self.settings.tree.folder_icon_filled,
                        folder_color: self
                            .settings
                            .tree
                            .folder_icon_color
                            .map(|[r, g, b, a]| egui::Color32::from_rgba_unmultiplied(r, g, b, a)),
                    },
                    git,
                )
            }
        }
    }

    /// Opens `path` (when it is not already active), selects the matched byte
    /// range and hands the keyboard to the editor.
    pub(super) fn open_search_match(&mut self, path: PathBuf, byte: usize, byte_len: usize) {
        if self.editor().path.as_deref() != Some(path.as_path()) {
            self.open_file(path);
        }
        self.editor_mut().select_byte_range(byte, byte + byte_len);
        self.compile.editor_gate.request_focus();
    }

    /// Applies grouped rename edits: open tabs are edited in memory and saved,
    /// files that are not open are rewritten on disk, then the project is
    /// recompiled and the total is toasted. Any read/write failure aborts with
    /// the `rename.failed` toast.
    pub(super) fn apply_symbol_rename(&mut self, edits: Vec<FileEdits>) {
        // D12: cross-file writes are disabled while a collaboration session runs.
        if self.collab.is_active() {
            self.ide.clear_rename();
            self.toast_warning(t("collab.toast.cross_file_disabled").into_owned());
            return;
        }
        if edits.is_empty() {
            self.ide.clear_rename();
            return;
        }
        let (count, files) = edit_counts(&edits);
        let paths: Vec<Option<PathBuf>> = self
            .session
            .tabs
            .iter()
            .map(|tab| tab.editor.path.clone())
            .collect();
        let mut active_changed = false;
        for file in &edits {
            match find_open_tab(&paths, &file.path) {
                Some(index) => {
                    let changed = self.session.tabs[index]
                        .editor
                        .apply_byte_edits(&file.edits);
                    if changed && !self.save_tab(index) {
                        self.ide.clear_rename();
                        self.toast_error(t("rename.failed"));
                        return;
                    }
                    active_changed |= changed && index == self.session.active_tab;
                }
                None => {
                    if let Err(err) = rewrite_file(&file.path, &file.edits) {
                        log::error!("cannot rewrite {}: {err}", file.path.display());
                        self.ide.clear_rename();
                        self.toast_error(t("rename.failed"));
                        return;
                    }
                }
            }
        }
        self.ide.clear_rename();
        self.trigger_compile();
        if active_changed {
            self.trigger_parse();
        }
        self.toast_success(t_with(
            "rename.done",
            &[("count", &count.to_string()), ("files", &files.to_string())],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_off_hides_the_history_view() {
        let disabled = left_view_entries(false);
        assert_eq!(disabled.len(), 3);
        assert!(disabled.iter().all(|(view, _)| *view != LeftView::History));

        let enabled = left_view_entries(true);
        assert_eq!(enabled.len(), 4);
        assert_eq!(
            enabled.last().map(|(view, _)| *view),
            Some(LeftView::History)
        );
    }
}
