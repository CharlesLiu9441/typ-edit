//! Host-fed raster preview widget: asynchronous region-based raster rendering with
//! pan/zoom, clamping, click reporting and the fading ring overlay.
//!
//! The crate is typst-free. The host supplies the document geometry
//! ([`PageSource`], page sizes in typographic points) and the theme colors
//! ([`PreviewStyle`]); the widget emits [`PreviewAction`]s back to the host — a
//! click resolved to a page-local point (`JumpAt`, which the host maps to a
//! source position) or a raster request the host fulfills asynchronously.
//!
//! The rendered frame is always displayed through [`view`]'s transform. A sharp
//! region render refines the visible area continuously; a whole-page fallback (at the
//! DPI configured in Settings) keeps content on screen during pans and at extreme
//! zoom levels.

mod fallback;
mod input;
pub(crate) mod overlay;
mod paint;
pub(crate) mod region;
pub(crate) mod view;

use std::path::PathBuf;

pub(crate) use fallback::FALLBACK_BUDGET_BYTES;
use fallback::{FallbackCache, FallbackFrame};
use typ_edit_i18n::t;
use typ_edit_model::{RasterRequest, RasterResult};
pub use view::PreviewView;
use view::{PageGeom, layout_pages};

/// Gap between stacked pages, in typographic points (docs/architecture.md §4).
const PAGE_GAP: f32 = 12.0;
/// Fading ring radius and stroke width, in screen px.
const RING_RADIUS: f32 = 14.0;
const RING_WIDTH: f32 = 2.5;
/// Height of the pane's top control bar (zoom / fit / page indicator).
const ZOOM_BAR_H: f32 = 26.0;
/// Margin of the "refreshing" badge from the canvas edges, in screen px.
const BADGE_MARGIN: f32 = 8.0;
/// Inner padding of the "refreshing" badge around its text, in screen px.
const BADGE_PAD_X: f32 = 6.0;
const BADGE_PAD_Y: f32 = 3.0;
/// Font size of the "refreshing" badge text, in screen px.
const BADGE_FONT_SIZE: f32 = 12.0;
/// Vertical gap between stacked top-right badges, in screen px.
const BADGE_GAP: f32 = 4.0;
/// Duration of the smooth auto-follow scroll, in milliseconds.
const SCROLL_ANIM_MS: f32 = 180.0;

/// Smooth-scroll tween for the follow `ensure_visible` path.
struct ScrollAnim {
    from: egui::Vec2,
    to: egui::Vec2,
    start: std::time::Instant,
}

/// Ease-out cubic: fast start, soft landing.
fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Everything the preview needs to know about the current document.
pub struct PageSource<'a> {
    pub generation: u64,
    /// Page sizes in typographic points, `(width, height)`.
    pub pages: &'a [(f32, f32)],
}

/// How the live cursor marker is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorStyle {
    #[default]
    Line,
    Ring,
    Mask,
}

/// A live caret line in the preview: 0-based page + page-local points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorMarker {
    pub page: usize,
    pub x_pt: f32,
    /// Left edge of the matched node's box (page-local pt).
    pub left_pt: f32,
    /// Right edge of the matched node's box (page-local pt).
    pub right_pt: f32,
    pub top_pt: f32,
    pub bottom_pt: f32,
}

/// One page-local selection mask rectangle (host-mapped from the engine's selection boxes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionMask {
    pub page: usize,
    pub left_pt: f32,
    pub top_pt: f32,
    pub right_pt: f32,
    pub bottom_pt: f32,
}

/// One page-local debug highlight rectangle (frame-tree inspector hover).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugHighlight {
    pub page: usize,
    pub left_pt: f32,
    pub top_pt: f32,
    pub right_pt: f32,
    pub bottom_pt: f32,
}

/// Colors the preview paints with, supplied by the host so the crate stays theme-free.
#[derive(Clone, Debug)]
pub struct PreviewStyle {
    pub background: egui::Color32,
    pub placeholder: egui::Color32,
    pub ring: egui::Color32,
    /// The live caret line color.
    pub cursor: egui::Color32,
    /// Stroke width of the live caret line, in screen px.
    pub cursor_width: f32,
    /// How the live cursor marker is drawn.
    pub cursor_style: CursorStyle,
    /// Whether the live cursor marker blinks.
    pub cursor_blink: bool,
    /// Blink phase duration in milliseconds (one visible/hidden phase).
    pub cursor_blink_interval_ms: f32,
    /// Fill of the selection masks (alpha baked by the host).
    pub selection_mask: egui::Color32,
    /// Fill of the frame-tree inspector's hover highlight (alpha baked by the
    /// host); the outline is drawn as the opaque version of the same RGB.
    pub debug_highlight: egui::Color32,
    pub hairline: egui::Color32,
    /// Outline of every visible page: keeps the paper edge visible when the
    /// canvas and the paper share the same white in the light theme.
    pub page_border: egui::Color32,
    /// Whether a newer compile is in flight while an older document is shown.
    pub refreshing: bool,
    /// Fill and text colour of the "正在编译…" refresh badge.
    pub badge_fill: egui::Color32,
    pub badge_text: egui::Color32,
    /// Host-provided reason shown on the fallback badge; `None` hides it.
    pub fallback_reason: Option<String>,
}

/// Host-supplied painter for resolution-independent page content. When one is
/// supplied, visible pages are painted through it and no rasters are requested.
pub trait VectorPagePainter {
    /// Paints page `page` (0-based) with its top-left at `page_rect.min` and
    /// `scale` screen px per typographic point. Returns `false` when the page
    /// could not be painted and the widget should keep its placeholder.
    fn paint_page(
        &mut self,
        ui: &mut egui::Ui,
        page: usize,
        page_rect: egui::Rect,
        scale: f32,
    ) -> bool;
}

/// Actions the preview requests from the app.
#[derive(Default, Clone)]
pub enum PreviewAction {
    #[default]
    None,
    /// Click mapped to a source position: open file + move caret.
    JumpToSource { path: PathBuf, byte_offset: usize },
    /// Click on page `page` (0-based) at a page-local point (`pt`): the app
    /// maps it to a source position.
    JumpAt { page: usize, point_pt: (f32, f32) },
    /// Start a raster render through the engine.
    RequestRaster(RasterRequest),
    /// The zoom bar's auto-scroll toggle was clicked; the app flips and persists
    /// the feature and may follow the current cursor immediately.
    ToggleAutoScroll,
}

/// What one frame's input/geometry pass decided: the pages to draw and the raster
/// request to issue (if any). Built by [`PreviewState::schedule_raster`].
pub(crate) struct FramePlan {
    pub(crate) visible: Vec<usize>,
    pub(crate) action: PreviewAction,
}

/// The last completed sharp region render.
struct SharpFrame {
    page: usize,
    region_pt: region::Region,
    pixel_per_pt: f32,
    texture: egui::TextureHandle,
}

/// What the single in-flight request was for, so the arriving result is filed right.
#[derive(Clone, Copy)]
enum RequestKind {
    Sharp,
    Fallback,
}

/// The single request currently in flight.
struct Pending {
    generation: u64,
    page: usize,
    kind: RequestKind,
}

/// Preview widget state: view transform, raster textures and ring animation.
pub struct PreviewState {
    view: PreviewView,
    ring: Option<overlay::Ring>,
    /// Live caret markers to paint: one per caret, 0-based page + page-local points.
    cursor_markers: Vec<CursorMarker>,
    /// Selection masks to paint: 0-based page + page-local points each.
    selection_masks: Vec<SelectionMask>,
    /// Debug highlight rectangles to paint: 0-based page + page-local points each.
    debug_highlights: Vec<DebugHighlight>,
    /// Phase origin of the blinking cursor marker; reset whenever the marker
    /// changes to `Some`, so a moved caret shows solid immediately.
    blink_epoch: std::time::Instant,
    /// Target of a pending "reveal" centering: `(page index, page-local point pt)`.
    pending_center: Option<(usize, (f32, f32))>,
    /// Target of a pending minimal cursor-follow scroll: `(page index,
    /// page-local point pt)`. Unlike `pending_center`, it never zooms or rings.
    pending_ensure: Option<(usize, (f32, f32))>,
    /// Page index to centre on the next `show` (zoom-bar ◀ / ▶ navigation).
    pending_goto: Option<usize>,
    /// Zoom factor to apply around the pane centre on the next `show` (Ctrl+= / Ctrl+-).
    pending_zoom_factor: Option<f32>,
    /// Last egui context seen in `show`; lets `reveal` request a repaint.
    ctx: Option<egui::Context>,
    /// Document generation the current textures belong to.
    last_generation: u64,
    /// Last completed sharp region render.
    last_sharp: Option<SharpFrame>,
    /// Whole-page fallbacks for the current generation, keyed by page index.
    fallbacks: FallbackCache,
    pending: Option<Pending>,
    in_flight: bool,
    /// Inertial pan velocity in points/second, set on drag release and decayed
    /// by [`kinetic_step`](input::kinetic_step).
    pan_vel: egui::Vec2,
    /// True while this canvas owns an egui scroll action; keeps the frame loop
    /// alive through zero-delta `Smoothing`/`InTouch` frames.
    wheel_scrolling: bool,
    /// Normalizes Windows precision-touchpad line events to the frame clock.
    wheel_resampler: typ_edit_widgets::wheel::WheelResampler,
    /// Active smooth-follow tween; `None` when the offset is user-owned.
    scroll_anim: Option<ScrollAnim>,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewState {
    /// Creates the preview state. Call once from `App::new`.
    pub fn new() -> Self {
        Self {
            view: PreviewView::default(),
            ring: None,
            cursor_markers: Vec::new(),
            selection_masks: Vec::new(),
            debug_highlights: Vec::new(),
            blink_epoch: std::time::Instant::now(),
            pending_center: None,
            pending_ensure: None,
            pending_goto: None,
            pending_zoom_factor: None,
            ctx: None,
            last_generation: u64::MAX,
            last_sharp: None,
            fallbacks: FallbackCache::default(),
            pending: None,
            in_flight: false,
            pan_vel: egui::Vec2::ZERO,
            wheel_scrolling: false,
            wheel_resampler: typ_edit_widgets::wheel::WheelResampler::default(),
            scroll_anim: None,
        }
    }

    /// Uploads a finished raster, then triggers the next refinement pass.
    pub fn apply_result(&mut self, ctx: &egui::Context, result: RasterResult) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.in_flight = false;
        if result.generation != pending.generation || result.page != pending.page {
            return;
        }

        let (width, height) = result.size_px;
        let expected = texture_bytes(width, height);
        if width == 0 || height == 0 || result.rgba.len() != expected {
            log::warn!("preview: dropped malformed raster {}x{}", width, height);
            return;
        }

        let image = egui::ColorImage::from_rgba_premultiplied(
            [width as usize, height as usize],
            &result.rgba,
        );
        let texture = ctx.load_texture(
            format!("preview-raster-{}-{}", result.generation, result.page),
            image,
            egui::TextureOptions::LINEAR,
        );

        match pending.kind {
            RequestKind::Sharp => {
                self.last_sharp = Some(SharpFrame {
                    page: result.page,
                    region_pt: result.region,
                    pixel_per_pt: result.pixel_per_pt,
                    texture,
                });
            }
            RequestKind::Fallback => {
                self.fallbacks.insert(
                    result.page,
                    FallbackFrame {
                        pixel_per_pt: result.pixel_per_pt,
                        texture,
                        bytes: expected,
                        last_used: 0,
                    },
                );
            }
        }
        ctx.request_repaint();
    }

    /// Shows the preview. Must never panic when `source` is `None`.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        source: Option<&PageSource>,
        preview_dpi: f32,
        style: PreviewStyle,
        auto_scroll: bool,
        vector: Option<&mut dyn VectorPagePainter>,
    ) -> PreviewAction {
        self.ctx = Some(ui.ctx().clone());

        let Some(source) = source else {
            let (rect, _response) = ui.allocate_at_least(ui.available_size(), egui::Sense::hover());
            ui.painter_at(rect).text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                t("preview.compiling").as_ref(),
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
            return PreviewAction::None;
        };

        // A slim control bar is reserved at the top of the pane; the canvas takes
        // the rest, so the controls can never spill into neighbouring panes.
        let avail = ui.available_rect_before_wrap();
        let bar_rect = egui::Rect::from_min_max(
            avail.min,
            egui::pos2(avail.max.x, (avail.min.y + ZOOM_BAR_H).min(avail.max.y)),
        );
        ui.allocate_rect(bar_rect, egui::Sense::hover());
        let (rect, response) =
            ui.allocate_at_least(ui.available_size(), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        // --- document geometry ---------------------------------------------
        let pages = layout_pages(source.pages, PAGE_GAP);

        if source.generation != self.last_generation {
            self.last_generation = source.generation;
            self.last_sharp = None;
            self.fallbacks.clear();
            self.pending = None;
            self.in_flight = false;
            self.scroll_anim = None;
        }

        // --- pending reveal centering ---------------------------------------
        if let Some((page_index, point_pt)) = self.pending_center.take()
            && let Some(page) = pages.get(page_index)
        {
            if self.view.zoom < 0.5 {
                self.view.zoom = 1.0;
            }
            let target = egui::pos2(point_pt.0, page.top + point_pt.1);
            self.view.offset = PreviewView::centering_offset(rect.size(), self.view.zoom, target);
            self.scroll_anim = None;
        }

        // --- pending page navigation (zoom-bar ◀ / ▶) -----------------------
        if let Some(page_index) = self.pending_goto.take()
            && let Some(page) = pages.get(page_index)
        {
            if self.view.zoom < 0.5 {
                self.view.zoom = 1.0;
            }
            let target = egui::pos2(page.width * 0.5, page.top + page.height * 0.5);
            self.view.offset = PreviewView::centering_offset(rect.size(), self.view.zoom, target);
            self.scroll_anim = None;
        }

        let jump = self.handle_input(ui, &response, &pages, rect);
        self.advance_scroll_anim(ui, rect, &pages);
        let bar_action = self.zoom_bar_ui(ui, &pages, bar_rect, rect, &style, auto_scroll);

        // After the zoom bar (so a zoom click settles first) and before raster
        // scheduling (so the visible pages reflect the final view).
        if let Some((page_index, point_pt)) = self.pending_ensure.take()
            && let Some(page) = pages.get(page_index)
        {
            let target = egui::pos2(point_pt.0, page.top + point_pt.1);
            let to = self
                .view
                .ensure_target_offset(rect, &pages, target, view::FOLLOW_MARGIN);
            if (to - self.view.offset).length() > 0.5 {
                self.scroll_anim = Some(ScrollAnim {
                    from: self.view.offset,
                    to,
                    start: std::time::Instant::now(),
                });
                ui.ctx().request_repaint();
            } else {
                self.scroll_anim = None;
            }
        }

        // A returned action suppresses this frame's raster scheduling, exactly
        // like a click jump: the next frame re-issues the request, so a
        // request that would be dropped is never left marked in flight.
        if bar_action.is_some() {
            ui.ctx().request_repaint();
        }
        let suppression = jump.as_ref().or(bar_action.as_ref());
        let vector_active = vector.is_some();
        let FramePlan { visible, action } = self.schedule_raster(input::RasterSchedule {
            ui,
            source,
            pages: &pages,
            rect,
            preview_dpi,
            suppress: suppression,
            vector_active,
        });
        self.paint(paint::PaintInput {
            ui,
            painter,
            rect,
            pages: &pages,
            visible,
            style: &style,
            vector,
        });
        self.scrollbars_ui(ui, &pages, rect, &style);
        self.update_cursor_hint(ui, response);

        jump.or(bar_action).unwrap_or(action)
    }

    /// Current zoom: screen points per typographic point.
    pub fn zoom(&self) -> f32 {
        self.view.zoom
    }

    /// Starts the fading light-blue ring at a page-local point (pt) and arms the
    /// next `show` to centre that point in the viewport.
    pub fn reveal(&mut self, page: usize, point_pt: (f32, f32)) {
        self.ring = Some(overlay::Ring::new(page, point_pt));
        self.pending_center = Some((page, point_pt));
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// Arms the next `show` to smoothly scroll (a short ease-out tween) so the
    /// page-local `point_pt` on `page` becomes visible. Unlike [`Self::reveal`]
    /// this never starts the ring nor changes the zoom — the auto-follow cursor
    /// path.
    pub fn ensure_visible(&mut self, page: usize, point_pt: (f32, f32)) {
        self.pending_ensure = Some((page, point_pt));
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// Replaces the live caret markers: one per caret, in host order. Repaints
    /// only on change; a non-empty list resets the blink phase so a moved caret
    /// shows solid immediately.
    pub fn set_cursor_markers(&mut self, markers: Vec<CursorMarker>) {
        if self.cursor_markers != markers {
            self.cursor_markers = markers;
            if !self.cursor_markers.is_empty() {
                self.blink_epoch = std::time::Instant::now();
            }
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint();
            }
        }
    }

    /// Replaces the selection masks. Repaints only on change.
    pub fn set_selection_masks(&mut self, masks: Vec<SelectionMask>) {
        if self.selection_masks != masks {
            self.selection_masks = masks;
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint();
            }
        }
    }

    /// Replaces the debug highlight rectangles. Repaints only on change.
    pub fn set_debug_highlights(&mut self, highlights: Vec<DebugHighlight>) {
        if self.debug_highlights != highlights {
            self.debug_highlights = highlights;
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint();
            }
        }
    }

    /// Steps the zoom by `factor` around the pane centre on the next `show`.
    pub fn zoom_step(&mut self, factor: f32) {
        self.pending_zoom_factor = Some(factor);
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    #[doc(hidden)]
    pub fn debug_view_offset(&self) -> egui::Vec2 {
        self.view.offset
    }

    /// Resets zoom to 100% and the pan offset to its default (Ctrl+0); the next
    /// `show` re-runs the clamp against the live viewport.
    pub fn reset_view(&mut self) {
        self.view.zoom = 1.0;
        self.view.offset = egui::Vec2::new(48.0, 48.0);
        self.scroll_anim = None;
        self.wheel_resampler.reset();
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// Advances the follow tween, if one is active, and keeps repainting while it runs.
    fn advance_scroll_anim(&mut self, ui: &egui::Ui, rect: egui::Rect, pages: &[PageGeom]) {
        let Some(anim) = self.scroll_anim.as_ref() else {
            return;
        };
        let t = (anim.start.elapsed().as_secs_f32() * 1000.0 / SCROLL_ANIM_MS).clamp(0.0, 1.0);
        let (from, to) = (anim.from, anim.to);
        self.view.offset = from + (to - from) * ease_out_cubic(t);
        self.view.clamp(rect, pages);
        if t >= 1.0 {
            self.scroll_anim = None;
        } else {
            ui.ctx().request_repaint();
        }
    }

    /// Maps a click in `pos` to a page-local [`PreviewAction::JumpAt`], or
    /// `None` when the click fell outside every page.
    fn jump_at(
        &self,
        pages: &[PageGeom],
        viewport: egui::Rect,
        pos: egui::Pos2,
    ) -> Option<PreviewAction> {
        let doc_pos = self.view.screen_to_doc((pos - viewport.min).to_pos2());
        let page_index = self.view.page_at(pages, doc_pos.y)?;
        let page = pages.get(page_index)?;
        let local_y = doc_pos.y - page.top;
        if doc_pos.x < 0.0 || local_y < 0.0 || doc_pos.x > page.width || local_y > page.height {
            return None; // click outside the page itself
        }
        Some(PreviewAction::JumpAt {
            page: page_index,
            point_pt: (doc_pos.x, local_y),
        })
    }
}

/// The UV rectangle covering a whole texture.
fn uv_full() -> egui::Rect {
    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0))
}

/// Area of the intersection of two screen rects (0 when they do not overlap).
fn intersection_area(a: egui::Rect, b: egui::Rect) -> f32 {
    let intersection = a.intersect(b);
    (intersection.width().max(0.0) * intersection.height().max(0.0)).max(0.0)
}

/// Heap bytes of an uploaded RGBA texture of `width` x `height` pixels.
fn texture_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

/// Current blink phase of the live cursor marker: `(visible, ms_until_next_toggle)`.
/// Visible during `[0, interval)`, hidden during `[interval, 2*interval)`, repeating.
/// A non-positive interval never blinks.
fn blink_phase(elapsed_ms: f32, interval_ms: f32) -> (bool, f32) {
    if interval_ms <= 0.0 {
        return (true, f32::INFINITY);
    }
    let cycle_ms = elapsed_ms.rem_euclid(interval_ms * 2.0);
    if cycle_ms < interval_ms {
        (true, interval_ms - cycle_ms)
    } else {
        (false, interval_ms * 2.0 - cycle_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_bytes_multiply_to_rgba_size() {
        assert_eq!(texture_bytes(0, 0), 0);
        assert_eq!(texture_bytes(1, 1), 4);
        assert_eq!(texture_bytes(2480, 3508), 2480 * 3508 * 4);
    }

    #[test]
    fn reveal_arms_pending_centering_and_the_ring() {
        let mut state = PreviewState::new();
        state.reveal(3, (10.0, 20.0));

        assert!(state.ring.is_some());
        assert!(
            matches!(state.pending_center, Some((3, (x, y))) if x == 10.0 && y == 20.0),
            "pending_center = {:?}",
            state.pending_center
        );
    }

    #[test]
    fn ensure_visible_arms_pending_ensure_without_a_ring() {
        let mut state = PreviewState::new();
        state.ensure_visible(2, (5.0, 6.0));

        assert!(
            matches!(state.pending_ensure, Some((2, (x, y))) if x == 5.0 && y == 6.0),
            "pending_ensure = {:?}",
            state.pending_ensure
        );
        assert!(state.ring.is_none(), "auto-follow must never ring");
        assert!(state.pending_center.is_none());
    }

    #[test]
    fn zoom_step_is_pending_until_show() {
        let mut state = PreviewState::new();
        state.zoom_step(1.25);

        assert_eq!(state.pending_zoom_factor, Some(1.25));
    }

    #[test]
    fn set_cursor_markers_stores_replaces_and_clears_every_marker() {
        let mut state = PreviewState::new();
        let first = CursorMarker {
            page: 1,
            x_pt: 10.0,
            left_pt: 10.0,
            right_pt: 10.0,
            top_pt: 20.0,
            bottom_pt: 32.0,
        };
        let second = CursorMarker {
            page: 2,
            x_pt: 40.0,
            left_pt: 40.0,
            right_pt: 40.0,
            top_pt: 50.0,
            bottom_pt: 62.0,
        };

        state.set_cursor_markers(vec![first, second]);
        assert_eq!(state.cursor_markers, vec![first, second]);

        // A no-change call keeps the markers (and would skip the repaint).
        state.set_cursor_markers(vec![first, second]);
        assert_eq!(state.cursor_markers, vec![first, second]);

        state.set_cursor_markers(vec![first]);
        assert_eq!(state.cursor_markers, vec![first]);

        state.set_cursor_markers(Vec::new());
        assert!(state.cursor_markers.is_empty());
    }

    #[test]
    fn set_selection_masks_stores_and_replaces() {
        let mut state = PreviewState::new();
        let masks = vec![
            SelectionMask {
                page: 0,
                left_pt: 1.0,
                top_pt: 2.0,
                right_pt: 3.0,
                bottom_pt: 4.0,
            },
            SelectionMask {
                page: 1,
                left_pt: 5.0,
                top_pt: 6.0,
                right_pt: 7.0,
                bottom_pt: 8.0,
            },
        ];

        state.set_selection_masks(masks.clone());
        assert_eq!(state.selection_masks, masks);

        // A no-change call keeps the same masks (and would skip the repaint).
        state.set_selection_masks(masks.clone());
        assert_eq!(state.selection_masks, masks);

        let replaced = vec![SelectionMask {
            page: 2,
            left_pt: 9.0,
            top_pt: 10.0,
            right_pt: 11.0,
            bottom_pt: 12.0,
        }];
        state.set_selection_masks(replaced.clone());
        assert_eq!(state.selection_masks, replaced);
    }

    #[test]
    fn set_debug_highlights_stores_replaces_and_clears() {
        let mut state = PreviewState::new();
        let highlights = vec![
            DebugHighlight {
                page: 0,
                left_pt: 1.0,
                top_pt: 2.0,
                right_pt: 3.0,
                bottom_pt: 4.0,
            },
            DebugHighlight {
                page: 1,
                left_pt: 5.0,
                top_pt: 6.0,
                right_pt: 7.0,
                bottom_pt: 8.0,
            },
        ];

        state.set_debug_highlights(highlights.clone());
        assert_eq!(state.debug_highlights, highlights);

        // A no-change call keeps the same highlights (and would skip the repaint).
        state.set_debug_highlights(highlights.clone());
        assert_eq!(state.debug_highlights, highlights);

        let replaced = vec![DebugHighlight {
            page: 2,
            left_pt: 9.0,
            top_pt: 10.0,
            right_pt: 11.0,
            bottom_pt: 12.0,
        }];
        state.set_debug_highlights(replaced.clone());
        assert_eq!(state.debug_highlights, replaced);

        state.set_debug_highlights(Vec::new());
        assert!(state.debug_highlights.is_empty());
    }

    #[test]
    fn blink_phase_alternates_and_reports_time_to_next_toggle() {
        // Visible from 0 to the interval.
        let (visible, remaining) = blink_phase(0.0, 500.0);
        assert!(visible);
        assert_eq!(remaining, 500.0);

        // Mid-phase: still visible, half a phase left.
        let (visible, remaining) = blink_phase(250.0, 500.0);
        assert!(visible);
        assert_eq!(remaining, 250.0);

        // Exactly at the interval the marker turns hidden for a full phase.
        let (visible, remaining) = blink_phase(500.0, 500.0);
        assert!(!visible);
        assert_eq!(remaining, 500.0);

        // 1.5 x interval: hidden, half a phase left.
        let (visible, remaining) = blink_phase(750.0, 500.0);
        assert!(!visible);
        assert_eq!(remaining, 250.0);

        // 2 x interval: the second cycle starts visible again.
        let (visible, remaining) = blink_phase(1000.0, 500.0);
        assert!(visible);
        assert_eq!(remaining, 500.0);

        // Non-positive interval: permanently visible, never repaint.
        let (visible, remaining) = blink_phase(123.0, 0.0);
        assert!(visible);
        assert_eq!(remaining, f32::INFINITY);
        assert!(blink_phase(123.0, -5.0).0);
    }

    #[test]
    fn blink_phase_repeats_every_two_intervals() {
        for elapsed in [
            0.0, 125.0, 250.0, 375.0, 500.0, 625.0, 750.0, 875.0, 1000.0, 2000.0, 3500.0,
        ] {
            let (visible, remaining) = blink_phase(elapsed, 500.0);
            let (wrapped, wrapped_remaining) = blink_phase(elapsed % 1000.0, 500.0);
            assert_eq!(visible, wrapped, "visible at {elapsed}");
            assert_eq!(remaining, wrapped_remaining, "remaining at {elapsed}");
        }
    }

    #[test]
    fn kinetic_pan_glides_then_stops() {
        // A fast fling moves the offset in the velocity direction and slows down.
        let mut offset = egui::Vec2::ZERO;
        let mut vel = egui::Vec2::new(600.0, -240.0);
        assert!(input::kinetic_step(&mut offset, &mut vel, 1.0 / 60.0));
        assert!(offset.x > 0.0 && offset.y < 0.0);
        assert!(vel.x > 0.0 && vel.x < 600.0, "{vel:?}");
        assert!(vel.y < 0.0 && vel.y > -240.0, "{vel:?}");

        // Below the stop speed the velocity snaps to zero and nothing moves.
        let mut offset = egui::Vec2::ZERO;
        let mut vel = egui::Vec2::new(5.0, 0.0);
        assert!(!input::kinetic_step(&mut offset, &mut vel, 1.0 / 60.0));
        assert_eq!(offset, egui::Vec2::ZERO);
        assert_eq!(vel, egui::Vec2::ZERO);
    }

    #[test]
    fn ease_out_cubic_is_anchored_monotonic_and_eases_out() {
        assert!((ease_out_cubic(0.0) - 0.0).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);

        let mid = ease_out_cubic(0.5);
        assert!(mid > 0.0 && mid < 1.0, "mid = {mid}");
        assert!(mid > 0.5, "ease-out must lead the linear ramp: {mid}");

        let mut prev = ease_out_cubic(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let value = ease_out_cubic(t);
            assert!(value >= prev, "not monotonic at t={t}: {prev} -> {value}");
            prev = value;
        }
    }
}
