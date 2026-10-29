//! Rename overlay rendering: the inline name field and the label-confirmation
//! modal. The flow that feeds them lives in [`super`].

use super::{IdeState, RenamePhase, edit_counts};
use std::path::PathBuf;
use typ_edit_analysis::rename::is_valid_ident;
use typ_edit_engine::Engine;
use typ_edit_i18n::{t, t_with};
use typ_edit_model::FileEdits;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::text_input::TextInput;
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

use super::super::document::DocumentController;

/// Stable id of the rename overlay.
const RENAME_OVERLAY_ID: &str = "typ-edit-rename-overlay";
/// Stable id of the label-confirmation modal.
const RENAME_CONFIRM_ID: &str = "typ-edit-rename-confirm";
/// Minimum width of the overlay field and the confirmation dialog.
const RENAME_WIDTH: f32 = 240.0;
/// Fallback offset below the content top when no caret rect anchored the query.
const RENAME_FALLBACK_TOP: f32 = 120.0;

impl IdeState {
    /// Draws the rename overlay (name field) or the label-confirmation modal.
    /// Returns the grouped edits of a confirmed label rename for the caller to
    /// apply; every other path returns `None`.
    pub(in crate::app) fn rename_ui(
        &mut self,
        ctx: &egui::Context,
        engine: &Engine,
        doc: &DocumentController,
        root: Option<PathBuf>,
    ) -> Option<Vec<FileEdits>> {
        let mut session = self.rename.take()?;
        match std::mem::replace(&mut session.phase, RenamePhase::Preparing) {
            RenamePhase::Preparing => {
                self.rename = Some(session);
                None
            }
            RenamePhase::Editing => {
                let submit = ctx
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                let cancel = ctx
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                let pos = session.anchor.unwrap_or_else(|| rename_fallback_pos(ctx));
                let focus_pending = std::mem::take(&mut session.focus_pending);
                egui::Area::new(egui::Id::new(RENAME_OVERLAY_ID))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .constrain_to(ctx.content_rect())
                    .show(ctx, |ui| {
                        let screen = ui.ctx().content_rect();
                        typ_edit_widgets::menu::block_background(
                            ctx,
                            egui::Id::new("typ_edit_rename_backdrop"),
                            screen,
                            ui.layer_id(),
                        );
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            let mut style = tui::column_gap(4.0);
                            tui::fill_width(&mut style);
                            tui::tui(ui, ui.id().with("rename_field"))
                                .reserve_width(RENAME_WIDTH)
                                .style(style)
                                .show(|tree| {
                                    tree.style(tui::label_leaf())
                                        .id(egui::Id::new(("rename_overlay", "field")))
                                        .ui(|ui| {
                                            let valid = is_valid_ident(&session.input);
                                            let field = TextInput::form(
                                                &mut session.input,
                                                "",
                                                ui.available_width(),
                                            )
                                            .id(ui.id().with("input"))
                                            .error(!valid)
                                            .show(ui);
                                            if focus_pending {
                                                field.request_focus();
                                            }
                                        });
                                    if !is_valid_ident(&session.input) {
                                        tree.id(egui::Id::new(("rename_overlay", "invalid"))).ui(
                                            |ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(t("rename.invalid"))
                                                            .color(crate::theme::error(ui))
                                                            .small(),
                                                    )
                                                    .wrap(),
                                                );
                                            },
                                        );
                                    }
                                });
                        });
                    });
                if cancel {
                    return None;
                }
                if submit && is_valid_ident(&session.input) {
                    self.send_rename(&session, engine, doc, root);
                }
                session.phase = RenamePhase::Editing;
                self.rename = Some(session);
                None
            }
            RenamePhase::ConfirmingLabel { edits } => {
                let (count, files) = edit_counts(&edits);
                let mut choice: Option<bool> = None;
                egui::Modal::new(egui::Id::new(RENAME_CONFIRM_ID)).show(ctx, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            let mut style = tui::column_gap(6.0);
                            tui::fill_width(&mut style);
                            tui::tui(ui, ui.id().with("rename_confirm"))
                                .reserve_width(340.0)
                                .style(style)
                                .show(|tree| {
                                    tree.id(egui::Id::new(("rename_confirm", "title")))
                                        .ui(|ui| {
                                            ui.label(
                                                egui::RichText::new(t("editor.menu.rename"))
                                                    .size(15.0)
                                                    .strong(),
                                            );
                                        });
                                    tree.id(egui::Id::new(("rename_confirm", "separator")))
                                        .ui(separator);
                                    tree.id(egui::Id::new(("rename_confirm", "message"))).ui(
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(t_with(
                                                    "rename.confirm",
                                                    &[
                                                        ("count", &count.to_string()),
                                                        ("files", &files.to_string()),
                                                    ],
                                                ))
                                                .wrap(),
                                            );
                                        },
                                    );
                                    if escape {
                                        choice = Some(false);
                                    }
                                    tree.style(tui::row_reverse_gap(6.0))
                                        .id(egui::Id::new(("rename_confirm", "actions")))
                                        .add(|tree| {
                                            tree.id(egui::Id::new(("rename_confirm", "ok"))).ui(
                                                |ui| {
                                                    if button(
                                                        ui,
                                                        &t("dialog.ok"),
                                                        ButtonVariant::Primary,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        choice = Some(true);
                                                    }
                                                },
                                            );
                                            tree.id(egui::Id::new(("rename_confirm", "cancel")))
                                                .ui(|ui| {
                                                    if button(
                                                        ui,
                                                        &t("dialog.cancel"),
                                                        ButtonVariant::Secondary,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        choice = Some(false);
                                                    }
                                                });
                                        });
                                });
                        });
                });
                match choice {
                    Some(true) => Some(edits),
                    Some(false) => {
                        session.phase = RenamePhase::Editing;
                        session.focus_pending = true;
                        self.rename = Some(session);
                        None
                    }
                    None => {
                        session.phase = RenamePhase::ConfirmingLabel { edits };
                        self.rename = Some(session);
                        None
                    }
                }
            }
        }
    }
}

/// Fallback overlay position when the editor reported no caret rect: near the
/// top-centre of the content area, below the title strip.
fn rename_fallback_pos(ctx: &egui::Context) -> egui::Pos2 {
    let rect = ctx.content_rect();
    egui::pos2(
        rect.center().x - RENAME_WIDTH * 0.5,
        rect.top() + RENAME_FALLBACK_TOP,
    )
}
