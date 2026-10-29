//! Export category: raster export resolution.

use egui::DragValue;
use typ_edit_i18n::t;

use super::grid::{ResetPage, page_reset_ui, settings_grid};

/// Export category: raster export resolution.
pub(super) fn export_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_export", |table| {
        table.row_note(
            &t("settings.export.dpi"),
            &t("settings.export.hint"),
            |ui| {
                changed |= ui
                    .add(
                        DragValue::new(&mut settings.export.png_dpi)
                            .range(36.0..=1200.0)
                            .speed(1.0)
                            .suffix(" dpi"),
                    )
                    .changed();
            },
        );
    });
    changed |= page_reset_ui(ui, "export", settings, ResetPage::Export);
    changed
}
