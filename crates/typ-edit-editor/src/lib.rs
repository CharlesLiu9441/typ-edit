//! Editor widget: syntax highlighting, error squiggles, hover diagnostics and the
//! "show in preview" context menu entry.
//!
//! Its public API — [`EditorState`], [`EditorInput`] and [`EditorOutput`] — is the
//! stable interface the app host drives each frame.

mod capture;
mod completion;
mod diagnostics;
mod find_bar;
mod highlight;
mod paint;
mod search;
mod show;
mod smart_edit;
mod virtual_text;
mod wrap;

pub mod clipboard_image;

#[cfg(test)]
mod bench;

#[cfg(test)]
mod tests;

pub use capture::{CapturedEdit, EditCapture};
pub use diagnostics::floor_char_boundary;

use capture::{CaptureHook, CaptureState};

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use typ_edit_model::{
    BytePos, ByteRange, CallParams, CompletionItem, Diag, FoldRange, HighlightKind, SpellIssue,
    TextSnapshot,
};

/// Id salt for the virtual widget's scrollbar, combined with the editor serial.
const SCROLL_ID: &str = "typ-edit-editor-scroll";
/// Hands every [`EditorState`] a unique widget id (tabs coexist in one context).
static EDITOR_SERIAL: AtomicU64 = AtomicU64::new(0);
/// Stable id of the find bar's query field.
const FIND_INPUT_ID: &str = "typ-edit-find-input";
/// Stable id of the find bar's replacement field.
const REPLACE_INPUT_ID: &str = "typ-edit-replace-input";
/// Stable id of the auto-completion popup's floating area.
const COMPLETION_POPUP_ID: &str = "typ-edit-completion";
/// Stable id of the selected candidate's detail tooltip.
const COMPLETION_TOOLTIP_ID: &str = "typ-edit-completion-tooltip";
/// Default maximum number of completion candidates kept in the popup. The live
/// limit comes from [`EditorInput::completion_max`] (clamped to `1..=1000`).
pub const COMPLETION_MAX_DEFAULT: usize = 50;
/// Fixed popup width (names left, kind tags right).
const COMPLETION_POPUP_WIDTH: f32 = 280.0;
/// Rows shown before the list starts scrolling.
const COMPLETION_MAX_ROWS: usize = 8;
/// Maximum height of the candidate list, in points.
const COMPLETION_MAX_HEIGHT: f32 = 200.0;
/// One indentation level (the editor never inserts a `\t`).
const INDENT: &str = "  ";
/// Cap for [`nearest_unmatched_opener`]'s backward scan.
const UNMATCHED_SCAN_LIMIT: usize = 20_000;
/// How long a find query must be idle before the full-buffer scan runs.
/// 120 ms keeps typing responsive while still updating quickly after a pause.
const FIND_DEBOUNCE: Duration = Duration::from_millis(120);
/// How long a find/replace result message stays visible next to the counter.
const FIND_FEEDBACK_TTL: Duration = Duration::from_secs(3);
/// Drag auto-scroll gain: fraction of the pointer's overshoot beyond the editor
/// viewport turned into scroll speed, in points per second.
const DRAG_SCROLL_GAIN: f32 = 4.0;
/// Upper bound on the drag auto-scroll speed, in points per second.
const DRAG_SCROLL_MAX_SPEED: f32 = 1500.0;

/// Editor state. Owns the buffer of the currently open file.
pub struct EditorState {
    /// Authoritative rope-backed model of the buffer text. Every in-crate edit
    /// passes through here (see [`Self::edit_bytes`]).
    buffer: typ_edit_buffer::TextBuffer,
    /// Rev-keyed **derived** cache of the buffer as a contiguous `Arc<String>`.
    ///
    /// This is the single contiguous view handed to the parse worker and read
    /// by the app; it is regenerated wholesale whenever [`Self::buffer`]'s
    /// revision changes and never mutated in place. Interior mutability is
    /// sound because editor state is driven on one UI thread.
    text_cache: std::cell::RefCell<Option<(u64, Arc<String>)>>,
    pub path: Option<PathBuf>,
    /// Shared flag marking an open undo group for the current edit run.
    ///
    /// A logical edit issues several buffer calls (a replace is a delete
    /// followed by an insert); the first mutation of a run opens a group
    /// (`new_op = true`) and the rest merge into it (`new_op = false`). The
    /// frame orchestrator keeps the group open across consecutive mutating
    /// frames and closes it on the next idle frame (and after undo/redo), so
    /// one keystroke run undoes as one group.
    group_open: std::cell::Cell<bool>,
    /// Unique per instance; salts the widget ids so coexisting tab editors do
    /// not share caret/scroll/undo state.
    serial: u64,
    /// Byte offset the caret must be moved to on the next frame (set by preview jumps).
    pub pending_jump: Option<usize>,
    /// Byte range the selection must be moved to on the next frame (set by
    /// [`Self::select_byte_range`]). Applied after `pending_jump`, so a range
    /// overrides a plain jump queued in the same frame. Cleared by
    /// [`Self::set_text`].
    pending_select: Option<Range<usize>>,
    /// Text queued by the app to splice into the buffer on the next `show`.
    pending_insert: Option<String>,
    /// Open auto-completion popup, if any.
    completion: Option<CompletionPopup>,
    /// Parse generation the popup was last refreshed for. A newer generation
    /// means fresh parse context arrived and the popup recomputes without
    /// waiting for the next keystroke.
    last_parse_generation: u64,
    /// `(text length, caret char)` at which the popup was dismissed. Keeps the
    /// refresh-on-parse from reopening it there; the next text change clears it.
    dismissed_at: Option<(usize, usize)>,
    /// Signature of the world-completion list last folded into the popup
    /// (`None` for an empty list). A change reopens or refreshes the popup when
    /// an asynchronously fetched member list arrives (or is cleared).
    last_world_completions: Option<(usize, u64)>,
    /// Menu-bar command queued for the next `show` (the context menu shares the
    /// same applier, so both entry points behave identically).
    command: Option<EditorCommand>,
    /// Find/replace bar state.
    find: FindState,
    /// Frame-persistent state for the virtualized path (anchor, cache, scrollbar).
    virtual_text: virtual_text::VirtualTextState,
    /// Local-edit capture, present only after [`Self::enable_edit_capture`].
    capture: Option<EditCapture>,
    /// Buffer revision the last `show` already reported. A mismatch at the next
    /// `show` means the buffer was edited outside the input pass (a remote
    /// apply), so [`EditorOutput::changed`] reports it once.
    reported_rev: u64,
    /// Monotonic per-editor buffer identity token. `TextBuffer::rev` restarts
    /// when `set_text` installs a fresh buffer; caches that combine this token
    /// with the rope revision still see a change after such a replacement.
    buffer_epoch: u64,
}

/// Buffer actions triggered from the menu bar (and the context menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCommand {
    Cut,
    Copy,
    Paste,
    SelectAll,
    Undo,
    Redo,
}

/// A collab-mode undo/redo request emitted instead of touching the local history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryRequest {
    /// The user asked to undo (server-global stack).
    Undo,
    /// The user asked to redo (server-global stack).
    Redo,
}

/// One remote participant's cursor in a file, in byte offsets.
///
/// `selections` are half-open `(start, end)` byte ranges; `carets` are byte
/// offsets. The host fills this only for the file the editor currently shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCaret {
    /// Display name of the participant.
    pub user: String,
    /// Stable palette index (0..8), assigned by the host.
    pub color_index: u8,
    /// The file this cursor belongs to.
    pub path: std::path::PathBuf,
    /// Byte offsets of the participant's carets.
    pub carets: Vec<usize>,
    /// Half-open byte selections.
    pub selections: Vec<(usize, usize)>,
}

/// How the editor decides whether a logical line soft-wraps.
///
/// [`WrapMode::Markup`] wraps a line only when it contains markup content.
/// Unblocked content inside `[...]` counts as markup and still wraps, even when
/// surrounded by a Typst code expression. Code spans themselves are kept as
/// unbreakable atoms: function arguments are never split, and a pure `#set …`
/// line stays a single row and scrolls horizontally. Styles are transparent to
/// wrapping. Lines longer than `virtual_text::window::LONG_LINE_CHARS` never
/// wrap under any mode; they are shaped through a bounded window instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WrapMode {
    /// Never wrap: every line is one row and scrolls horizontally.
    #[default]
    Off,
    /// Always soft-wrap to the viewport width.
    Always,
    /// Wrap only lines that are entirely markup mode.
    Markup,
}

/// State of the find/replace bar.
#[derive(Default)]
struct FindState {
    /// Whether the bar is visible.
    open: bool,
    /// Current search pattern.
    query: String,
    /// Text used by 替换 / 全部替换.
    replacement: String,
    /// Whether `query` is matched case-sensitively.
    case_sensitive: bool,
    /// Whether `query` is a regular expression rather than literal text.
    regex: bool,
    /// Whether matches must be flanked by word boundaries.
    whole_word: bool,
    /// Byte range captured at find-open time; when set, matches are restricted
    /// to this range. Clamped to the text length on every refresh.
    in_selection: Option<Range<usize>>,
    /// Last non-empty selection byte range seen while the bar was rendered.
    ///
    /// The live selection can collapse once focus moves into the bar (a query
    /// change selects the active match); this hint keeps the 在选区内 toggle
    /// usable after that. It is never cleared on blur or edits.
    scope_hint: Option<Range<usize>>,
    /// Regex compile error of the current query, if any (shown as a tooltip).
    regex_error: Option<String>,
    /// Ascending, non-overlapping byte ranges of the current matches.
    matches: Vec<Range<usize>>,
    /// Index of the active match in `matches` (`0` when there is none).
    active: usize,
    /// Focus the query field on the next frame (set by [`EditorState::open_find`]).
    focus_pending: bool,
    /// Match to select and scroll to in the frame's input pre-roll.
    pending_select: Option<Range<usize>>,
    /// Transient replace result (`已替换 N 处` / `无匹配`) and when it was set.
    feedback: Option<(String, Instant)>,
    /// A find action edited the buffer; propagate the change once.
    commit: bool,
    /// Query edits wait for this idle deadline before scanning the buffer.
    /// Large documents must not re-scan on every keystroke.
    pending_refresh: Option<Instant>,
    /// Whether the pending refresh should also re-anchor/select the active
    /// match. Query changes do; an edit-triggered refresh only updates matches
    /// so typing does not steal the editor selection.
    pending_refresh_activates: bool,
}

/// Auto-completion popup shown next to the caret.
struct CompletionPopup {
    /// Candidates in display order (already ranked and capped).
    items: Vec<CompletionItem>,
    /// Index of the highlighted candidate in `items`.
    selected: usize,
    /// Char index where the typed prefix starts (accept replaces from here).
    word_start: usize,
    /// Last scroll offset of the list, for detecting wheel scrolling.
    scroll_offset: f32,
    /// Whether the list was scrolling on the previous frame. The first frame
    /// after a scroll settles recomputes the hover selection once.
    was_scrolling: bool,
    /// Whether the list is the surrounding call's named parameters.
    params: bool,
    /// Whether the user moved the selection with the arrow keys.
    navigated: bool,
}

impl EditorState {
    pub fn new() -> Self {
        let mut buffer = typ_edit_buffer::TextBuffer::new();
        let virtual_text = virtual_text::VirtualTextState::default();
        buffer.add_hook(virtual_text.caret_hook());
        buffer.add_hook(virtual_text.delta_hook());
        buffer.add_hook(virtual_text.fold_hook());
        buffer.add_hook(virtual_text.bookmark_hook());
        Self {
            buffer,
            text_cache: std::cell::RefCell::new(None),
            path: None,
            group_open: std::cell::Cell::new(false),
            serial: EDITOR_SERIAL.fetch_add(1, Ordering::Relaxed),
            pending_jump: None,
            pending_select: None,
            pending_insert: None,
            completion: None,
            last_parse_generation: 0,
            dismissed_at: None,
            last_world_completions: None,
            command: None,
            find: FindState::default(),
            virtual_text,
            capture: None,
            reported_rev: 0,
            buffer_epoch: 0,
        }
    }

    /// Replaces the buffer when a different file is opened.
    pub fn set_file(&mut self, path: PathBuf, text: String) {
        self.path = Some(path);
        self.set_text(text);
    }

    /// Replaces the buffer text, dropping state derived from the old one, while
    /// leaving the path untouched.
    ///
    /// Rebuilds the buffer, so undo/redo history is dropped: this is the genuine
    /// fresh-load path. In-place rewrites that must stay undoable (format-on-save,
    /// the formatter, 全部替换) go through [`Self::replace_all_text`] instead.
    /// [`Self::set_file`] is the variant that also adopts a new file path.
    pub fn set_text(&mut self, text: String) {
        self.set_buffer_text(&text);
        self.pending_jump = None;
        self.pending_select = None;
        self.pending_insert = None;
        self.virtual_text.reset_scroll_settle();
        self.completion = None;
        self.command = None;
        self.dismissed_at = None;
        self.last_world_completions = None;
        // The old matches describe the old buffer; keep query/replacement/case.
        self.find.matches.clear();
        self.find.active = 0;
        self.find.pending_select = None;
        if self.find.open {
            self.refresh_find_matches();
        }
    }

    /// Requests a caret move (byte offset) on the next `show`.
    pub fn jump_to_byte(&mut self, offset: usize) {
        self.pending_jump = Some(offset);
    }

    /// Requests a selection (byte `start..end`) on the next `show`, overriding a
    /// plain [`Self::jump_to_byte`] queued in the same frame. A collapsed range
    /// (`start == end`) moves the caret instead.
    pub fn select_byte_range(&mut self, start: usize, end: usize) {
        self.pending_select = Some(start..end);
    }

    /// Monotonic identity of the current buffer (bumped by `set_text`).
    pub fn buffer_epoch(&self) -> u64 {
        self.buffer_epoch
    }

    /// Number of characters in the buffer. O(1).
    pub fn len_chars(&self) -> usize {
        self.buffer.len_chars()
    }

    /// Number of bytes in the buffer. O(1).
    pub fn len_bytes(&self) -> usize {
        self.buffer.len_bytes()
    }

    /// Whether the buffer has no characters. O(1).
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// The line text containing byte `byte`, plus the byte offset inside that
    /// line. Materializes only one line, not the document.
    pub fn line_text_at_byte(&self, byte: usize) -> (String, usize) {
        let at = self.buffer.byte_to_char(byte);
        let line = self.buffer.line_of_char(at);
        let start = self.buffer.line_start(line);
        let end = self.buffer.line_start(line + 1);
        let byte_start = self.buffer.char_to_byte(start);
        let byte_at = self.buffer.char_to_byte(at);
        (
            self.buffer.slice(start..end),
            byte_at.saturating_sub(byte_start),
        )
    }

    /// Number of lines as `str::lines()` would count them: an empty buffer is
    /// 0 and a trailing newline does not create a final empty line. O(log n).
    pub fn line_count_without_trailing_newline(&self) -> usize {
        let chars = self.buffer.len_chars();
        if chars == 0 {
            return 0;
        }
        let lines = self.buffer.line_count();
        if self.buffer.char_at(chars - 1) == Some('\n') {
            lines.saturating_sub(1)
        } else {
            lines
        }
    }

    /// Number of logical lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// 1-based line/column of a byte offset, using the buffer's incremental
    /// line index instead of scanning the contiguous text from byte 0.
    pub fn line_column(&self, byte: usize) -> (usize, usize) {
        let at = self.buffer.byte_to_char(byte);
        let line = self.buffer.line_of_char(at);
        let line_start = self.buffer.line_start(line);
        (line + 1, at.saturating_sub(line_start) + 1)
    }

    /// The buffer's text as a shared contiguous snapshot.
    ///
    /// This is the single read path for the buffer's contents: the parse
    /// worker's zero-copy input and the app's view for saves, blame and status.
    /// It is a derived value — regenerated from the authoritative rope whenever
    /// the buffer's revision changes, never mutated in place. Revs are
    /// monotonic across edits and undo/redo, and [`Self::set_buffer_text`]
    /// clears the cache because a fresh buffer restarts its revision at 0.
    pub fn shared_text(&self) -> Arc<String> {
        let rev = self.buffer.rev();
        if let Some((cached, text)) = self.text_cache.borrow().as_ref()
            && *cached == rev
        {
            return Arc::clone(text);
        }
        let text = Arc::new(self.buffer.slice(0..self.buffer.len_chars()));
        *self.text_cache.borrow_mut() = Some((rev, Arc::clone(&text)));
        text
    }

    /// An O(1) handle to the buffer's current text, for the parse and spell
    /// workers.
    ///
    /// Unlike [`Self::shared_text`], this does not materialize the buffer: it
    /// takes a rope clone ([`typ_edit_buffer::TextBuffer::snapshot`], O(1)) and
    /// wraps it in a model [`TextSnapshot`] the worker can materialize later, on
    /// its own thread. Request-time cost is a rope clone, never a full copy.
    pub fn text_snapshot(&self) -> TextSnapshot {
        TextSnapshot::new(BufferTextSource(self.buffer.snapshot()))
    }

    /// Streams the buffer to `writer` without materializing the whole text.
    /// Used by the save path so a large document is never copied on the UI
    /// thread just to reach the filesystem.
    pub fn write_to(&self, writer: &mut dyn std::io::Write) -> std::io::Result<()> {
        self.buffer.write_to(writer)
    }

    pub fn text_equals(&self, other: &str) -> bool {
        self.buffer.text_equals(other)
    }

    /// Drops every shaped line so the next frame re-shapes with the current
    /// egui fonts. Call this after `Context::set_fonts`; the line cache cannot
    /// observe font-data changes by itself.
    pub fn invalidate_font_layout(&mut self) {
        self.virtual_text.cache.invalidate_fonts();
    }

    /// Current virtual-text scroll anchor, exposed for headless tests.
    #[doc(hidden)]
    pub fn debug_scroll_anchor(&self) -> (usize, f32) {
        (
            self.virtual_text.anchor.top_line,
            self.virtual_text.anchor.offset,
        )
    }

    #[doc(hidden)]
    pub fn debug_visible_lines(&self) -> Range<usize> {
        self.virtual_text_visible_lines()
    }

    pub(crate) fn virtual_text_visible_lines(&self) -> Range<usize> {
        self.virtual_text.last_visible.clone()
    }

    /// Queues `text` to splice into the buffer at the caret on the next `show`.
    pub fn queue_insert_text(&mut self, text: String) {
        self.pending_insert = Some(text);
    }

    /// Opens the find bar and focuses its query field.
    pub fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus_pending = true;
        self.refresh_find_matches();
    }

    /// Queues a buffer command from the menu bar; `show` applies it exactly like
    /// the corresponding context-menu entry.
    pub fn queue_command(&mut self, command: EditorCommand) {
        self.command = Some(command);
    }

    /// Whether the buffer has an undoable group. Drives the Edit menu's 撤销
    /// item enablement (the editor keeps its own Ctrl+Z handling).
    pub fn can_undo(&self) -> bool {
        self.buffer.can_undo()
    }

    /// Whether the buffer has a redoable group. Drives the Edit menu's 重做
    /// item enablement (the editor keeps its own Ctrl+Y handling).
    pub fn can_redo(&self) -> bool {
        self.buffer.can_redo()
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-frame inputs.
pub struct EditorInput<'a> {
    pub highlights: &'a [(ByteRange, HighlightKind)],
    /// Diagnostics already filtered to the open file.
    pub diagnostics: &'a [Diag],
    /// Spell-check issues already filtered to the open file (byte ranges).
    /// Rendered as dotted blue underlines and offered through the context menu.
    pub spell_issues: &'a [SpellIssue],
    /// Debug inspector hover highlights (byte ranges). Transient, compile-derived
    /// — NOT from the editor parse, so it must bypass any parse-lag adjustment.
    pub debug_highlights: &'a [std::ops::Range<usize>],
    /// Fill of the debug highlights (translucent; alpha baked by the host).
    pub debug_highlight_color: egui::Color32,
    /// Code / math / raw byte ranges — markup helpers (auto-pairing) stay out.
    pub blocked_ranges: &'a [ByteRange],
    /// Byte ranges of code mode (identifiers are evaluated there).
    pub code_ranges: &'a [ByteRange],
    /// Byte ranges of math mode (`$...$`).
    pub math_ranges: &'a [ByteRange],
    /// Byte ranges of unbreakable math atoms (leaf-level): every math leaf is
    /// an atom, and only a function name plus its `(` and a number plus its unit
    /// are glued. A break may fall before any atom.
    pub math_atoms: &'a [ByteRange],
    /// Foldable syntax ranges from the parse (byte offsets; empty in plain mode).
    pub folds: &'a [FoldRange],
    /// Line-start byte offsets of list items (list-aware indentation).
    pub list_lines: &'a [BytePos],
    /// Standard-library completion candidates (large, sorted by name).
    pub completions: &'a [CompletionItem],
    /// Document-local completion candidates (`#let`, parameters, labels).
    pub definitions: &'a [CompletionItem],
    /// World-backed member candidates for the caret byte they were requested
    /// at (empty when none are available). When non-empty they replace the
    /// static + definition list, being context-accurate for the member
    /// position; the app clears them whenever the text changes.
    pub world_completions: &'a [CompletionItem],
    /// Maximum number of completion candidates kept in the popup. Clamped to
    /// `1..=1000` per frame so a bad config can never hang the popup.
    pub completion_max: usize,
    /// Generation of the parse result that produced the ranges above.
    pub parse_generation: u64,
    /// Named parameters of the call around the caret, when the parse found one.
    pub call_params: Option<&'a CallParams>,
    pub font_family: &'a str,
    pub font_size: f32,
    /// How long lines wrap; see [`WrapMode`].
    pub wrap_mode: WrapMode,
    /// Multiplier of the base font row height for the gap between consecutive
    /// logical lines (`1.0` is the default layout).
    pub line_spacing: f32,
    /// Multiplier of the base font row height for the gap between soft-wrap
    /// continuation rows of one logical line (`1.0` is the default layout).
    pub wrap_line_spacing: f32,
    /// Whether the active file is plain text rather than Typst. Plain mode
    /// forces [`WrapMode::Off`], ignores every parse-derived range (highlights,
    /// atoms, mode ranges, diagnostics) and suppresses completion.
    pub plain_text: bool,
    /// Take keyboard focus this frame (set when the first-parse gate clears so
    /// the editor reclaims the focus the placeholder interrupted).
    pub request_focus: bool,
    /// Optional dimmed suffix painted at the end of this logical line (`line` is
    /// 0-based). `None` while inline blame is off or unavailable.
    pub blame_inline: Option<(usize, String)>,
    /// Collab mode: Ctrl+Z/Y request the server-global undo/redo instead of
    /// touching the local history, and the editor reports
    /// [`EditorOutput::history_request`] without editing the buffer.
    pub collab_history: bool,
    /// Remote participants' cursors for the active file (byte offsets). Empty
    /// outside collab mode; painted as colored carets/selections with a label.
    pub remote_carets: &'a [RemoteCaret],
}

/// A spell-check action chosen from the editor context menu, for the app to
/// apply later (it owns the dictionary and the one-shot ignore set).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpellAction {
    /// Add the word to the user dictionary.
    AddToDictionary(String),
    /// Ignore this occurrence once.
    Ignore(String),
}

/// Per-frame outputs.
#[derive(Default)]
pub struct EditorOutput {
    pub changed: bool,
    /// Caret position as a byte offset.
    pub cursor_byte: BytePos,
    /// Byte offset requested through the "在预览区中显示" context menu.
    pub show_in_preview: Option<BytePos>,
    /// Byte offset of a Ctrl/Cmd+click, for jump-to-definition in the app.
    pub ctrl_click: Option<BytePos>,
    /// Byte offset just past the glyph under the pointer, for hover tooltips.
    pub hover_byte: Option<BytePos>,
    /// Byte boundary nearest the pointer while it is over the text area, for
    /// drag-and-drop insertion. Reported every frame the pointer is over the
    /// viewport, regardless of button state or focus.
    pub pointer_byte: Option<BytePos>,
    /// Bitmap read from the clipboard on Ctrl/Cmd+V; the app saves it as PNG.
    pub paste_image: Option<crate::clipboard_image::ClipboardImage>,
    /// Byte range of every caret's selection, in caret-list order. A collapsed
    /// caret contributes an empty range, so the primary is always represented.
    pub selections: Vec<ByteRange>,
    /// A spell-check action chosen from the context menu this frame, for the
    /// app to apply (dictionary update or one-shot ignore).
    pub spell_action: Option<SpellAction>,
    /// Caret byte where a rename was requested (F2 or the context menu's
    /// 重命名符号 row); the app drives the inline rename overlay from it.
    pub rename_request: Option<BytePos>,
    /// Caret byte where find-references was requested (the context menu's
    /// 查找引用 row); the app drives the references list from it.
    pub references_request: Option<BytePos>,
    /// Caret byte at which the editor wants world-backed completions fetched:
    /// typing `.` after an identifier-ish char, or an explicit Ctrl/Cmd+Space.
    /// The app answers with `IdeQuery::Complete`.
    pub completion_request: Option<BytePos>,
    /// Screen rect of the active caret on this frame, when its line was
    /// painted. The rename overlay is anchored to its bottom-left.
    pub caret_screen: Option<egui::Rect>,
    /// Collab mode: the undo/redo the user requested (routed to the server-global
    /// stacks by the host). `None` otherwise.
    pub history_request: Option<HistoryRequest>,
}

impl EditorState {
    /// Closes the popup and remembers where it was dismissed.
    ///
    /// A parse result arriving at the same buffer position would otherwise
    /// reopen the popup (the refresh runs once per parse generation); the next
    /// text change clears the guard.
    fn dismiss_completion(&mut self, caret_char: usize) {
        self.completion = None;
        self.dismissed_at = Some((self.buffer.len_bytes(), caret_char));
    }

    /// Dismisses the popup when the caret moved without an edit.
    ///
    /// `previous_caret` is the caret loaded at the end of the last frame
    /// (`None` on the first frame). Any relocation — arrows, Home/End,
    /// word-wise moves, a mouse click — leaves the popup behind; `text_changed`
    /// is excluded because typing refreshes the list instead.
    fn dismiss_if_caret_moved(
        &mut self,
        previous_caret: Option<usize>,
        caret_char: usize,
        text_changed: bool,
    ) {
        if self.completion.is_some() && !text_changed && previous_caret != Some(caret_char) {
            self.dismiss_completion(caret_char);
        }
    }

    /// Applies `with` over the **byte** range `range` to the authoritative
    /// [`Self::buffer`].
    ///
    /// Call sites keep computing byte offsets; the buffer converts them to char
    /// indices.
    ///
    /// The delete and the insert share one undo group: whichever applies first
    /// opens it (using the frame flag), the second merges in.
    fn edit_bytes(&mut self, range: Range<usize>, with: &str) {
        let new_op = !self.group_open.get();
        let char_start = self.buffer.byte_to_char(range.start);
        let char_end = self.buffer.byte_to_char(range.end);
        let mut opened = false;
        if char_start < char_end {
            self.buffer.delete(char_start..char_end, new_op);
            opened = true;
        }
        if !with.is_empty() {
            self.buffer
                .insert(char_start, with, if opened { false } else { new_op });
            opened = true;
        }
        if opened {
            self.group_open.set(true);
        }
    }

    /// Replaces a byte `range` with `with` for a context-menu spell suggestion
    /// and drops the remembered vertical caret position, since the replacement
    /// changes the line's width.
    fn replace_byte_range(&mut self, range: Range<usize>, with: &str) {
        self.edit_bytes(range, with);
        self.virtual_text.set_h_pos(None);
    }

    /// Replaces the whole buffer (smart-edit line rewrites, 全部替换 and the
    /// formatter), **keeping the undo history**: the char diff is recorded as a
    /// single undo group, so one undo restores the pre-replace text.
    ///
    /// The whole-buffer replace always opens a *fresh* group rather than merging
    /// into a still-open keystroke run, so formatting (repeatedly) yields one
    /// undo step per replace and Ctrl+Z after a format-on-save restores the
    /// pre-format text. Buffer hooks fire for every changed span, re-anchoring
    /// the caret/selection through the diff. Identical `new_text` is a no-op:
    /// no revision bump, no hook, no undo entry.
    ///
    /// Genuine fresh loads use [`Self::set_text`] / [`Self::set_file`], which
    /// rebuild the buffer and drop history.
    pub fn replace_all_text(&mut self, new_text: &str) {
        let rev_before = self.buffer.rev();
        self.buffer.replace_all(new_text, true);
        if self.buffer.rev() != rev_before {
            self.group_open.set(true);
        }
    }

    /// Enables local-edit capture and returns the handle. Idempotent: the same
    /// handle is returned on repeat calls.
    ///
    /// The internal [`CaptureHook`] is registered now and re-registered by
    /// [`Self::set_buffer_text`], so it survives buffer swaps; new callers only
    /// need to `take()` once per logic frame.
    pub fn enable_edit_capture(&mut self) -> EditCapture {
        if let Some(existing) = &self.capture {
            return existing.clone();
        }
        let state = std::rc::Rc::new(std::cell::RefCell::new(CaptureState::default()));
        self.buffer.add_hook(Box::new(CaptureHook(state.clone())));
        let handle = EditCapture(state);
        self.capture = Some(handle.clone());
        handle
    }

    /// The capture handle, if capture was enabled.
    pub fn edit_capture(&self) -> Option<&EditCapture> {
        self.capture.as_ref()
    }

    /// Sets the capture suppression flag (a no-op without capture enabled).
    fn set_capture_suppressed(&self, suppressed: bool) {
        if let Some(capture) = &self.capture {
            capture.0.borrow_mut().set_suppressed(suppressed);
        }
    }

    /// Applies a batch of remote edits atomically.
    ///
    /// Every edit is validated against a shadow copy **before** anything is
    /// applied (bounds, char boundaries and, for a delete, an exact match of the
    /// removed span against [`CapturedEdit::insert`]); any failure returns
    /// `false` with the buffer unchanged. On success each edit goes through
    /// [`Self::edit_bytes`] with capture suppressed, so all normal hooks (caret
    /// sync, folds, bookmarks, parse deltas) fire but nothing is re-captured; the
    /// whole batch forms one undo group.
    pub fn apply_remote_edits(&mut self, edits: &[CapturedEdit]) -> bool {
        if edits.is_empty() {
            return true;
        }
        let mut shadow = (*self.shared_text()).clone();
        for edit in edits {
            let at = edit.at_byte;
            let end = at.saturating_add(edit.delete_bytes);
            if at > shadow.len() || end > shadow.len() {
                return false;
            }
            if !shadow.is_char_boundary(at) || !shadow.is_char_boundary(end) {
                return false;
            }
            if edit.delete_bytes == 0 {
                shadow.insert_str(at, &edit.insert);
            } else {
                if shadow[at..end] != edit.insert {
                    return false;
                }
                shadow.replace_range(at..end, "");
            }
        }
        self.set_capture_suppressed(true);
        self.group_open.set(false);
        for edit in edits {
            if edit.delete_bytes == 0 {
                self.edit_bytes(edit.at_byte..edit.at_byte, &edit.insert);
            } else {
                self.edit_bytes(edit.at_byte..edit.at_byte + edit.delete_bytes, "");
            }
        }
        self.set_capture_suppressed(false);
        true
    }

    /// Replaces the whole buffer for a resync/snapshot without capturing the
    /// change. Normal hooks fire so carets/folds re-anchor.
    pub fn replace_all_text_remote(&mut self, text: &str) {
        self.set_capture_suppressed(true);
        self.replace_all_text(text);
        self.set_capture_suppressed(false);
    }

    /// Applies grouped rename edits to the buffer as a single undo step.
    ///
    /// The edits are normalized (clamped to the text, snapped to char
    /// boundaries), sorted descending by start and applied back-to-front, so
    /// earlier offsets stay valid while later ones are rewritten. Empty,
    /// invalid and overlapping edits are skipped. The first applied edit opens
    /// the undo group and the rest merge into it, so the whole batch undoes at
    /// once. Returns whether any edit changed the buffer.
    pub fn apply_byte_edits(&mut self, edits: &[typ_edit_model::TextEdit]) -> bool {
        let text = self.shared_text();
        let mut ordered: Vec<(Range<usize>, &str)> = edits
            .iter()
            .filter_map(|edit| normalize_edit(text.as_str(), edit))
            .collect();
        if ordered.is_empty() {
            return false;
        }
        ordered.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        let mut changed = false;
        let mut min_start: Option<usize> = None;
        for (range, replacement) in ordered {
            if min_start.is_some_and(|start| range.end > start) {
                continue;
            }
            let start = range.start;
            self.edit_bytes(range, replacement);
            min_start = Some(start);
            changed = true;
        }
        if changed {
            self.virtual_text.set_h_pos(None);
        }
        changed
    }

    /// Primes the buffer with `text` without resetting any other state; shared
    /// by [`Self::set_text`] and crate tests.
    ///
    /// Rebuilds the `TextBuffer` — dropping undo history — and re-adds the
    /// reactive hooks. The derived text cache must be cleared because the fresh
    /// buffer restarts its revision at 0, which could otherwise alias a cached
    /// entry from the previous buffer.
    fn set_buffer_text(&mut self, text: &str) {
        self.buffer = typ_edit_buffer::TextBuffer::from_text(text);
        self.buffer_epoch = self.buffer_epoch.wrapping_add(1);
        self.virtual_text.reset_parse_deltas();
        self.virtual_text.reset_folds();
        self.virtual_text.reset_bookmarks();
        self.virtual_text.reset_scroll_settle();
        let caret_hook = self.virtual_text.caret_hook();
        self.buffer.add_hook(caret_hook);
        let delta_hook = self.virtual_text.delta_hook();
        self.buffer.add_hook(delta_hook);
        let fold_hook = self.virtual_text.fold_hook();
        self.buffer.add_hook(fold_hook);
        let bookmark_hook = self.virtual_text.bookmark_hook();
        self.buffer.add_hook(bookmark_hook);
        // A capture-enabled editor keeps recording across a buffer swap.
        if let Some(capture) = &self.capture {
            self.buffer
                .add_hook(Box::new(CaptureHook(capture.0.clone())));
        }
        self.group_open.set(false);
        self.text_cache.borrow_mut().take();
        self.reported_rev = self.buffer.rev();
    }

    /// Byte range of a char selection, resolved through the rope (O(log n)).
    ///
    /// Callers keep the selection as char indices and only pay for this
    /// conversion in the paths that consume the bytes.
    fn selection_bytes(&self, chars: &Range<usize>) -> Range<usize> {
        self.buffer.char_to_byte(chars.start)..self.buffer.char_to_byte(chars.end)
    }

    /// Byte range of every caret's selection, in caret-list order. A collapsed
    /// caret yields an empty range at itself. Never empty: a missing caret list
    /// is reported as one empty range so the primary is always present.
    fn selection_byte_ranges(&self) -> Vec<Range<usize>> {
        let carets = self.virtual_text.carets();
        if carets.is_empty() {
            return std::iter::once(0..0).collect::<Vec<_>>();
        }
        carets
            .list()
            .iter()
            .map(|sel| {
                let chars = sel.selection().unwrap_or(sel.caret..sel.caret);
                self.selection_bytes(&chars)
            })
            .collect()
    }

    /// Number of carets in the editor (one per cursor).
    pub fn cursor_count(&self) -> usize {
        self.virtual_text.carets().len()
    }
}

/// Clamps `edit` to `text`, snapping to char boundaries, or `None` when the
/// range is inverted, starts past the end, or replaces nothing with nothing.
fn normalize_edit<'a>(
    text: &str,
    edit: &'a typ_edit_model::TextEdit,
) -> Option<(Range<usize>, &'a str)> {
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

/// Adapts the buffer's rope snapshot to the model's dependency-free
/// [`TextSnapshot`] contract; both are O(1) handles, so wrapping adds no copy.
struct BufferTextSource(typ_edit_buffer::TextSnapshot);

impl typ_edit_model::TextSource for BufferTextSource {
    fn rev(&self) -> u64 {
        self.0.rev()
    }

    fn materialize(&self) -> String {
        self.0.materialize()
    }
}
