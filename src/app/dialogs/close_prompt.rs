use crate::app::state::SessionState;
use crate::app::tabs::tab_title;
use crate::app::{CloseChoice, ClosePrompt};
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

/// Minimum content width of the unsaved-changes dialog.
const DIALOG_WIDTH: f32 = 320.0;

/// The app-only action the unsaved-changes dialog resolved to, applied by the
/// caller after the render returns. The save/close orchestration (which needs
/// the whole app) stays on the caller; the session prompt bookkeeping is done
/// here.
pub(in crate::app) enum ClosePromptOutcome {
    /// No decision yet, or the user cancelled: nothing to do.
    None,
    /// Save the tab at the index; on success the caller removes it, on failure
    /// it re-arms the tab prompt.
    SaveTab(usize),
    /// Remove the tab at the index.
    RemoveTab(usize),
    /// Save every dirty tab, then close the application on success (else re-arm
    /// the app prompt).
    SaveAllClose,
    /// Close the application window.
    CloseApp,
}

impl SessionState {
    /// Renders the unsaved-changes dialog and resolves its choice.
    ///
    /// The prompt is taken out of `self` first, so the closure never borrows
    /// it; it is restored while the dialog must stay open.
    pub(in crate::app) fn close_prompt_ui(&mut self, ctx: &egui::Context) -> ClosePromptOutcome {
        let Some(prompt) = self.close_prompt.take() else {
            return ClosePromptOutcome::None;
        };
        let body = match prompt {
            ClosePrompt::Tab(index) => match self.tabs.get(index) {
                Some(tab) => t_with("dialog.unsaved.tab", &[("name", &tab_title(tab))]),
                None => return ClosePromptOutcome::None,
            },
            ClosePrompt::App => {
                let dirty = self.tabs.iter().filter(|tab| tab.dirty).count();
                t_with("dialog.unsaved.app", &[("count", &dirty.to_string())])
            }
        };

        let mut choice: Option<CloseChoice> = None;
        // Backdrop-click close is intentionally NOT enabled here (only the settings overlay closes on backdrop/Escape).
        egui::Modal::new(egui::Id::new("close-prompt-dialog")).show(ctx, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    let base = ui.id().with(("typ_edit_close_prompt", "content"));
                    let row_gap = ui.spacing().item_spacing.x;
                    let theme = Theme::from_ui(ui);
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let actions: Vec<String> = match prompt {
                        ClosePrompt::Tab(_) => vec![
                            t("dialog.save").into_owned(),
                            t("dialog.discard").into_owned(),
                            t("dialog.cancel").into_owned(),
                        ],
                        ClosePrompt::App => vec![
                            t("dialog.save_all_quit").into_owned(),
                            t("dialog.discard_quit").into_owned(),
                            t("dialog.cancel").into_owned(),
                        ],
                    };
                    // Keep the dialog wide enough for its action row in every
                    // language (the former auto-width did the same).
                    let actions_width: f32 = actions
                        .iter()
                        .map(|label| {
                            ui.painter()
                                .layout_no_wrap(
                                    label.clone(),
                                    font.clone(),
                                    egui::Color32::PLACEHOLDER,
                                )
                                .size()
                                .x
                                + 2.0 * theme.spacing.control_pad_x
                        })
                        .sum::<f32>()
                        + row_gap * (actions.len() as f32 - 1.0);
                    let dialog_width = DIALOG_WIDTH.max(actions_width);
                    ui.set_width(dialog_width);
                    let mut root_style = tui::column_gap(ui.spacing().item_spacing.y);
                    tui::fill_width(&mut root_style);
                    tui::tui(ui, base)
                        .reserve_width(dialog_width)
                        .style(root_style)
                        .show(|tree| {
                            tree.id(base.with("title")).label(
                                egui::RichText::new(t("dialog.unsaved.title"))
                                    .size(15.0)
                                    .strong(),
                            );
                            tree.id(base.with("separator")).ui(separator);
                            tree.id(base.with("body")).label(body);
                            if tree.egui_ui().input(|i| i.key_pressed(egui::Key::Escape)) {
                                choice = Some(CloseChoice::Cancel);
                            }
                            tree.id(base.with("actions"))
                                .style(tui::row_reverse_gap(row_gap))
                                .add(|actions| match prompt {
                                    ClosePrompt::Tab(_) => {
                                        if actions
                                            .id(base.with("save"))
                                            .ui(|ui| {
                                                button(
                                                    ui,
                                                    &t("dialog.save"),
                                                    ButtonVariant::Primary,
                                                    true,
                                                )
                                            })
                                            .clicked()
                                        {
                                            choice = Some(CloseChoice::Save);
                                        }
                                        if actions
                                            .id(base.with("discard"))
                                            .ui(|ui| {
                                                button(
                                                    ui,
                                                    &t("dialog.discard"),
                                                    ButtonVariant::Danger,
                                                    true,
                                                )
                                            })
                                            .clicked()
                                        {
                                            choice = Some(CloseChoice::Discard);
                                        }
                                        if actions
                                            .id(base.with("cancel"))
                                            .ui(|ui| {
                                                button(
                                                    ui,
                                                    &t("dialog.cancel"),
                                                    ButtonVariant::Secondary,
                                                    true,
                                                )
                                            })
                                            .clicked()
                                        {
                                            choice = Some(CloseChoice::Cancel);
                                        }
                                    }
                                    ClosePrompt::App => {
                                        if actions
                                            .id(base.with("save_all_quit"))
                                            .ui(|ui| {
                                                button(
                                                    ui,
                                                    &t("dialog.save_all_quit"),
                                                    ButtonVariant::Primary,
                                                    true,
                                                )
                                            })
                                            .clicked()
                                        {
                                            choice = Some(CloseChoice::Save);
                                        }
                                        if actions
                                            .id(base.with("discard_quit"))
                                            .ui(|ui| {
                                                button(
                                                    ui,
                                                    &t("dialog.discard_quit"),
                                                    ButtonVariant::Danger,
                                                    true,
                                                )
                                            })
                                            .clicked()
                                        {
                                            choice = Some(CloseChoice::Discard);
                                        }
                                        if actions
                                            .id(base.with("cancel"))
                                            .ui(|ui| {
                                                button(
                                                    ui,
                                                    &t("dialog.cancel"),
                                                    ButtonVariant::Secondary,
                                                    true,
                                                )
                                            })
                                            .clicked()
                                        {
                                            choice = Some(CloseChoice::Cancel);
                                        }
                                    }
                                });
                        });
                });
        });

        match (prompt, choice) {
            (_, None) => {
                self.prompt_close(prompt);
                ClosePromptOutcome::None
            }
            (_, Some(CloseChoice::Cancel)) => ClosePromptOutcome::None,
            (ClosePrompt::Tab(index), Some(CloseChoice::Save)) => {
                ClosePromptOutcome::SaveTab(index)
            }
            (ClosePrompt::Tab(index), Some(CloseChoice::Discard)) => {
                ClosePromptOutcome::RemoveTab(index)
            }
            (ClosePrompt::App, Some(CloseChoice::Save)) => ClosePromptOutcome::SaveAllClose,
            (ClosePrompt::App, Some(CloseChoice::Discard)) => ClosePromptOutcome::CloseApp,
        }
    }
}
