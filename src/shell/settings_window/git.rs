//! Git category: master integration toggle and inline blame.

use typ_edit_i18n::t;
use typ_edit_widgets::checkbox::checkbox;

use super::grid::{ResetPage, page_reset_ui, settings_grid};

/// Git category: master integration toggle and inline blame.
pub(super) fn git_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_git", |table| {
        table.row_note(&t("settings.git.enabled"), &t("settings.git.hint"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.git.git_enabled,
                &t("settings.editor.enable"),
                true,
            )
            .changed();
        });

        let enabled = settings.git.git_enabled;
        table.row(&t("settings.git.blame_inline"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.git.git_blame_inline,
                &t("settings.editor.enable"),
                enabled,
            )
            .changed();
        });
    });
    changed |= page_reset_ui(ui, "git", settings, ResetPage::Git);
    changed
}
