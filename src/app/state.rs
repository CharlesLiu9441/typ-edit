//! App state types: the owned session/compile/hover/export/UI substates of
//! [`super::TypEditApp`] plus the small value types they hold.

use super::hover::HoverPopup;
use crate::shell::settings_window::package_view::PackageGroup;
use crate::theme::SemanticKind;
use crossbeam_channel::{Receiver, Sender};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use typ_edit_editor::EditorState;
use typ_edit_engine::CompileResult;
use typ_edit_engine::hover::HoverInfo;
use typ_edit_engine::registry::RegistryEntry;
use typ_edit_engine::template::TemplateInit;
use typ_edit_model::CompletionItem;

/// Open-buffer session: the tabs, which one is active, and the unsaved-changes
/// prompt that gates closing a tab or the whole application.
pub(super) struct SessionState {
    /// Open buffers. Never empty: closing the last tab resets it to a fresh one.
    pub(super) tabs: Vec<EditorTab>,
    pub(super) active_tab: usize,
    /// Pending unsaved-changes question, if any.
    pub(super) close_prompt: Option<ClosePrompt>,
    /// The user confirmed an application close; the next close request wins.
    pub(super) allow_close: bool,
    /// Next stable tab id. Ids are never reused, so a parse belonging to a
    /// closed or replaced tab keeps its own identity.
    pub(super) next_tab_id: u64,
}

impl SessionState {
    /// Allocates the next stable tab id.
    pub(super) fn alloc_tab_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    /// Makes tab `index` the active one.
    pub(super) fn set_active(&mut self, index: usize) {
        self.active_tab = index;
    }

    /// Queues an unsaved-changes prompt.
    pub(super) fn prompt_close(&mut self, prompt: ClosePrompt) {
        self.close_prompt = Some(prompt);
    }

    /// Records that the user confirmed the application close.
    pub(super) fn allow_close_now(&mut self) {
        self.allow_close = true;
    }

    /// Writes tab `index`'s buffer to `path` and adopts it as the tab's file,
    /// restoring the previous path when the write fails. The app-level
    /// orchestration around a save (format-on-save, toast, reparse, git refresh)
    /// stays on [`super::TypEditApp`].
    pub(super) fn write_tab_to_path(&mut self, index: usize, path: PathBuf) -> WriteOutcome {
        let previous_path = self.tabs[index].editor.path.clone();
        match crate::platform::fs_ops::write_atomic_streaming(&path, |file| {
            self.tabs[index].editor.write_to(file)
        }) {
            Ok(()) => {
                self.tabs[index].editor.path = Some(path);
                self.tabs[index].dirty = false;
                WriteOutcome::Saved
            }
            Err(error) => {
                // A failed save-as must not leave the tab pointing at a file
                // that was never written.
                self.tabs[index].editor.path = previous_path;
                WriteOutcome::Failed { path, error }
            }
        }
    }
}

/// Result of [`SessionState::write_tab_to_path`].
pub(super) enum WriteOutcome {
    /// The bytes were written; the tab now points at the file and is clean.
    Saved,
    /// The write failed; the tab's previous path was restored. Carries the
    /// failed path and the error for the caller's log and toast.
    Failed {
        path: PathBuf,
        error: std::io::Error,
    },
}

/// Cursor auto-follow state: the pending debounced follow and the last
/// `(cursor byte, document generation)` pair that was followed.
#[derive(Default)]
pub(super) struct FollowState {
    /// Cursor byte + start time of a follow waiting out its quiet period.
    pub(super) pending: Option<(usize, Instant)>,
    /// Last followed `(byte, document generation)`, so an unchanged cursor
    /// never scrolls the preview twice for the same document.
    pub(super) last: Option<(usize, u64)>,
}

/// Semantic-hover state: the completion candidates and the tooltip's dwell
/// anchor/cache, memoized per byte and compile generation.
pub(super) struct HoverState {
    /// Completion candidates: keywords + the standard library (built once).
    pub(super) completions: Arc<Vec<CompletionItem>>,
    /// World-backed member completions paired with the caret byte they were
    /// requested at, so the candidate list stays tied to its position. `None`
    /// until an answer arrives; cleared whenever the buffer changes.
    pub(super) world_completions: Option<(usize, Vec<CompletionItem>)>,
    /// Semantic hover for `(byte, compile generation)`, so the world analysis
    /// runs only when the pointer or the document changes.
    pub(super) hover_cache: Option<(usize, u64, Option<HoverInfo>)>,
    /// Dwell anchor of the hover tooltip: byte, pointer position, and start time.
    pub(super) hover_anchor: Option<(usize, egui::Pos2, Instant)>,
    /// Whether Escape dismissed the tooltip for the currently anchored byte.
    pub(super) hover_suppressed: bool,
    /// The open rich hover popup, or `None`. Kept across frames so the pointer
    /// can rest inside it (to scroll or select) without it flickering away.
    pub(super) hover_popup: Option<HoverPopup>,
}

/// Background export state: the worker channel ends and the PNG export dialog.
pub(super) struct ExportState {
    /// Background export results (`None` = the user cancelled a dialog).
    pub(super) rx: Receiver<Option<crate::export::ExportReport>>,
    pub(super) tx: Sender<Option<crate::export::ExportReport>>,
    /// Whether the PNG export settings dialog is open.
    pub(super) png_dialog: bool,
    /// Whether an export thread is running. Each spawn reports exactly one
    /// result, so a plain flag is enough to serialize exports.
    pub(super) exporting: bool,
}

impl ExportState {
    /// Claims the single export slot unless one is already in flight.
    pub(super) fn begin_export(&mut self) -> bool {
        if self.exporting {
            return false;
        }
        self.exporting = true;
        true
    }

    /// Releases the export slot when the worker's result is drained.
    pub(super) fn finish_export(&mut self) {
        self.exporting = false;
    }

    /// Opens the PNG export settings dialog.
    pub(super) fn open_dialog(&mut self) {
        self.png_dialog = true;
    }

    /// Dismisses the PNG export settings dialog.
    pub(super) fn close_dialog(&mut self) {
        self.png_dialog = false;
    }
}

/// What the PNG export dialog resolved to this frame, for the app to apply.
pub(in crate::app) enum ExportDialogAction {
    /// Start a PNG export with the settings chosen in the dialog.
    ExportPng,
    /// Dismiss the dialog without exporting.
    Close,
    /// The dialog is closed, or still open with no decision this frame.
    None,
}

/// A feedback message before the UI gives it a timestamp.
#[derive(Clone, Debug)]
pub(super) struct ToastMsg {
    pub(super) message: String,
    pub(super) kind: SemanticKind,
}

impl ToastMsg {
    pub(super) fn new(kind: SemanticKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
        }
    }

    pub(super) fn info(message: impl Into<String>) -> Self {
        Self::new(SemanticKind::Info, message)
    }

    pub(super) fn success(message: impl Into<String>) -> Self {
        Self::new(SemanticKind::Success, message)
    }

    pub(super) fn warning(message: impl Into<String>) -> Self {
        Self::new(SemanticKind::Warning, message)
    }

    pub(super) fn error(message: impl Into<String>) -> Self {
        Self::new(SemanticKind::Error, message)
    }
}

/// One transient feedback message plus its semantic family.
#[derive(Clone, Debug)]
pub(super) struct Toast {
    pub(super) message: String,
    pub(super) kind: SemanticKind,
    pub(super) at: Instant,
}

/// Transient UI/overlay state: the toast, the window title, the error list, and
/// the visibility of the settings/shortcuts/tree-dialog overlays.
pub(super) struct UiState {
    /// Transient status message shown as a toast.
    pub(super) toast: Option<Toast>,
    /// Last window title sent to the OS, so it is only updated on change.
    pub(super) last_title: String,
    /// Compile diagnostics rendered as a clickable list when there is no document.
    pub(super) error_rows: Vec<ErrorRow>,
    pub(super) settings_open: bool,
    /// Whether the read-only shortcuts overview window is open.
    pub(super) shortcuts_open: bool,
    /// Whether the in-app Typst quick-start window is open.
    pub(super) typst_help_open: bool,
    /// Whether the Ctrl+Shift+P command palette overlay is open.
    pub(super) palette_open: bool,
    /// Current fuzzy query typed into the palette.
    pub(super) palette_query: String,
    /// Selected result row of the palette.
    pub(super) palette_selected: usize,
    /// One-shot focus request for the palette's input field.
    pub(super) palette_focus: bool,
    /// Modal tree dialog (new file/folder prompt or delete confirmation).
    pub(super) tree_dialog: TreeDialog,
    /// Whether the one-shot oversized-window clamp has already run.
    pub(super) window_size_clamped: bool,
    /// Whether the background font scan's result has been installed into egui.
    pub(super) fonts_ready: bool,
    /// Whether the landing page is shown over an open workspace. The home
    /// button sets it, and the landing page's back-to-project button and any
    /// workspace adoption clear it.
    pub(super) landing: bool,
    /// The first pipeline that stopped permanently, shown in the status bar.
    pub(super) worker_degraded: Option<String>,
    /// Title-strip icon texture, decoded and uploaded once per app instance.
    pub(super) icon_small: Option<egui::TextureHandle>,
    /// Landing-page hero icon texture, decoded and uploaded once per app instance.
    pub(super) icon_large: Option<egui::TextureHandle>,
}

/// A save waiting for the formatter before its write is allowed to start.
pub(super) struct PendingSave {
    /// Stable tab identity: the tab may move while the formatter runs.
    pub(super) tab_id: u64,
    /// Destination chosen when the save was requested.
    pub(super) path: PathBuf,
    /// Buffer revision the formatter was asked to format.
    pub(super) source_rev: u64,
}

/// A disk copy of an open file whose bytes differ from the editor buffer.
pub(super) struct ExternalChange {
    /// Stable tab identity owning this conflict.
    pub(super) tab_id: u64,
    /// File path the disk copy was read from.
    pub(super) path: PathBuf,
    /// Latest disk contents observed by the watcher.
    pub(super) disk_text: String,
}

/// The user's resolution of a tab-level external-change prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExternalChangeChoice {
    /// Discard the disk copy and keep the in-memory buffer.
    KeepMemory,
    /// Replace the buffer with the disk copy.
    UseDisk,
}

/// One open buffer: its editor plus the unsaved-changes flag.
pub(super) struct EditorTab {
    /// Stable identity, independent of the tab's position. Reordering or
    /// closing tabs never changes it, so async work can name its tab.
    pub(super) id: u64,
    pub(super) editor: EditorState,
    pub(super) dirty: bool,
}

impl EditorTab {
    /// A fresh editor tab with the given stable `id`. This is the single place
    /// tabs are constructed, so every creation and reset site uses the widget
    /// the app ships.
    pub(super) fn new(id: u64) -> Self {
        Self {
            id,
            editor: EditorState::new(),
            dirty: false,
        }
    }
}

/// Current document: compiled pages plus the compile result (source/jump/hover
/// queries), plus the page sizes handed to the preview widget each frame.
pub(super) struct DocResources {
    pub(super) generation: u64,
    pub(super) document: Arc<typst_layout::PagedDocument>,
    pub(super) result: Arc<CompileResult>,
    pub(super) page_sizes: Vec<(f32, f32)>,
}

/// The unsaved-changes question currently on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClosePrompt {
    /// A dirty tab is being closed.
    Tab(usize),
    /// The application is closing over one or more dirty tabs.
    App,
}

/// Which button the unsaved-changes dialog resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

/// One row of the preview error list.
#[derive(Clone)]
pub(super) struct ErrorRow {
    pub(super) location: String,
    pub(super) message: String,
    pub(super) path: PathBuf,
    pub(super) offset: usize,
    pub(super) severity: typ_edit_model::Severity,
}

/// The new-project dialog: a name, a parent folder, and a template choice
/// (`template` is always `None` today; P1-6c fills it from the template gallery).
#[derive(Default)]
pub(super) struct NewProjectDialog {
    /// Whether the dialog is currently on screen.
    pub(super) open: bool,
    /// Project (and target folder) name typed by the user.
    pub(super) name: String,
    /// Parent folder the project folder is created under.
    pub(super) parent: Option<PathBuf>,
    /// Selected template id; `None` is the blank project.
    pub(super) template: Option<String>,
    /// Inline validation/creation error, shown above the buttons.
    pub(super) error: Option<String>,
    /// Whether a template is being prepared on a worker thread.
    pub(super) preparing: bool,
    /// Result of the in-flight template instantiation, if any.
    pub(super) init_rx: Option<Receiver<Result<TemplateInit, String>>>,
}

/// The registry index plus the locally installed versions it was loaded with.
#[derive(Default)]
pub(super) struct GalleryLoad {
    pub(super) entries: Vec<RegistryEntry>,
    pub(super) installed: BTreeMap<String, Vec<String>>,
}

/// A finished install or uninstall worker result.
pub(super) struct PackageOpResult {
    pub(super) name: String,
    pub(super) kind: PackageOp,
    pub(super) result: Result<(), String>,
    /// Installed versions of `name` re-scanned after the operation.
    pub(super) installed: Vec<String>,
}

/// Which package operation ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PackageOp {
    Install,
    Uninstall,
}

/// State of the template gallery / package manager window: the loaded index (a
/// background fetch), the selected tab/search, and the async install jobs.
pub(super) struct GalleryState {
    pub(super) open: bool,
    /// The loaded `@preview` index, if a load has finished.
    pub(super) index: Option<Vec<RegistryEntry>>,
    /// Package entries grouped by name, rebuilt once per load.
    pub(super) groups: Vec<PackageGroup>,
    pub(super) loading: bool,
    /// Last index/package error, shown inline.
    pub(super) error: Option<String>,
    /// Search text, shared by the template list and the package pane.
    pub(super) query: String,
    /// Selected template tag chip; `None` means 全部 (no tag filter).
    pub(super) tag: Option<String>,
    /// Whether the package list hides packages that are not installed.
    pub(super) installed_only: bool,
    /// Set by 刷新 to bypass the on-disk cache on the next load.
    pub(super) force_refresh: bool,
    /// Locally installed versions per package name.
    pub(super) installed: BTreeMap<String, Vec<String>>,
    /// Version index chosen in each package row's combo (default latest).
    pub(super) selected_versions: BTreeMap<String, usize>,
    /// Whether an install/uninstall worker is running (serializes operations).
    pub(super) op_in_flight: bool,
    /// Transient status text with the time it was set.
    pub(super) status: Option<(String, Instant)>,
    pub(super) index_rx: Receiver<Result<GalleryLoad, String>>,
    pub(super) index_tx: Sender<Result<GalleryLoad, String>>,
    pub(super) op_rx: Receiver<PackageOpResult>,
    pub(super) op_tx: Sender<PackageOpResult>,
}

impl Default for GalleryState {
    fn default() -> Self {
        // Gallery channels: bounded(1). `ensure_loaded`/`loading` and
        // `op_in_flight` serialize each worker, so at most one message is ever in
        // flight; a blocking send waits only until the next frame drains it, and
        // runs on the worker thread so the UI never blocks.
        let (index_tx, index_rx) = crossbeam_channel::bounded(1);
        let (op_tx, op_rx) = crossbeam_channel::bounded(1);
        Self {
            open: false,
            index: None,
            groups: Vec::new(),
            loading: false,
            error: None,
            query: String::new(),
            tag: None,
            installed_only: false,
            force_refresh: false,
            installed: BTreeMap::new(),
            selected_versions: BTreeMap::new(),
            op_in_flight: false,
            status: None,
            index_rx,
            index_tx,
            op_rx,
            op_tx,
        }
    }
}

/// Modal dialog opened from the project tree.
pub(super) enum TreeDialog {
    None,
    /// Name prompt for a new file or folder inside `dir`.
    NewEntry {
        dir: PathBuf,
        is_dir: bool,
        name: String,
        focus_pending: bool,
        error: Option<String>,
    },
    /// Rename prompt for `path`.
    Rename {
        path: PathBuf,
        name: String,
        focus_pending: bool,
        error: Option<String>,
    },
    /// Confirmation prompt before deleting `path`.
    ConfirmDelete {
        path: PathBuf,
        is_dir: bool,
        dirty: Vec<String>,
        error: Option<String>,
    },
}

/// Where a command-line startup path should be opened.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum StartupTarget {
    Workspace(PathBuf),
    File(PathBuf),
    Ignore,
}
