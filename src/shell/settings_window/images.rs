//! Image import sub-page: the paste-image directory.

use typ_edit_i18n::t;
use typ_edit_widgets::text_input::form_input;

use super::grid::{ResetPage, page_reset_ui, settings_grid};

/// Image import sub-page: the paste-image directory.
pub(super) fn images_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_images", |table| {
        table.row_note(
            &t("settings.images.dir"),
            &t("settings.images.hint"),
            |ui| {
                changed |=
                    form_input(ui, &mut settings.workspace.image_dir, "image", 240.0).changed();
            },
        );
    });
    changed |= page_reset_ui(ui, "editor_images", settings, ResetPage::EditorImages);
    changed
}
