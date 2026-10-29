//! Integration hub: worker message pump, layout, menus, status bar, settings.
//!
//! Pane bodies live in the specific modules and are called through their public APIs.

mod background_drop;
mod caret;
mod collab;
mod collab_ui;
mod command_palette;
mod diag;
mod dialogs;
pub(crate) mod dispatch;
mod document;
mod downloads;
mod editor_pane;
mod errors;
mod external_change;
mod files;
mod format_worker;
mod frame;
mod frame_tree;
pub(crate) mod gallery;
mod gate;
mod git;
mod git_history;
mod git_ui;
mod hover;
mod ide;
mod image_import;
mod landing;
mod menu;
mod outline;
mod preview;
mod recovery;
mod search;
mod shortcuts;
mod spell;
mod startup;
pub(crate) mod state;
mod tabs;
mod typst_help;
mod vector_preview;
mod watcher;
mod window_chrome;
mod worker;

#[cfg(target_os = "macos")]
mod macos_window;

#[cfg(test)]
mod git_tests;
#[cfg(test)]
mod harness_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod ui_probe;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::keybindings::KeyBindings;
use crate::settings::Settings;
use crate::shell::layout::ColumnLayout;
use crate::shell::tree::ProjectTree;
use document::DocumentController;
use preview::PreviewController;
use state::{
    CloseChoice, ClosePrompt, DocResources, EditorTab, ErrorRow, ExportDialogAction, ExportState,
    GalleryState, HoverState, NewProjectDialog, SessionState, StartupTarget, TreeDialog, UiState,
    WriteOutcome,
};
use typ_edit_engine::Engine;
use typ_edit_engine::fonts::Fonts;

/// The application root: owned subsystem handles plus the grouped per-session state.
pub struct TypEditApp {
    engine: Engine,
    fonts: Arc<Fonts>,
    pub settings: Settings,
    /// Whether settings changed since the last frame-end persist.
    settings_dirty: bool,
    /// When the settings last changed, driving the persist debounce.
    settings_changed_at: Option<Instant>,
    /// One-shot latch for the settings-save failure toast (clears on success).
    settings_save_failed: bool,
    /// One-shot latch for the read-only (newer schema) settings warning.
    settings_read_only_warned: bool,
    /// Runtime keybinding table (defaults + `settings.shortcuts` overrides).
    pub(in crate::app) bindings: KeyBindings,
    preview: PreviewController,
    frame_tree: frame_tree::FrameTreeState,
    tree: ProjectTree,
    project_lock: Option<crate::workspace::ProjectLock>,
    layout: ColumnLayout,
    /// Formatting worker and its in-flight slot.
    format_worker: format_worker::FormatWorker,
    /// A save waiting for the formatter's reply before writing the tab.
    pending_save: Option<state::PendingSave>,
    /// Disk copies that differ from open tabs, keyed by stable tab id. Only the
    /// active tab's entry renders a modal; the others wait for activation.
    external_changes: BTreeMap<u64, state::ExternalChange>,
    /// Paths received from the recursive watcher while the debounce is open.
    watch_pending: BTreeSet<PathBuf>,
    /// When the coalesced watcher batch is due.
    watch_due: Option<Instant>,
    /// Recursive project watcher (`notify`: ReadDirectoryChangesW / FSEvents).
    watcher: watcher::ProjectWatcher,
    /// UI context used to wake the frame loop when a worker finishes.
    ui_ctx: egui::Context,
    /// Open buffers, the active tab, and the pending close prompt.
    session: SessionState,
    /// Parse/compile pipeline, current document, and editor gates.
    compile: DocumentController,
    /// Inline rename overlay and find-references routing state.
    ide: ide::IdeState,
    /// Spell-check issues, request bookkeeping, dictionary and ignore set.
    spell: spell::SpellState,
    /// Which view the left pane shows (project tree or project search).
    left_view: search::LeftView,
    /// Project-wide search query, options, request bookkeeping and results.
    search: search::SearchState,
    /// Document-outline filter, focus request and collapsed-node set.
    outline: outline::OutlineState,
    /// Completion candidates and the semantic-hover tooltip state.
    hover: HoverState,
    /// Export worker channels and the PNG export dialog.
    export: ExportState,
    /// The new-project dialog: scaffolding of a blank project on the landing page.
    new_project: NewProjectDialog,
    /// The template gallery / package manager window.
    gallery: GalleryState,
    /// Toast, window title, error list, and overlay/dialog visibility.
    ui: UiState,
    /// Autosave snapshot of unsaved tabs for crash recovery.
    recovery: recovery::Recovery,
    /// Git worker, status snapshot and latest history/blame/diff answers.
    git: git::GitState,
    /// Git UI session state: history selection and the open diff window.
    git_ui: git_ui::GitUiState,
    /// LAN collaboration session, discovery and join state.
    collab: collab::CollabController,
    /// On-demand assets fetched from the collaboration server.
    pub(super) downloads: downloads::DownloadState,
}
