//! Collaboration category: the limits of a session this machine hosts.

use egui::DragValue;
use typ_edit_i18n::t;

use super::grid::{ResetPage, page_reset_ui, settings_grid};

pub(super) fn collab_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_collab", |table| {
        table.row_note(
            &t("settings.collab.max_connections"),
            &t("settings.collab.hint"),
            |ui| {
                changed |= ui
                    .add(
                        DragValue::new(&mut settings.collab.max_connections)
                            .range(0..=64usize)
                            .speed(1.0),
                    )
                    .changed();
            },
        );

        table.row(&t("settings.collab.idle_timeout"), |ui| {
            changed |= ui
                .add(
                    DragValue::new(&mut settings.collab.idle_timeout_secs)
                        .range(0..=600u64)
                        .speed(1.0)
                        .suffix(" s"),
                )
                .changed();
        });
    });
    changed |= page_reset_ui(ui, "collab", settings, ResetPage::Collab);
    changed
}
