//! Presentation-only tab-style view switcher.
//!
//! A single left-aligned, content-sized row of browser-style tabs that reuses
//! the tab bar's visual language — the same silhouette, hover fill and active
//! accent (see [`super::paint`]) — but none of the editor bar's chrome: no close
//! glyph, no dirty dot, no drag-reorder and no context menu. The widget is
//! presentation-only: it paints `labels` and returns the index of a clicked tab
//! that is not already `active`, leaving state application to the caller.
//!
//! When the labels overflow the pane, the strip reuses the editor bar's overflow
//! mechanism (see [`super::tab_bar`]): a reserved scrollbar row above the tabs
//! drives the shared [`crate::scrollbar`] widget, and the tabs scroll through a
//! hidden-bar [`egui::ScrollArea`] whose offset persists in egui temp memory
//! under `id_salt` (the same read-back-and-store dance the editor bar performs).

use std::sync::Arc;

use egui::{Color32, Rect, TextStyle, Vec2};

use super::paint;
use super::{
    TAB_ACCENT_H, TAB_GAP, TAB_HEIGHT, TAB_MAX_WIDTH, TAB_PAD_X, TAB_RADIUS, TAB_SCROLLBAR_H,
    TabBarOrientation,
};
use crate::theme::Theme;

/// Text budget of one switch label: the editor bar's [`TAB_MAX_WIDTH`] minus the
/// two label paddings and the two corner-radius reserves.
const LABEL_MAX: f32 = TAB_MAX_WIDTH - TAB_PAD_X * 2.0 - TAB_RADIUS as f32 * 2.0;

/// The one small flare reserve each tab carries, and the left inset before the
/// first tab: half a radius, enough to seat the active tab's outward foot
/// without the editor bar's close-zone blank.
const SWITCH_INSET: f32 = TAB_RADIUS as f32 * 0.5;

/// Draws a left-aligned, content-sized row of tab-styled options.
///
/// `labels` are laid out in the body font with the editor tab bar's colour
/// language; `active` may be out of range (no tab is painted active). A click on
/// a tab other than `active` returns its index; clicking the active tab, empty
/// `labels` and clicks outside the tabs return `None`. `id_salt` seeds the
/// row's interaction id and must be unique per instance and stable across
/// frames.
pub fn tab_switch(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    labels: &[&str],
    active: usize,
) -> Option<usize> {
    if labels.is_empty() {
        return None;
    }
    // `Id::new(&id_salt)` hashes the salt itself (references forward to the
    // inner value), so the id is stable across frames and independent of the
    // pushed-id stack — exactly like the editor bar's `base`.
    let base = egui::Id::new(&id_salt);
    ui.push_id(id_salt, |ui| tab_switch_body(ui, labels, active, base))
        .inner
}

fn tab_switch_body(
    ui: &mut egui::Ui,
    labels: &[&str],
    active: usize,
    base: egui::Id,
) -> Option<usize> {
    let font = TextStyle::Body.resolve(ui.style());
    let galleys: Vec<Arc<egui::Galley>> = labels
        .iter()
        .map(|label| {
            let mut job = egui::text::LayoutJob::simple_singleline(
                (*label).to_owned(),
                font.clone(),
                Color32::PLACEHOLDER,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(LABEL_MAX);
            ui.painter().layout_job(job)
        })
        .collect();
    let widths: Vec<f32> = galleys
        .iter()
        .map(|galley| switch_width(galley.size().x))
        .collect();
    let total = super::geometry::strip_width(&widths);

    // The pane's visible width, captured before the content-sized row advances
    // the cursor: the accent line below runs from the strip's start to the
    // pane's right edge, not just to the last tab (mirrors the bar's `strip_x`).
    let pane_x = ui
        .available_rect_before_wrap()
        .intersect(ui.clip_rect())
        .x_range();

    let avail_w = ui.available_width();
    let max_scroll = (total - avail_w).max(0.0);
    let scroll_id = base.with("scroll");
    let mut scroll = ui
        .ctx()
        .memory(|m| m.data.get_temp::<f32>(scroll_id))
        .unwrap_or(0.0)
        .clamp(0.0, max_scroll);

    // Reserved row above the tabs holding the shared scrollbar widget; only
    // allocated while the tabs overflow, so fitting tabs keep no dead band. It
    // sits above the tabs (not below) because the accent underline owns the
    // row under them.
    // tui-whitelist: 单叶 allocate_exact_size（切换器滚动条行），滑块为纯绘制。
    if max_scroll > 0.5 {
        let (bar, _) =
            ui.allocate_exact_size(egui::vec2(avail_w, TAB_SCROLLBAR_H), egui::Sense::hover());
        let outer_hovered = ui.rect_contains_pointer(bar);
        let frac = (scroll / max_scroll).clamp(0.0, 1.0);
        let visible = avail_w / total;
        if let Some(frac) = crate::scrollbar::scrollbar(
            ui,
            crate::scrollbar::ScrollbarInput {
                id: base.with("scrollbar"),
                track: bar,
                horizontal: true,
                frac,
                visible,
                outer_hovered,
                always_visible: true,
            },
        ) {
            scroll = frac * max_scroll;
        }
    }

    let mut activate = None;
    let mut focused = false;
    // tui-whitelist: ScrollArea 是允许的容器机制；其中整条切换器行是一个自绘叶。
    let out = egui::ScrollArea::horizontal()
        .id_salt(base.with("area"))
        .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .horizontal_scroll_offset(scroll)
        .show(ui, |ui| {
            // One clickable response over the whole content row — the horizontal
            // bar's registration shape — with hits resolved from the pointer
            // position against the tab rects rather than per-tab `interact`
            // calls. Rects inside a `ScrollArea` are already in screen space, so
            // the pointer hit test below needs no offset handling.
            // tui-whitelist: 单叶 allocate_exact_size（整条切换器行 + 命中区）。
            let (row, response) =
                ui.allocate_exact_size(Vec2::new(total, TAB_HEIGHT), egui::Sense::click());
            let rects = switch_rects(row, &widths);

            let hover_pos = ui.input(|i| i.pointer.hover_pos());
            let hovered =
                hover_pos.and_then(|pos| rects.iter().position(|rect| rect.contains(pos)));

            // The registered response wins the widget hit test in isolation. When
            // a host registers an overlapping click rect after this row — the
            // left pane's full-size project-tree background — that sibling wins
            // instead, so the raw primary click is the fallback that still names
            // the tab under the pointer.
            let raw_click = ui.is_enabled() && ui.input(|i| i.pointer.primary_clicked());
            let clicked = response.clicked() || raw_click;
            let click_pos = response
                .interact_pointer_pos()
                .or_else(|| ui.input(|i| i.pointer.interact_pos()));
            if clicked
                && let Some(pos) = click_pos
                && let Some(index) = rects.iter().position(|rect| rect.contains(pos))
                && index != active
            {
                activate = Some(index);
            }

            let visuals = ui.visuals().clone();
            let theme = Theme::from_ui(ui);
            let painter = ui.painter();
            // Inactive silhouettes first, the active one last: its flared feet
            // may overlap the gap and a neighbour's bottom corners, exactly like
            // the bar.
            for (index, rect) in rects.iter().enumerate() {
                if index == active {
                    continue;
                }
                if hovered == Some(index) {
                    paint::paint_tab(
                        painter,
                        *rect,
                        visuals.widgets.hovered.bg_fill.gamma_multiply(0.5),
                        false,
                        TabBarOrientation::Horizontal,
                    );
                }
            }
            if let Some(rect) = rects.get(active) {
                paint::paint_active_accent(
                    painter,
                    *rect,
                    visuals.hyperlink_color,
                    TabBarOrientation::Horizontal,
                    super::TAB_ACCENT_H,
                );
            }

            for (index, rect) in rects.iter().enumerate() {
                let color = if index == active {
                    visuals.widgets.active.fg_stroke.color
                } else if hovered == Some(index) {
                    visuals.text_color()
                } else {
                    theme.colors.text_muted
                };
                painter.galley(
                    egui::pos2(
                        rect.min.x + TAB_PAD_X,
                        rect.center().y - galleys[index].size().y * 0.5,
                    ),
                    galleys[index].clone(),
                    color,
                );
                if index == active && response.has_focus() {
                    painter.rect_stroke(
                        *rect,
                        egui::CornerRadius::same(theme.radii.control),
                        egui::Stroke::new(theme.strokes.accent, theme.colors.accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            focused = response.has_focus();

            if hovered.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            (row, rects)
        });
    scroll = out.state.offset.x.clamp(0.0, max_scroll);
    ui.ctx()
        .memory_mut(|m| m.data.insert_temp(scroll_id, scroll));

    let (row, rects) = out.inner;

    if focused && !labels.is_empty() {
        let mut step = 0isize;
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                step -= 1;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                step += 1;
            }
        });
        if step != 0 {
            let next = (active as isize + step).rem_euclid(labels.len() as isize) as usize;
            if next != active {
                activate = Some(next);
            }
        }
    }

    // The accent line spans the pane's full width, opened under the active tab
    // by one flare radius per side — the same "∩" continuation the horizontal
    // bar draws, extended past the content-sized strip to the pane edge. It is
    // painted after (outside) the `ScrollArea` so the scroll viewport's clip
    // does not cut it short. An out-of-range `active` leaves the line unbroken,
    // exactly like the bar.
    let visuals = ui.visuals().clone();
    let accent = egui::Stroke::new(TAB_ACCENT_H, visuals.hyperlink_color);
    let (open_left, open_right) = rects.get(active).map_or((pane_x.min, pane_x.min), |rect| {
        (
            (rect.left() - f32::from(TAB_RADIUS)).max(pane_x.min),
            (rect.right() + f32::from(TAB_RADIUS)).min(pane_x.max),
        )
    });
    let y = row.bottom() + TAB_ACCENT_H / 2.0;
    if pane_x.min < open_left {
        ui.painter()
            .hline(egui::Rangef::new(pane_x.min, open_left), y, accent);
    }
    if open_right < pane_x.max {
        ui.painter()
            .hline(egui::Rangef::new(open_right, pane_x.max), y, accent);
    }

    activate
}

/// Content width of one switch tab from its label width: the label plus its two
/// label paddings and the one small flare reserve — the editor bar's
/// `tab_width` without its close zone and without a second corner-radius
/// reserve.
fn switch_width(label_width: f32) -> f32 {
    TAB_PAD_X * 2.0 + label_width + SWITCH_INSET
}

/// Left-to-right tab rects inside `row`, starting after the small flare inset so
/// the active tab's flared feet stay inside the allocated rect.
fn switch_rects(row: Rect, widths: &[f32]) -> Vec<Rect> {
    let mut x = row.min.x + SWITCH_INSET;
    widths
        .iter()
        .map(|width| {
            let rect = Rect::from_min_size(egui::pos2(x, row.min.y), Vec2::new(*width, TAB_HEIGHT));
            x += width + TAB_GAP;
            rect
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_width_is_compact_with_no_close_zone_or_double_radius() {
        let width = switch_width(10.0);
        assert_eq!(width, TAB_PAD_X * 2.0 + 10.0 + SWITCH_INSET);
        assert!(
            SWITCH_INSET <= f32::from(TAB_RADIUS),
            "the flare reserve must be at most one radius"
        );
        assert!(
            width < TAB_PAD_X * 2.0 + 10.0 + f32::from(TAB_RADIUS) * 2.0,
            "a second corner-radius reserve would read as a close-button blank"
        );
    }

    #[test]
    fn switch_rects_place_tabs_left_to_right_after_a_small_inset() {
        let row = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(200.0, TAB_HEIGHT));
        let rects = switch_rects(row, &[30.0, 40.0]);

        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].min.x, SWITCH_INSET);
        assert_eq!(rects[0].width(), 30.0);
        assert_eq!(rects[0].height(), TAB_HEIGHT);
        assert_eq!(rects[1].min.x, SWITCH_INSET + 30.0 + TAB_GAP);
        assert_eq!(rects[1].width(), 40.0);
    }

    /// One `tab_switch` frame with `events`, returning what it resolved.
    fn frame(
        ctx: &egui::Context,
        labels: &[&str],
        active: usize,
        events: Vec<egui::Event>,
    ) -> Option<usize> {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 200.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = None;
        ctx.run_ui(raw, |ui| {
            out = Some(tab_switch(ui, "tab-switch-test", labels, active));
        })
        .drop_without_applying_deltas();
        out.expect("a frame output")
    }

    /// Presses and releases at `pos`, returning the release frame's resolution.
    fn click(
        ctx: &egui::Context,
        labels: &[&str],
        active: usize,
        pos: egui::Pos2,
    ) -> Option<usize> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        // A warm move makes the tab hovered before the press is attributed.
        frame(ctx, labels, active, vec![egui::Event::PointerMoved(pos)]);
        frame(
            ctx,
            labels,
            active,
            vec![egui::Event::PointerMoved(pos), button(true)],
        );
        frame(ctx, labels, active, vec![button(false)])
    }

    /// One `tab_switch` frame inside a pane-body-like child of a host that has
    /// already reserved the whole pane with a hover sense and later registers a
    /// full-area click sibling — mirroring the real left-pane context:
    /// `src/shell/layout.rs`'s full-pane hover plus `src/shell/tree/mod.rs`'s
    /// background click rect registered after the switcher.
    fn nested_frame(
        ctx: &egui::Context,
        labels: &[&str],
        active: usize,
        events: Vec<egui::Event>,
    ) -> Option<usize> {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 200.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = None;
        ctx.run_ui(raw, |ui| {
            let full = ui.available_rect_before_wrap();
            // tui-whitelist: 测试夹具，在普通 Ui 上模拟宿主预留/覆盖命中区，非 tui 树节点。
            let _ = ui.allocate_rect(full, egui::Sense::hover());
            ui.scope_builder(
                egui::UiBuilder::new().id_salt("pane-body").max_rect(full),
                |ui| {
                    out = Some(tab_switch(ui, "tab-switch-nested", labels, active));
                    let _ = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("tree-bg"),
                        egui::Sense::click(),
                    );
                },
            );
        })
        .drop_without_applying_deltas();
        out.expect("a frame output")
    }

    fn nested_click(
        ctx: &egui::Context,
        labels: &[&str],
        active: usize,
        pos: egui::Pos2,
    ) -> Option<usize> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        nested_frame(ctx, labels, active, vec![egui::Event::PointerMoved(pos)]);
        nested_frame(
            ctx,
            labels,
            active,
            vec![egui::Event::PointerMoved(pos), button(true)],
        );
        nested_frame(ctx, labels, active, vec![button(false)])
    }

    /// Width of the narrow pane the overflow test runs in; well under the four
    /// CJK labels' content width so the strip overflows and the bar appears.
    const NARROW_W: f32 = 120.0;

    /// One `tab_switch` frame inside a narrow (left-pane-width) child, returning
    /// the scroll offset the widget persisted afterwards (0.0 when absent).
    fn narrow_frame(
        ctx: &egui::Context,
        labels: &[&str],
        active: usize,
        events: Vec<egui::Event>,
    ) -> f32 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 200.0),
            )),
            events,
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            let pane = egui::Rect::from_min_size(ui.min_rect().min, egui::vec2(NARROW_W, 160.0));
            // tui-whitelist: 测试夹具，模拟窄面板宿主，非 tui 树节点。
            ui.scope_builder(
                egui::UiBuilder::new().id_salt("pane-narrow").max_rect(pane),
                |ui| {
                    let _ = tab_switch(ui, "tab-switch-overflow", labels, active);
                },
            );
        })
        .drop_without_applying_deltas();
        ctx.memory(|m| {
            m.data
                .get_temp::<f32>(egui::Id::new("tab-switch-overflow").with("scroll"))
                .unwrap_or(0.0)
        })
    }

    #[test]
    fn overflowing_switcher_scrolls_with_horizontal_wheel_and_clamps() {
        let ctx = egui::Context::default();
        let labels = ["项目", "搜索", "大纲", "历史"];
        // Over the strip: below the reserved scrollbar row and inside the tab
        // row / the scroll viewport.
        let strip = egui::pos2(10.0, 12.0);

        // Warm the pointer inside the narrow pane so the overflow bar appears.
        let start = narrow_frame(&ctx, &labels, 0, vec![egui::Event::PointerMoved(strip)]);
        assert_eq!(start, 0.0, "a fresh overflowing strip starts at offset 0");

        // A seeded offset far past the content is clamped back into range on the
        // next frame; a positive clamp proves the labels overflow the pane.
        ctx.memory_mut(|m| {
            m.data.insert_temp(
                egui::Id::new("tab-switch-overflow").with("scroll"),
                10_000.0_f32,
            )
        });
        let max_scroll = narrow_frame(&ctx, &labels, 0, vec![egui::Event::PointerMoved(strip)]);
        assert!(
            max_scroll > 0.0,
            "four CJK labels overflow a {NARROW_W}px pane, so max_scroll > 0"
        );
        assert!(
            max_scroll < 10_000.0,
            "the seeded offset is clamped to max_scroll, not left huge"
        );

        // From 0, a horizontal wheel over the strip scrolls it; the offset grows
        // and never exceeds the clamp.
        ctx.memory_mut(|m| {
            m.data
                .insert_temp(egui::Id::new("tab-switch-overflow").with("scroll"), 0.0_f32)
        });
        let wheel = || egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-30.0, 0.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        };
        let mut offset = 0.0;
        for _ in 0..40 {
            offset = narrow_frame(
                &ctx,
                &labels,
                0,
                vec![egui::Event::PointerMoved(strip), wheel()],
            );
        }
        assert!(
            offset > 0.0,
            "the horizontal wheel scrolls the overflowing strip: {offset}"
        );
        assert!(
            offset <= max_scroll + 0.5,
            "the offset stays clamped at max_scroll ({offset} vs {max_scroll})"
        );
    }

    #[test]
    fn tab_switch_returns_none_for_empty_labels() {
        let ctx = egui::Context::default();
        assert_eq!(frame(&ctx, &[], 0, vec![]), None);
    }

    #[test]
    fn tab_switch_click_on_non_active_returns_its_index() {
        let ctx = egui::Context::default();
        let pos = egui::pos2(SWITCH_INSET + 1.0, TAB_HEIGHT * 0.5);
        assert_eq!(click(&ctx, &["项目", "搜索"], 1, pos), Some(0));
    }

    #[test]
    fn tab_switch_click_on_active_returns_none() {
        let ctx = egui::Context::default();
        let pos = egui::pos2(SWITCH_INSET + 1.0, TAB_HEIGHT * 0.5);
        assert_eq!(click(&ctx, &["项目", "搜索"], 0, pos), None);
    }

    #[test]
    fn tab_switch_click_under_full_pane_hover_and_later_tree_background() {
        let ctx = egui::Context::default();
        let pos = egui::pos2(SWITCH_INSET + 1.0, TAB_HEIGHT * 0.5);
        assert_eq!(nested_click(&ctx, &["项目", "搜索"], 1, pos), Some(0));
    }
}
