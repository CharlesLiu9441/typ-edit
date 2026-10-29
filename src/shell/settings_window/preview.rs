//! Preview category: fallback raster resolution plus cursor and selection styling.

use egui::DragValue;
use typ_edit_i18n::t;
use typ_edit_widgets::checkbox::checkbox;
use typ_edit_widgets::slider::slider;

use crate::settings::CursorStylePref;

use super::grid::{ResetPage, page_reset_ui, settings_grid};
use super::subtabs::sub_tabs;

/// Preview category: a raster page and a cursor/selection page.
pub(super) fn preview_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    sub_tabs(
        ui,
        "preview",
        &[
            "settings.subtabs.preview",
            "settings.subtabs.cursor_selection",
        ],
        |ui, page| match page {
            0 => preview_general_ui(ui, settings),
            1 => preview_cursor_ui(ui, settings),
            _ => false,
        },
    )
}

fn preview_general_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_preview", |table| {
        table.row_note(
            &t("settings.preview.dpi"),
            &t("settings.preview.hint"),
            |ui| {
                changed |= ui
                    .add(
                        DragValue::new(&mut settings.preview.preview_dpi)
                            .range(24.0..=600.0)
                            .speed(2.0)
                            .suffix(" dpi"),
                    )
                    .changed();
            },
        );
    });
    changed |= page_reset_ui(ui, "preview_general", settings, ResetPage::PreviewGeneral);
    changed
}

fn preview_cursor_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_preview_cursor", |table| {
        table.row(&t("settings.editor.cursor_width"), |ui| {
            changed |= ui
                .add(
                    DragValue::new(&mut settings.preview.cursor_width)
                        .range(0.5..=6.0)
                        .speed(0.1)
                        .suffix(" px"),
                )
                .changed();
        });

        table.row(&t("settings.editor.cursor_style"), |ui| {
            let line = t("settings.editor.cursor_style.line");
            let ring = t("settings.editor.cursor_style.ring");
            let mask = t("settings.editor.cursor_style.mask");
            changed |= typ_edit_widgets::segmented::segmented(
                ui,
                &[
                    (CursorStylePref::Line, line.as_ref()),
                    (CursorStylePref::Ring, ring.as_ref()),
                    (CursorStylePref::Mask, mask.as_ref()),
                ],
                &mut settings.preview.cursor_style,
            );
        });

        table.row(&t("settings.editor.cursor_blink"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.preview.cursor_blink,
                &t("settings.editor.enable"),
                true,
            )
            .changed();
        });

        table.row(&t("settings.editor.blink_interval"), |ui| {
            changed |= ui
                .add_enabled(
                    settings.preview.cursor_blink,
                    DragValue::new(&mut settings.preview.cursor_blink_interval)
                        .range(200.0..=2000.0)
                        .speed(10.0)
                        .suffix(" ms"),
                )
                .changed();
        });

        table.row(&t("settings.editor.selection_color"), |ui| {
            changed |= ui
                .color_edit_button_srgb(&mut settings.preview.selection_color)
                .changed();
        });

        table.row(&t("settings.editor.selection_opacity"), |ui| {
            changed |= slider(
                ui,
                egui::Slider::new(&mut settings.preview.selection_opacity, 0.0..=1.0)
                    .step_by(0.05)
                    .fixed_decimals(2),
            )
            .changed();
        });
    });
    changed |= page_reset_ui(ui, "preview_cursor", settings, ResetPage::PreviewCursor);
    changed
}
