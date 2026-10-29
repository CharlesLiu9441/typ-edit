//! Shared interaction-state color helpers for the hand-drawn widgets.

use egui::Color32;

/// The hover shade of `selection`: brighter on the dark theme, dimmer on the
/// light one (§3.5).
pub(crate) fn selection_hover(selection: Color32, dark: bool) -> Color32 {
    selection.gamma_multiply(if dark { 1.15 } else { 0.92 })
}
