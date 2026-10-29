//! Icon style sub-page: folder icon style, color source and preview.

use egui::{Color32, Sense, Vec2};
use typ_edit_i18n::t;
use typ_edit_widgets::checkbox::checkbox;
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::TuiBuilderLogic;

use super::grid::{ResetPage, page_reset_ui, settings_grid};

/// Explicit folder-icon color applied when "follow accent color" is switched off and no
/// previous explicit color is remembered.
const DEFAULT_FOLDER_ICON_COLOR: [u8; 4] = [120, 170, 255, 255];

/// Last explicit folder-icon color, remembered while "follow accent color" is checked so
/// unchecking it later restores the previous color instead of a default.
fn folder_color_id() -> egui::Id {
    egui::Id::new("typ_edit_settings_folder_color")
}

/// Icon style sub-page: folder icon style, color source and preview.
pub(super) fn icons_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_icons", |table| {
        // Style selector plus its live preview: two side-by-side controls.
        table.row_tui(&t("settings.icons.style"), |tree| {
            tree.ui(|ui| {
                let outline = t("settings.icons.style.outline");
                let filled = t("settings.icons.style.filled");
                changed |= typ_edit_widgets::segmented::segmented(
                    ui,
                    &[(false, outline.as_ref()), (true, filled.as_ref())],
                    &mut settings.tree.folder_icon_filled,
                );
            });
            tree.ui(|ui| {
                // Live preview of the current style/color combination.
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::splat(typ_edit_widgets::icons::ICON_PX),
                    Sense::hover(),
                );
                let color = settings
                    .tree
                    .folder_icon_color
                    .map(|[r, g, b, a]| Color32::from_rgba_unmultiplied(r, g, b, a))
                    .unwrap_or_else(|| Theme::from_ui(ui).colors.accent);
                typ_edit_widgets::icons::folder(
                    ui,
                    rect,
                    false,
                    settings.tree.folder_icon_filled,
                    color,
                );
            });
        });

        // Follow-accent checkbox plus the optional explicit color button.
        table.row_tui("", |tree| {
            tree.ui(|ui| {
                let mut follow_accent = settings.tree.folder_icon_color.is_none();
                if checkbox(
                    ui,
                    &mut follow_accent,
                    &t("settings.icons.follow_accent"),
                    true,
                )
                .changed()
                {
                    if follow_accent {
                        // Remember the explicit color so unchecking restores it later.
                        if let Some(rgba) = settings.tree.folder_icon_color.take() {
                            ui.ctx()
                                .data_mut(|data| data.insert_temp(folder_color_id(), rgba));
                        }
                    } else {
                        let cached = ui.ctx().data(|data| data.get_temp(folder_color_id()));
                        settings.tree.folder_icon_color =
                            Some(cached.unwrap_or(DEFAULT_FOLDER_ICON_COLOR));
                    }
                    changed = true;
                }
            });

            if settings.tree.folder_icon_color.is_some() {
                tree.ui(|ui| {
                    if let Some(rgba) = &mut settings.tree.folder_icon_color {
                        let mut color =
                            Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                        if ui.color_edit_button_srgba(&mut color).changed() {
                            *rgba = color.to_array();
                            // Keep the memory cache in sync so a follow-accent round trip is lossless.
                            ui.ctx()
                                .data_mut(|data| data.insert_temp(folder_color_id(), *rgba));
                            changed = true;
                        }
                    }
                });
            }
        });
    });
    changed |= page_reset_ui(ui, "interface_icons", settings, ResetPage::InterfaceIcons);
    changed
}
