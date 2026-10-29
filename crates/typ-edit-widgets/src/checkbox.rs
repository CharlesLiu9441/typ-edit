//! A checkbox: a square box with an optional check mark and a trailing label.
//!
//! Presentation-only apart from the toggle: a click anywhere on the row flips
//! `checked`. Keyboard Space/Enter activation is inherited from
//! `Sense::click()` and [`Response::clicked`].

use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Rect, Response, Sense, Stroke, StrokeKind, TextStyle,
    Ui, Vec2,
};

use crate::theme::Theme;

/// Draws a checkbox row and toggles `checked` when clicked.
///
/// The whole row is the click target; `enabled == false` paints the disabled
/// state and returns a non-clickable response.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str, enabled: bool) -> Response {
    let can_interact = enabled && ui.is_enabled();
    ui.add_enabled_ui(enabled, |ui| {
        let theme = Theme::from_ui(ui);
        let box_size = theme.spacing.checkbox_box;
        let gap = theme.spacing.checkbox_gap;
        let font = TextStyle::Body.resolve(ui.style());
        let label_galley =
            ui.painter()
                .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);

        let size = Vec2::new(
            box_size + gap + label_galley.size().x,
            theme.spacing.list_row_h,
        );
        // tui-whitelist: 单叶 allocate_exact_size（复选框整行 + 命中区），方框/标签均自绘。
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
        if can_interact && response.clicked() {
            *checked = !*checked;
            response.mark_changed();
        }

        let hovered = can_interact && response.hovered();
        let pressed = can_interact && response.is_pointer_button_down_on();
        let (box_fill, box_stroke, check_color, label_color) = if !can_interact {
            (
                theme.colors.band,
                Stroke::new(
                    theme.strokes.card,
                    theme.colors.widget_stroke.gamma_multiply(0.5),
                ),
                theme.colors.text_weak,
                theme.colors.text_weak,
            )
        } else {
            // The box is a line-type control: a neutral fill plus the default
            // border at rest, a grayscale hover, and an accent outline when
            // checked or actively pressed. No solid accent surface is used.
            let fill = if hovered || pressed {
                theme.colors.widget_hover_fill
            } else {
                theme.colors.band
            };
            let stroke = if *checked {
                Stroke::new(theme.strokes.accent, theme.colors.accent)
            } else if pressed {
                Stroke::new(theme.strokes.card, theme.colors.accent)
            } else if hovered {
                Stroke::new(theme.strokes.card, theme.colors.widget_hover_stroke)
            } else {
                Stroke::new(theme.strokes.card, theme.colors.widget_stroke)
            };
            (
                fill,
                stroke,
                theme.colors.accent,
                if hovered || pressed || *checked {
                    theme.colors.text_strong
                } else {
                    theme.colors.text
                },
            )
        };

        let box_rect = Rect::from_min_size(
            Pos2::new(rect.left(), rect.center().y - box_size * 0.5),
            Vec2::splat(box_size),
        );
        let radius = CornerRadius::same(theme.radii.small);
        let painter = ui.painter();
        painter.rect_filled(box_rect, radius, box_fill);
        painter.rect_stroke(box_rect, radius, box_stroke, StrokeKind::Inside);
        if *checked {
            crate::icons::check(ui, box_rect, check_color);
        }
        if can_interact && response.has_focus() {
            painter.rect_stroke(
                box_rect,
                radius,
                Stroke::new(theme.strokes.accent, theme.colors.accent),
                StrokeKind::Inside,
            );
        }
        painter.galley(
            Pos2::new(
                rect.left() + box_size + gap,
                rect.center().y - label_galley.size().y * 0.5,
            ),
            label_galley,
            label_color,
        );
        if hovered {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        response
    })
    .inner
}
