//! Menu rows: full-width items with an optional right-aligned shortcut, a
//! checkable item and a hairline separator.
//!
//! These are presentation helpers for the egui menu shells
//! (`ui.menu_button`, `Response::context_menu`, `egui::Popup::menu`); the
//! open/close, keyboard and focus plumbing stays with egui. Each row returns
//! its [`Response`] so tooltips stay chainable.
//!
//! tui-whitelist: a menu row is one painted leaf (`allocate_exact_size`); the
//! label/shortcut/check cell are painted into that rect, not laid out as child
//! widgets. The popup itself is an egui container mechanism, so no tui() is
//! opened here (the P4 menu popups intentionally keep egui's shell).

use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Rect, Response, Sense, Stroke, StrokeKind, TextStyle,
    Ui, Vec2,
};

use crate::separator::SEPARATOR_PAD;
use crate::theme::Theme;

/// Side of the leading cell reserved for a check mark inside a menu row.
const CHECK_CELL: f32 = 16.0;

/// Gap between a label and its right-aligned shortcut.
const SHORTCUT_GAP: f32 = 8.0;

/// The interaction state driving a menu row's colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowState {
    Normal,
    Hovered,
    Pressed,
}

/// Narrowest popup a content-sized menu shrinks to.
const SIZED_MENU_MIN_W: f32 = 60.0;
/// Widest popup a content-sized menu grows to.
const SIZED_MENU_MAX_W: f32 = 460.0;

/// Sizes the popup to `rows` instead of a fixed width. Each row is
/// `(label, optional shortcut)`; pass `has_check = true` when the popup renders
/// [`menu_check_item`]s so the leading check cell is reserved on every row. For
/// a menu with conditional rows, pass the union of every row that can appear so
/// the width stays stable across frames. This is the entry point for all menus
/// (the unused fixed-width `menu_begin` fallback was removed in P6).
pub fn menu_begin_sized(ui: &mut Ui, rows: &[(&str, Option<&str>)], has_check: bool) {
    let screen = ui.ctx().content_rect();
    block_background(
        ui.ctx(),
        ui.id().with("menu_backdrop"),
        screen,
        ui.layer_id(),
    );
    let theme = Theme::from_ui(ui);
    let font = TextStyle::Body.resolve(ui.style());
    let measured: Vec<(f32, Option<f32>)> = rows
        .iter()
        .map(|(label, shortcut)| {
            (
                measure_text(ui, label, &font),
                shortcut.map(|shortcut| measure_text(ui, shortcut, &font)),
            )
        })
        .collect();
    let width = sized_menu_width(&measured, has_check, theme.spacing.menu_pad_x);
    ui.set_width(width);
}

/// Puts an invisible full-screen layer between the page and an open popup.
///
/// A popup's own area only covers its content, so hovering beside a menu would
/// otherwise reach the page underneath. `Order::Middle` is above page content
/// but below foreground popups, so menu rows keep their hover and clicks. The
/// caller's own overlay layer becomes the modal barrier; when it closes on a
/// click, lower foreground layers (not just the page) are blocked as well.
pub fn block_background(
    ctx: &egui::Context,
    id: egui::Id,
    screen: egui::Rect,
    layer: egui::LayerId,
) {
    egui::Area::new(id)
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let _ = ui.allocate_exact_size(screen.size(), egui::Sense::click());
        });
    // Register the overlay's own layer as the barrier. Using the backdrop's
    // Middle layer would be ignored when the overlay itself sits above another
    // foreground modal; the popup layer is guaranteed to be above both.
    ctx.memory_mut(|memory| memory.set_modal_layer(layer));
}

/// The popup width for `rows` (each `(label width, shortcut width)`): the widest
/// row — both paddings, the optional check cell, the label, and the optional
/// shortcut with its gap — clamped to the content-sized floor and cap.
fn sized_menu_width(rows: &[(f32, Option<f32>)], has_check: bool, pad_x: f32) -> f32 {
    let check = if has_check { CHECK_CELL } else { 0.0 };
    rows.iter()
        .map(|(label_w, shortcut_w)| {
            let shortcut = shortcut_w.map_or(0.0, |width| SHORTCUT_GAP + width);
            2.0 * pad_x + check + label_w + shortcut
        })
        .fold(0.0, f32::max)
        .clamp(SIZED_MENU_MIN_W, SIZED_MENU_MAX_W)
}

/// Lays out one line of `text` and returns its width in points.
fn measure_text(ui: &Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER)
        .size()
        .x
}

/// Draws one full-width menu item with an optional trailing shortcut.
///
/// `shortcut` is painted right-aligned; the label is truncated (never wrapped)
/// to the space left before it. `enabled == false` paints the disabled state
/// and returns a non-clickable response.
pub fn menu_item(ui: &mut Ui, label: &str, shortcut: Option<&str>, enabled: bool) -> Response {
    let can_interact = enabled && ui.is_enabled();
    ui.add_enabled_ui(enabled, |ui| {
        let theme = Theme::from_ui(ui);
        let pad_x = theme.spacing.menu_pad_x;
        let width = ui.available_width();
        let font = TextStyle::Body.resolve(ui.style());

        let shortcut_galley = shortcut.map(|shortcut| {
            ui.painter()
                .layout_no_wrap(shortcut.to_owned(), font.clone(), Color32::PLACEHOLDER)
        });
        let shortcut_w = shortcut_galley
            .as_ref()
            .map_or(0.0, |galley| galley.size().x);
        let label_max = (width
            - 2.0 * pad_x
            - shortcut_w
            - if shortcut.is_some() {
                SHORTCUT_GAP
            } else {
                0.0
            })
        .max(0.0);
        let label_galley = truncated_label(ui, label, font, label_max);

        // tui-whitelist: 单叶 allocate_exact_size（菜单行整行 + 命中区），标签/快捷键自绘。
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, theme.spacing.menu_row_h), Sense::click());
        let state = row_state(can_interact, &response);
        let (fill, text, shortcut_text) = match state {
            None => (
                Color32::TRANSPARENT,
                theme.colors.text_weak,
                theme.colors.text_weak,
            ),
            Some(RowState::Pressed) => (
                theme.colors.widget_active_fill,
                theme.colors.widget_active_text,
                theme.colors.widget_active_text,
            ),
            Some(RowState::Hovered) => (
                theme.colors.widget_hover_fill,
                theme.colors.text_strong,
                theme.colors.text,
            ),
            Some(RowState::Normal) => (
                Color32::TRANSPARENT,
                theme.colors.text,
                theme.colors.text_muted,
            ),
        };

        let radius = CornerRadius::same(theme.radii.control);
        let painter = ui.painter();
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect, radius, fill);
        }
        if state.is_some() && (response.has_focus() || state == Some(RowState::Pressed)) {
            let width = if response.has_focus() {
                theme.strokes.accent
            } else {
                theme.strokes.card
            };
            painter.rect_stroke(
                rect,
                radius,
                Stroke::new(width, theme.colors.accent),
                StrokeKind::Inside,
            );
        }
        painter.galley(
            Pos2::new(
                rect.left() + pad_x,
                rect.center().y - label_galley.size().y * 0.5,
            ),
            label_galley,
            text,
        );
        if let Some(galley) = shortcut_galley {
            painter.galley(
                Pos2::new(
                    rect.right() - pad_x - galley.size().x,
                    rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                shortcut_text,
            );
        }
        if state.is_some() && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        response
    })
    .inner
}

/// Draws a full-width checkable menu item; toggles `checked` on click.
///
/// The check mark occupies a leading [`CHECK_CELL`]-wide cell and is painted in
/// the accent color (weak text when disabled).
pub fn menu_check_item(ui: &mut Ui, checked: &mut bool, label: &str, enabled: bool) -> Response {
    let can_interact = enabled && ui.is_enabled();
    ui.add_enabled_ui(enabled, |ui| {
        let theme = Theme::from_ui(ui);
        let pad_x = theme.spacing.menu_pad_x;
        let width = ui.available_width();
        let font = TextStyle::Body.resolve(ui.style());
        let label_max = (width - 2.0 * pad_x - CHECK_CELL).max(0.0);
        let label_galley = truncated_label(ui, label, font, label_max);

        // tui-whitelist: 单叶 allocate_exact_size（可勾选菜单行 + 命中区），勾/标签自绘。
        let (rect, mut response) =
            ui.allocate_exact_size(Vec2::new(width, theme.spacing.menu_row_h), Sense::click());
        if can_interact && response.clicked() {
            *checked = !*checked;
            response.mark_changed();
        }
        let state = row_state(can_interact, &response);
        let (fill, text) = match state {
            None => (Color32::TRANSPARENT, theme.colors.text_weak),
            Some(RowState::Pressed) => (
                theme.colors.widget_active_fill,
                theme.colors.widget_active_text,
            ),
            Some(RowState::Hovered) => (theme.colors.widget_hover_fill, theme.colors.text_strong),
            Some(RowState::Normal) => (Color32::TRANSPARENT, theme.colors.text),
        };
        let check_color = if state.is_some() {
            theme.colors.accent
        } else {
            theme.colors.text_weak
        };

        let radius = CornerRadius::same(theme.radii.control);
        let painter = ui.painter();
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect, radius, fill);
        }
        if state.is_some() && (response.has_focus() || state == Some(RowState::Pressed)) {
            let width = if response.has_focus() {
                theme.strokes.accent
            } else {
                theme.strokes.card
            };
            painter.rect_stroke(
                rect,
                radius,
                Stroke::new(width, theme.colors.accent),
                StrokeKind::Inside,
            );
        }
        if *checked {
            let cell = Rect::from_min_size(
                Pos2::new(rect.left() + pad_x, rect.center().y - CHECK_CELL * 0.5),
                Vec2::splat(CHECK_CELL),
            );
            crate::icons::check(ui, cell, check_color);
        }
        painter.galley(
            Pos2::new(
                rect.left() + pad_x + CHECK_CELL,
                rect.center().y - label_galley.size().y * 0.5,
            ),
            label_galley,
            text,
        );
        if state.is_some() && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        response
    })
    .inner
}

/// Draws a full-width hairline menu separator: 4 pt, the line, 4 pt.
pub fn menu_separator(ui: &mut Ui) {
    let theme = Theme::from_ui(ui);
    let line = theme.strokes.hairline;
    // tui-whitelist: 单叶 allocate_exact_size（菜单分隔条体积），线为纯绘制。
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 2.0 * SEPARATOR_PAD + line),
        Sense::hover(),
    );
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(line, theme.colors.hairline),
    );
}

/// Lays out `label` on a single line, truncating it to `max_width`.
fn truncated_label(
    ui: &Ui,
    label: &str,
    font: egui::FontId,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job =
        egui::text::LayoutJob::simple_singleline(label.to_owned(), font, Color32::PLACEHOLDER);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    ui.painter().layout_job(job)
}

/// Maps a row response to a state, or `None` while disabled.
fn row_state(can_interact: bool, response: &Response) -> Option<RowState> {
    if !can_interact {
        None
    } else if response.is_pointer_button_down_on() {
        Some(RowState::Pressed)
    } else if response.hovered() {
        Some(RowState::Hovered)
    } else {
        Some(RowState::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_row_click_does_not_reach_a_lower_full_screen_sensor() {
        use std::cell::Cell;

        let ctx = egui::Context::default();
        let bg_clicked = Cell::new(false);
        let item_clicked = Cell::new(false);
        let item_rect = Cell::new(egui::Rect::NOTHING);
        let run = |events: Vec<egui::Event>| {
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0));
            let mut raw = egui::RawInput {
                screen_rect: Some(rect),
                events,
                ..Default::default()
            };
            raw.time = Some(1.0);
            if let Some(info) = raw.viewports.get_mut(&egui::ViewportId::ROOT) {
                info.inner_rect = Some(rect);
                info.outer_rect = Some(rect);
                info.focused = Some(true);
            }
            ctx.run_ui(raw, |ui| {
                // Arm the lower sensor only after the popup has rendered once,
                // so the opening click cannot be confused with the row click.
                if item_rect.get().is_positive() {
                    let bg = ui.interact(rect, egui::Id::new("test_bg"), egui::Sense::click());
                    bg_clicked.set(bg_clicked.get() || bg.clicked());
                }
                let open = ui.put(
                    egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(120.0, 28.0)),
                    egui::Button::new("open"),
                );
                egui::Popup::menu(&open).show(|ui| {
                    menu_begin_sized(ui, &[("item", None)], false);
                    let response = menu_item(ui, "item", None, true);
                    item_rect.set(response.rect);
                    if response.clicked() {
                        item_clicked.set(true);
                        ui.close();
                    }
                });
            })
            .drop_without_applying_deltas();
        };
        let open_pos = egui::pos2(80.0, 34.0);
        let click = |pos: egui::Pos2| {
            run(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                run(vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        };
        click(open_pos);
        run(Vec::new());
        let item = item_rect.get();
        assert!(item.is_positive(), "the popup must render a row");
        assert_eq!(
            ctx.memory(|memory| memory.top_modal_layer().map(|layer| layer.order)),
            Some(egui::Order::Foreground),
            "the popup layer, not the Middle backdrop, must be the modal barrier"
        );
        bg_clicked.set(false);
        click(item.center());
        assert!(item_clicked.get(), "the menu row handles its click");
        assert!(
            !bg_clicked.get(),
            "the lower full-screen sensor must not consume the menu-row click"
        );
    }

    #[test]
    fn menu_row_click_does_not_reach_a_lower_foreground_sensor() {
        use std::cell::Cell;

        let ctx = egui::Context::default();
        let sensor_clicked = Cell::new(false);
        let item_clicked = Cell::new(false);
        let item_rect = Cell::new(egui::Rect::NOTHING);
        let frame = Cell::new(0usize);
        let run = |events: Vec<egui::Event>| {
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0));
            let mut raw = egui::RawInput {
                screen_rect: Some(rect),
                events,
                ..Default::default()
            };
            raw.time = Some(3.0);
            if let Some(info) = raw.viewports.get_mut(&egui::ViewportId::ROOT) {
                info.inner_rect = Some(rect);
                info.outer_rect = Some(rect);
                info.focused = Some(true);
            }
            ctx.run_ui(raw, |ui| {
                egui::Area::new(egui::Id::new("lower_foreground"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(egui::Pos2::ZERO)
                    .show(ui.ctx(), |ui| {
                        let sensor = ui.interact(
                            rect,
                            egui::Id::new("lower_foreground_sensor"),
                            egui::Sense::click(),
                        );
                        sensor_clicked.set(sensor_clicked.get() || sensor.clicked());
                    });
                let open = ui.put(
                    egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(120.0, 28.0)),
                    egui::Button::new("open"),
                );
                let popup = egui::Popup::menu(&open);
                let popup = if frame.get() == 0 {
                    popup.open_memory(Some(egui::containers::SetOpenCommand::Bool(true)))
                } else {
                    popup
                };
                popup.show(|ui| {
                    menu_begin_sized(ui, &[("item", None)], false);
                    let response = menu_item(ui, "item", None, true);
                    item_rect.set(response.rect);
                    if response.clicked() {
                        item_clicked.set(true);
                        ui.close();
                    }
                });
                frame.set(frame.get() + 1);
            })
            .drop_without_applying_deltas();
        };
        let click = |pos: egui::Pos2| {
            run(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                run(vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        };
        run(Vec::new());
        let item = item_rect.get();
        assert!(item.is_positive(), "the popup must render a row");
        sensor_clicked.set(false);
        click(item.center());
        assert!(item_clicked.get(), "the menu row handles its click");
        assert!(
            !sensor_clicked.get(),
            "the foreground sensor behind the menu must not consume its click"
        );
    }

    #[test]
    fn sized_menu_width_tracks_the_widest_row() {
        let pad = 8.0;
        assert_eq!(
            sized_menu_width(&[(20.0, None), (30.0, None)], false, pad),
            SIZED_MENU_MIN_W
        );
        assert_eq!(
            sized_menu_width(&[(200.0, Some(40.0))], false, pad),
            2.0 * pad + 200.0 + SHORTCUT_GAP + 40.0
        );
        assert_eq!(
            sized_menu_width(&[(100.0, None)], true, pad),
            2.0 * pad + CHECK_CELL + 100.0
        );
        assert_eq!(
            sized_menu_width(&[(10_000.0, None)], false, pad),
            SIZED_MENU_MAX_W
        );
    }
}
