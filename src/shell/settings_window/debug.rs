//! Debug category: frame-tree preview renderer toggle and displayed info.

use typ_edit_i18n::t;
use typ_edit_widgets::checkbox::checkbox;

use super::grid::{ResetPage, page_reset_ui, settings_grid};

/// Debug category: the frame-tree renderer toggle plus which per-node details
/// the tree displays. The display checkboxes are greyed while the feature is off.
pub(super) fn debug_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    let enabled = settings.debug.frame_tree.enabled;
    settings_grid(ui, "typ_edit_settings_grid_debug", |table| {
        table.row_note(
            &t("settings.debug.enable"),
            &t("settings.debug.enable_hint"),
            |ui| {
                changed |= checkbox(
                    ui,
                    &mut settings.debug.frame_tree.enabled,
                    &t("settings.editor.enable"),
                    true,
                )
                .changed();
            },
        );

        table.row(&t("settings.debug.types"), |_| {});

        table.row(&t("settings.debug.group_nodes"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.debug.frame_tree.show_group_nodes,
                "",
                enabled,
            )
            .changed();
        });

        table.row(&t("settings.debug.text_nodes"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.debug.frame_tree.show_text_nodes,
                "",
                enabled,
            )
            .changed();
        });

        table.row(&t("settings.debug.shape_nodes"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.debug.frame_tree.show_shape_nodes,
                "",
                enabled,
            )
            .changed();
        });

        table.row(&t("settings.debug.image_nodes"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.debug.frame_tree.show_image_nodes,
                "",
                enabled,
            )
            .changed();
        });

        table.row(&t("settings.debug.link_nodes"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.debug.frame_tree.show_link_nodes,
                "",
                enabled,
            )
            .changed();
        });

        table.row(&t("settings.debug.tag_nodes"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.debug.frame_tree.show_tag_nodes,
                "",
                enabled,
            )
            .changed();
        });

        table.row(&t("settings.debug.display"), |_| {});

        table.row(&t("settings.debug.show_children"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.debug.frame_tree.show_children,
                "",
                enabled,
            )
            .changed();
        });

        table.row(&t("settings.debug.show_text"), |ui| {
            changed |=
                checkbox(ui, &mut settings.debug.frame_tree.show_text, "", enabled).changed();
        });

        table.row(&t("settings.debug.show_geometry"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.debug.frame_tree.show_geometry,
                "",
                enabled,
            )
            .changed();
        });

        table.row(&t("settings.debug.show_style"), |ui| {
            changed |=
                checkbox(ui, &mut settings.debug.frame_tree.show_style, "", enabled).changed();
        });

        table.row(&t("settings.debug.show_span"), |ui| {
            changed |=
                checkbox(ui, &mut settings.debug.frame_tree.show_span, "", enabled).changed();
        });

        table.row(&t("settings.debug.highlight_color"), |ui| {
            let [r, g, b, a] = settings.debug.frame_tree.highlight_color;
            let mut color = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
            ui.add_enabled_ui(enabled, |ui| {
                if ui.color_edit_button_srgba(&mut color).changed() {
                    settings.debug.frame_tree.highlight_color = color.to_srgba_unmultiplied();
                    changed = true;
                }
            });
        });
    });
    changed |= page_reset_ui(ui, "debug", settings, ResetPage::Debug);
    changed
}
