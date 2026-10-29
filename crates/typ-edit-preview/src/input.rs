//! Input handling, the floating zoom bar and the per-frame raster scheduling for
//! [`PreviewState`]. The state itself and its types stay in [`super`].

use super::*;
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::{
    button::{ButtonVariant, button, icon_button},
    list::toggle_chip,
    theme::Theme,
};

/// Below this pane width the preview control bar switches to icon-only
/// fit/actual-size buttons and tighter spacing, so page navigation and the
/// Follow toggle stay visible at 1024-wide windows.
const COMPACT_BAR_W: f32 = 460.0;

/// Inputs for one frame's raster scheduling pass.
pub(crate) struct RasterSchedule<'a, 'b> {
    pub ui: &'a egui::Ui,
    pub source: &'a PageSource<'b>,
    pub pages: &'a [PageGeom],
    pub rect: egui::Rect,
    pub preview_dpi: f32,
    pub suppress: Option<&'a PreviewAction>,
    pub vector_active: bool,
}

impl PreviewState {
    /// Consumes pointer/wheel input for one frame and returns the source jump a
    /// single click resolved to, if any.
    pub(crate) fn handle_input(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        pages: &[PageGeom],
        rect: egui::Rect,
    ) -> Option<PreviewAction> {
        // --- input ----------------------------------------------------------
        let mut jump = None;
        let dt = ui.input(|i| i.stable_dt);
        let hovered = response.hovered();
        let source = if hovered {
            ui.input(|i| i.smooth_scroll_delta)
        } else {
            egui::Vec2::ZERO
        };
        let hand_panning = response.dragged() || response.drag_stopped();
        let step = if hovered && !hand_panning {
            Some(
                self.wheel_resampler
                    .step(ui, source, egui::Vec2::splat(1.0)),
            )
        } else {
            self.wheel_resampler.reset();
            None
        };
        let wheel = step.map_or(egui::Vec2::ZERO, |step| step.delta);
        let wheel_owned =
            !hand_panning && (source != egui::Vec2::ZERO || step.is_some_and(|step| step.active));
        let scrolling = ui.input(|i| i.is_scrolling());
        if self.wheel_scrolling && !scrolling && !wheel_owned {
            self.wheel_scrolling = false;
        }
        if hovered && (scrolling || wheel_owned) {
            self.wheel_scrolling = true;
        }
        if response.dragged() {
            // Hand-panning owns the offset; a fling only starts on release.
            self.wheel_resampler.reset();
            self.pan_vel = egui::Vec2::ZERO;
            let drag = response.drag_delta();
            if drag != egui::Vec2::ZERO {
                self.view.offset += drag;
                self.scroll_anim = None;
            }
        } else if response.drag_stopped() {
            // Inertial pan: start with the pointer's release velocity and take
            // the first step in the release frame itself (no one-frame pause).
            self.wheel_resampler.reset();
            self.pan_vel = ui.input(|i| i.pointer.velocity());
            if kinetic_step(&mut self.view.offset, &mut self.pan_vel, dt) {
                self.scroll_anim = None;
                ui.ctx().request_repaint();
            }
        } else if !wheel_owned && kinetic_step(&mut self.view.offset, &mut self.pan_vel, dt) {
            // A wheel frame owns the motion; applying a previous coast in the
            // same frame would double the step and make the tail lurch.
            self.scroll_anim = None;
            ui.ctx().request_repaint();
        }
        if wheel_owned {
            // Wheel / trackpad → pan (Ctrl+wheel and pinch are folded into
            // zoom_delta). `smooth_scroll_delta` is already the per-frame amount
            // after egui's smoothing; never add a second velocity tail.
            self.view.offset += wheel;
            // A wheel frame owns the motion; a leftover drag fling must not add
            // a second step on top of egui's smoothed delta.
            self.pan_vel = egui::Vec2::ZERO;
            self.scroll_anim = None;
            // Consume the axes this canvas panned. Without this, a second
            // pass/outer widget could apply the same egui delta again.
            ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
            ui.ctx().request_repaint();
        }
        if hovered {
            let zoom_delta = ui.input(|i| i.zoom_delta());
            if (zoom_delta - 1.0).abs() > 1e-4 {
                let anchor = ui
                    .input(|i| i.pointer.hover_pos())
                    .map(|p| (p - rect.min).to_pos2())
                    .unwrap_or_else(|| (rect.size() / 2.0).to_pos2());
                self.view
                    .zoom_about(anchor, self.view.zoom * zoom_delta, rect, pages);
                self.pan_vel = egui::Vec2::ZERO;
                self.scroll_anim = None;
            }
        }
        let pre_clamp = self.view.offset;
        self.view.clamp(rect, pages);
        // Hitting a pan limit kills that axis' remaining glide.
        if self.view.offset.x != pre_clamp.x {
            self.pan_vel.x = 0.0;
        }
        if self.view.offset.y != pre_clamp.y {
            self.pan_vel.y = 0.0;
        }
        if response.clicked() {
            // Single click follows the Typst web preview. Cancel any in-flight
            // fling/follow tween so it cannot override the jump target.
            self.pan_vel = egui::Vec2::ZERO;
            self.scroll_anim = None;
            if let Some(pos) = response.interact_pointer_pos() {
                jump = self.jump_at(pages, rect, pos);
            }
        }
        if self.wheel_scrolling {
            // Keep consuming egui's smoothed wheel on zero-delta intermediate
            // frames; otherwise the next delta waits for unrelated input.
            ui.ctx().request_repaint();
        }
        jump
    }

    /// Draws the pane's top control bar, applies the zoom it selected, and
    /// returns the bar's action (currently only the auto-scroll toggle).
    ///
    /// The bar is a reserved row inside the preview pane (not a floating area),
    /// so it stays clipped to the pane and cannot cover neighbouring panes.
    pub(crate) fn zoom_bar_ui(
        &mut self,
        ui: &mut egui::Ui,
        pages: &[PageGeom],
        bar: egui::Rect,
        canvas: egui::Rect,
        style: &PreviewStyle,
        auto_scroll: bool,
    ) -> Option<PreviewAction> {
        let center = (canvas.size() * 0.5).to_pos2();
        let mut target_zoom = self
            .pending_zoom_factor
            .take()
            .map(|factor| self.view.zoom * factor);
        let current_page = self.view.centered_page(canvas, pages);
        let compact = bar.width() < COMPACT_BAR_W;
        let mut action = None;
        ui.painter().hline(
            bar.x_range(),
            bar.bottom(),
            egui::Stroke::new(1.0, style.hairline),
        );
        // tui-whitelist: this bar is drawn inside the preview canvas's tui leaf
        // (`preview.rs` adds the canvas as one `tree.ui(...)` node). Opening
        // `tui()` here would nest a tree inside that leaf and hit the P1 0×0
        // host bug, so the pre-P6 egui row layout is kept unchanged.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                let theme = Theme::from_ui(ui);
                if compact {
                    ui.spacing_mut().item_spacing.x = 4.0;
                }
                ui.add_space(6.0);
                // A small epsilon keeps the buttons disabled once the clamped zoom
                // has reached an end stop instead of only when it is exactly there.
                const ZOOM_EPS: f32 = 1e-4;
                let zoom = self.view.zoom;
                let at_min = zoom <= PreviewView::MIN_ZOOM + ZOOM_EPS;
                let at_max = zoom >= PreviewView::MAX_ZOOM - ZOOM_EPS;
                let zoom_out = icon_button(ui, "−", theme.spacing.icon_btn, !at_min)
                    .on_hover_text(t("preview.zoom_out").as_ref())
                    .on_disabled_hover_text(t("preview.zoom_out").as_ref());
                if zoom_out.clicked() {
                    target_zoom = Some(zoom / 1.25);
                }
                ui.label(format!("{:.0}%", zoom * 100.0));
                let zoom_in = icon_button(ui, "+", theme.spacing.icon_btn, !at_max)
                    .on_hover_text(t("preview.zoom_in").as_ref())
                    .on_disabled_hover_text(t("preview.zoom_in").as_ref());
                if zoom_in.clicked() {
                    target_zoom = Some(zoom * 1.25);
                }
                let fit = if compact {
                    icon_button(ui, "↔", theme.spacing.icon_btn, true)
                        .on_hover_text(t("preview.fit_width_hover").as_ref())
                } else {
                    button(
                        ui,
                        t("preview.fit_width").as_ref(),
                        ButtonVariant::Quiet,
                        true,
                    )
                    .on_hover_text(t("preview.fit_width_hover").as_ref())
                };
                if fit.clicked() {
                    let width = PreviewView::widest_visible_width(&self.view, canvas, pages)
                        .unwrap_or_else(|| pages.iter().map(|p| p.width).fold(0.0f32, f32::max));
                    target_zoom = Some(PreviewView::fit_width_zoom(canvas.width(), width));
                }
                let actual = if compact {
                    icon_button(ui, "1:1", theme.spacing.icon_btn, true)
                        .on_hover_text(t("preview.actual_size").as_ref())
                } else {
                    button(ui, "100%", ButtonVariant::Quiet, true)
                        .on_hover_text(t("preview.actual_size").as_ref())
                };
                if actual.clicked() {
                    target_zoom = Some(1.0);
                }
                if let Some(page) = current_page {
                    // Vertical hairline divider between the zoom controls and
                    // the page navigation. The shared `separator` helper paints
                    // a full-width horizontal rule, which would consume the rest
                    // of this left-to-right row, so the divider is drawn in
                    // place with the same Theme token and egui's separator
                    // spacing (6 pt) to keep the layout and hit areas intact.
                    // tui-whitelist: 单叶 allocate_exact_size（竖向分隔线体积），线为纯绘制。
                    let (divider, _) = ui.allocate_exact_size(
                        egui::vec2(6.0, ui.available_height()),
                        egui::Sense::hover(),
                    );
                    ui.painter().vline(
                        divider.center().x,
                        divider.y_range(),
                        egui::Stroke::new(theme.strokes.hairline, theme.colors.hairline),
                    );
                    let on_first = page == 0;
                    let on_last = page + 1 >= pages.len();
                    let prev = icon_button(ui, "◀", theme.spacing.icon_btn, !on_first)
                        .on_hover_text(t("preview.prev_page").as_ref())
                        .on_disabled_hover_text(t("preview.prev_page").as_ref());
                    if prev.clicked() {
                        self.pending_goto = Some(page.saturating_sub(1));
                    }
                    ui.label(
                        egui::RichText::new(t_with(
                            "preview.page_indicator",
                            &[
                                ("page", &(page + 1).to_string()),
                                ("total", &pages.len().to_string()),
                            ],
                        ))
                        .color(theme.colors.text_muted),
                    );
                    let next = icon_button(ui, "▶", theme.spacing.icon_btn, !on_last)
                        .on_hover_text(t("preview.next_page").as_ref())
                        .on_disabled_hover_text(t("preview.next_page").as_ref());
                    if next.clicked() {
                        self.pending_goto = Some((page + 1).min(pages.len() - 1));
                    }
                }
                // tui-whitelist: same leaf-host constraint as `scope_builder`
                // above; this is the zoom bar's right-aligned follow toggle.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    let toggle = toggle_chip(ui, auto_scroll, t("preview.follow").as_ref());
                    if toggle
                        .on_hover_text(t("preview.follow_hover").as_ref())
                        .clicked()
                    {
                        action = Some(PreviewAction::ToggleAutoScroll);
                    }
                });
            },
        );
        if let Some(zoom) = target_zoom {
            self.view.zoom_about(center, zoom, canvas, pages);
            self.pan_vel = egui::Vec2::ZERO;
            self.scroll_anim = None;
        }
        action
    }

    /// Draws and drives the preview's overlay scrollbars.
    ///
    /// Each bar maps the pan clamp's reachable offset range
    /// ([`PreviewView::scroll_ranges`] — the view can keep panning until only
    /// ~10 px of a page remain) onto its track, so the handle length is the
    /// visible share of that travel and dragging reaches the exact pan limits.
    /// The bars render and drag through [`typ_edit_widgets::scrollbar`], egui's
    /// floating look included.
    pub(crate) fn scrollbars_ui(
        &mut self,
        ui: &mut egui::Ui,
        pages: &[PageGeom],
        rect: egui::Rect,
        style: &PreviewStyle,
    ) {
        if pages.is_empty() {
            return;
        }
        let (x_range, y_range) = self.view.scroll_ranges(rect, pages);
        let bar_width = ui.style().spacing.scroll.bar_width;
        let outer_hovered = ui.rect_contains_pointer(rect);

        let mut adjusted = false;
        if x_range.max - x_range.min > 1.0 {
            let len = x_range.max - x_range.min;
            let frac = (x_range.max - self.view.offset.x) / len;
            let visible = rect.width() / len;
            let track = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - bar_width),
                egui::pos2(rect.right(), rect.bottom()),
            );
            if let Some(frac) = typ_edit_widgets::scrollbar::scrollbar(
                ui,
                typ_edit_widgets::scrollbar::ScrollbarInput {
                    id: egui::Id::new("preview-scrollbar-h"),
                    track,
                    horizontal: true,
                    frac,
                    visible,
                    outer_hovered,
                    always_visible: false,
                },
            ) {
                self.view.offset.x = x_range.max - frac * len;
                adjusted = true;
            }
        }
        if y_range.max - y_range.min > 1.0 {
            let len = y_range.max - y_range.min;
            let frac = (y_range.max - self.view.offset.y) / len;
            let visible = rect.height() / len;
            let track = egui::Rect::from_min_max(
                egui::pos2(rect.right() - bar_width, rect.top()),
                egui::pos2(rect.right(), rect.bottom()),
            );
            if let Some(frac) = typ_edit_widgets::scrollbar::scrollbar(
                ui,
                typ_edit_widgets::scrollbar::ScrollbarInput {
                    id: egui::Id::new("preview-scrollbar-v"),
                    track,
                    horizontal: false,
                    frac,
                    visible,
                    outer_hovered,
                    always_visible: false,
                },
            ) {
                self.view.offset.y = y_range.max - frac * len;
                adjusted = true;
            }

            // Page-boundary ticks: each boundary (the top of every page after the
            // first) is placed where the handle sits when that boundary reaches
            // the viewport top, `offset = -zoom * page.top`, mapped through the
            // same `frac` the bar uses. Handle length and travel mirror
            // `typ_edit_widgets::scrollbar`'s geometry without touching it.
            let track_len = track.height();
            let min_handle = ui.style().spacing.scroll.handle_min_length;
            let handle_len =
                (track_len * visible.clamp(0.0, 1.0)).clamp(min_handle.min(track_len), track_len);
            let travel = track_len - handle_len;
            if travel > 0.0 {
                let zoom = self.view.zoom;
                for page in pages.iter().skip(1) {
                    let frac = ((y_range.max + zoom * page.top) / len).clamp(0.0, 1.0);
                    let y = track.min.y + frac * travel;
                    ui.painter()
                        .hline(track.x_range(), y, egui::Stroke::new(1.0, style.hairline));
                }
            }
        }
        if adjusted {
            self.scroll_anim = None;
            self.view.clamp(rect, pages);
        }
    }

    /// Computes the visible pages and, if the picture is stale, the single raster
    /// request to issue this frame. Returns the pages to paint alongside the action.
    pub(crate) fn schedule_raster(&mut self, request: RasterSchedule<'_, '_>) -> FramePlan {
        let RasterSchedule {
            ui,
            source,
            pages,
            rect,
            preview_dpi,
            suppress,
            vector_active,
        } = request;
        // --- visible pages, sharp target and fallback target -----------------
        let visible: Vec<usize> = (0..pages.len())
            .filter(|&i| self.view.page_screen_rect(rect, &pages[i]).intersects(rect))
            .collect();
        if vector_active {
            return FramePlan {
                visible,
                action: PreviewAction::None,
            };
        }
        let primary = visible.iter().copied().max_by(|&a, &b| {
            intersection_area(self.view.page_screen_rect(rect, &pages[a]), rect).total_cmp(
                &intersection_area(self.view.page_screen_rect(rect, &pages[b]), rect),
            )
        });

        let visible_span = match (visible.iter().min(), visible.iter().max()) {
            (Some(&first), Some(&last)) => Some(first..=last),
            _ => None,
        };
        self.fallbacks.touch_visible(&visible);

        let ppp = ui.ctx().pixels_per_point();
        let max_side = ui.ctx().input(|i| i.max_texture_side).max(1) as u32;

        let sharp = primary.and_then(|index| {
            let page = &pages[index];
            let visible_pt = region::visible_rect(&self.view, rect, page)?;
            let region_pt = region::expand_region(visible_pt, (page.width, page.height));
            let target = region::clamp_pixel_per_pt(region_pt, self.view.zoom * ppp, max_side);
            Some((index, visible_pt, region_pt, target))
        });

        // --- continuous refinement (3): re-render whenever the picture is stale.
        let sharp_needed = match (&self.last_sharp, sharp) {
            (None, Some(_)) => true,
            (Some(last), Some((page, visible_pt, _, target))) => {
                last.page != page
                    || !region::scale_matches(last.pixel_per_pt, target)
                    || !region::covers(last.region_pt, visible_pt)
            }
            (_, None) => false,
        };

        // --- whole-page fallback (5) -----------------------------------------
        // The target is deterministic per document + DPI + quantized view scale, so a
        // change in any of them replaces the fallback texture.
        let view_scale = self.view.zoom * ppp;
        let missing_fallback = visible.iter().copied().find(|&index| {
            let page = &pages[index];
            let target = region::fallback_pixel_per_pt(
                (page.width, page.height),
                preview_dpi,
                max_side,
                view_scale,
            );
            self.fallbacks
                .get(index)
                .is_none_or(|frame| frame.pixel_per_pt != target)
        });

        let mut action = PreviewAction::None;
        // A returned widget action (click jump, zoom-bar toggle) suppresses
        // this frame's raster scheduling even when the app-side resolution yields
        // nothing (URL/unmapped): the next frame re-issues the request, so nothing
        // is dropped.
        if suppress.is_none() && !self.in_flight {
            if let Some((page, _, region_pt, target)) = sharp.filter(|_| sharp_needed) {
                self.in_flight = true;
                self.pending = Some(Pending {
                    generation: source.generation,
                    page,
                    kind: RequestKind::Sharp,
                });
                action = PreviewAction::RequestRaster(RasterRequest {
                    generation: source.generation,
                    page,
                    region: Some(region_pt),
                    pixel_per_pt: target,
                });
            } else if let Some(page) = missing_fallback {
                let page_geom = &pages[page];
                let target = region::fallback_pixel_per_pt(
                    (page_geom.width, page_geom.height),
                    preview_dpi,
                    max_side,
                    view_scale,
                );
                self.in_flight = true;
                self.pending = Some(Pending {
                    generation: source.generation,
                    page,
                    kind: RequestKind::Fallback,
                });
                action = PreviewAction::RequestRaster(RasterRequest {
                    generation: source.generation,
                    page,
                    region: None,
                    pixel_per_pt: target,
                });
            }
        }

        if let Some(span) = visible_span {
            self.fallbacks.evict_to_budget(span, FALLBACK_BUDGET_BYTES);
        }

        FramePlan { visible, action }
    }

    /// Sets the cursor icon for the current drag/hover state. The old idle
    /// tooltip ("click…, Ctrl+wheel…, drag…") is intentionally gone: it
    /// repeated obvious controls and made the canvas feel noisy.
    pub(crate) fn update_cursor_hint(&self, ui: &egui::Ui, response: egui::Response) {
        if response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
}

/// Advances one inertial-pan step for each axis; returns whether anything moved.
///
/// Mirrors egui's kinetic scrolling (velocity decays by a constant friction and
/// is dropped below `STOP_SPEED`, or when the friction step alone would cross
/// zero), with the friction tuned higher than egui's 1000 for a shorter glide.
pub(crate) fn kinetic_step(offset: &mut egui::Vec2, vel: &mut egui::Vec2, dt: f32) -> bool {
    const STOP_SPEED: f32 = 20.0;
    const FRICTION: f32 = 2000.0;
    let mut moved = false;
    for d in 0..2 {
        let friction = FRICTION * dt;
        if friction > vel[d].abs() || vel[d].abs() < STOP_SPEED {
            vel[d] = 0.0;
        } else {
            vel[d] -= friction * vel[d].signum();
            offset[d] += vel[d] * dt;
            moved = true;
        }
    }
    moved
}
