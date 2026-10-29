//! A full-width hairline separator: 4 pt, a 1 px [`Theme`] hairline, 4 pt.

use egui::{Sense, Stroke, Ui, Vec2};

use crate::theme::Theme;

/// Vertical breathing room above and below a separator line, in points.
pub(crate) const SEPARATOR_PAD: f32 = 4.0;

/// Draws a full-width hairline separator with 4 pt of space on either side.
pub fn separator(ui: &mut Ui) {
    let theme = Theme::from_ui(ui);
    let line = theme.strokes.hairline;
    // tui-whitelist: 单叶 allocate_exact_size（分隔条体积），线为纯绘制。
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
