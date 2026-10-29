//! List rows: a selectable option row, an independent toggle chip and a
//! wrapped, clickable error row.
//!
//! See `docs/design/widgets-redesign.md` §2.6 and §2.12.

use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Response, Sense, Stroke, StrokeKind, TextStyle, Ui,
    Vec2,
};

use crate::theme::Theme;

/// Draws one full-width list option row (for example a font-family entry).
///
/// The selected row is marked by a 2 px accent ring and strong text; an
/// unselected row only shows a translucent hover fill.
pub fn option_row(ui: &mut Ui, selected: bool, label: &str) -> Response {
    let theme = Theme::from_ui(ui);
    let font = TextStyle::Body.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    // tui-whitelist: 单叶 allocate_exact_size（列表行整行 + 命中区），文字自绘。
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), theme.spacing.list_row_h),
        Sense::click(),
    );
    let radius = CornerRadius::same(theme.radii.control);
    let hovered = response.hovered();

    let painter = ui.painter();
    if selected {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(theme.strokes.accent, theme.colors.accent),
            StrokeKind::Inside,
        );
    } else if hovered {
        painter.rect_filled(
            rect,
            radius,
            theme.colors.widget_hover_fill.gamma_multiply(0.5),
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(theme.strokes.accent, theme.colors.accent),
            StrokeKind::Inside,
        );
    }
    let color = if selected || hovered {
        theme.colors.text_strong
    } else {
        theme.colors.text
    };
    painter.galley(
        Pos2::new(
            rect.left() + theme.spacing.menu_pad_x,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        color,
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

/// Draws an independent toggle chip (for example a DPI preset or 跟随).
///
/// The selected state is a `band` fill with a 2 px accent ring; hover and press
/// use translucent widget fills. The width is the label plus
/// `spacing.control_pad_x` on either side.
pub fn toggle_chip(ui: &mut Ui, selected: bool, label: &str) -> Response {
    let theme = Theme::from_ui(ui);
    let font = TextStyle::Body.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let width = galley.size().x + 2.0 * theme.spacing.control_pad_x;
    // tui-whitelist: 单叶 allocate_exact_size（chip 自身体积 + 命中区）。
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, theme.spacing.list_row_h), Sense::click());
    let radius = CornerRadius::same(theme.radii.small);
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    let focused = response.has_focus();

    let fill = if selected {
        theme.colors.band
    } else if pressed {
        theme.colors.widget_active_fill.gamma_multiply(0.5)
    } else if hovered {
        theme.colors.widget_hover_fill.gamma_multiply(0.5)
    } else {
        Color32::TRANSPARENT
    };
    let stroke = if selected || focused || pressed {
        Stroke::new(theme.strokes.accent, theme.colors.accent)
    } else if hovered {
        Stroke::new(theme.strokes.card, theme.colors.widget_hover_stroke)
    } else {
        Stroke::new(theme.strokes.card, theme.colors.widget_stroke)
    };
    let color = if selected || focused || hovered || pressed {
        theme.colors.text_strong
    } else {
        theme.colors.text
    };

    let painter = ui.painter();
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, radius, fill);
    }
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    painter.galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        color,
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

/// Draws a wrapped, clickable list row for diagnostics, tinted `color`.
///
/// `hint` is an optional plain-language explanation appended on a second line
/// in the muted text color. The text wraps at `available_width - 8`; the row
/// shows a faint hover fill, a focus ring and the pointing-hand cursor.
pub fn error_row(ui: &mut Ui, text: &str, hint: Option<&str>, color: Color32) -> Response {
    let theme = Theme::from_ui(ui);
    let font = TextStyle::Body.resolve(ui.style());
    let width = ui.available_width().max(1.0);
    let wrap_width = (width - 8.0).max(1.0);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: font.clone(),
            color,
            ..Default::default()
        },
    );
    if let Some(hint) = hint {
        job.append(
            "\n",
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
        job.append(
            hint,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(font.size * 0.85),
                color: theme.colors.text_muted,
                ..Default::default()
            },
        );
    }
    let galley = ui.painter().layout_job(job);
    // tui-whitelist: 单叶 allocate_exact_size（错误行体积 + 命中区），换行由 galley 负责。
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, galley.size().y + 6.0), Sense::click());
    let radius = CornerRadius::same(theme.radii.small);
    let hovered = response.hovered();

    let painter = ui.painter();
    if hovered {
        painter.rect_filled(
            rect,
            radius,
            theme.colors.widget_hover_fill.gamma_multiply(0.35),
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(theme.strokes.accent, theme.colors.accent),
            StrokeKind::Inside,
        );
    }
    painter.galley(
        Pos2::new(rect.left() + 4.0, rect.top() + 3.0),
        galley,
        color,
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}
