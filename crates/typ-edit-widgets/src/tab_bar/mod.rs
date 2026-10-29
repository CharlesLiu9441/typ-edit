//! Generalized tab-bar widget with two orientations (see
//! [`TabBarOrientation`]): a compact, horizontally scrollable strip of
//! browser-style tabs with dirty markers, close buttons, a context menu and
//! drag-to-reorder, or a vertical stack of presentation-only tabs (label,
//! hover fill and activation).
//!
//! The widget is presentation-only: it paints the tabs from a slice of
//! [`TabItem`] and returns at most one [`TabAction`] for the caller to apply.
//! The UI strings are Chinese (the app ships first in Chinese), so the
//! context-menu labels and the tooltips stay verbatim. The context menu, close
//! buttons, dirty markers, drag-to-reorder and scrolling exist in the
//! horizontal orientation only.
//!
//! Every egui id the horizontal widget touches is derived from the
//! caller-supplied `id_salt`, so the salt must be unique per tab-bar instance
//! and stable across frames. In particular a [`TabAction::Reorder`] must be
//! applied by the caller in the very same frame (see [`tab_bar`]). The vertical
//! strip keeps no cross-frame state and derives its ids from the response it
//! allocates, so the camera-relative `id_salt` only matters when the horizontal
//! orientation is used.
//!
//! The implementation is split across four private submodules: [`geometry`]
//! (silhouette/outline geometry, tab-width computation and reorder math),
//! [`paint`] (the shared silhouette and accent painting paths), [`context`]
//! (the right-click context menu and the actions it produces) and [`switcher`]
//! (the presentation-only [`tab_switch`] view switcher).

mod context;
mod geometry;
mod paint;
mod switcher;

use std::sync::Arc;

use crate::theme::Theme;
use context::TabContext;

/// Height of one tab row, in points.
const TAB_HEIGHT: f32 = 28.0;
/// Horizontal gap between two neighbouring tabs.
const TAB_GAP: f32 = 4.0;
/// Horizontal padding on each side of a tab's label.
const TAB_PAD_X: f32 = 10.0;
/// Width of the reserved close-button zone at a tab's trailing edge.
const TAB_CLOSE_W: f32 = Theme::LIGHT.spacing.tab_close_w;
/// Thickness of the active tab's accent line.
const TAB_ACCENT_H: f32 = 2.0;
/// Radius of a tab's convex top corners and of its outward (concave) bottom
/// fillets; see docs/design/ui-cards.md §0.3/§0.6.
pub(crate) const TAB_RADIUS: u8 = 8;
/// Upper bound of a single tab; longer names are elided with `…`.
const TAB_MAX_WIDTH: f32 = 180.0;
/// Text budget left for the label once the padding, the close zone and the two
/// corner-radius reserves `tab_width` adds are taken out, so `tab_width(TAB_LABEL_MAX)`
/// lands exactly on `TAB_MAX_WIDTH`.
const TAB_LABEL_MAX: f32 = TAB_MAX_WIDTH - TAB_PAD_X - TAB_CLOSE_W - TAB_RADIUS as f32 * 2.0;
/// Height of the reserved row above the tab strip carrying the shared
/// scrollbar widget; the row is only allocated while the tabs overflow.
pub(crate) const TAB_SCROLLBAR_H: f32 = 8.0;
/// Chords each rounded corner of the tab silhouette is sampled into.
const TAB_ARC_STEPS: usize = 7;

/// One tab as seen by the widget: a precomputed label, an optional file path and
/// the unsaved-changes flag.
#[derive(Clone, Debug)]
pub struct TabItem {
    /// Display label, precomputed by the caller (may include parent-dir
    /// disambiguation).
    pub label: String,
    /// File path as a display string; shown as hover tooltip and enables the
    /// 复制路径 / 在资源管理器中显示 menu items. `None` for unnamed buffers.
    pub path: Option<String>,
    /// Whether the buffer has unsaved changes (paints a dot while dirty,
    /// including while the close affordance is hovered).
    pub dirty: bool,
}

/// A single interaction the widget resolved this frame; the caller applies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAction {
    /// Make `index` the active tab.
    Activate(usize),
    /// Close `index`.
    Close(usize),
    /// Close every tab but `index`.
    CloseOthers(usize),
    /// Close every tab.
    CloseAll,
    /// Copy tab `index`'s file path to the clipboard.
    CopyPath(usize),
    /// Reveal tab `index`'s file in the system file manager.
    RevealInExplorer(usize),
    /// Move the tab at `from` into slot `to`.
    Reorder { from: usize, to: usize },
}

/// Orientation of a tab strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBarOrientation {
    /// Tabs laid out left to right; the strip opens onto the content below it.
    Horizontal,
    /// Tabs stacked top to bottom; the strip opens onto the content on its right.
    Vertical,
}

/// Draws the tab strip and returns at most one resolved action.
///
/// `orientation` picks the strip's layout:
/// [`TabBarOrientation::Horizontal`] paints the full browser-style bar, while
/// [`TabBarOrientation::Vertical`] paints a presentation-only stack of label
/// rows that resolves nothing but [`TabAction::Activate`].
///
/// `id_salt` seeds every egui id the horizontal widget uses and must be unique
/// per instance and stable across frames. `active` may be out of range (an
/// empty or short strip); in that case no tab is painted as active. When `tabs`
/// is empty the widget returns `None` immediately.
///
/// A returned [`TabAction::Reorder`] must be applied by the caller in the same
/// frame: the widget has already committed the new slot to its drag memory, so
/// painting and hit-testing on the next frame expect the caller's tab order to
/// match.
pub fn tab_bar(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    tabs: &[TabItem],
    active: usize,
    orientation: TabBarOrientation,
) -> Option<TabAction> {
    if tabs.is_empty() {
        return None;
    }

    match orientation {
        // Vertical tabs are presentation-only: hand off before any horizontal
        // bookkeeping (scroll memory, drag memory, context menu) is touched.
        TabBarOrientation::Vertical => return tab_bar_vertical(ui, tabs, active),
        TabBarOrientation::Horizontal => {}
    }

    let base = egui::Id::new(id_salt);
    let drag_id = base.with("drag");
    let context_id = base.with("context");
    let mut rects: Vec<egui::Rect> = Vec::with_capacity(tabs.len());
    let mut activate = None;
    let mut close = None;
    let mut middle_close = None;
    let mut menu = TabContext::default();
    let mut drag_started_source = None;
    let mut drag_x = None;

    // The strip's full width (not just the tabs' content width): the scroll
    // area itself may shrink-wrap to a few narrow tabs.
    let strip_x = ui.available_rect_before_wrap().x_range();
    let visuals = ui.visuals().clone();
    let theme = Theme::from_ui(ui);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galleys: Vec<Arc<egui::Galley>> = tabs
        .iter()
        .map(|tab| {
            let mut job = egui::text::LayoutJob::simple_singleline(
                tab.label.clone(),
                font.clone(),
                egui::Color32::PLACEHOLDER,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(TAB_LABEL_MAX);
            ui.painter().layout_job(job)
        })
        .collect();
    let widths: Vec<f32> = galleys
        .iter()
        .map(|galley| geometry::tab_width(galley.size().x))
        .collect();
    let total = geometry::strip_width(&widths);

    let avail_w = ui.available_width();
    let max_scroll = (total - avail_w).max(0.0);
    let scroll_id = base.with("scroll");
    let mut scroll = ui
        .ctx()
        .memory(|m| m.data.get_temp::<f32>(scroll_id))
        .unwrap_or(0.0)
        .clamp(0.0, max_scroll);

    // Reserved row above the tabs holding the shared scrollbar widget; only
    // allocated while the tabs overflow, so fitting tabs keep no dead band.
    // tui-whitelist: 单叶 allocate_exact_size（滚动条行），滑块为纯绘制。
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

    // tui-whitelist: ScrollArea 是允许的容器机制；其中整条 tab 行是一个自绘叶，
    // 轮廓/标签/关闭区全部按矩形绘制与命中，不构成 tui 子元素排布。
    let out = egui::ScrollArea::horizontal()
        .id_salt(base.with("area"))
        .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .horizontal_scroll_offset(scroll)
        .show(ui, |ui| {
            // tui-whitelist: 单叶 allocate_exact_size（整条 tab 行 + 拖拽命中区）。
            let (row_rect, response) = ui
                .allocate_exact_size(egui::vec2(total, TAB_HEIGHT), egui::Sense::click_and_drag());

            let mut x = row_rect.min.x + (TAB_RADIUS as f32);
            for width in &widths {
                rects.push(egui::Rect::from_min_size(
                    egui::pos2(x, row_rect.min.y),
                    egui::vec2(*width, TAB_HEIGHT),
                ));
                x += width + TAB_GAP;
            }

            let hover_pos = ui.input(|i| i.pointer.hover_pos());
            let hovered =
                hover_pos.and_then(|pos| rects.iter().position(|rect| rect.contains(pos)));
            let dragged = response.dragged();
            let drag_index = ui.ctx().memory(|m| m.data.get_temp::<usize>(drag_id));
            let painter = ui.painter();

            // Inactive silhouettes first, the active one last: its flared
            // feet may overlap the 2px gap and the neighbours' bottom
            // corners, exactly like a browser tab bar.
            for (index, rect) in rects.iter().enumerate() {
                if index == active {
                    continue;
                }
                let tab_hovered = hovered == Some(index) && !dragged;
                if tab_hovered || (dragged && drag_index == Some(index)) {
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
                    TAB_ACCENT_H,
                );
            }

            for (index, (rect, tab)) in rects.iter().zip(tabs).enumerate() {
                let is_active = index == active;
                let tab_hovered = hovered == Some(index) && !dragged;
                let color = if is_active {
                    visuals.widgets.active.fg_stroke.color
                } else if tab_hovered {
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

                let zone = geometry::close_zone(*rect);
                let zone_hovered = hover_pos.is_some_and(|pos| zone.contains(pos)) && !dragged;
                if zone_hovered {
                    painter.rect_filled(
                        zone.shrink2(egui::vec2(0.0, (TAB_HEIGHT - TAB_CLOSE_W) * 0.5)),
                        egui::CornerRadius::same(4),
                        visuals.widgets.active.bg_fill,
                    );
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                // The dirty dot stays visible even while the close zone is
                // hovered: it must survive the moment the user decides to
                // close the tab. The close glyph then appears beside it.
                if tab.dirty {
                    painter.circle_filled(
                        egui::pos2(zone.left() - 6.0, zone.center().y),
                        2.5,
                        visuals.text_color(),
                    );
                }
                if tab_hovered || is_active {
                    painter.text(
                        zone.center(),
                        egui::Align2::CENTER_CENTER,
                        "×",
                        font.clone(),
                        visuals.text_color(),
                    );
                }
                if response.has_focus() && is_active {
                    painter.rect_stroke(
                        *rect,
                        egui::CornerRadius::same(theme.radii.control),
                        egui::Stroke::new(theme.strokes.accent, theme.colors.accent),
                        egui::StrokeKind::Inside,
                    );
                }

                if !dragged {
                    // tui-whitelist: 命中区 interact（tab 的 hover 提示）。
                    let hover = ui.interact(
                        *rect,
                        response.id.with(("tab-tooltip", index)),
                        egui::Sense::hover(),
                    );
                    if let Some(path) = &tab.path {
                        hover.on_hover_text(path.clone());
                    }
                }
            }

            if response.clicked()
                && !dragged
                && let Some(pos) = response.interact_pointer_pos()
                && let Some(index) = rects.iter().position(|rect| rect.contains(pos))
            {
                if geometry::close_zone(rects[index]).contains(pos) {
                    close = Some(index);
                } else if index != active {
                    activate = Some(index);
                }
            }
            if response.clicked_by(egui::PointerButton::Middle)
                && !dragged
                && let Some(pos) = response.interact_pointer_pos()
                && let Some(index) = rects.iter().position(|rect| rect.contains(pos))
            {
                middle_close = Some(index);
            }
            // Row-level keyboard model: the whole strip is one focus stop, so
            // Left/Right moves the active tab and Delete closes it. This keeps
            // tabs operable without a pointer while preserving the painted
            // browser-tab silhouette.
            if response.has_focus() && active < tabs.len() {
                let mut step = 0isize;
                let mut close_active = false;
                ui.input_mut(|input| {
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                        step -= 1;
                    }
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                        step += 1;
                    }
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::Delete) {
                        close_active = true;
                    }
                });
                if close_active {
                    close = Some(active);
                } else if step != 0 {
                    let next = (active as isize + step).rem_euclid(tabs.len() as isize) as usize;
                    if next != active {
                        activate = Some(next);
                    }
                }
            }
            context::record_target(ui, &response, context_id, &rects);
            menu = context::context_menu(&response, context_id, tabs);
            if response.drag_started()
                && let Some(origin) = ui.input(|i| i.pointer.press_origin())
                && let Some(index) = rects.iter().position(|rect| rect.contains(origin))
                && !geometry::close_zone(rects[index]).contains(origin)
            {
                drag_started_source = Some(index);
            }
            if dragged {
                drag_x = ui.input(|i| i.pointer.interact_pos()).map(|pos| pos.x);
            }
        });
    scroll = out.state.offset.x.clamp(0.0, max_scroll);
    ui.ctx()
        .memory_mut(|m| m.data.insert_temp(scroll_id, scroll));

    if let Some(source) = drag_started_source {
        ui.ctx().memory_mut(|m| m.data.insert_temp(drag_id, source));
    }
    // The drag's target slot, committed to memory here (after painting) so the
    // highlight lands correctly on the next frame; the caller applies it now.
    let mut reorder = None;
    if let Some(pointer_x) = drag_x {
        if let Some(from) = ui.ctx().memory(|m| m.data.get_temp::<usize>(drag_id)) {
            let to = geometry::reorder_index(&rects, pointer_x);
            if to != from {
                ui.ctx().memory_mut(|m| m.data.insert_temp(drag_id, to));
                reorder = Some(TabAction::Reorder { from, to });
            }
        }
    } else if drag_started_source.is_none() {
        ui.ctx()
            .memory_mut(|m| m.data.remove_temp::<usize>(drag_id));
    }

    // The accent frame spans the strip's full width, opened under the active
    // tab by one flare radius per side, so its "∩" outline and these bottom
    // segments read as one continuous accent frame around the editing area.
    let accent = egui::Stroke::new(TAB_ACCENT_H, ui.visuals().hyperlink_color);
    if let Some(first) = rects.first() {
        let (open_left, open_right) =
            rects
                .get(active)
                .map_or((strip_x.min, strip_x.min), |rect| {
                    (
                        (rect.left() - f32::from(TAB_RADIUS)).max(strip_x.min),
                        (rect.right() + f32::from(TAB_RADIUS)).min(strip_x.max),
                    )
                });
        if strip_x.min < open_left {
            ui.painter().hline(
                egui::Rangef::new(strip_x.min, open_left),
                first.bottom() + TAB_ACCENT_H / 2.0,
                accent,
            );
        }
        if open_right < strip_x.max {
            ui.painter().hline(
                egui::Rangef::new(open_right, strip_x.max),
                first.bottom() + TAB_ACCENT_H / 2.0,
                accent,
            );
        }
    }

    // At most one action per frame, resolved in the original dispatch order.
    // A drag and a click are mutually exclusive in egui, so `reorder` can be
    // the fallback without ever competing with a click action.
    if let Some(index) = close {
        Some(TabAction::Close(index))
    } else if let Some(index) = activate {
        Some(TabAction::Activate(index))
    } else if let Some(index) = middle_close {
        Some(TabAction::Close(index))
    } else if let Some(action) = menu.action() {
        Some(action)
    } else {
        reorder
    }
}

/// Vertical tab strip: presentation tabs stacked top to bottom, one per row.
///
/// The strip spans the UI's full available width; every row carries the label
/// at its left and the active accent on the row's right (open) edge. Only the
/// label, the hover fill and activation exist here, so the returned action is
/// always `Some(TabAction::Activate(index))` for the row under a click. The
/// leading and trailing `TAB_RADIUS` reserves mirror the horizontal strip's
/// start offset so the active tab's flared feet stay inside the allocated rect.
fn tab_bar_vertical(ui: &mut egui::Ui, tabs: &[TabItem], active: usize) -> Option<TabAction> {
    tab_bar_vertical_with(ui, tabs, active, TAB_ACCENT_H)
}

/// Presentation-only vertical tabs used by nested settings sub-pages. The
/// accent thickness matches the category rail so the two read as one system.
pub fn vertical_tabs_thin(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    labels: &[&str],
    active: usize,
) -> Option<usize> {
    if labels.is_empty() {
        return None;
    }
    let items: Vec<TabItem> = labels
        .iter()
        .map(|label| TabItem {
            label: (*label).to_owned(),
            path: None,
            dirty: false,
        })
        .collect();
    ui.push_id(id_salt, |ui| {
        tab_bar_vertical_with(ui, &items, active, TAB_ACCENT_H)
    })
    .inner
    .and_then(|action| match action {
        TabAction::Activate(index) => Some(index),
        _ => None,
    })
}

fn tab_bar_vertical_with(
    ui: &mut egui::Ui,
    tabs: &[TabItem],
    active: usize,
    accent_h: f32,
) -> Option<TabAction> {
    let radius = f32::from(TAB_RADIUS);

    // Both captured before the allocation: the width sizes every row and the
    // y-range lets the accent frame span the strip's full height, mirroring the
    // horizontal `strip_x` capture.
    let width = ui.available_width();
    let strip_y = ui.available_rect_before_wrap().y_range();
    let total_h = 2.0 * radius + tabs.len() as f32 * TAB_HEIGHT + (tabs.len() - 1) as f32 * TAB_GAP;
    // tui-whitelist: 单叶 allocate_exact_size（整条纵向 tab 轨道 + 命中区）。
    let (row, response) = ui.allocate_exact_size(egui::vec2(width, total_h), egui::Sense::click());

    let visuals = ui.visuals().clone();
    let theme = Theme::from_ui(ui);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galleys: Vec<Arc<egui::Galley>> = tabs
        .iter()
        .map(|tab| {
            let mut job = egui::text::LayoutJob::simple_singleline(
                tab.label.clone(),
                font.clone(),
                egui::Color32::PLACEHOLDER,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(width - TAB_PAD_X * 2.0);
            ui.painter().layout_job(job)
        })
        .collect();
    let rects: Vec<egui::Rect> = (0..tabs.len())
        .map(|index| {
            egui::Rect::from_min_size(
                egui::pos2(
                    row.min.x,
                    row.min.y + radius + index as f32 * (TAB_HEIGHT + TAB_GAP),
                ),
                egui::vec2(width, TAB_HEIGHT),
            )
        })
        .collect();

    let hover_pos = ui.input(|i| i.pointer.hover_pos());
    let hovered = hover_pos.and_then(|pos| rects.iter().position(|rect| rect.contains(pos)));
    let painter = ui.painter();

    // Inactive silhouettes first, the active one last, exactly like the
    // horizontal strip.
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
                TabBarOrientation::Vertical,
            );
        }
    }
    if let Some(rect) = rects.get(active) {
        paint::paint_active_accent(
            painter,
            *rect,
            visuals.hyperlink_color,
            TabBarOrientation::Vertical,
            accent_h,
        );
    }

    for (index, (rect, tab)) in rects.iter().zip(tabs).enumerate() {
        let is_active = index == active;
        let tab_hovered = hovered == Some(index);
        let color = if is_active {
            visuals.widgets.active.fg_stroke.color
        } else if tab_hovered {
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

        // tui-whitelist: 命中区 interact（纵向 tab 的 hover 提示）。
        let hover = ui.interact(
            *rect,
            response.id.with(("tab-tooltip", index)),
            egui::Sense::hover(),
        );
        if let Some(path) = &tab.path {
            hover.on_hover_text(path.clone());
        }
    }
    if response.has_focus()
        && let Some(rect) = rects.get(active)
    {
        painter.rect_stroke(
            *rect,
            egui::CornerRadius::same(theme.radii.control),
            egui::Stroke::new(theme.strokes.accent, theme.colors.accent),
            egui::StrokeKind::Inside,
        );
    }

    // The accent frame spans the strip's full height, opened beside the active
    // tab by one flare radius per side, so its "⊂" outline and these right-edge
    // segments read as one continuous accent frame around the content area.
    let accent = egui::Stroke::new(accent_h, ui.visuals().hyperlink_color);
    let x = row.right() + accent_h / 2.0;
    let (open_top, open_bottom) = rects
        .get(active)
        .map_or((strip_y.min, strip_y.min), |rect| {
            (
                (rect.top() - radius).max(strip_y.min),
                (rect.bottom() + radius).min(strip_y.max),
            )
        });
    if strip_y.min < open_top {
        ui.painter()
            .vline(x, egui::Rangef::new(strip_y.min, open_top), accent);
    }
    if open_bottom < strip_y.max {
        ui.painter()
            .vline(x, egui::Rangef::new(open_bottom, strip_y.max), accent);
    }

    if response.has_focus() && active < tabs.len() {
        let mut step = 0isize;
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                step -= 1;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                step += 1;
            }
        });
        if step != 0 {
            let next = (active as isize + step).rem_euclid(tabs.len() as isize) as usize;
            if next != active {
                return Some(TabAction::Activate(next));
            }
        }
    }

    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(index) = rects.iter().position(|rect| rect.contains(pos))
    {
        return Some(TabAction::Activate(index));
    }
    None
}

pub use geometry::{move_item, moved_index};
pub use switcher::tab_switch;
