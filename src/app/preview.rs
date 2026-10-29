//! Preview cluster controller: the single owner of the preview viewport state,
//! the GPU-canvas backend, the live cursor-marker / selection-mask caches, and
//! the debounced cursor auto-follow.
//!
//! Every mutation of this state goes through a method here; callers read the
//! `pub(in crate::app)` fields directly. Methods never take `&mut TypEditApp`:
//! foreign data (the document controller, settings, the workspace root, the
//! error rows) arrives as discrete parameters, and app-level orchestration
//! (opening files, requesting rasters, starting exports) stays on `TypEditApp`.
//! Rendering returns a [`PreviewPaneOutput`] of deferred side effects for the
//! caller to apply in order.

use super::document::DocumentController;
use super::errors::{error_bar_header, error_list};
use super::state::{ErrorRow, FollowState};
use super::vector_preview::VectorPreview;
use crate::settings::Settings;
use crate::theme;
use crate::theme::SemanticKind;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use typ_edit_engine::CursorAnchor;
use typ_edit_i18n::{t, t_with};
use typ_edit_model::RasterResult;
use typ_edit_preview::{
    CursorMarker, PageSource, PreviewAction, PreviewState, PreviewStyle, SelectionMask,
};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{Dimension, LengthPercentage, Rect as TaffyRect, Size},
};

/// Stable id of the preview pane's bottom error bar; one id keeps a user-resized height
/// across frames and error episodes.
const ERROR_BAR_ID: &str = "preview-error-bar";
/// Default height (points) of the bottom error bar before the user resizes it.
const ERROR_BAR_HEIGHT: f32 = 160.0;
/// Smallest / largest error-bar height the drag handle allows.
const MIN_ERROR_BAR_HEIGHT: f32 = 80.0;
const MAX_ERROR_BAR_HEIGHT: f32 = 480.0;
/// Quiet period after the last cursor byte change before an auto-follow fires.
const FOLLOW_DEBOUNCE: Duration = Duration::from_millis(200);
/// Below this zoom the GPU canvas's sub-pixel text/MSAA quality degrades while
/// the raster fallback is both cheaper and sharper, so the GPU path is only
/// used at readable zooms.
const MIN_VECTOR_ZOOM: f32 = 0.5;

/// The stored error-bar height, clamped into the draggable range.
fn error_bar_height(ctx: &egui::Context) -> f32 {
    ctx.data(|data| data.get_temp::<f32>(egui::Id::new(ERROR_BAR_ID)))
        .unwrap_or(ERROR_BAR_HEIGHT)
        .clamp(MIN_ERROR_BAR_HEIGHT, MAX_ERROR_BAR_HEIGHT)
}

/// Side effects produced by rendering the preview pane, to be applied by the
/// caller after the render returns. `bar_action` is applied only when not
/// `PreviewAction::None`; the apply order is open-root, toast, bar action, then
/// the canvas action that the caller keeps.
pub(in crate::app) struct PreviewPaneOutput {
    /// Action from the canvas body (a jump/raster request, else `None`).
    pub(in crate::app) canvas_action: PreviewAction,
    /// Action from the bottom error bar (a click in the error list).
    pub(in crate::app) bar_action: PreviewAction,
    /// Folder picked by the empty-state "open folder" button, if any.
    pub(in crate::app) open_root: Option<PathBuf>,
    /// One-time GPU-fallback notice to surface as a toast, if any.
    pub(in crate::app) toast: Option<String>,
}

/// One off-UI cursor-anchor request. The compile result is an `Arc`, so the
/// worker does not copy the document; the carets are the exact key the UI will
/// accept back.
struct CursorAnchorRequest {
    carets: Vec<usize>,
    generation: u64,
    path: PathBuf,
    result: Arc<typ_edit_engine::CompileResult>,
}

struct CursorAnchorReply {
    carets: Vec<usize>,
    generation: u64,
    anchors: Vec<Option<CursorAnchor>>,
}

/// Starts the single cursor-anchor worker owned by a [`PreviewController`].
/// Keeping one long-lived worker avoids spawning on every keystroke; a stale
/// reply is discarded by the controller's in-flight key.
fn cursor_anchor_worker() -> (Sender<CursorAnchorRequest>, Receiver<CursorAnchorReply>) {
    let (request_tx, request_rx) = mpsc::channel::<CursorAnchorRequest>();
    let (reply_tx, reply_rx) = mpsc::channel::<CursorAnchorReply>();
    let _ = std::thread::Builder::new()
        .name("typ-edit-cursor-anchor".to_owned())
        .spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let anchors = request
                    .carets
                    .iter()
                    .map(|byte| request.result.cursor_anchor(&request.path, *byte))
                    .collect();
                if reply_tx
                    .send(CursorAnchorReply {
                        carets: request.carets,
                        generation: request.generation,
                        anchors,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
    (request_tx, reply_rx)
}

/// One off-UI selection-mask request. Like the cursor worker, the Arc'd compile
/// result avoids copying the document; the ranges and generation are the key the
/// UI accepts back.
struct SelectionMaskRequest {
    ranges: Vec<Range<usize>>,
    generation: u64,
    path: PathBuf,
    result: Arc<typ_edit_engine::CompileResult>,
}

struct SelectionMaskReply {
    ranges: Vec<Range<usize>>,
    generation: u64,
    masks: Vec<SelectionMask>,
}

/// Starts the single selection-mask worker. Frame-span walks are document-sized,
/// so they stay off the editor frame; a stale reply is dropped by the in-flight
/// key.
fn selection_masks_worker() -> (Sender<SelectionMaskRequest>, Receiver<SelectionMaskReply>) {
    let (request_tx, request_rx) = mpsc::channel::<SelectionMaskRequest>();
    let (reply_tx, reply_rx) = mpsc::channel::<SelectionMaskReply>();
    let _ = std::thread::Builder::new()
        .name("typ-edit-selection-masks".to_owned())
        .spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let masks = masks_from_ranges(&request.ranges, |range| {
                    request.result.selection_masks(&request.path, range.clone())
                });
                if reply_tx
                    .send(SelectionMaskReply {
                        ranges: request.ranges,
                        generation: request.generation,
                        masks,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
    (request_tx, reply_rx)
}

/// Preview cluster: the viewport state, the GPU backend, the marker/mask caches,
/// and the debounced cursor auto-follow.
pub(in crate::app) struct PreviewController {
    /// Preview viewport: zoom/pan, the reveal ring, and the pending raster queue.
    pub(in crate::app) state: PreviewState,
    /// GPU canvas backend (per-page BVH cache) and its fallback bookkeeping.
    pub(in crate::app) vector: VectorPreview,
    /// Debounced auto-follow trigger state.
    pub(in crate::app) follow: FollowState,
    /// Last computed live cursor markers: `(caret bytes, document generation,
    /// one anchor per caret)`. The byte list is the cache key, so an unchanged
    /// caret set in the same document never re-resolves the anchors.
    pub(in crate::app) cursor_markers_cache: Option<(Vec<usize>, u64, Vec<Option<CursorAnchor>>)>,
    /// Cache key of the last computed selection masks: `(ranges, document generation)`.
    pub(in crate::app) selection_masks_cache: Option<(Vec<Range<usize>>, u64)>,
    /// Request channel to the cursor-anchor worker (kept alive with the controller).
    cursor_anchor_tx: Sender<CursorAnchorRequest>,
    /// Replies from the cursor-anchor worker.
    cursor_anchor_rx: Receiver<CursorAnchorReply>,
    /// Key of the request currently owned by the worker; stale replies are dropped.
    cursor_anchor_in_flight: Option<(Vec<usize>, u64)>,
    /// Request channel to the selection-mask worker (kept alive with the controller).
    selection_masks_tx: Sender<SelectionMaskRequest>,
    /// Replies from the selection-mask worker.
    selection_masks_rx: Receiver<SelectionMaskReply>,
    /// Key of the selection request currently owned by the worker.
    selection_masks_in_flight: Option<(Vec<Range<usize>>, u64)>,
    /// Latest selection key the UI still wants; an older reply is discarded.
    selection_masks_wanted: Option<(Vec<Range<usize>>, u64)>,
}

impl PreviewController {
    /// Builds the controller from the resolved host resources; the debounced
    /// auto-follow and both marker caches start idle.
    pub(in crate::app) fn new(state: PreviewState, vector: VectorPreview) -> Self {
        let (cursor_anchor_tx, cursor_anchor_rx) = cursor_anchor_worker();
        let (selection_masks_tx, selection_masks_rx) = selection_masks_worker();
        Self {
            state,
            vector,
            follow: FollowState::default(),
            cursor_markers_cache: None,
            selection_masks_cache: None,
            cursor_anchor_tx,
            cursor_anchor_rx,
            cursor_anchor_in_flight: None,
            selection_masks_tx,
            selection_masks_rx,
            selection_masks_in_flight: None,
            selection_masks_wanted: None,
        }
    }

    /// Resets the preview viewport to the default zoom and offset.
    pub(in crate::app) fn reset_view(&mut self) {
        self.state.reset_view();
    }

    /// Steps the preview zoom by `factor`, clamped by the widget.
    pub(in crate::app) fn zoom_step(&mut self, factor: f32) {
        self.state.zoom_step(factor);
    }

    /// Uploads a finished raster result and schedules the next refinement pass.
    pub(in crate::app) fn apply_result(&mut self, ctx: &egui::Context, result: RasterResult) {
        self.state.apply_result(ctx, result);
    }

    /// Arms the debounced auto-follow for `byte`, observed at `now`.
    pub(in crate::app) fn arm_follow(&mut self, byte: usize, now: Instant) {
        self.follow.pending = Some((byte, now));
    }

    /// Drops a pending auto-follow without firing it.
    pub(in crate::app) fn clear_follow_pending(&mut self) {
        self.follow.pending = None;
    }

    /// Records the last followed `(byte, generation)` so it never re-fires.
    fn mark_followed(&mut self, byte: usize, generation: u64) {
        self.follow.last = Some((byte, generation));
    }

    /// Stores the memoized cursor-marker anchors for `(carets, generation)`.
    fn set_cursor_markers_cache(
        &mut self,
        carets: Vec<usize>,
        generation: u64,
        anchors: Vec<Option<CursorAnchor>>,
    ) {
        self.cursor_markers_cache = Some((carets, generation, anchors));
    }

    /// Drops the memoized cursor-marker anchors.
    fn clear_cursor_markers_cache(&mut self) {
        self.cursor_markers_cache = None;
    }

    /// Stores the memoized selection-mask key `(ranges, generation)`.
    fn set_selection_masks_cache(&mut self, ranges: Vec<Range<usize>>, generation: u64) {
        self.selection_masks_cache = Some((ranges, generation));
    }

    /// Drops the memoized selection-mask key.
    fn clear_selection_masks_cache(&mut self) {
        self.selection_masks_cache = None;
    }

    /// The preview pane body: the last good render (with a resizable bottom error bar
    /// while the latest compile has errors), or one of three honest empty states.
    /// Adds its parts as tui children of the pane's body container.
    pub(in crate::app) fn preview_pane(
        &mut self,
        tree: &mut Tui,
        doc: &DocumentController,
        settings: &Settings,
        tree_root: Option<&Path>,
        error_rows: &[ErrorRow],
    ) -> PreviewPaneOutput {
        let mut open_root = None;
        let mut toast = None;
        let mut bar_action = PreviewAction::None;
        let mut error_rect: Option<egui::Rect> = None;
        let error_count = doc.compile_error_count();

        let canvas_action = if doc.doc.is_some() {
            let id = egui::Id::new("typ_edit_preview_canvas");
            let (action, notice) = tree
                .style(tui::flexible())
                .id(id)
                .ui(|ui| self.preview_canvas_ui(ui, doc, settings));
            toast = notice;
            action
        } else {
            open_root = self.preview_empty(tree, doc, tree_root);
            PreviewAction::None
        };

        if error_count > 0 {
            let showing_previous_preview = doc.doc.is_some() && doc.preview_failure.is_some();
            let header = error_bar_header(showing_previous_preview, error_count);
            let height = error_bar_height(tree.egui_ctx());
            let bar_id = egui::Id::new(ERROR_BAR_ID);
            let mut style = tui::column();
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
            style.flex_basis = Dimension::length(height);
            style.size = Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(height),
            };
            tree.style(style).id(bar_id).add_with_background_ui(
                |ui, container| {
                    let rect = container.full_container();
                    error_rect = Some(rect);
                    // A feedback container: semantic error surface plus a
                    // semantic error edge, not a neutral card sub-area.
                    ui.painter().rect_filled(
                        rect,
                        egui::CornerRadius::same(0),
                        theme::semantic_bg(ui, SemanticKind::Error),
                    );
                    ui.painter().hline(
                        rect.x_range(),
                        rect.top(),
                        egui::Stroke::new(1.0, theme::semantic(ui, SemanticKind::Error)),
                    );
                },
                |tree, _| {
                    let mut content = tui::column();
                    tui::fill_main(&mut content);
                    content.padding = TaffyRect {
                        left: LengthPercentage::length(8.0),
                        right: LengthPercentage::length(8.0),
                        top: LengthPercentage::length(8.0),
                        bottom: LengthPercentage::length(8.0),
                    };
                    tree.style(content).id(bar_id.with("content")).add(|tree| {
                        tree.style(tui::label_leaf())
                            .id(bar_id.with("header"))
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .ui(|ui| {
                                ui.label(
                                    egui::RichText::new(header)
                                        .color(theme::error_text(ui))
                                        .small(),
                                );
                            });
                        tree.id(bar_id.with("separator")).ui(separator);
                        tree.style(tui::flexible())
                            .id(bar_id.with("list"))
                            .ui(|ui| {
                                bar_action =
                                    error_list(ui, error_rows, doc.compile_error.as_deref());
                            });
                    });
                },
            );
            // The former `Panel::bottom` resize handle, now a tui-row boundary:
            // drag it to change the bar height (stored in egui memory).
            if let Some(rect) = error_rect {
                let zone = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.top() - 3.0),
                    egui::pos2(rect.right(), rect.top() + 3.0),
                );
                let response =
                    tree.egui_ui_mut()
                        .interact(zone, bar_id.with("resize"), egui::Sense::drag());
                if response.hovered() || response.dragged() {
                    tree.egui_ctx()
                        .set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                if response.dragged() {
                    let next = (height - response.drag_delta().y)
                        .clamp(MIN_ERROR_BAR_HEIGHT, MAX_ERROR_BAR_HEIGHT);
                    tree.egui_ctx()
                        .data_mut(|data| data.insert_temp(bar_id, next));
                }
            }
        }

        PreviewPaneOutput {
            canvas_action,
            bar_action,
            open_root,
            toast,
        }
    }

    /// The docked preview body: the resizable frame-tree inspector and the preview
    /// canvas share one egui leaf, because the inspector (an off-limits self-drawn
    /// pane) owns a resizable `Panel::right` internally. The error bar stays the
    /// same `Panel::bottom` shape as before.
    pub(in crate::app) fn preview_docked_ui(
        &mut self,
        ui: &mut egui::Ui,
        doc: &DocumentController,
        settings: &Settings,
        error_rows: &[ErrorRow],
    ) -> PreviewPaneOutput {
        let mut bar_action = PreviewAction::None;
        let error_count = doc.compile_error_count();
        if error_count > 0 {
            let showing_previous_preview = doc.doc.is_some() && doc.preview_failure.is_some();
            let header = error_bar_header(showing_previous_preview, error_count);
            let style = ui.style().clone();
            let band = egui::Panel::bottom(ERROR_BAR_ID)
                .resizable(true)
                .default_size(ERROR_BAR_HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::side_top_panel(&style)
                        .fill(theme::semantic_bg(ui, SemanticKind::Error))
                        .stroke(egui::Stroke::new(
                            1.0,
                            theme::semantic(ui, SemanticKind::Error),
                        ))
                        .inner_margin(egui::Margin::same(8)),
                )
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(header)
                            .color(theme::error_text(ui))
                            .small(),
                    );
                    separator(ui);
                    bar_action = error_list(ui, error_rows, doc.compile_error.as_deref());
                });
            ui.painter().hline(
                band.response.rect.x_range(),
                band.response.rect.top(),
                egui::Stroke::new(1.0, crate::theme::hairline(ui)),
            );
        }

        let (canvas_action, toast) = if doc.doc.is_some() {
            self.preview_canvas_ui(ui, doc, settings)
        } else {
            (PreviewAction::None, None)
        };
        PreviewPaneOutput {
            canvas_action,
            bar_action,
            open_root: None,
            toast,
        }
    }

    /// Draws the canvas (or the GPU/raster fallback) for the current document.
    /// Returns the canvas action and the optional one-time fallback toast.
    fn preview_canvas_ui(
        &mut self,
        ui: &mut egui::Ui,
        doc: &DocumentController,
        settings: &Settings,
    ) -> (PreviewAction, Option<String>) {
        let Some(resources) = doc.doc.as_ref() else {
            return (PreviewAction::None, None);
        };
        let source = Some(PageSource {
            generation: resources.generation,
            pages: &resources.page_sizes,
        });
        let theme = Theme::from_ui(ui);
        let vector_active =
            settings.preview.gpu_canvas_preview && self.state.zoom() >= MIN_VECTOR_ZOOM;
        let fallback_active = settings.preview.gpu_canvas_preview
            && vector_active
            && self.vector.gpu_failed(resources.generation);
        let fallback_reason = if fallback_active {
            let reason = self
                .vector
                .fallback_reason(resources.generation)
                .unwrap_or("");
            Some(t_with("preview.fallback_header", &[("reason", reason)]))
        } else {
            None
        };
        let style = PreviewStyle {
            background: theme::card(ui),
            placeholder: theme::placeholder(ui),
            ring: theme::reveal(ui),
            cursor: theme::reveal(ui),
            cursor_width: settings.preview.cursor_width,
            cursor_style: settings.preview.cursor_style.to_preview(),
            cursor_blink: settings.preview.cursor_blink,
            cursor_blink_interval_ms: settings.preview.cursor_blink_interval,
            selection_mask: {
                let [r, g, b] = settings.preview.selection_color;
                egui::Color32::from_rgba_unmultiplied(
                    r,
                    g,
                    b,
                    (settings.preview.selection_opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
                )
            },
            debug_highlight: {
                let [r, g, b, a] = settings.debug.frame_tree.highlight_color;
                egui::Color32::from_rgba_unmultiplied(r, g, b, a)
            },
            hairline: theme::hairline(ui),
            page_border: theme.colors.page_border,
            refreshing: doc.compile_in_flight && resources.generation < doc.compile_generation,
            badge_fill: theme.colors.widget_inactive_fill,
            badge_text: theme.colors.widget_inactive_text,
            fallback_reason,
        };

        let canvas_action = if vector_active && !self.vector.gpu_failed(resources.generation) {
            self.vector
                .set_image_max_side(settings.preview.gpu_image_max_side);
            let mut vector = self.vector.painter(resources);
            self.state.show(
                ui,
                source.as_ref(),
                settings.preview.preview_dpi,
                style,
                settings.preview.auto_scroll_preview,
                Some(&mut vector),
            )
        } else {
            self.state.show(
                ui,
                source.as_ref(),
                settings.preview.preview_dpi,
                style,
                settings.preview.auto_scroll_preview,
                None,
            )
        };

        // Surface the whole-document GPU fallback once per failed generation, and
        // only while the vector path would otherwise be in effect (low zoom is
        // raster by design and stays silent).
        let notice = if fallback_active {
            self.vector.take_fallback_notice(resources.generation)
        } else {
            None
        };
        let toast =
            notice.map(|reason| t_with("toast.gpu_fallback", &[("reason", reason.as_str())]));
        (canvas_action, toast)
    }

    /// One of the three honest empty states, as tui children of the pane body.
    fn preview_empty(
        &mut self,
        tree: &mut Tui,
        doc: &DocumentController,
        tree_root: Option<&Path>,
    ) -> Option<PathBuf> {
        let mut picked = None;
        let id = egui::Id::new("typ_edit_preview_empty");
        tree.style(tui::centered_column()).id(id).add(|tree| {
            if tree_root.is_none() {
                tree.style(tui::label_leaf())
                    .id(id.with("open_hint"))
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .ui(|ui| {
                        ui.label(
                            egui::RichText::new(t("preview.open_to_start"))
                                .color(theme::text_muted(ui)),
                        );
                    });
                tree.style(tui::fixed(6.0))
                    .id(id.with("open_gap"))
                    .add_empty();
                tree.id(id.with("open_folder")).ui(|ui| {
                    if button(ui, &t("preview.open_folder"), ButtonVariant::Cta, true).clicked() {
                        picked = rfd::FileDialog::new().pick_folder();
                    }
                });
            } else if doc.compile_in_flight {
                tree.style(tui::label_leaf())
                    .id(id.with("compiling"))
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .ui(|ui| {
                        ui.label(
                            egui::RichText::new(t("preview.compiling"))
                                .color(theme::text_muted(ui)),
                        );
                    });
            } else {
                tree.style(tui::label_leaf())
                    .id(id.with("empty"))
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .ui(|ui| {
                        ui.label(
                            egui::RichText::new(t("preview.empty")).color(theme::text_muted(ui)),
                        );
                    });
            }
        });
        picked
    }

    /// Recomputes one live caret marker per editor selection and feeds them all
    /// to the preview. Cached per `(caret bytes, document generation)`, so the
    /// per-frame call is cheap; plain-text buffers and a missing document clear
    /// the markers. `path` and `plain_text` are the caller's active buffer.
    pub(in crate::app) fn update_cursor_marker(
        &mut self,
        ctx: &egui::Context,
        doc: &DocumentController,
        path: Option<&Path>,
        plain_text: bool,
    ) {
        self.poll_cursor_anchors();
        let Some(resources) = doc.doc.as_ref() else {
            self.clear_cursor_markers();
            return;
        };
        if path.is_none() || plain_text {
            self.clear_cursor_markers();
            return;
        }

        let generation = resources.generation;
        let mut carets = caret_bytes(&doc.selection);
        if carets.is_empty() {
            // Before the editor has reported a selection list the primary cursor
            // is still the only caret, so it alone is shown.
            carets.push(doc.cursor_byte);
        }
        if let Some((cached_bytes, cached_generation, cached)) = &self.cursor_markers_cache
            && *cached_bytes == carets
            && *cached_generation == generation
        {
            let markers = cached
                .iter()
                .filter_map(|anchor| anchor.map(Self::cursor_marker))
                .collect();
            self.state.set_cursor_markers(markers);
            return;
        }

        // Frame-span reverse walks are document-sized. Never run them in the
        // editor frame: hand the Arc'd compile result to the cursor worker and
        // keep the old markers until its reply arrives.
        if self.cursor_anchor_in_flight.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        let Some(path) = path else {
            return;
        };
        let request = CursorAnchorRequest {
            carets: carets.clone(),
            generation,
            path: path.to_path_buf(),
            result: Arc::clone(&resources.result),
        };
        if self.cursor_anchor_tx.send(request).is_ok() {
            self.cursor_anchor_in_flight = Some((carets, generation));
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    /// Drains completed cursor-anchor work. Only the reply matching the
    /// in-flight key is used; all older replies are stale.
    fn poll_cursor_anchors(&mut self) {
        while let Ok(reply) = self.cursor_anchor_rx.try_recv() {
            let key = (reply.carets.clone(), reply.generation);
            if self.cursor_anchor_in_flight.as_ref() != Some(&key) {
                continue;
            }
            self.cursor_anchor_in_flight = None;
            let markers = reply
                .anchors
                .iter()
                .filter_map(|anchor| anchor.map(Self::cursor_marker))
                .collect();
            self.set_cursor_markers_cache(reply.carets, reply.generation, reply.anchors);
            self.state.set_cursor_markers(markers);
        }
    }

    fn clear_cursor_markers(&mut self) {
        self.cursor_anchor_in_flight = None;
        self.clear_cursor_markers_cache();
        self.state.set_cursor_markers(Vec::new());
    }

    /// Recomputes the preview's selection masks from every editor selection.
    /// Cached per `(ranges, document generation)`; clears on plain text / no doc /
    /// no path / no non-empty selection.
    pub(in crate::app) fn update_selection_masks(
        &mut self,
        ctx: &egui::Context,
        doc: &DocumentController,
        path: Option<&Path>,
        plain_text: bool,
    ) {
        self.poll_selection_masks();
        let Some(resources) = doc.doc.as_ref() else {
            self.clear_selection_masks();
            return;
        };
        let Some(path) = path else {
            self.clear_selection_masks();
            return;
        };
        if plain_text {
            self.clear_selection_masks();
            return;
        }
        let ranges = non_empty_ranges(&doc.selection);
        if ranges.is_empty() {
            self.clear_selection_masks();
            return;
        }
        let generation = resources.generation;
        self.selection_masks_wanted = Some((ranges.clone(), generation));
        if let Some((cached_ranges, cached_generation)) = &self.selection_masks_cache
            && *cached_ranges == ranges
            && *cached_generation == generation
        {
            return;
        }
        if self.selection_masks_in_flight.is_some() {
            // Keep the previous masks visible, and poll for the worker reply.
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        let key_ranges = ranges.clone();
        let request = SelectionMaskRequest {
            ranges,
            generation,
            path: path.to_path_buf(),
            result: Arc::clone(&resources.result),
        };
        if self.selection_masks_tx.send(request).is_ok() {
            self.selection_masks_in_flight = Some((key_ranges, generation));
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    /// Drains completed selection-mask work. Only the reply matching the
    /// in-flight key is used; all older replies are stale.
    fn poll_selection_masks(&mut self) {
        while let Ok(reply) = self.selection_masks_rx.try_recv() {
            let key = (reply.ranges.clone(), reply.generation);
            if self.selection_masks_in_flight.as_ref() != Some(&key) {
                continue;
            }
            self.selection_masks_in_flight = None;
            if self.selection_masks_wanted.as_ref() != Some(&key) {
                continue;
            }
            self.set_selection_masks_cache(reply.ranges, reply.generation);
            self.state.set_selection_masks(reply.masks);
        }
    }

    fn clear_selection_masks(&mut self) {
        self.selection_masks_in_flight = None;
        self.selection_masks_wanted = None;
        self.clear_selection_masks_cache();
        self.state.set_selection_masks(Vec::new());
    }

    /// Converts an engine anchor into the preview's page-local marker input.
    fn cursor_marker(anchor: CursorAnchor) -> CursorMarker {
        CursorMarker {
            page: anchor.page,
            x_pt: anchor.x.to_pt() as f32,
            left_pt: anchor.left.to_pt() as f32,
            right_pt: anchor.right.to_pt() as f32,
            top_pt: anchor.top.to_pt() as f32,
            bottom_pt: anchor.bottom.to_pt() as f32,
        }
    }

    /// Maps a byte offset in the active buffer to a preview position: a 0-based
    /// page plus a page-local point (pt), or `None` when it cannot be mapped.
    fn cursor_preview_position(
        &self,
        doc: &DocumentController,
        path: Option<&Path>,
        byte: usize,
    ) -> Option<(usize, (f32, f32))> {
        let resources = doc.doc.as_ref()?;
        let path = path?;
        let anchor = resources.result.cursor_anchor(path, byte)?;
        let y_pt = (anchor.top.to_pt() + anchor.bottom.to_pt()) * 0.5;
        Some((anchor.page, (anchor.x.to_pt() as f32, y_pt as f32)))
    }

    /// "在预览区中显示": map the caret to a page position and start the ring.
    pub(in crate::app) fn reveal_cursor_in_preview(
        &mut self,
        doc: &DocumentController,
        path: Option<&Path>,
        byte_offset: usize,
    ) {
        if let Some((page, point)) = self.cursor_preview_position(doc, path, byte_offset) {
            self.state.reveal(page, point);
        }
    }

    /// Auto-follow: scroll the preview minimally to the caret's mapped position
    /// and remember `(byte, document generation)` so an unchanged cursor never
    /// re-fires.
    pub(in crate::app) fn follow_cursor_in_preview(
        &mut self,
        doc: &DocumentController,
        path: Option<&Path>,
        byte: usize,
    ) {
        let Some(doc_gen) = doc.doc.as_ref().map(|resources| resources.generation) else {
            return;
        };
        if self.follow.last == Some((byte, doc_gen)) {
            return;
        }
        let Some((page, point)) = self.cursor_preview_position(doc, path, byte) else {
            return;
        };
        self.state.ensure_visible(page, point);
        self.mark_followed(byte, doc_gen);
    }

    /// Fires the pending auto-follow once the cursor has been quiet for
    /// [`FOLLOW_DEBOUNCE`] (only when no compile is in flight; a compiling edit
    /// is served by `follow_after_compile`). Also schedules the wake-up frame.
    /// `path` is the active buffer's path.
    pub(in crate::app) fn evaluate_auto_follow(
        &mut self,
        ctx: &egui::Context,
        doc: &DocumentController,
        settings: &Settings,
        preview_collapsed: bool,
        path: Option<&Path>,
    ) {
        if !settings.preview.auto_scroll_preview {
            return;
        }
        let Some((byte, since)) = self.follow.pending else {
            return;
        };
        if preview_collapsed {
            self.clear_follow_pending();
            return;
        }
        let elapsed = since.elapsed();
        if !follow_due(elapsed, doc.compile_in_flight, FOLLOW_DEBOUNCE) {
            if !doc.compile_in_flight {
                ctx.request_repaint_after(FOLLOW_DEBOUNCE - elapsed);
            }
            return;
        }
        self.clear_follow_pending();
        self.follow_cursor_in_preview(doc, path, byte);
    }

    /// After a compile result lands: serve a pending follow with the fresh
    /// document. A plain recompile with an unmoved cursor has no pending follow
    /// and must not jump. `path` is the active buffer's path.
    pub(in crate::app) fn follow_after_compile(
        &mut self,
        doc: &DocumentController,
        settings: &Settings,
        preview_collapsed: bool,
        path: Option<&Path>,
    ) {
        if !settings.preview.auto_scroll_preview {
            return;
        }
        if preview_collapsed {
            return;
        }
        if self.follow.pending.is_none() || doc.doc.is_none() {
            return;
        }
        self.clear_follow_pending();
        self.follow_cursor_in_preview(doc, path, doc.cursor_byte);
    }
}

/// Whether an armed auto-follow may fire: no compile may be in flight (a
/// compiling edit is served once its result lands) and the cursor must have
/// been quiet for `debounce`.
pub(super) fn follow_due(age: Duration, in_flight: bool, debounce: Duration) -> bool {
    !in_flight && age >= debounce
}

/// The byte ranges that can produce preview masks: non-empty (`start < end`),
/// kept in their stored order.
fn non_empty_ranges(ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter(|range| range.start < range.end)
        .cloned()
        .collect()
}

/// Concatenates the preview masks of every byte range, in range order: each
/// range is mapped to its page-local boxes by `map`, which the caller wires to
/// the engine query. Pure so the aggregation is testable without a document.
fn masks_from_ranges(
    ranges: &[Range<usize>],
    mut map: impl FnMut(&Range<usize>) -> Vec<(usize, typst::layout::Rect)>,
) -> Vec<SelectionMask> {
    let mut masks = Vec::new();
    for range in ranges {
        masks.extend(map(range).into_iter().map(|(page, rect)| SelectionMask {
            page,
            left_pt: rect.min.x.to_pt() as f32,
            top_pt: rect.min.y.to_pt() as f32,
            right_pt: rect.max.x.to_pt() as f32,
            bottom_pt: rect.max.y.to_pt() as f32,
        }));
    }
    masks
}

/// The preview caret byte of every selection, in list order: the selection end
/// for a non-empty range, the caret position for a collapsed one.
fn caret_bytes(ranges: &[Range<usize>]) -> Vec<usize> {
    ranges
        .iter()
        .map(|range| {
            if range.start < range.end {
                range.end
            } else {
                range.start
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_ranges_drops_collapsed_and_preserves_order() {
        let ranges = vec![2..2, 5..9, 9..9, 0..1];
        assert_eq!(non_empty_ranges(&ranges), vec![5..9, 0..1]);
        assert!(non_empty_ranges(&[]).is_empty());
        assert!(non_empty_ranges(std::slice::from_ref(&(3..3))).is_empty());
    }

    #[test]
    fn masks_from_ranges_concatenates_every_range_in_order() {
        let masks = masks_from_ranges(&[10..12, 20..24], |range| {
            vec![(
                0usize,
                typst::layout::Rect::new(
                    typst::layout::Point::new(
                        typst::layout::Abs::pt(range.start as f64),
                        typst::layout::Abs::pt(1.0),
                    ),
                    typst::layout::Point::new(
                        typst::layout::Abs::pt(range.end as f64),
                        typst::layout::Abs::pt(3.0),
                    ),
                ),
            )]
        });

        assert_eq!(masks.len(), 2, "one mask per range");
        assert_eq!(
            (masks[0].page, masks[0].left_pt, masks[0].right_pt),
            (0, 10.0, 12.0),
            "the first range's mask comes first"
        );
        assert_eq!(
            (masks[1].page, masks[1].left_pt, masks[1].right_pt),
            (0, 20.0, 24.0),
            "the second range's mask follows in order"
        );

        assert!(
            masks_from_ranges(&[], |_| unreachable!("no range means no mapping")).is_empty(),
            "no ranges yields no masks"
        );
    }

    #[test]
    fn caret_bytes_uses_selection_end_and_collapsed_start_in_order() {
        assert_eq!(caret_bytes(&[2..2, 5..9, 9..9, 0..1]), vec![2, 9, 9, 1]);
        assert!(caret_bytes(&[]).is_empty());
    }
}
