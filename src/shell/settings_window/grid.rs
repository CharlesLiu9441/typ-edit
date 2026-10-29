//! Shared settings-grid, separator, font-warning and page-reset helpers for the category panes.
//!
//! The striped-grid primitives now live in [`typ_edit_widgets::striped_grid`];
//! this module re-exports the tui table under the names the settings panes
//! already use and keeps the settings-specific warnings, page reset and
//! hairline.

use typ_edit_engine::fonts::FontCatalog;
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::theme::Theme;

pub(super) use typ_edit_widgets::striped_grid::tui_grid as settings_grid;

/// Which settings page's defaults a reset button restores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResetPage {
    InterfaceGeneral,
    InterfaceFonts,
    InterfaceIcons,
    EditorEditing,
    EditorFonts,
    EditorImages,
    PreviewGeneral,
    PreviewCursor,
    Export,
    Git,
    Collab,
    Experimental,
    Debug,
}

impl ResetPage {
    fn apply(self, settings: &mut crate::settings::Settings) {
        let defaults = crate::settings::Settings::default();
        match self {
            Self::InterfaceGeneral => {
                settings.appearance = defaults.appearance;
                settings.language = defaults.language;
                settings.workspace.recent_projects_enabled =
                    defaults.workspace.recent_projects_enabled;
                settings.workspace.recent_projects_limit = defaults.workspace.recent_projects_limit;
            }
            Self::InterfaceFonts => {
                settings.fonts.ui_font = defaults.fonts.ui_font;
                settings.fonts.ui_font_size = defaults.fonts.ui_font_size;
            }
            Self::InterfaceIcons => {
                settings.tree.folder_icon_filled = defaults.tree.folder_icon_filled;
                settings.tree.folder_icon_color = defaults.tree.folder_icon_color;
            }
            Self::EditorEditing => {
                settings.editor.completion_max = defaults.editor.completion_max;
                settings.editor.wrap = defaults.editor.wrap;
                settings.editor.line_spacing = defaults.editor.line_spacing;
                settings.editor.wrap_line_spacing = defaults.editor.wrap_line_spacing;
                settings.editor.format_on_save = defaults.editor.format_on_save;
                settings.editor.spell_check_enabled = defaults.editor.spell_check_enabled;
                settings.recovery.crash_recovery_enabled = defaults.recovery.crash_recovery_enabled;
            }
            Self::EditorFonts => {
                settings.fonts.editor_font = defaults.fonts.editor_font;
                settings.fonts.editor_font_size = defaults.fonts.editor_font_size;
            }
            Self::EditorImages => {
                settings.workspace.image_dir = defaults.workspace.image_dir;
            }
            Self::PreviewGeneral => {
                settings.preview.preview_dpi = defaults.preview.preview_dpi;
            }
            Self::PreviewCursor => {
                settings.preview.cursor_width = defaults.preview.cursor_width;
                settings.preview.cursor_style = defaults.preview.cursor_style;
                settings.preview.cursor_blink = defaults.preview.cursor_blink;
                settings.preview.cursor_blink_interval = defaults.preview.cursor_blink_interval;
                settings.preview.selection_color = defaults.preview.selection_color;
                settings.preview.selection_opacity = defaults.preview.selection_opacity;
            }
            Self::Export => settings.export.png_dpi = defaults.export.png_dpi,
            Self::Git => {
                settings.git.git_enabled = defaults.git.git_enabled;
                settings.git.git_blame_inline = defaults.git.git_blame_inline;
            }
            Self::Collab => {
                settings.collab.max_connections = defaults.collab.max_connections;
                settings.collab.idle_timeout_secs = defaults.collab.idle_timeout_secs;
            }
            Self::Experimental => {
                settings.preview.gpu_canvas_preview = defaults.preview.gpu_canvas_preview;
                settings.preview.gpu_image_max_side = defaults.preview.gpu_image_max_side;
            }
            Self::Debug => settings.debug = defaults.debug,
        }
    }
}

/// Draws a full-width 1px hairline separator inside the card.
pub(super) fn hairline(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, crate::theme::hairline(ui)),
    );
}

/// Inline warning when a requested family is not installed (a fallback is used instead).
pub(super) fn missing_font_warning(ui: &mut egui::Ui, family: &str, catalog: &FontCatalog) {
    if !catalog.has_family(family) {
        ui.colored_label(
            Theme::from_ui(ui).colors.warn_text,
            t_with("settings.missing_font", &[("family", family)]),
        );
    }
}

/// Inline-confirm reset for one settings page, aligned to the page's bottom-right.
pub(super) fn page_reset_ui(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    settings: &mut crate::settings::Settings,
    page: ResetPage,
) -> bool {
    let id = egui::Id::new(("typ_edit_settings_page_reset", id_salt));
    let mut confirming: bool = ui.ctx().data(|data| data.get_temp(id)).unwrap_or(false);
    let mut changed = false;
    ui.add_space(6.0);
    hairline(ui);
    ui.add_space(6.0);
    let width = ui.available_width();
    let row_h = ui.spacing().interact_size.y + 4.0;
    ui.allocate_ui_with_layout(
        egui::vec2(width, row_h),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            if confirming {
                if button(ui, &t("settings.ok"), ButtonVariant::Primary, true).clicked() {
                    page.apply(settings);
                    changed = true;
                    confirming = false;
                }
                if button(ui, &t("settings.cancel"), ButtonVariant::Secondary, true).clicked() {
                    confirming = false;
                }
                ui.label(t("settings.reset.page_confirm"));
            } else if button(
                ui,
                &t("settings.reset.page_button"),
                ButtonVariant::Quiet,
                true,
            )
            .clicked()
            {
                confirming = true;
            }
        },
    );
    ui.ctx().data_mut(|data| {
        data.insert_temp(id, confirming);
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::ResetPage;
    use crate::settings::{Settings, ThemePref};

    #[test]
    fn interface_page_reset_does_not_touch_editor_fonts() {
        let mut settings = Settings::default();
        settings.appearance.theme = ThemePref::Dark;
        settings.appearance.ui_scale = 1.8;
        settings.fonts.editor_font_size = 29.0;
        ResetPage::InterfaceGeneral.apply(&mut settings);
        assert_eq!(settings.appearance.theme, ThemePref::System);
        assert_eq!(settings.appearance.ui_scale, 1.0);
        assert_eq!(settings.fonts.editor_font_size, 29.0);
    }

    #[test]
    fn experimental_reset_restores_image_side_limit() {
        let mut settings = Settings::default();
        settings.preview.gpu_image_max_side = 8192;
        ResetPage::Experimental.apply(&mut settings);
        assert_eq!(
            settings.preview.gpu_image_max_side,
            crate::settings::GPU_IMAGE_MAX_SIDE_DEFAULT
        );
    }

    #[test]
    fn preview_cursor_reset_does_not_touch_render_dpi() {
        let mut settings = Settings::default();
        settings.preview.preview_dpi = 300.0;
        settings.preview.cursor_width = 4.0;
        settings.preview.cursor_blink = false;
        ResetPage::PreviewCursor.apply(&mut settings);
        assert_eq!(settings.preview.preview_dpi, 300.0);
        assert_eq!(settings.preview.cursor_width, 1.5);
        assert!(!settings.preview.cursor_blink);
    }
}
