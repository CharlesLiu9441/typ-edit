//! Painting for [`PreviewState`]: page fallbacks, the sharp region, the fading ring
//! and the page placeholder fills. The state itself and its types stay in [`super`].

use super::*;
use typ_edit_i18n::t;

/// Inputs for one preview paint pass.
pub(crate) struct PaintInput<'ui, 'a, 'v> {
    pub ui: &'ui mut egui::Ui,
    pub painter: egui::Painter,
    pub rect: egui::Rect,
    pub pages: &'a [PageGeom],
    pub visible: Vec<usize>,
    pub style: &'a PreviewStyle,
    pub vector: Option<&'v mut dyn VectorPagePainter>,
}

impl PreviewState {
    /// Paints the viewport background, visible page fallbacks, the sharp region and the
    /// fading reveal ring, and keeps repainting while the ring animates.
    pub(crate) fn paint(&mut self, input: PaintInput<'_, '_, '_>) {
        let PaintInput {
            ui,
            painter,
            rect,
            pages,
            visible,
            style,
            vector,
        } = input;
        // --- paint ----------------------------------------------------------
        // The canvas is the preview card's surface, so the card reads as one
        // continuous sheet (docs/design/ui-cards.md §5 row 7, §6.4).
        painter.rect_filled(rect, 0.0, style.background);

        if let Some(vector) = vector {
            let mut canvas_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            canvas_ui.set_clip_rect(rect);
            for &index in &visible {
                let page_rect = self.view.page_screen_rect(rect, &pages[index]);
                if !vector.paint_page(&mut canvas_ui, index, page_rect, self.view.zoom) {
                    painter.rect_filled(page_rect, 2.0, style.placeholder);
                }
            }
        } else {
            // Placeholders for visible pages whose fallback has not arrived yet.
            for &index in &visible {
                if !self.fallbacks.contains(index) {
                    painter.rect_filled(
                        self.view.page_screen_rect(rect, &pages[index]),
                        2.0,
                        style.placeholder,
                    );
                }
            }

            for &index in &visible {
                if let Some(frame) = self.fallbacks.get(index) {
                    painter.image(
                        frame.texture.id(),
                        self.view.page_screen_rect(rect, &pages[index]),
                        uv_full(),
                        egui::Color32::WHITE,
                    );
                }
            }

            // The sharp region is drawn on top of the fallbacks, anchored in page space.
            if let Some(last) = &self.last_sharp
                && let Some(page) = pages.get(last.page)
            {
                let page_rect = self.view.page_screen_rect(rect, page);
                let zoom = self.view.zoom;
                let texture_rect = egui::Rect::from_min_size(
                    page_rect.min + egui::vec2(last.region_pt.0 * zoom, last.region_pt.1 * zoom),
                    egui::vec2(last.region_pt.2 * zoom, last.region_pt.3 * zoom),
                );
                painter.image(
                    last.texture.id(),
                    texture_rect,
                    uv_full(),
                    egui::Color32::WHITE,
                );
            }
        }

        // Page outlines, stroked after every raster so no texture can cover the
        // paper edge (needed because the light theme's paper is canvas-white).
        for &index in &visible {
            painter.rect_stroke(
                self.view.page_screen_rect(rect, &pages[index]),
                egui::CornerRadius::same(2),
                egui::Stroke::new(1.0, style.page_border),
                egui::StrokeKind::Inside,
            );
        }

        // Selection masks, above the page textures/outlines and below the live
        // cursor marker (page-local points).
        for mask in &self.selection_masks {
            if let Some(page) = pages.get(mask.page) {
                let min = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(mask.left_pt, page.top + mask.top_pt))
                        .to_vec2();
                let max = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(mask.right_pt, page.top + mask.bottom_pt))
                        .to_vec2();
                painter.rect_filled(
                    egui::Rect::from_min_max(min, max),
                    2.0,
                    style.selection_mask,
                );
            }
        }

        // Debug highlights, above the page textures/masks so a hovered frame is
        // clearly visible, and below the live cursor so the caret stays legible.
        for highlight in &self.debug_highlights {
            if let Some(page) = pages.get(highlight.page) {
                let min = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(highlight.left_pt, page.top + highlight.top_pt))
                        .to_vec2();
                let max = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(
                            highlight.right_pt,
                            page.top + highlight.bottom_pt,
                        ))
                        .to_vec2();
                let rect = egui::Rect::from_min_max(min, max);
                painter.rect_filled(rect, 2.0, style.debug_highlight);
                let c = style.debug_highlight;
                painter.rect_stroke(
                    rect,
                    2.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(c.r(), c.g(), c.b())),
                    egui::StrokeKind::Inside,
                );
            }
        }

        // Live caret markers at the engine-mapped cursor anchors (page-local
        // points). The blink phase is computed once for the whole list so every
        // caret blinks in sync and a single repaint is scheduled while blinking.
        if !self.cursor_markers.is_empty() {
            let interval = style.cursor_blink_interval_ms.clamp(100.0, 5000.0);
            let (visible, remaining) = if style.cursor_blink {
                blink_phase(self.blink_epoch.elapsed().as_secs_f32() * 1000.0, interval)
            } else {
                (true, f32::INFINITY)
            };
            if visible {
                for marker in &self.cursor_markers {
                    if let Some(page) = pages.get(marker.page) {
                        self.paint_cursor(&painter, rect, page, marker, style);
                    }
                }
            }
            if style.cursor_blink {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(
                        remaining.ceil() as u64 + 1,
                    ));
            }
        }

        // Fading ring at the revealed page position.
        if let Some(ring) = self.ring {
            match ring.alpha() {
                Some(alpha) => {
                    if let Some(page) = pages.get(ring.page) {
                        let doc_pos = egui::pos2(ring.point_pt.0, page.top + ring.point_pt.1);
                        let center = rect.min + self.view.doc_to_screen(doc_pos).to_vec2();
                        let base = style.ring;
                        let color = egui::Color32::from_rgba_unmultiplied(
                            base.r(),
                            base.g(),
                            base.b(),
                            (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                        );
                        painter.circle_stroke(
                            center,
                            RING_RADIUS,
                            egui::Stroke::new(RING_WIDTH, color),
                        );
                    }
                }
                None => self.ring = None,
            }
        }
        if self
            .ring
            .as_ref()
            .is_some_and(|ring| ring.alpha().is_some())
        {
            ui.ctx().request_repaint();
        }

        // Stacked status chips pinned to the canvas top-right. The "refreshing"
        // chip is paint-only; the "fallback" chip is hoverable for its reason
        // tooltip.
        let mut chip_top = rect.top() + BADGE_MARGIN;
        if style.refreshing && !visible.is_empty() {
            let chip = draw_badge(
                &painter,
                rect,
                chip_top,
                t("preview.compiling").as_ref(),
                style,
            );
            chip_top = chip.bottom() + BADGE_GAP;
        }
        if let Some(reason) = &style.fallback_reason {
            let chip = draw_badge(
                &painter,
                rect,
                chip_top,
                t("preview.fallback_badge").as_ref(),
                style,
            );
            ui.interact(
                chip,
                ui.id().with("preview-fallback-badge"),
                egui::Sense::hover(),
            )
            .on_hover_text(reason.as_str());
        }
    }

    /// Paints one live caret marker on `page`, using the page-local
    /// [`CursorMarker`] geometry; every caret shares the configured style.
    fn paint_cursor(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        page: &PageGeom,
        marker: &CursorMarker,
        style: &PreviewStyle,
    ) {
        match style.cursor_style {
            CursorStyle::Line => {
                let top = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(marker.x_pt, page.top + marker.top_pt))
                        .to_vec2();
                let bottom = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(marker.x_pt, page.top + marker.bottom_pt))
                        .to_vec2();
                painter.line_segment(
                    [top, bottom],
                    egui::Stroke::new(style.cursor_width.clamp(0.5, 6.0), style.cursor),
                );
            }
            CursorStyle::Ring => {
                let center = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(
                            marker.x_pt,
                            page.top + (marker.top_pt + marker.bottom_pt) * 0.5,
                        ))
                        .to_vec2();
                let radius =
                    ((marker.bottom_pt - marker.top_pt) * self.view.zoom * 0.5).clamp(4.0, 24.0);
                painter.circle_stroke(
                    center,
                    radius,
                    egui::Stroke::new(style.cursor_width.clamp(0.5, 6.0), style.cursor),
                );
            }
            CursorStyle::Mask => {
                let min = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(marker.left_pt, page.top + marker.top_pt))
                        .to_vec2();
                let max = rect.min
                    + self
                        .view
                        .doc_to_screen(egui::pos2(marker.right_pt, page.top + marker.bottom_pt))
                        .to_vec2();
                let fill = egui::Color32::from_rgba_unmultiplied(
                    style.cursor.r(),
                    style.cursor.g(),
                    style.cursor.b(),
                    56,
                );
                painter.rect_filled(egui::Rect::from_min_max(min, max), 2.0, fill);
            }
        }
    }
}

/// Draws one top-right status chip with its top edge at `top`; returns its rect.
fn draw_badge(
    painter: &egui::Painter,
    rect: egui::Rect,
    top: f32,
    text: &str,
    style: &PreviewStyle,
) -> egui::Rect {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(BADGE_FONT_SIZE),
        style.badge_text,
    );
    let size = galley.size() + egui::vec2(BADGE_PAD_X * 2.0, BADGE_PAD_Y * 2.0);
    let chip =
        egui::Rect::from_min_size(egui::pos2(rect.right() - BADGE_MARGIN - size.x, top), size);
    painter.rect_filled(chip, egui::CornerRadius::same(4), style.badge_fill);
    painter.galley(
        chip.min + egui::vec2(BADGE_PAD_X, BADGE_PAD_Y),
        galley,
        style.badge_text,
    );
    chip
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(fallback_reason: Option<String>) -> PreviewStyle {
        PreviewStyle {
            background: egui::Color32::BLACK,
            placeholder: egui::Color32::GRAY,
            ring: egui::Color32::RED,
            cursor: egui::Color32::WHITE,
            cursor_width: 1.0,
            cursor_style: CursorStyle::Line,
            cursor_blink: false,
            cursor_blink_interval_ms: 500.0,
            selection_mask: egui::Color32::TRANSPARENT,
            debug_highlight: egui::Color32::TRANSPARENT,
            hairline: egui::Color32::GRAY,
            page_border: egui::Color32::GRAY,
            refreshing: false,
            badge_fill: egui::Color32::DARK_GRAY,
            badge_text: egui::Color32::WHITE,
            fallback_reason,
        }
    }

    #[test]
    fn paint_survives_with_and_without_a_fallback_badge() {
        for reason in [None, Some("PDF 图片".to_owned())] {
            let ctx = egui::Context::default();
            let mut state = PreviewState::new();
            let style = style(reason);
            let mut output = ctx.run_ui(Default::default(), |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter().clone();
                state.paint(PaintInput {
                    ui,
                    painter,
                    rect,
                    pages: &[],
                    visible: Vec::new(),
                    style: &style,
                    vector: None,
                });
            });
            output.textures_delta.clear();
        }
    }
}
