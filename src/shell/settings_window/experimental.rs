//! Experimental category: GPU canvas preview rendering.

use typ_edit_i18n::t;
use typ_edit_widgets::checkbox::checkbox;
use typ_edit_widgets::slider::slider;

use super::grid::{ResetPage, page_reset_ui, settings_grid};

/// Experimental category: GPU canvas preview rendering.
pub(super) fn experimental_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_experimental", |table| {
        table.row_note(
            &t("settings.experimental.gpu"),
            &t("settings.experimental.hint"),
            |ui| {
                changed |= checkbox(
                    ui,
                    &mut settings.preview.gpu_canvas_preview,
                    &t("settings.editor.enable"),
                    true,
                )
                .changed();
            },
        );
        table.row_note(
            &t("settings.experimental.image_max_side"),
            &t("settings.experimental.image_max_side_hint"),
            |ui| {
                changed |= slider(
                    ui,
                    egui::Slider::new(
                        &mut settings.preview.gpu_image_max_side,
                        crate::settings::GPU_IMAGE_MAX_SIDE_MIN
                            ..=crate::settings::GPU_IMAGE_MAX_SIDE_MAX,
                    )
                    .logarithmic(true)
                    .integer()
                    .suffix(" px"),
                )
                .changed();
            },
        );
    });
    changed |= page_reset_ui(ui, "experimental", settings, ResetPage::Experimental);
    changed
}
