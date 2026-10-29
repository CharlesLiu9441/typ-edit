use crate::app::TreeDialog;
use crate::app::errors::create_error_message;
use crate::app::files::tree_entry_name;
use crate::app::state::{SessionState, UiState};
use crate::shell::platform_ops;
use crate::shell::tree::ProjectTree;
use crate::theme;
use std::path::{Path, PathBuf};
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::text_input::TextInput;
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

/// Fixed content width of the tree dialogs.
const DIALOG_WIDTH: f32 = 320.0;

/// A completed tree delete: the caller recompiles (when the main file was
/// deleted) before clearing the open tabs under `path`.
pub(in crate::app) struct DeleteOutcome {
    pub(in crate::app) path: PathBuf,
    pub(in crate::app) main_deleted: bool,
}

/// App-only effects produced by rendering the tree dialog, applied by the
/// caller after the render returns. At most one is set per frame.
#[derive(Default)]
pub(in crate::app) struct TreeDialogOutcome {
    /// A newly created file to open.
    pub(in crate::app) open_file: Option<PathBuf>,
    /// A completed rename `(old, new)` to apply.
    pub(in crate::app) rename: Option<(PathBuf, PathBuf)>,
    /// A completed delete for the caller to finish.
    pub(in crate::app) delete: Option<DeleteOutcome>,
}

impl SessionState {
    /// Renders the modal tree dialog (new file/folder prompt, delete confirmation).
    ///
    /// The dialog state is taken out of `ui_state` first, so the window closures
    /// never borrow it; it is put back only while the dialog must stay open.
    pub(in crate::app) fn tree_dialog_ui(
        &mut self,
        ctx: &egui::Context,
        ui_state: &mut UiState,
        tree: &mut ProjectTree,
    ) -> TreeDialogOutcome {
        let mut out = TreeDialogOutcome::default();
        if matches!(ui_state.tree_dialog, TreeDialog::None) {
            return out;
        }
        let dialog = std::mem::replace(&mut ui_state.tree_dialog, TreeDialog::None);
        match dialog {
            TreeDialog::None => {}
            TreeDialog::NewEntry {
                dir,
                is_dir,
                mut name,
                focus_pending,
                mut error,
            } => {
                let mut confirm = false;
                let mut cancel = false;
                // Backdrop-click close is intentionally NOT enabled here (only the settings overlay closes on backdrop/Escape).
                egui::Modal::new(egui::Id::new("tree-new-entry-dialog")).show(ctx, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.set_width(DIALOG_WIDTH);
                            let base = ui.id().with(("typ_edit_tree_new_entry", "content"));
                            let row_gap = ui.spacing().item_spacing.x;
                            let mut root_style = tui::column_gap(ui.spacing().item_spacing.y);
                            tui::fill_width(&mut root_style);
                            let mut response: Option<egui::Response> = None;
                            tui::tui(ui, base)
                                .reserve_width(DIALOG_WIDTH)
                                .style(root_style)
                                .show(|tree| {
                                    let title = if is_dir {
                                        t("dialog.new_folder")
                                    } else {
                                        t("dialog.new_file")
                                    };
                                    tree.id(base.with("title"))
                                        .label(egui::RichText::new(title).size(15.0).strong());
                                    tree.id(base.with("separator")).ui(separator);
                                    tree.id(base.with("name_row"))
                                        .style(tui::row_gap(row_gap))
                                        .add(|row| {
                                            row.id(base.with("name_label"))
                                                .style(tui::label_leaf())
                                                .wrap_mode(egui::TextWrapMode::Extend)
                                                .label(t("dialog.name"));
                                            response = Some(
                                                row.id(base.with("name_input"))
                                                    .style(tui::flexible())
                                                    .ui(|ui| {
                                                        TextInput::form(&mut name, "", 260.0)
                                                            .error(error.is_some())
                                                            .show(ui)
                                                    }),
                                            );
                                        });
                                    if focus_pending && let Some(response) = &response {
                                        response.request_focus();
                                        if let Some(mut state) = egui::TextEdit::load_state(
                                            tree.egui_ui().ctx(),
                                            response.id,
                                        ) {
                                            let end =
                                                egui::text::CCursor::new(name.chars().count());
                                            state.cursor.set_char_range(Some(
                                                egui::text::CCursorRange::two(
                                                    egui::text::CCursor::new(0),
                                                    end,
                                                ),
                                            ));
                                            state.store(tree.egui_ui().ctx(), response.id);
                                        }
                                    }
                                    if let Some(message) = &error {
                                        tree.id(base.with("error"))
                                            .ui(|ui| ui.colored_label(theme::error(ui), message));
                                    }
                                    tree.id(base.with("actions"))
                                        .style(tui::row_reverse_gap(row_gap))
                                        .add(|actions| {
                                            if actions
                                                .id(base.with("ok"))
                                                .ui(|ui| {
                                                    button(
                                                        ui,
                                                        &t("dialog.ok"),
                                                        ButtonVariant::Primary,
                                                        true,
                                                    )
                                                })
                                                .clicked()
                                            {
                                                confirm = true;
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
                                                cancel = true;
                                            }
                                        });
                                    if response.as_ref().is_some_and(|r| r.lost_focus())
                                        && tree.egui_ui().input(|i| i.key_pressed(egui::Key::Enter))
                                    {
                                        confirm = true;
                                    }
                                    if tree.egui_ui().input(|i| i.key_pressed(egui::Key::Escape)) {
                                        cancel = true;
                                    }
                                });
                        });
                });

                let mut keep_open = true;
                if cancel {
                    keep_open = false;
                } else if confirm {
                    keep_open = match platform_ops::validate_name(&name) {
                        Err(message) => {
                            error = Some(message);
                            true
                        }
                        Ok(()) => {
                            let created = if is_dir {
                                platform_ops::create_dir(&dir, &name)
                            } else {
                                platform_ops::create_file(&dir, &name)
                            };
                            match created {
                                Ok(path) => {
                                    tree.expanded.insert(dir.clone());
                                    tree.invalidate(&path);
                                    if !is_dir {
                                        out.open_file = Some(path);
                                    }
                                    false
                                }
                                Err(err) => {
                                    error = Some(create_error_message(&err, is_dir));
                                    true
                                }
                            }
                        }
                    };
                }
                if keep_open {
                    ui_state.tree_dialog = TreeDialog::NewEntry {
                        dir,
                        is_dir,
                        name,
                        focus_pending: false,
                        error,
                    };
                }
            }
            TreeDialog::Rename {
                path,
                mut name,
                focus_pending,
                mut error,
            } => {
                let mut confirm = false;
                let mut cancel = false;
                // Backdrop-click close is intentionally NOT enabled here (only the settings overlay closes on backdrop/Escape).
                egui::Modal::new(egui::Id::new("tree-rename-dialog")).show(ctx, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.set_width(DIALOG_WIDTH);
                            let base = ui.id().with(("typ_edit_tree_rename", "content"));
                            let row_gap = ui.spacing().item_spacing.x;
                            let mut root_style = tui::column_gap(ui.spacing().item_spacing.y);
                            tui::fill_width(&mut root_style);
                            let mut response: Option<egui::Response> = None;
                            tui::tui(ui, base)
                                .reserve_width(DIALOG_WIDTH)
                                .style(root_style)
                                .show(|tree| {
                                    tree.id(base.with("title")).label(
                                        egui::RichText::new(t("dialog.rename")).size(15.0).strong(),
                                    );
                                    tree.id(base.with("separator")).ui(separator);
                                    tree.id(base.with("name_row"))
                                        .style(tui::row_gap(row_gap))
                                        .add(|row| {
                                            row.id(base.with("name_label"))
                                                .style(tui::label_leaf())
                                                .wrap_mode(egui::TextWrapMode::Extend)
                                                .label(t("dialog.name"));
                                            response = Some(
                                                row.id(base.with("name_input"))
                                                    .style(tui::flexible())
                                                    .ui(|ui| {
                                                        TextInput::form(&mut name, "", 260.0)
                                                            .error(error.is_some())
                                                            .show(ui)
                                                    }),
                                            );
                                        });
                                    if focus_pending && let Some(response) = &response {
                                        response.request_focus();
                                        if let Some(mut state) = egui::TextEdit::load_state(
                                            tree.egui_ui().ctx(),
                                            response.id,
                                        ) {
                                            let end =
                                                egui::text::CCursor::new(name.chars().count());
                                            state.cursor.set_char_range(Some(
                                                egui::text::CCursorRange::two(
                                                    egui::text::CCursor::new(0),
                                                    end,
                                                ),
                                            ));
                                            state.store(tree.egui_ui().ctx(), response.id);
                                        }
                                    }
                                    if let Some(message) = &error {
                                        tree.id(base.with("error"))
                                            .ui(|ui| ui.colored_label(theme::error(ui), message));
                                    }
                                    tree.id(base.with("actions"))
                                        .style(tui::row_reverse_gap(row_gap))
                                        .add(|actions| {
                                            if actions
                                                .id(base.with("ok"))
                                                .ui(|ui| {
                                                    button(
                                                        ui,
                                                        &t("dialog.ok"),
                                                        ButtonVariant::Primary,
                                                        true,
                                                    )
                                                })
                                                .clicked()
                                            {
                                                confirm = true;
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
                                                cancel = true;
                                            }
                                        });
                                    if response.as_ref().is_some_and(|r| r.lost_focus())
                                        && tree.egui_ui().input(|i| i.key_pressed(egui::Key::Enter))
                                    {
                                        confirm = true;
                                    }
                                    if tree.egui_ui().input(|i| i.key_pressed(egui::Key::Escape)) {
                                        cancel = true;
                                    }
                                });
                        });
                });

                let mut keep_open = true;
                if cancel {
                    keep_open = false;
                } else if confirm {
                    keep_open = match platform_ops::validate_name(&name) {
                        Err(message) => {
                            error = Some(message);
                            true
                        }
                        Ok(()) => match path.parent().map(|parent| parent.join(name.trim())) {
                            Some(new_path) if new_path == path => false,
                            Some(new_path) => match platform_ops::rename_path(&path, &new_path) {
                                Ok(()) => {
                                    out.rename = Some((path.clone(), new_path));
                                    false
                                }
                                Err(err) => {
                                    error = Some(t_with(
                                        "dialog.rename.failed",
                                        &[("err", &err.to_string())],
                                    ));
                                    true
                                }
                            },
                            None => {
                                error = Some(t("dialog.rename.cannot").into_owned());
                                true
                            }
                        },
                    };
                }
                if keep_open {
                    ui_state.tree_dialog = TreeDialog::Rename {
                        path,
                        name,
                        focus_pending: false,
                        error,
                    };
                }
            }
            TreeDialog::ConfirmDelete {
                path,
                is_dir,
                dirty,
                mut error,
            } => {
                let mut confirm = false;
                let mut cancel = false;
                // Backdrop-click close is intentionally NOT enabled here (only the settings overlay closes on backdrop/Escape).
                egui::Modal::new(egui::Id::new("tree-delete-dialog")).show(ctx, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.set_width(DIALOG_WIDTH);
                            let base = ui.id().with(("typ_edit_tree_delete", "content"));
                            let row_gap = ui.spacing().item_spacing.x;
                            let mut root_style = tui::column_gap(ui.spacing().item_spacing.y);
                            tui::fill_width(&mut root_style);
                            tui::tui(ui, base)
                                .reserve_width(DIALOG_WIDTH)
                                .style(root_style)
                                .show(|tree| {
                                    tree.id(base.with("title")).label(
                                        egui::RichText::new(t("dialog.delete")).size(15.0).strong(),
                                    );
                                    tree.id(base.with("separator")).ui(separator);
                                    tree.id(base.with("confirm")).label(t_with(
                                        "dialog.delete.confirm",
                                        &[("name", &tree_entry_name(&path))],
                                    ));
                                    if is_dir {
                                        tree.id(base.with("contents"))
                                            .label(t("dialog.delete.contents"));
                                    }
                                    if !dirty.is_empty() {
                                        tree.id(base.with("dirty")).ui(|ui| {
                                            ui.colored_label(
                                                theme::warn(ui),
                                                t_with(
                                                    "dialog.delete.dirty",
                                                    &[("count", &dirty.len().to_string())],
                                                ),
                                            )
                                        });
                                    }
                                    tree.id(base.with("irreversible"))
                                        .label(t("dialog.delete.irreversible"));
                                    if let Some(message) = &error {
                                        tree.id(base.with("error"))
                                            .ui(|ui| ui.colored_label(theme::error(ui), message));
                                    }
                                    if tree.egui_ui().input(|i| i.key_pressed(egui::Key::Escape)) {
                                        cancel = true;
                                    }
                                    tree.id(base.with("actions"))
                                        .style(tui::row_reverse_gap(row_gap))
                                        .add(|actions| {
                                            if actions
                                                .id(base.with("delete"))
                                                .ui(|ui| {
                                                    button(
                                                        ui,
                                                        &t("dialog.delete"),
                                                        ButtonVariant::Danger,
                                                        true,
                                                    )
                                                })
                                                .clicked()
                                            {
                                                confirm = true;
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
                                                cancel = true;
                                            }
                                        });
                                });
                        });
                });

                let mut keep_open = true;
                if cancel {
                    keep_open = false;
                } else if confirm {
                    match platform_ops::delete_path(&path, is_dir) {
                        Ok(()) => {
                            keep_open = false;
                            tree.expanded.retain(|p| !p.starts_with(&path));
                            tree.invalidate(&path);
                            let main_deleted = tree
                                .main_file
                                .as_deref()
                                .is_some_and(|main| main.starts_with(&path));
                            if main_deleted {
                                tree.main_file = None;
                            }
                            out.delete = Some(DeleteOutcome {
                                path: path.clone(),
                                main_deleted,
                            });
                        }
                        Err(err) => {
                            error =
                                Some(t_with("dialog.delete.failed", &[("err", &err.to_string())]));
                        }
                    }
                }
                if keep_open {
                    ui_state.tree_dialog = TreeDialog::ConfirmDelete {
                        path,
                        is_dir,
                        dirty,
                        error,
                    };
                }
            }
        }
        out
    }

    /// Clears the file path of every open tab under a deleted tree path.
    pub(in crate::app) fn clear_deleted_paths(&mut self, deleted: &Path) {
        for tab in &mut self.tabs {
            if tab
                .editor
                .path
                .as_deref()
                .is_some_and(|open| open.starts_with(deleted))
            {
                tab.editor.path = None;
                tab.dirty = false;
            }
        }
    }
}
