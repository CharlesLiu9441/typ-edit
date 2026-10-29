//! Width-adaptive wrapper for [`egui::Slider`].
//!
//! tui-whitelist: stock egui widget; `ui.scope` only patches its style.
//!
//! egui sizes the slider rail from `Spacing::slider_width`, which defaults to a
//! fixed 100 pt and ignores the parent's width. The wrapper stretches the rail
//! to fill the space left after reserving room for the numeric value display
//! egui lays out after the rail.

use egui::{Response, Slider, Ui};

/// Fixed rail width for settings sliders.
///
/// The rail must not follow `ui.available_width()`: switching between Chinese
/// and English changes the tab rail/content width, and an adaptive slider would
/// absorb that difference and slowly widen the settings modal.
pub const FIXED_RAIL_WIDTH: f32 = 200.0;

/// Adds `slider` with its rail at a stable width and its filled part painted in
/// the accent color.
pub fn slider(ui: &mut Ui, slider: Slider<'_>) -> Response {
    let theme = crate::theme::Theme::from_ui(ui);
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = FIXED_RAIL_WIDTH;
        ui.visuals_mut().slider_trailing_fill = true;
        ui.visuals_mut().selection.bg_fill = theme.colors.accent;
        ui.visuals_mut().selection.stroke =
            egui::Stroke::new(theme.strokes.accent, theme.colors.accent);
        ui.add(slider)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::FIXED_RAIL_WIDTH;

    #[test]
    fn settings_slider_rail_is_fixed() {
        assert_eq!(FIXED_RAIL_WIDTH, 200.0);
    }
}
