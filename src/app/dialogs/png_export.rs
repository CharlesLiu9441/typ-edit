use crate::app::state::{ExportDialogAction, ExportState};
use crate::settings::Settings;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::list::toggle_chip;
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::striped_grid::tui_grid;
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

/// Fixed content width of the PNG export dialog.
const DIALOG_WIDTH: f32 = 360.0;

impl ExportState {
    /// Renders the PNG export settings dialog and reports the user's decision.
    pub(in crate::app) fn dialog(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
    ) -> ExportDialogAction {
        if !self.png_dialog {
            return ExportDialogAction::None;
        }
        let ctx = ui.ctx().clone();
        let exporting = self.exporting;
        let mut export_now = false;
        let mut cancel = false;
        // Backdrop-click close is intentionally NOT enabled here (only the settings overlay closes on backdrop/Escape).
        egui::Modal::new(egui::Id::new("png-export-dialog")).show(&ctx, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(DIALOG_WIDTH);
                    ui.label(
                        egui::RichText::new(t("dialog.png.title"))
                            .size(15.0)
                            .strong(),
                    );
                    separator(ui);
                    tui_grid(ui, "png-export-grid", |table| {
                        table.row(&t("dialog.png.resolution"), |ui| {
                            ui.add(
                                egui::DragValue::new(&mut settings.export.png_dpi)
                                    .range(36.0..=1200.0)
                                    .speed(2.0)
                                    .suffix(" dpi"),
                            );
                        });
                        table.row_tui("", |tree| {
                            for preset in [72.0f32, 96.0, 144.0, 288.0, 600.0] {
                                let selected = (settings.export.png_dpi - preset).abs() < 1.0;
                                if tree
                                    .ui(|ui| toggle_chip(ui, selected, &format!("{preset:.0}")))
                                    .clicked()
                                {
                                    settings.export.png_dpi = preset;
                                }
                            }
                        });
                    });
                    separator(ui);
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    }
                    let base = ui.id().with(("typ_edit_png_export", "actions"));
                    let mut actions_style = tui::row_reverse_gap(ui.spacing().item_spacing.x);
                    tui::fill_width(&mut actions_style);
                    let actions_width = ui.available_width();
                    tui::tui(ui, base)
                        .reserve_width(actions_width)
                        .style(actions_style)
                        .show(|actions| {
                            if actions
                                .id(base.with("export"))
                                .ui(|ui| {
                                    button(
                                        ui,
                                        &t("dialog.export"),
                                        ButtonVariant::Primary,
                                        !exporting,
                                    )
                                })
                                .clicked()
                            {
                                export_now = true;
                            }
                            if actions
                                .id(base.with("cancel"))
                                .ui(|ui| {
                                    button(ui, &t("dialog.cancel"), ButtonVariant::Secondary, true)
                                })
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                });
        });
        if export_now {
            ExportDialogAction::ExportPng
        } else if cancel {
            ExportDialogAction::Close
        } else {
            ExportDialogAction::None
        }
    }
}
