use super::format_worker::{FormatOutcome, FormatPurpose, FormatReply};
use super::state::PendingSave;
use super::tabs::clamp_caret;
use super::tabs::{active_slot_reusable, find_open_tab, tab_display_name};
use super::{EditorTab, StartupTarget, TreeDialog, TypEditApp};
use crate::shell::layout::Pane;
use crate::shell::platform_ops;
use crate::shell::tree::TreeAction;
use std::io;
use std::path::{Path, PathBuf};
use typ_edit_i18n::{t, t_with};

/// Classifies a startup path: a directory becomes the workspace, a `.typ` file
/// opens in the editor, anything else is ignored.
pub(super) fn classify_startup_path(path: &Path) -> StartupTarget {
    // `std::path::absolute` (unlike `canonicalize`) does not add the `\\?\` UNC
    // prefix on Windows, which would break comparisons against the workspace root.
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if path.is_dir() {
        StartupTarget::Workspace(path)
    } else if path.is_file()
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("typ"))
    {
        StartupTarget::File(path)
    } else {
        StartupTarget::Ignore
    }
}

/// Whether `path` names a Typst source: its extension is `typ`
/// (case-insensitive).
pub(super) fn is_typst_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("typ"))
}

/// Whether an open path parses as Typst. An unnamed scratch buffer (`None`) is
/// Typst; a named file is Typst only when [`is_typst_path`] holds.
pub(super) fn is_typst_source(path: Option<&Path>) -> bool {
    path.is_none_or(is_typst_path)
}

/// Whether format-on-save applies to a write of `path`: only when the feature
/// is enabled and the target is a Typst source.
pub(super) fn format_on_save_applies(enabled: bool, path: &Path) -> bool {
    enabled && is_typst_path(path)
}

/// Display name of a tree entry (file name; full path as a fallback).
pub(super) fn tree_entry_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Pushes `path` to the front of a most-recently-used list, removing any
/// existing equal entry (case-insensitive, so Windows paths compare naturally)
/// and truncating the list to `limit` entries.
pub(super) fn push_recent(list: &mut Vec<PathBuf>, path: &Path, limit: usize) {
    let key = path.to_string_lossy();
    list.retain(|existing| !existing.to_string_lossy().eq_ignore_ascii_case(&key));
    list.insert(0, path.to_path_buf());
    list.truncate(limit);
}

/// Removes `path` from a recent-projects list, comparing case-insensitively.
pub(super) fn remove_recent(list: &mut Vec<PathBuf>, path: &Path) {
    let key = path.to_string_lossy();
    list.retain(|existing| !existing.to_string_lossy().eq_ignore_ascii_case(&key));
}

/// Rewrites `path` from under `old` to under `new`; `None` when it is unrelated.
pub(super) fn reroot_path(path: &Path, old: &Path, new: &Path) -> Option<PathBuf> {
    if path == old {
        return Some(new.to_path_buf());
    }
    path.strip_prefix(old).ok().map(|rest| new.join(rest))
}

/// `to` expressed relative to `from_dir`, with forward slashes and `..`
/// segments. `None` when the two paths share no component (different drives).
pub(super) fn relative_snippet_path(from_dir: &Path, to: &Path) -> Option<String> {
    let from: Vec<_> = from_dir.components().collect();
    let to_components: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(&to_components)
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return None;
    }
    let mut parts: Vec<String> = vec!["..".to_owned(); from.len() - common];
    parts.extend(
        to_components[common..]
            .iter()
            .map(|component| component.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Display names of the dirty tabs whose file is `path` or lies under it.
pub(super) fn dirty_tabs_under(tabs: &[EditorTab], path: &Path) -> Vec<String> {
    let paths: Vec<Option<PathBuf>> = tabs.iter().map(|tab| tab.editor.path.clone()).collect();
    tabs.iter()
        .enumerate()
        .filter(|(_, tab)| {
            tab.dirty
                && tab
                    .editor
                    .path
                    .as_deref()
                    .is_some_and(|open| open.starts_with(path))
        })
        .map(|(index, _)| tab_display_name(index, &paths))
        .collect()
}

impl TypEditApp {
    /// Whether the active tab is a plain-text (non-Typst) file. An unnamed
    /// scratch buffer counts as Typst.
    pub(super) fn active_plain_text(&self) -> bool {
        !is_typst_source(self.editor().path.as_deref())
    }

    /// Points the workspace at `root` and adopts the main file stored in the
    /// workspace marker.
    pub(super) fn set_root(&mut self, root: PathBuf) {
        self.tree.main_file = crate::workspace::main_file(&root);
        self.tree.set_root(Some(root.clone()));
        self.ui.landing = false;
        self.external_changes.clear();
        self.pending_save = None;
        self.watch_pending.clear();
        self.watch_due = None;
        if let Err(err) = self.watcher.watch(root.clone()) {
            log::warn!("cannot watch {}: {err}", root.display());
        }
        self.record_recent(&root);
        self.git
            .refresh(self.settings.git.git_enabled, self.tree.root.clone());
    }

    pub(super) fn with_default_dialog_dir(&self, dialog: rfd::FileDialog) -> rfd::FileDialog {
        match self.tree.root.as_deref() {
            Some(root) => dialog.set_directory(root),
            None => dialog,
        }
    }

    fn claim_project_lock(&mut self, root: &Path) -> bool {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if self
            .project_lock
            .as_ref()
            .is_some_and(|lock| lock.root() == canonical.as_path())
        {
            return true;
        }
        match crate::workspace::ProjectLock::acquire(&canonical) {
            Ok(lock) => {
                self.project_lock = Some(lock);
                true
            }
            Err(crate::workspace::ProjectLockError::InUse(pid)) => {
                self.toast_warning(t_with("toast.project_in_use", &[("pid", &pid.to_string())]));
                false
            }
            Err(err) => {
                self.toast_error(t_with(
                    "toast.project_lock_failed",
                    &[("err", &err.to_string())],
                ));
                false
            }
        }
    }

    pub(super) fn adopt_root(&mut self, root: &Path) -> bool {
        if !self.claim_project_lock(root) {
            return false;
        }
        self.set_root(root.to_path_buf());
        true
    }

    pub(super) fn release_project_lock(&mut self) {
        self.project_lock = None;
    }

    /// Records `root` in the recent-projects MRU list and persists it, but only
    /// while the feature is enabled. A disabled feature leaves the stored list
    /// untouched (this never clears it).
    fn record_recent(&mut self, root: &Path) {
        if !self.settings.workspace.recent_projects_enabled {
            return;
        }
        // Collab mirrors and other app-owned temp directories are not projects.
        if crate::workspace::is_transient_root(root) {
            return;
        }
        let limit =
            crate::settings::clamp_recent_limit(self.settings.workspace.recent_projects_limit);
        push_recent(&mut self.settings.workspace.recent_projects, root, limit);
        self.mark_settings_dirty();
    }

    /// Opens a recently used project root. A path that is no longer a directory
    /// is dropped from the list (with a toast) instead of silently opening
    /// nothing.
    pub(super) fn open_recent(&mut self, path: PathBuf) {
        if !path.is_dir() {
            let missing = path.display().to_string();
            self.toast_warning(t_with("toast.recent_missing", &[("path", &missing)]));
            remove_recent(&mut self.settings.workspace.recent_projects, &path);
            self.mark_settings_dirty();
            return;
        }
        self.open_root(path);
    }

    /// Persists the current main file into the workspace marker file.
    pub(super) fn write_main_marker(&self) -> io::Result<()> {
        match self.tree.root.as_deref() {
            Some(root) => crate::workspace::set_main_file(root, self.tree.main_file.as_deref()),
            None => Ok(()),
        }
    }

    pub(super) fn open_root(&mut self, root: PathBuf) {
        if !self.adopt_root(&root) {
            return;
        }
        // Open the main file or the first .typ file in the folder.
        let first = self
            .tree
            .main_file
            .clone()
            .filter(|p| p.starts_with(&root) && p.is_file())
            .or_else(|| {
                let main_candidate = root.join("main.typ");
                if main_candidate.is_file() {
                    return Some(main_candidate);
                }
                match crate::platform::fs_ops::read_dir(&root) {
                    Ok(rd) => rd
                        .flatten()
                        .map(|e| e.path())
                        .find(|p| p.extension().is_some_and(|e| e == "typ")),
                    Err(err) => {
                        log::warn!("cannot list {}: {err}", root.display());
                        None
                    }
                }
            });
        if self.recovery.arm_project_recovery(&root, first.clone()) {
            // A restore prompt is pending; the auto-open waits for its answer.
            return;
        }
        if let Some(path) = first {
            self.open_file(path);
        }
    }

    pub(super) fn open_file(&mut self, path: PathBuf) {
        // A remote edit to a closed file is queued for a debounced mirror write.
        // Opening it must see that write, or the tab would read the old text.
        if self.collab.is_client() {
            self.flush_mirror_writes(true);
        }
        let paths: Vec<Option<PathBuf>> = self
            .session
            .tabs
            .iter()
            .map(|tab| tab.editor.path.clone())
            .collect();
        if let Some(index) = find_open_tab(&paths, &path) {
            self.activate_tab(index);
            return;
        }

        let active = self.session.active_tab;
        let slot = &self.session.tabs[active];
        let reusable = active_slot_reusable(&paths, active, slot.editor.is_empty(), slot.dirty);

        match crate::platform::fs_ops::read_to_string(&path) {
            Ok(text) => {
                let index = if reusable {
                    active
                } else {
                    let id = self.session.alloc_tab_id();
                    self.session.tabs.push(EditorTab::new(id));
                    self.session.tabs.len() - 1
                };
                self.session.tabs[index].editor.set_file(path.clone(), text);
                self.session.tabs[index].dirty = false;
                // Catch edits made before the first flush ever registered a
                // capture: enable now while a session is active (m4).
                if self.collab.is_active() {
                    let _ = self.session.tabs[index].editor.enable_edit_capture();
                }
                self.session.set_active(index);
                self.on_tab_switched();
                if is_typst_path(&path) {
                    let text_len = self.session.tabs[index].editor.len_bytes();
                    self.compile
                        .editor_gate
                        .arm(self.compile.edit_generation, text_len);
                } else {
                    // Plain-text files never parse, so no placeholder may wait
                    // on a parse that will not arrive.
                    self.compile.editor_gate.disarm();
                }
            }
            Err(err) => {
                log::error!("cannot open {}: {err}", path.display());
                let path_display = path.display().to_string();
                let error = err.to_string();
                self.toast_error(t_with(
                    "toast.open_failed",
                    &[("path", &path_display), ("err", &error)],
                ));
            }
        }
    }

    /// "打开文件…" flow shared by the 文件 menu and the blank editor state.
    pub(super) fn pick_and_open_file(&mut self) {
        let dialog =
            self.with_default_dialog_dir(rfd::FileDialog::new().add_filter("Typst", &["typ"]));
        let Some(file) = dialog.pick_file() else {
            return;
        };
        if let Some(parent) = file.parent()
            && !self.adopt_root(parent)
        {
            return;
        }
        self.open_file(file);
    }

    /// "打开文件夹…" flow shared by the 文件 menu and the blank editor state.
    pub(super) fn pick_and_open_folder(&mut self) {
        let dialog = self.with_default_dialog_dir(rfd::FileDialog::new());
        if let Some(dir) = dialog.pick_folder() {
            self.open_root(dir);
        }
    }

    /// Ctrl+F / 查找: reveal the editor pane, then focus find — or, on the empty
    /// scratch buffer where find cannot render, fall back to opening a file.
    pub(super) fn open_find_or_recover(&mut self) {
        let editor = Pane::Editor as usize;
        if self.layout.collapsed[editor] {
            self.layout.collapsed[editor] = false;
        } else if self.blank_scratch() {
            self.pick_and_open_file();
        } else {
            self.editor_mut().open_find();
        }
    }

    /// Saves the open buffer (Ctrl+S / menu).
    ///
    /// With format-on-save enabled the formatter runs first and the write is
    /// deferred until its reply lands. The status bar reports that wait as
    /// `保存中`, and a single Ctrl+S therefore saves the formatted buffer.
    pub(super) fn save_file(&mut self) {
        self.begin_save(self.session.active_tab, None);
    }

    /// Issues an immediate compile and cancels a pending debounced one, so the
    /// just-saved buffer is not compiled a second time by [`Self::pump`].
    fn compile_now(&mut self) {
        let before = self.compile.compile_generation;
        self.trigger_compile();
        if self.compile.compile_generation != before {
            self.compile.clear_pending_edit();
        }
    }

    /// 另存为: writes the active buffer to a user-chosen path (Ctrl+Shift+S / menu).
    pub(super) fn save_as(&mut self) {
        let index = self.session.active_tab;
        let Some(path) = self.prompt_save_path(index) else {
            return;
        };
        self.begin_save(index, Some(path));
    }

    /// Starts a save, formatting first when the setting applies.
    ///
    /// The format worker owns the formatter; only its reply may write the tab,
    /// which is the ordering guarantee this path exists to enforce. Saves that
    /// do not need formatting take the direct write path immediately.
    fn begin_save(&mut self, index: usize, path: Option<PathBuf>) {
        if self.pending_save.is_some() || index >= self.session.tabs.len() {
            return;
        }
        let path = match path.or_else(|| self.session.tabs[index].editor.path.clone()) {
            Some(path) => path,
            None => match self.prompt_save_path(index) {
                Some(path) => path,
                None => return,
            },
        };
        if !format_on_save_applies(self.settings.editor.format_on_save, &path) {
            if self.commit_saved_tab(index, path) {
                self.compile_now();
            }
            return;
        }
        if self.format_worker.is_busy() {
            self.toast_warning(t("toast.save_waiting_format"));
            return;
        }
        let tab = &self.session.tabs[index];
        let tab_id = tab.id;
        let source_rev = tab.editor.text_snapshot().rev();
        let snapshot = tab.editor.text_snapshot();
        let ctx = self.ui_ctx.clone();
        if self
            .format_worker
            .request(tab_id, snapshot, ctx, FormatPurpose::Save)
        {
            self.pending_save = Some(PendingSave {
                tab_id,
                path,
                source_rev,
            });
            // The reply wakes the loop; repaint now so the status bar can
            // paint 保存中 before the formatter finishes.
            self.ui_ctx.request_repaint();
        }
    }

    /// Completes the write half of a formatted save after the reply is applied.
    fn finish_saved_tab(&mut self, index: usize, path: PathBuf) -> bool {
        let saved = self.commit_saved_tab(index, path);
        if saved {
            self.compile_now();
            self.tree.open_file = self.session.tabs[index].editor.path.clone();
        }
        saved
    }

    /// Writes the already-formatted buffer and clears tab-level external-change
    /// state, since the disk now matches this tab's memory copy.
    fn commit_saved_tab(&mut self, index: usize, path: PathBuf) -> bool {
        let tab_id = self.session.tabs[index].id;
        let saved = self.write_tab_now(index, path);
        if saved {
            self.external_changes.remove(&tab_id);
        }
        saved
    }

    /// Applies the formatter's save reply, then performs the deferred write.
    fn apply_save_format_reply(&mut self, reply: FormatReply) {
        let Some(pending) = self.pending_save.as_ref() else {
            return;
        };
        if pending.tab_id != reply.tab_id {
            return;
        }
        let pending = self.pending_save.take().expect("checked above");
        let Some(index) = self
            .session
            .tabs
            .iter()
            .position(|tab| tab.id == pending.tab_id)
        else {
            return;
        };
        if pending.source_rev != reply.source_rev
            || self.session.tabs[index].editor.text_snapshot().rev() != reply.source_rev
        {
            self.toast_warning(t("toast.save_cancelled"));
            return;
        }
        let editor = &mut self.session.tabs[index].editor;
        match reply.outcome {
            FormatOutcome::Empty | FormatOutcome::Skipped => {}
            FormatOutcome::Formatted(formatted) => {
                if !editor.text_equals(&formatted) {
                    let caret = (index == self.session.active_tab)
                        .then(|| clamp_caret(&formatted, self.compile.cursor_byte));
                    editor.replace_all_text(&formatted);
                    self.session.tabs[index].dirty = true;
                    if let Some(caret) = caret {
                        self.session.tabs[index].editor.pending_jump = Some(caret);
                        self.compile.set_cursor_byte(caret);
                        self.trigger_parse();
                    }
                }
            }
        }
        self.finish_saved_tab(index, pending.path);
    }

    /// 格式化文档: pretty-prints the active buffer in place (Ctrl+Shift+F / menu).
    pub(super) fn format_document(&mut self) {
        if self.blank_scratch() {
            return;
        }
        if self.active_plain_text() {
            self.toast_warning(t("toast.format_skipped_plain"));
            return;
        }
        let index = self.session.active_tab;
        if self.format_worker.is_busy() || self.pending_save.is_some() {
            return;
        }
        let tab_id = self.session.tabs[index].id;
        let snapshot = self.session.tabs[index].editor.text_snapshot();
        self.format_worker
            .request(tab_id, snapshot, self.ui_ctx.clone(), FormatPurpose::Manual);
    }

    pub(super) fn apply_format_reply(&mut self, reply: FormatReply) {
        if reply.purpose == FormatPurpose::Save {
            self.apply_save_format_reply(reply);
            return;
        }
        let Some(index) = self
            .session
            .tabs
            .iter()
            .position(|tab| tab.id == reply.tab_id)
        else {
            return;
        };
        let editor = &mut self.session.tabs[index].editor;
        if editor.text_snapshot().rev() != reply.source_rev {
            return;
        }
        match reply.outcome {
            FormatOutcome::Empty => {}
            FormatOutcome::Skipped => self.toast_warning(t("toast.format_skipped_syntax")),
            FormatOutcome::Formatted(formatted) => {
                if !editor.text_equals(&formatted) {
                    editor.replace_all_text(&formatted);
                    self.session.tabs[index].dirty = true;
                    self.recovery.note_change();
                    self.trigger_parse();
                }
                self.toast_success(t("toast.formatted"));
            }
        }
    }

    /// Applies a successful rename of `old` to `new` to the settings, the tabs and the tree.
    pub(super) fn apply_rename(&mut self, old: &Path, new: &Path) {
        if let Some(main) = self.tree.main_file.clone()
            && let Some(updated) = reroot_path(&main, old, new)
        {
            self.tree.main_file = Some(updated);
            if let Err(err) = self.write_main_marker() {
                log::warn!("cannot rewrite workspace marker: {err}");
            }
            self.trigger_compile();
        }
        for tab in &mut self.session.tabs {
            if let Some(updated) = tab
                .editor
                .path
                .as_deref()
                .and_then(|path| reroot_path(path, old, new))
            {
                tab.editor.path = Some(updated);
            }
        }
        if let Some(open) = self.tree.open_file.clone()
            && let Some(updated) = reroot_path(&open, old, new)
        {
            self.tree.open_file = Some(updated);
        }
        if let Some(cursor) = self.tree.cursor.clone()
            && let Some(updated) = reroot_path(&cursor, old, new)
        {
            self.tree.cursor = Some(updated);
        }
        let moved: Vec<(PathBuf, PathBuf)> = self
            .tree
            .expanded
            .iter()
            .filter_map(|path| reroot_path(path, old, new).map(|new| (path.clone(), new)))
            .collect();
        for (old_path, new_path) in moved {
            self.tree.expanded.remove(&old_path);
            self.tree.expanded.insert(new_path);
        }
        self.tree.invalidate(old);
        self.tree.invalidate(new);
    }

    /// Preview target: designated main file, else the open file.
    pub(super) fn preview_target(&self) -> Option<PathBuf> {
        self.tree
            .main_file
            .clone()
            .filter(|p| p.is_file())
            .or_else(|| self.editor().path.clone())
    }

    /// Dispatches one action coming from the project tree.
    pub(super) fn handle_tree_action(&mut self, tree_action: TreeAction) {
        match tree_action {
            TreeAction::None => {}
            TreeAction::OpenFolder => {
                let dialog = self.with_default_dialog_dir(rfd::FileDialog::new());
                if let Some(dir) = dialog.pick_folder() {
                    self.open_root(dir);
                }
            }
            TreeAction::OpenFile(path) => self.open_file(path),
            TreeAction::SetMainFile(path) => {
                self.tree.main_file = Some(path);
                if let Err(err) = self.write_main_marker() {
                    log::warn!("cannot write workspace marker: {err}");
                    self.toast_error(t_with("toast.save_failed", &[("err", &err.to_string())]));
                }
                self.trigger_compile();
            }
            TreeAction::ClearMainFile => {
                self.tree.main_file = None;
                if let Err(err) = self.write_main_marker() {
                    log::warn!("cannot clear workspace marker: {err}");
                }
                self.trigger_compile();
            }
            TreeAction::NewFile(dir) => {
                self.ui.tree_dialog = TreeDialog::NewEntry {
                    dir,
                    is_dir: false,
                    name: "untitled.typ".to_owned(),
                    focus_pending: true,
                    error: None,
                };
            }
            TreeAction::NewFolder(dir) => {
                self.ui.tree_dialog = TreeDialog::NewEntry {
                    dir,
                    is_dir: true,
                    name: t("dialog.new_folder").into_owned(),
                    focus_pending: true,
                    error: None,
                };
            }
            TreeAction::Rename(path) => {
                let name = tree_entry_name(&path);
                self.ui.tree_dialog = TreeDialog::Rename {
                    path,
                    name,
                    focus_pending: true,
                    error: None,
                };
            }
            TreeAction::RefreshDir(dir) => self.tree.refresh_dir(&dir),
            TreeAction::Delete(path, is_dir) => {
                let dirty = dirty_tabs_under(&self.session.tabs, &path);
                self.ui.tree_dialog = TreeDialog::ConfirmDelete {
                    path,
                    is_dir,
                    dirty,
                    error: None,
                };
            }
            TreeAction::RevealInExplorer(path, is_dir) => {
                if let Err(err) = platform_ops::reveal_in_explorer(&path, is_dir) {
                    log::warn!("cannot reveal {}: {err}", path.display());
                }
            }
            TreeAction::OpenInTerminal(dir) => {
                if let Err(err) = platform_ops::open_terminal(&dir) {
                    log::warn!("cannot open terminal at {}: {err}", dir.display());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> PathBuf {
        PathBuf::from(value)
    }

    #[test]
    fn push_recent_moves_an_existing_entry_to_the_front() {
        let mut list = vec![path("alpha"), path("beta"), path("gamma")];
        push_recent(&mut list, &path("gamma"), 10);
        assert_eq!(list, vec![path("gamma"), path("alpha"), path("beta")]);
    }

    #[test]
    fn push_recent_dedupes_case_insensitively() {
        let mut list = vec![path(r"C:\Projects\Alpha"), path("beta")];
        push_recent(&mut list, &path(r"c:\projects\alpha"), 10);
        assert_eq!(list, vec![path(r"c:\projects\alpha"), path("beta")]);
    }

    #[test]
    fn push_recent_truncates_to_the_limit() {
        let mut list = vec![path("alpha"), path("beta"), path("gamma")];
        push_recent(&mut list, &path("delta"), 2);
        assert_eq!(list, vec![path("delta"), path("alpha")]);
    }

    #[test]
    fn remove_recent_drops_a_case_insensitive_match() {
        let mut list = vec![path("alpha"), path("Beta"), path("gamma")];
        remove_recent(&mut list, &path("beta"));
        assert_eq!(list, vec![path("alpha"), path("gamma")]);
    }
}
