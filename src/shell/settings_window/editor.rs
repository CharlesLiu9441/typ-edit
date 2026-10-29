//! Editor category: editing behaviour, editor fonts, and image import sub-pages.

use egui::DragValue;
use typ_edit_engine::fonts::FontCatalog;
use typ_edit_i18n::t;
use typ_edit_widgets::checkbox::checkbox;
use typ_edit_widgets::editable_combo::editable_number;
use typ_edit_widgets::search_combo::search_combo;
use typ_edit_widgets::slider::slider;

use crate::settings::WrapPref;

use super::grid::{ResetPage, missing_font_warning, page_reset_ui, settings_grid};
use super::images::images_ui;
use super::subtabs::sub_tabs;

const EDITOR_FONT_SIZES: &[f32] = &[
    10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 22.0, 24.0, 28.0, 32.0,
];

/// Editor category: editing behaviour, editor font and image import pages.
pub(super) fn editor_ui(
    ui: &mut egui::Ui,
    settings: &mut crate::settings::Settings,
    catalog: &FontCatalog,
) -> bool {
    sub_tabs(
        ui,
        "editor",
        &[
            "settings.subtabs.editing",
            "settings.subtabs.fonts",
            "settings.subtabs.images",
        ],
        |ui, page| match page {
            0 => editing_ui(ui, settings),
            1 => editor_fonts_ui(ui, settings, catalog),
            2 => images_ui(ui, settings),
            _ => false,
        },
    )
}

fn editing_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_editor", |table| {
        table.row(&t("settings.editor.completion_max"), |ui| {
            changed |= ui
                .add(
                    DragValue::new(&mut settings.editor.completion_max)
                        .range(5..=500)
                        .speed(1.0),
                )
                .changed();
        });

        table.row(&t("settings.editor.wrap"), |ui| {
            let off = t("settings.editor.wrap.off");
            let always = t("settings.editor.wrap.always");
            let markup = t("settings.editor.wrap.markup");
            changed |= typ_edit_widgets::segmented::segmented(
                ui,
                &[
                    (WrapPref::Off, off.as_ref()),
                    (WrapPref::Always, always.as_ref()),
                    (WrapPref::Markup, markup.as_ref()),
                ],
                &mut settings.editor.wrap,
            );
        });

        table.row(&t("settings.editor.line_spacing"), |ui| {
            changed |= slider(
                ui,
                egui::Slider::new(&mut settings.editor.line_spacing, 0.5..=3.0)
                    .step_by(0.05)
                    .suffix("×"),
            )
            .changed();
        });

        table.row(&t("settings.editor.wrap_line_spacing"), |ui| {
            changed |= slider(
                ui,
                egui::Slider::new(&mut settings.editor.wrap_line_spacing, 0.5..=3.0)
                    .step_by(0.05)
                    .suffix("×"),
            )
            .changed();
        });

        table.row("", |ui| {
            changed |= checkbox(
                ui,
                &mut settings.editor.format_on_save,
                &t("settings.editor.format_on_save"),
                true,
            )
            .changed();
        });

        table.row(&t("settings.editor.crash_recovery"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.recovery.crash_recovery_enabled,
                &t("settings.editor.enable"),
                true,
            )
            .changed();
        });

        table.row_note("", &t("settings.editor.spell_check_hint"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.editor.spell_check_enabled,
                &t("settings.editor.spell_check"),
                true,
            )
            .changed();
        });
    });
    changed |= page_reset_ui(ui, "editor_editing", settings, ResetPage::EditorEditing);
    changed
}

fn editor_fonts_ui(
    ui: &mut egui::Ui,
    settings: &mut crate::settings::Settings,
    catalog: &FontCatalog,
) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_editor_fonts", |table| {
        table.row_note(
            &t("settings.editor.font"),
            &t("settings.editor.font_hint"),
            |ui| {
                changed |= search_combo(
                    ui,
                    "editor_font_combo",
                    &mut settings.fonts.editor_font,
                    catalog.families(),
                    &t("settings.font.search"),
                    &t("settings.no_matching_fonts"),
                    (ui.available_width() - 4.0).clamp(160.0, 240.0),
                );
            },
        );

        table.row_note(
            &t("settings.editor.font_size"),
            &t("settings.editor.font_size_hint"),
            |ui| {
                changed |= editable_number(
                    ui,
                    "editor_font_size",
                    &mut settings.fonts.editor_font_size,
                    EDITOR_FONT_SIZES,
                    8.0..=32.0,
                    "pt",
                    96.0,
                );
            },
        );

        if !catalog.has_family(&settings.fonts.editor_font) {
            table.row("", |ui| {
                missing_font_warning(ui, &settings.fonts.editor_font, catalog);
            });
        }
    });
    changed |= page_reset_ui(ui, "editor_fonts", settings, ResetPage::EditorFonts);
    changed
}
