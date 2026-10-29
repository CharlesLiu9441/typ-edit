use crate::app::recovery::{Recovery, RecoveryChoice, format_utc_ms};
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

/// Fixed content width of the recovery prompt.
const DIALOG_WIDTH: f32 = 360.0;

impl Recovery {
    /// Renders the crash-recovery prompt and returns the choice it resolved to,
    /// or `None` while it stays open. The caller applies the choice with
    /// [`Recovery::resolve_choice`].
    ///
    /// The prompt's fields are read into locals before the modal closure, so the
    /// closure never borrows the recovery state.
    pub(in crate::app) fn prompt_ui(&mut self, ctx: &egui::Context) -> Option<RecoveryChoice> {
        let prompt = self.prompt.as_ref()?;
        let saved_at_ms = prompt.saved_at_ms;
        let labels: Vec<String> = prompt
            .candidates
            .iter()
            .map(|candidate| candidate.label())
            .collect();
        let mut choice: Option<RecoveryChoice> = None;
        // Backdrop-click close is intentionally NOT enabled here (only the settings overlay closes on backdrop/Escape).
        egui::Modal::new(egui::Id::new("recovery-prompt-dialog")).show(ctx, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    let base = ui.id().with(("typ_edit_recovery_prompt", "content"));
                    let row_gap = ui.spacing().item_spacing.x;
                    let mut root_style = tui::column_gap(ui.spacing().item_spacing.y);
                    tui::fill_width(&mut root_style);
                    tui::tui(ui, base)
                        .reserve_width(DIALOG_WIDTH)
                        .style(root_style)
                        .show(|tree| {
                            tree.id(base.with("title")).label(
                                egui::RichText::new(t("recovery.title")).size(15.0).strong(),
                            );
                            tree.id(base.with("separator")).ui(separator);
                            tree.id(base.with("message")).label(t_with(
                                "recovery.message",
                                &[("time", &format_utc_ms(saved_at_ms))],
                            ));
                            for (index, label) in labels.iter().enumerate() {
                                tree.id(base.with(("candidate", index)))
                                    .label(egui::RichText::new(label).weak());
                            }
                            if tree.egui_ui().input(|i| i.key_pressed(egui::Key::Escape)) {
                                choice = Some(RecoveryChoice::Later);
                            }
                            tree.id(base.with("actions"))
                                .style(tui::row_reverse_gap(row_gap))
                                .add(|actions| {
                                    if actions
                                        .id(base.with("restore"))
                                        .ui(|ui| {
                                            button(
                                                ui,
                                                &t("recovery.restore"),
                                                ButtonVariant::Primary,
                                                true,
                                            )
                                        })
                                        .clicked()
                                    {
                                        choice = Some(RecoveryChoice::Restore);
                                    }
                                    if actions
                                        .id(base.with("later"))
                                        .ui(|ui| {
                                            button(
                                                ui,
                                                &t("recovery.later"),
                                                ButtonVariant::Secondary,
                                                true,
                                            )
                                        })
                                        .clicked()
                                    {
                                        choice = Some(RecoveryChoice::Later);
                                    }
                                    if actions
                                        .id(base.with("discard"))
                                        .ui(|ui| {
                                            button(
                                                ui,
                                                &t("recovery.discard"),
                                                ButtonVariant::Danger,
                                                true,
                                            )
                                        })
                                        .clicked()
                                    {
                                        choice = Some(RecoveryChoice::Discard);
                                    }
                                });
                        });
                });
        });
        choice
    }
}
