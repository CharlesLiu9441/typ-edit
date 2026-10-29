//! Interface category: general appearance, fonts, and icon style sub-pages.

use typ_edit_engine::fonts::FontCatalog;
use typ_edit_i18n::t;
use typ_edit_widgets::checkbox::checkbox;
use typ_edit_widgets::editable_combo::editable_number;
use typ_edit_widgets::search_combo::search_combo;
use typ_edit_widgets::slider::slider;

use crate::settings::{LanguagePref, ThemePref};

use super::grid::{ResetPage, missing_font_warning, page_reset_ui, settings_grid};
use super::icons::icons_ui;
use super::subtabs::sub_tabs;

const UI_FONT_SIZES: &[f32] = &[
    10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 22.0, 24.0,
];

/// Whether a slider interaction should commit the new value now.
///
/// While the thumb is dragged the value stays live in the settings, but the
/// frame must not re-apply the whole-interface zoom on every sample. A release
/// (or a non-drag click/keyboard change) is the commit point.
fn scale_commit(dragged: bool, drag_stopped: bool, changed: bool) -> bool {
    drag_stopped || (!dragged && changed)
}

/// Interface category: a general appearance page, a font page and an icon page.
pub(super) fn interface_ui(
    ui: &mut egui::Ui,
    settings: &mut crate::settings::Settings,
    catalog: &FontCatalog,
) -> bool {
    sub_tabs(
        ui,
        "interface",
        &[
            "settings.subtabs.general",
            "settings.subtabs.fonts",
            "settings.subtabs.icons",
        ],
        |ui, page| match page {
            0 => general_ui(ui, settings),
            1 => fonts_ui(ui, settings, catalog),
            2 => icons_ui(ui, settings),
            _ => false,
        },
    )
}

fn general_ui(ui: &mut egui::Ui, settings: &mut crate::settings::Settings) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_interface", |table| {
        table.row(&t("settings.interface.theme"), |ui| {
            let system = t("settings.interface.theme.system");
            let light = t("settings.interface.theme.light");
            let dark = t("settings.interface.theme.dark");
            changed |= typ_edit_widgets::segmented::segmented(
                ui,
                &[
                    (ThemePref::System, system.as_ref()),
                    (ThemePref::Light, light.as_ref()),
                    (ThemePref::Dark, dark.as_ref()),
                ],
                &mut settings.appearance.theme,
            );
        });

        table.row_note(
            &t("settings.interface.compact_mode"),
            &t("settings.interface.compact_mode_hint"),
            |ui| {
                changed |= checkbox(
                    ui,
                    &mut settings.appearance.compact_mode,
                    &t("settings.editor.enable"),
                    true,
                )
                .changed();
            },
        );

        table.row(&t("settings.language.label"), |ui| {
            let system = t("settings.language.system");
            changed |= typ_edit_widgets::segmented::segmented(
                ui,
                &[
                    (LanguagePref::System, system.as_ref()),
                    (LanguagePref::ZhCn, "简体中文"),
                    (LanguagePref::EnUs, "English"),
                ],
                &mut settings.language,
            );
        });

        table.row_note(
            &t("settings.interface.scale"),
            &t("settings.interface.scale_hint"),
            |ui| {
                let response = slider(
                    ui,
                    egui::Slider::new(&mut settings.appearance.ui_scale, 0.75..=2.0)
                        .step_by(0.05)
                        .custom_formatter(|value, _| format!("{:.0}%", value * 100.0)),
                );
                // Keep the live value for the slider while dragging, but only
                // report a settings change on release (or a keyboard/click
                // commit). The frame then re-applies the zoom once, instead of
                // relayouting the whole interface on every drag sample.
                if scale_commit(
                    response.dragged(),
                    response.drag_stopped(),
                    response.changed(),
                ) {
                    changed = true;
                }
            },
        );

        table.row(&t("settings.interface.recent_enabled"), |ui| {
            changed |= checkbox(
                ui,
                &mut settings.workspace.recent_projects_enabled,
                &t("settings.editor.enable"),
                true,
            )
            .changed();
        });

        table.row(&t("settings.interface.recent_limit"), |ui| {
            let mut limit =
                crate::settings::clamp_recent_limit(settings.workspace.recent_projects_limit);
            let response = ui.add(
                egui::DragValue::new(&mut limit)
                    .range(
                        crate::settings::RECENT_PROJECTS_LIMIT_MIN
                            ..=crate::settings::RECENT_PROJECTS_LIMIT_MAX,
                    )
                    .speed(1.0),
            );
            if response.changed() {
                settings.workspace.recent_projects_limit =
                    crate::settings::clamp_recent_limit(limit);
                changed = true;
            }
        });

        table.row(&t("settings.interface.overlay_opacity"), |ui| {
            changed |= slider(
                ui,
                egui::Slider::new(&mut settings.appearance.overlay_opacity, 0.0..=1.0)
                    .step_by(0.05)
                    .custom_formatter(|value, _| format!("{:.0}%", value * 100.0)),
            )
            .changed();
        });
    });
    changed |= page_reset_ui(
        ui,
        "interface_general",
        settings,
        ResetPage::InterfaceGeneral,
    );
    changed
}

fn fonts_ui(
    ui: &mut egui::Ui,
    settings: &mut crate::settings::Settings,
    catalog: &FontCatalog,
) -> bool {
    let mut changed = false;
    settings_grid(ui, "typ_edit_settings_grid_interface_fonts", |table| {
        table.row_note(
            &t("settings.interface.font"),
            &t("settings.interface.font_hint"),
            |ui| {
                changed |= search_combo(
                    ui,
                    "ui_font_combo",
                    &mut settings.fonts.ui_font,
                    catalog.families(),
                    &t("settings.font.search"),
                    &t("settings.no_matching_fonts"),
                    (ui.available_width() - 4.0).clamp(160.0, 240.0),
                );
            },
        );

        table.row_note(
            &t("settings.interface.font_size"),
            &t("settings.interface.font_size_hint"),
            |ui| {
                changed |= editable_number(
                    ui,
                    "ui_font_size",
                    &mut settings.fonts.ui_font_size,
                    UI_FONT_SIZES,
                    8.0..=24.0,
                    "pt",
                    96.0,
                );
            },
        );

        if !catalog.has_family(&settings.fonts.ui_font) {
            table.row("", |ui| {
                missing_font_warning(ui, &settings.fonts.ui_font, catalog);
            });
        }
    });
    changed |= page_reset_ui(ui, "interface_fonts", settings, ResetPage::InterfaceFonts);
    changed
}

#[cfg(test)]
mod tests {
    use super::scale_commit;

    #[test]
    fn scale_commits_on_release_not_on_drag_samples() {
        assert!(
            !scale_commit(true, false, true),
            "dragging must not relayout"
        );
        assert!(scale_commit(false, true, true), "release commits the drag");
        assert!(
            scale_commit(false, false, true),
            "a click or keyboard change commits immediately"
        );
        assert!(!scale_commit(false, false, false), "no change is a no-op");
    }
}
