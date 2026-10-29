//! Custom virtualized text-rendering path (Stage C: full editing input).
//!
//! `EditorState::show` routes here unconditionally. Stage A painted + scrolled,
//! Stage B added pointer interaction (click/drag/selection/hover); Stage C adds the editing
//! input path: keyboard navigation, clipboard, undo/redo routing, IME (CJK)
//! preedit/commit and the `pending_jump`/`pending_insert`/`command` queues, all
//! editing through the authoritative [`typ_edit_buffer::TextBuffer`].
//!
//! Overlays (diagnostic squiggles, find highlights, completion anchoring) stay
//! Stage D; diagnostics painting here remains Stage A-level.

mod bookmarks;
mod caret;
mod caret_sync;
mod chrome;
mod completion;
mod edit;
mod find;
mod folds;
mod hit;
mod ime;
mod keys;
mod layout;
mod menu;
mod momentum;
mod multi_caret;
mod nav;
mod overlay;
mod parse_delta;
mod remote;
mod render;
pub mod scroll;
mod scrollview;
mod select;
mod smart;
mod touch;
mod window;

use bookmarks::Bookmarks;
use caret_sync::SelectionHandle;
use folds::Folds;
use layout::LineCache;
use scroll::ScrollAnchor;
use select::Carets;

/// Frame-persistent state for the virtualized path.
pub(crate) struct VirtualTextState {
    /// Vertical scroll position.
    pub anchor: ScrollAnchor,
    /// Horizontal pixel offset (used only when soft wrapping is off).
    pub h_offset: f32,
    /// Largest valid [`Self::h_offset`] for the current frame:
    /// `(horizontal_extent - text viewport width).max(0.0)`. Computed once per
    /// frame from the visible lines; every `h_offset` mutation clamps to
    /// `0.0..=max_h_offset` so the derived long-line window is never empty.
    pub max_h_offset: f32,
    /// Text viewport width (points) the horizontal max and the caret scroll use;
    /// equal to the horizontal scrollbar's track width. Set by the scroll pass.
    pub h_viewport_width: f32,
    /// Input clock of the last frame that actually scrolled (wheel, pan, coast,
    /// or scrollbar drag). The first later idle frame normalizes the anchor.
    pub last_scroll_time: f64,
    /// True once this viewport consumes a wheel while egui is scrolling, and
    /// cleared when egui reports the scroll action finished. Keeps the repaint
    /// loop alive even if the pointer briefly leaves the viewport mid-scroll.
    pub wheel_scrolling: bool,
    /// Normalizes Windows precision-touchpad line events to the frame clock.
    pub wheel_resampler: typ_edit_widgets::wheel::WheelResampler,
    /// Whether the custom vertical scrollbar thumb currently owns the pointer.
    pub dragging_scrollbar: bool,
    /// Whether the custom horizontal scrollbar thumb currently owns the pointer.
    pub dragging_hscrollbar: bool,
    /// Active touch points (`id -> last screen position`) for two-finger pan.
    pub touches: Vec<(egui::TouchId, egui::Pos2)>,
    /// Centroid of the active touches on the previous frame (`None` when idle).
    pub touch_centroid: Option<egui::Pos2>,
    /// Whether a two-finger gesture has engaged; stays true until a finger lifts.
    pub touch_panning: bool,
    /// Whether >= 2 fingers are down, which suppresses pointer selection.
    pub multi_touch: bool,
    /// Active two-finger pan coast velocity (points/second); `None` when the
    /// pan has stopped. Wheel/trackpad scrolling is owned by egui's per-frame
    /// `smooth_scroll_delta` and never seeds this field.
    pub momentum: Option<egui::Vec2>,
    /// Input clock of the last two-finger coast step.
    pub momentum_time: f64,
    /// Recent `(time, centroid)` samples used to estimate the release velocity.
    pub touch_samples: Vec<(f64, egui::Vec2)>,
    /// Per-line galley cache.
    pub cache: LineCache,
    /// Logical line range painted on the last frame (tests inspect it).
    pub last_visible: std::ops::Range<usize>,
    /// Caret list and selection anchors (global char indices), shared with the
    /// buffer's [`caret_sync::CaretSyncHook`] so edits keep every caret aligned.
    /// The field name is kept from the single-caret model to minimize churn.
    pub selection: SelectionHandle,
    /// Input clock when the caret was last moved/scrolled: blink resets to
    /// solid for ~0.5s after it.
    pub blink_origin: f64,
    /// Char range of the active IME preedit text inserted into the buffer.
    /// `None` when no composition is in flight.
    pub preedit_range: Option<std::ops::Range<usize>>,
    /// Fold candidates and the collapsed set derived from the parse.
    pub(crate) folds: Folds,
    /// Bookmarked logical line starts, shifted across edits.
    pub(crate) bookmarks: Bookmarks,
    /// Byte-space deltas applied since [`Self::render_generation`].
    pub parse_deltas: parse_delta::DeltaHandle,
    /// Parse generation whose ranges the frame last rendered; a change clears
    /// [`Self::parse_deltas`].
    pub render_generation: Option<u64>,
    /// Logical line a queued jump must settle onto. While set, each frame
    /// re-asserts the scroll-into-view so a jump lands on a visible caret in one
    /// frame instead of converging as wrapped lines get measured; cleared once
    /// the line appears in the painted slice (or after a bounded retry).
    pub pending_scroll_settle: Option<usize>,
    /// Frames [`Self::pending_scroll_settle`] has been re-asserted so far.
    pub scroll_settle_frames: u8,
    /// Screen rect of the text viewport on the last frame (tests read it).
    #[cfg(test)]
    pub test_viewport: Option<egui::Rect>,
    /// `(line, screen_y, height)` of each painted line on the last frame, for
    /// pointer-driven tests.
    #[cfg(test)]
    pub test_line_origins: Vec<(usize, f32, f32)>,
    /// Horizontal scrollbar track rect on the last frame, if one was drawn.
    #[cfg(test)]
    pub test_hscrollbar: Option<egui::Rect>,
    /// Horizontal scrollbar thumb rect on the last frame, if one was drawn.
    #[cfg(test)]
    pub test_hscrollbar_thumb: Option<egui::Rect>,
    /// Whether the pointer hovered/dragged the vertical bar on the last frame.
    #[cfg(test)]
    pub test_scrollbar_hovered: bool,
    /// Whether the pointer hovered/dragged the horizontal bar on the last frame.
    #[cfg(test)]
    pub test_hscrollbar_hovered: bool,
}

impl Default for VirtualTextState {
    fn default() -> Self {
        Self {
            anchor: ScrollAnchor::new(),
            h_offset: 0.0,
            max_h_offset: f32::INFINITY,
            h_viewport_width: 0.0,
            last_scroll_time: 0.0,
            wheel_scrolling: false,
            wheel_resampler: typ_edit_widgets::wheel::WheelResampler::default(),
            dragging_scrollbar: false,
            dragging_hscrollbar: false,
            touches: Vec::new(),
            touch_centroid: None,
            touch_panning: false,
            multi_touch: false,
            momentum: None,
            momentum_time: 0.0,
            touch_samples: Vec::new(),
            cache: LineCache::new(),
            last_visible: 0..0,
            selection: caret_sync::new_handle(),
            blink_origin: 0.0,
            preedit_range: None,
            folds: Folds::default(),
            bookmarks: Bookmarks::default(),
            parse_deltas: parse_delta::new_delta_handle(),
            render_generation: None,
            pending_scroll_settle: None,
            scroll_settle_frames: 0,
            #[cfg(test)]
            test_viewport: None,
            #[cfg(test)]
            test_line_origins: Vec::new(),
            #[cfg(test)]
            test_hscrollbar: None,
            #[cfg(test)]
            test_hscrollbar_thumb: None,
            #[cfg(test)]
            test_scrollbar_hovered: false,
            #[cfg(test)]
            test_hscrollbar_hovered: false,
        }
    }
}

impl VirtualTextState {
    /// The primary caret as a global char index.
    pub(crate) fn caret(&self) -> usize {
        self.selection.borrow().caret()
    }

    /// The active primary selection range, if any.
    pub(crate) fn selection_range(&self) -> Option<std::ops::Range<usize>> {
        self.selection.borrow().selection()
    }

    /// Whether any caret carries a non-empty selection.
    pub(crate) fn any_selection(&self) -> bool {
        self.selection.borrow().any_selection()
    }

    /// Shared access to the caret list.
    pub(crate) fn carets(&self) -> std::cell::Ref<'_, Carets> {
        self.selection.borrow()
    }

    /// Mutable access to the caret list.
    pub(crate) fn carets_mut(&self) -> std::cell::RefMut<'_, Carets> {
        self.selection.borrow_mut()
    }

    /// Every caret as `(index, normalized range)`, descending by edit position.
    pub(crate) fn edit_ranges_descending(&self) -> Vec<(usize, std::ops::Range<usize>)> {
        self.carets().edit_ranges_descending()
    }

    /// Replaces every caret with a single full-document selection.
    pub(crate) fn select_all(&self, len: usize) {
        self.selection.borrow_mut().select_all(len);
    }

    /// The primary caret's remembered vertical x.
    pub(crate) fn h_pos(&self) -> Option<f32> {
        self.selection.borrow().h_pos()
    }

    /// Sets the primary caret's remembered vertical x.
    pub(crate) fn set_h_pos(&self, h_pos: Option<f32>) {
        self.selection.borrow_mut().set_h_pos(h_pos);
    }

    /// A buffer hook that keeps this state's caret aligned with every edit.
    pub(crate) fn caret_hook(&self) -> Box<dyn typ_edit_buffer::EditHook> {
        Box::new(caret_sync::CaretSyncHook::new(self.selection.clone()))
    }

    /// A buffer hook that logs every edit as a byte-space delta.
    pub(crate) fn delta_hook(&self) -> Box<dyn typ_edit_buffer::EditHook> {
        Box::new(parse_delta::ParseDeltaHook::new(self.parse_deltas.clone()))
    }

    /// Arms the scroll settle for `line`, reasserted by later frames until the
    /// line is painted.
    pub(crate) fn arm_scroll_settle(&mut self, line: usize) {
        self.pending_scroll_settle = Some(line);
        self.scroll_settle_frames = 0;
    }

    /// Clears any armed scroll settle (new buffer).
    pub(crate) fn reset_scroll_settle(&mut self) {
        self.pending_scroll_settle = None;
        self.scroll_settle_frames = 0;
    }

    /// Drops the delta log and forgets the rendered generation (new buffer).
    pub(crate) fn reset_parse_deltas(&mut self) {
        self.parse_deltas.borrow_mut().clear();
        self.render_generation = None;
        self.cache.clear_raw_spans();
    }

    /// A buffer hook that keeps the collapsed fold offsets aligned with edits.
    pub(crate) fn fold_hook(&self) -> Box<dyn typ_edit_buffer::EditHook> {
        Box::new(folds::FoldSyncHook::new(self.folds.collapsed.clone()))
    }

    /// Forgets every fold candidate and collapsed offset (new buffer).
    pub(crate) fn reset_folds(&mut self) {
        self.folds = Folds::default();
    }

    /// A buffer hook that keeps the bookmark offsets aligned with edits.
    pub(crate) fn bookmark_hook(&self) -> Box<dyn typ_edit_buffer::EditHook> {
        Box::new(bookmarks::BookmarkSyncHook::new(
            self.bookmarks.positions.clone(),
        ))
    }

    /// Forgets every bookmark (new buffer).
    pub(crate) fn reset_bookmarks(&mut self) {
        self.bookmarks = Bookmarks::default();
    }

    /// The hidden logical line ranges of the user's collapsed folds, merged and
    /// sorted. Empty when nothing is collapsed.
    pub(crate) fn combined_hidden(
        &self,
        buffer: &typ_edit_buffer::TextBuffer,
    ) -> Vec<std::ops::Range<usize>> {
        self.folds.hidden_lines(buffer)
    }

    /// Relocates the caret and any selection anchor out of a hidden line, to the
    /// end of the nearest visible line before the hidden region. A no-op when
    /// nothing is hidden or both endpoints are already visible.
    pub(crate) fn relocate_out_of_hidden(&mut self, buffer: &typ_edit_buffer::TextBuffer) {
        let hidden = self.combined_hidden(buffer);
        if hidden.is_empty() {
            return;
        }
        let target = |pos: usize| -> Option<usize> {
            let line = buffer.line_of_char(pos);
            hidden
                .iter()
                .find(|range| range.start <= line && line < range.end)
                .map(|range| hit::line_content_end(buffer, range.start.saturating_sub(1)))
        };
        {
            let mut carets = self.selection.borrow_mut();
            for selection in carets.list_mut() {
                if let Some(caret) = target(selection.caret) {
                    selection.caret = caret;
                }
                if let Some(anchor) = selection.anchor.and_then(target) {
                    selection.anchor = Some(anchor);
                }
                selection.h_pos = None;
            }
        }
    }

    /// Number of collapsed folds (tests).
    #[cfg(test)]
    pub(crate) fn fold_collapsed_count(&self) -> usize {
        self.folds.collapsed_count()
    }

    /// Hidden logical line ranges for the current folds (tests).
    #[cfg(test)]
    pub(crate) fn fold_hidden_lines(
        &self,
        buffer: &typ_edit_buffer::TextBuffer,
    ) -> Vec<std::ops::Range<usize>> {
        self.folds.hidden_lines(buffer)
    }

    /// Number of bookmarks (tests).
    #[cfg(test)]
    pub(crate) fn bookmark_count(&self) -> usize {
        self.bookmarks.len()
    }

    /// Moves the primary caret and clears its selection.
    pub(super) fn set_caret(&self, caret: usize) {
        self.selection.borrow_mut().set_caret(caret);
    }

    /// Sets an explicit anchor + caret pair on the primary.
    pub(super) fn set_selection(&self, anchor: usize, caret: usize) {
        self.selection.borrow_mut().set_selection(anchor, caret);
    }

    /// Clamps **every** caret and anchor into `[0, len]` after a buffer change.
    pub(super) fn clamp(&self, len: usize) {
        self.selection.borrow_mut().clamp(len);
    }
}
