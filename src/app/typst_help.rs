//! In-app Typst quick-start window.
//!
//! A writer who has never used Typst should not have to leave the app to learn
//! that headings start with `=`, bold uses `*...*`, and lists use `-`. The
//! window is deliberately static: six copyable examples and the full-reference
//! pointer, opened from Help and from the landing page.

use egui::RichText;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;

use super::state::UiState;

impl UiState {
    /// Draws the Typst quick-start modal while it is open. Backdrop click,
    /// Escape and the close button all dismiss it.
    pub(super) fn typst_help_window(&mut self, ctx: &egui::Context) {
        if !self.typst_help_open {
            return;
        }
        let mut open = true;
        let modal = egui::Modal::new(egui::Id::new("typst-help-window")).show(ctx, |ui| {
            egui::Frame::new()
                .fill(crate::theme::raised(ui))
                .stroke(egui::Stroke::new(1.0, crate::theme::border_default(ui)))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    let max_w = (ctx.content_rect().width() - 24.0).max(280.0);
                    ui.set_width(520.0_f32.min(max_w));
                    ui.label(RichText::new(t("help.typst.title")).size(15.0).strong());
                    separator(ui);
                    ui.label(t("help.typst.intro"));
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .id_salt("typst_help_scroll")
                        .auto_shrink([false, false])
                        .max_height(340.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new(t("help.typst.examples")).monospace());
                        });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(t("help.typst.footer"))
                            .small()
                            .color(crate::theme::text_muted(ui)),
                    );
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if button(ui, &t("settings.close"), ButtonVariant::Secondary, true)
                            .clicked()
                        {
                            open = false;
                        }
                    });
                });
        });
        if modal.should_close() {
            open = false;
        }
        if !open {
            self.typst_help_open = false;
        }
    }
}
