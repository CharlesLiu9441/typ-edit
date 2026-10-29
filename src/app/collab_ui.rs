//! Collaboration dialogs and their pure formatting helpers.
//!
//! Rendering lives here as methods on [`TypEditApp`] (the app owns the state the
//! dialogs mutate); the controller in [`super::collab`] holds the session. Pure
//! helpers (status text, discovery filtering, progress formatting) are unit
//! tested in their home module.
//!
//! Each dialog window hosts one top-level tui column: the windows are plain
//! egui hosts, so opening a fresh tree per dialog is legal (never nested inside
//! another tui).

use super::TypEditApp;
use super::collab::{ServerStart, filter_services, progress_text};
use std::path::PathBuf;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::list::option_row;
use typ_edit_widgets::text_input::{form_input, form_password};
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{AvailableSpace, Dimension, Size, Style},
};

impl TypEditApp {
    /// Renders every open collaboration dialog. Called in the frame's fixed
    /// dialog order, after the gallery and before the command palette.
    pub(super) fn collab_dialogs(&mut self, ui: &mut egui::Ui) {
        if self.collab.share_open {
            self.collab_share_dialog(ui);
        }
        if self.collab.join_open {
            self.collab_join_dialog(ui);
        }
        if self.collab.users_open {
            self.collab_users_dialog(ui);
        }
        if self.collab.info_open {
            self.collab_info_dialog(ui);
        }
        if self.collab.stop_confirm {
            self.collab_confirm_dialog(ui, "collab.stop_server.confirm", Confirm::Stop);
        }
        if self.collab.disconnect_confirm {
            self.collab_confirm_dialog(ui, "collab.disconnect.confirm", Confirm::Disconnect);
        }
        if self.collab.password_prompt() {
            self.collab_password_dialog(ui);
        }
    }

    fn collab_share_dialog(&mut self, ui: &mut egui::Ui) {
        let mut start = false;
        let mut cancel = false;
        let gap = ui.spacing().item_spacing.x;
        let input_width = ui.spacing().text_edit_width.max(160.0);
        let collab = &mut self.collab;
        egui::Modal::new(egui::Id::new("collab-share-dialog")).show(ui.ctx(), |ui| {
            ui.heading(t("collab.share.title"));
            dialog_column_auto(ui, "collab_share_dialog", |tree| {
                let base = tree.current_id();
                tree.id(base.with("username_label"))
                    .ui(|ui| ui.label(t("collab.share.username")));
                tree.id(base.with("username_input"))
                    .style(fixed_width_style(input_width))
                    .ui(|ui| {
                        form_input(ui, &mut collab.share_username, "", input_width);
                    });
                tree.id(base.with("password_label"))
                    .ui(|ui| ui.label(t("collab.share.password")));
                tree.id(base.with("password_input"))
                    .style(fixed_width_style(input_width))
                    .ui(|ui| {
                        form_password(ui, &mut collab.share_password, "", input_width);
                    });
                tree.id(base.with("optional"))
                    .ui(|ui| ui.small(t("collab.share.optional")));
                tree.id(base.with("port_label"))
                    .ui(|ui| ui.label(t("collab.share.port")));
                tree.id(base.with("port_input"))
                    .style(fixed_width_style(input_width))
                    .ui(|ui| {
                        form_input(ui, &mut collab.share_port, "", input_width);
                    });
                tree.id(base.with("separator")).ui(|ui| ui.separator());
                tree.id(base.with("actions"))
                    .style(tui::row_gap(gap))
                    .add(|row| {
                        if row
                            .id(base.with("start"))
                            .ui(|ui| {
                                button(ui, &t("collab.share.start"), ButtonVariant::Primary, true)
                            })
                            .clicked()
                        {
                            start = true;
                        }
                        if row
                            .id(base.with("cancel"))
                            .ui(|ui| {
                                button(
                                    ui,
                                    &t("collab.share.cancel"),
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
        if cancel {
            self.collab.close_share();
            return;
        }
        if !start {
            return;
        }
        let username = self.collab.share_username.trim().to_owned();
        if username.is_empty() {
            self.toast_warning(t("collab.share.username_required").into_owned());
            return;
        }
        let Some(port) = self
            .collab
            .share_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
        else {
            self.toast_warning(t("collab.share.port_invalid").into_owned());
            return;
        };
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let password =
            (!self.collab.share_password.is_empty()).then(|| self.collab.share_password.clone());
        let main = self.share_main_file();
        let initial_docs = self.collab_initial_docs();
        let start = ServerStart {
            root,
            main_file: main,
            username,
            password,
            port,
            initial_docs,
        };
        match self.collab.start_server(&mut self.settings, start) {
            Ok(toast) => {
                self.mark_settings_dirty();
                self.raise_toast(toast);
                if self
                    .session
                    .tabs
                    .iter()
                    .any(|tab| tab.dirty && tab.editor.path.is_none())
                {
                    self.toast_warning(t("collab.share.untitled_unsaved").into_owned());
                }
            }
            Err(err) => self.toast_error(err),
        }
    }

    /// The main file the server should advertise.
    ///
    /// Only an explicit workspace main is authoritative. A project without a
    /// main file must let every participant use their own active editor file;
    /// inferring the host's active file here used to force that file on every
    /// joining client.
    pub(super) fn share_main_file(&self) -> Option<PathBuf> {
        self.tree.main_file.clone()
    }

    fn collab_join_dialog(&mut self, ui: &mut egui::Ui) {
        let mut connect = false;
        let mut cancel = false;
        let mut pick: Option<usize> = None;
        let gap = ui.spacing().item_spacing.x;
        let input_width = ui.spacing().text_edit_width.max(160.0);
        let scanning = self.collab.join_scanning();
        let services = filter_services(&self.collab.discovered);
        let collab = &mut self.collab;
        egui::Modal::new(egui::Id::new("collab-join-dialog")).show(ui.ctx(), |ui| {
            ui.heading(t("collab.join.title"));
            dialog_column_auto(ui, "collab_join_dialog", |tree| {
                let base = tree.current_id();
                tree.id(base.with("username_row"))
                    .style(tui::row_gap(gap))
                    .add(|row| {
                        row.style(tui::label_leaf())
                            .id(base.with("username_label"))
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .label(t("collab.join.username"));
                        row.style(flexible_with_min_width(input_width))
                            .id(base.with("username_input"))
                            .ui(|ui| {
                                let response =
                                    form_input(ui, &mut collab.join_username, "", input_width);
                                #[cfg(test)]
                                super::ui_probe::record("collab.join.username", response.rect);
                                #[cfg(not(test))]
                                let _ = response;
                            });
                    });
                tree.id(base.with("password_row"))
                    .style(tui::row_gap(gap))
                    .add(|row| {
                        row.style(tui::label_leaf())
                            .id(base.with("password_label"))
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .label(t("collab.join.password"));
                        row.style(flexible_with_min_width(input_width))
                            .id(base.with("password_input"))
                            .ui(|ui| {
                                form_password(ui, &mut collab.join_password, "", input_width);
                            });
                    });
                tree.id(base.with("password_optional"))
                    .ui(|ui| ui.small(t("collab.join.password_optional")));
                tree.id(base.with("separator_top")).ui(|ui| ui.separator());
                if scanning {
                    tree.id(base.with("scanning"))
                        .ui(|ui| ui.label(t("collab.join.scanning")));
                } else if services.is_empty() {
                    tree.id(base.with("empty"))
                        .ui(|ui| ui.weak(t("collab.join.list_empty")));
                } else {
                    // tui-whitelist: ScrollArea 内的虚拟化行,不可嵌套 tui
                    tree.id(base.with("discovered")).ui(|ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("collab-join-services")
                            .max_height(180.0)
                            .show(ui, |ui| {
                                for (index, service) in services.iter().enumerate() {
                                    let lock = if service.needs_password {
                                        "\u{1f512}"
                                    } else {
                                        ""
                                    };
                                    let address = service
                                        .addresses
                                        .first()
                                        .map(|addr| addr.to_string())
                                        .unwrap_or_default();
                                    let label = format!(
                                        "{lock} {} — {}  [{address}]",
                                        service.project, service.user
                                    );
                                    if option_row(ui, false, &label).clicked() {
                                        pick = Some(index);
                                    }
                                }
                            });
                    });
                }
                tree.id(base.with("separator_mid")).ui(|ui| ui.separator());
                tree.id(base.with("manual"))
                    .ui(|ui| ui.label(t("collab.join.manual")));
                tree.id(base.with("address_row"))
                    .style(tui::row_gap(gap))
                    .add(|row| {
                        row.style(tui::label_leaf())
                            .id(base.with("address_label"))
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .label(t("collab.join.address"));
                        row.style(flexible_with_min_width(input_width))
                            .id(base.with("address_input"))
                            .ui(|ui| {
                                let response =
                                    form_input(ui, &mut collab.join_address, "", input_width);
                                #[cfg(test)]
                                super::ui_probe::record("collab.join.address", response.rect);
                                #[cfg(not(test))]
                                let _ = response;
                            });
                        let connect_button = row.id(base.with("connect")).ui(|ui| {
                            button(ui, &t("collab.join.connect"), ButtonVariant::Primary, true)
                        });
                        #[cfg(test)]
                        super::ui_probe::record("collab.join.connect", connect_button.rect);
                        if connect_button.clicked() {
                            connect = true;
                        }
                    });
                if let Some(progress) = progress_text(collab.join) {
                    tree.id(base.with("progress")).ui(|ui| ui.label(progress));
                }
                tree.id(base.with("separator_bottom"))
                    .ui(|ui| ui.separator());
                if tree
                    .id(base.with("cancel"))
                    .ui(|ui| button(ui, &t("collab.join.cancel"), ButtonVariant::Secondary, true))
                    .clicked()
                {
                    cancel = true;
                }
            });
        });
        if cancel {
            self.collab.close_join();
            return;
        }
        if let Some(index) = pick
            && let Some(service) = services.get(index).cloned()
            && let Some(toast) = self.collab.join_discovered(&service)
        {
            self.toast_error(toast);
        }
        if connect && let Some(toast) = self.collab.join_manual() {
            self.toast_error(toast);
        }
    }

    fn collab_users_dialog(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        let users = self.collab.users().to_vec();
        egui::Modal::new(egui::Id::new("collab-users-dialog")).show(ui.ctx(), |ui| {
            ui.heading(t("collab.users.title"));
            dialog_column_auto(ui, "collab_users_dialog", |tree| {
                let base = tree.current_id();
                tree.id(base.with("count")).ui(|ui| {
                    ui.label(typ_edit_i18n::t_with(
                        "collab.users.count",
                        &[("count", &users.len().to_string())],
                    ));
                });
                tree.id(base.with("separator_top")).ui(|ui| ui.separator());
                for (index, user) in users.iter().enumerate() {
                    tree.id(base.with(("user", index)))
                        .ui(|ui| ui.label(format!("{} · {}", user.sid, user.user)));
                }
                tree.id(base.with("separator_bottom"))
                    .ui(|ui| ui.separator());
                if tree
                    .id(base.with("close"))
                    .ui(|ui| button(ui, &t("collab.users.close"), ButtonVariant::Secondary, true))
                    .clicked()
                {
                    close = true;
                }
            });
        });
        if close {
            self.collab.close_users();
        }
    }

    fn collab_info_dialog(&mut self, ui: &mut egui::Ui) {
        let mut disconnect = false;
        let mut close = false;
        let gap = ui.spacing().item_spacing.x;
        let status = self.collab.status_text();
        egui::Modal::new(egui::Id::new("collab-info-dialog")).show(ui.ctx(), |ui| {
            ui.heading(t("collab.info.title"));
            dialog_column_auto(ui, "collab_info_dialog", |tree| {
                let base = tree.current_id();
                if let Some(status) = &status {
                    tree.id(base.with("status")).ui(|ui| ui.label(status));
                }
                tree.id(base.with("separator")).ui(|ui| ui.separator());
                tree.id(base.with("actions"))
                    .style(tui::row_gap(gap))
                    .add(|row| {
                        if row
                            .id(base.with("disconnect"))
                            .ui(|ui| {
                                button(
                                    ui,
                                    &t("collab.command.disconnect"),
                                    ButtonVariant::Danger,
                                    true,
                                )
                            })
                            .clicked()
                        {
                            disconnect = true;
                        }
                        if row
                            .id(base.with("close"))
                            .ui(|ui| {
                                button(ui, &t("collab.users.close"), ButtonVariant::Secondary, true)
                            })
                            .clicked()
                        {
                            close = true;
                        }
                    });
            });
        });
        if disconnect {
            self.collab.request_disconnect();
        }
        if close {
            self.collab.close_info();
        }
    }

    fn collab_confirm_dialog(&mut self, ui: &mut egui::Ui, prompt: &str, action: Confirm) {
        let mut yes = false;
        let mut no = false;
        let gap = ui.spacing().item_spacing.x;
        egui::Modal::new(egui::Id::new("collab-confirm-dialog")).show(ui.ctx(), |ui| {
            ui.heading(t("collab.menu.title"));
            dialog_column_auto(ui, "collab_confirm_dialog", |tree| {
                let base = tree.current_id();
                tree.id(base.with("prompt")).ui(|ui| ui.label(t(prompt)));
                tree.id(base.with("separator")).ui(|ui| ui.separator());
                tree.id(base.with("actions"))
                    .style(tui::row_gap(gap))
                    .add(|row| {
                        if row
                            .id(base.with("yes"))
                            .ui(|ui| {
                                button(ui, &t("collab.confirm.yes"), ButtonVariant::Danger, true)
                            })
                            .clicked()
                        {
                            yes = true;
                        }
                        if row
                            .id(base.with("no"))
                            .ui(|ui| {
                                button(ui, &t("collab.confirm.no"), ButtonVariant::Secondary, true)
                            })
                            .clicked()
                        {
                            no = true;
                        }
                    });
            });
        });
        if no {
            self.collab.cancel_stop();
            self.collab.cancel_disconnect();
        }
        if yes {
            match action {
                Confirm::Stop => self.collab.stop_server(),
                Confirm::Disconnect => self.collab.disconnect(),
            }
        }
    }

    fn collab_password_dialog(&mut self, ui: &mut egui::Ui) {
        let mut confirm = false;
        let mut cancel = false;
        let gap = ui.spacing().item_spacing.x;
        let input_width = ui.spacing().text_edit_width.max(160.0);
        let collab = &mut self.collab;
        egui::Modal::new(egui::Id::new("collab-password-dialog")).show(ui.ctx(), |ui| {
            ui.heading(t("collab.join.password_prompt"));
            dialog_column_auto(ui, "collab_password_dialog", |tree| {
                let base = tree.current_id();
                tree.id(base.with("password_input"))
                    .style(fixed_width_style(input_width))
                    .ui(|ui| {
                        form_password(ui, &mut collab.join_password, "", input_width);
                    });
                tree.id(base.with("actions"))
                    .style(tui::row_gap(gap))
                    .add(|row| {
                        if row
                            .id(base.with("connect"))
                            .ui(|ui| {
                                button(ui, &t("collab.join.connect"), ButtonVariant::Primary, true)
                            })
                            .clicked()
                        {
                            confirm = true;
                        }
                        if row
                            .id(base.with("cancel"))
                            .ui(|ui| {
                                button(ui, &t("collab.users.close"), ButtonVariant::Secondary, true)
                            })
                            .clicked()
                        {
                            cancel = true;
                        }
                    });
            });
        });
        let _ = collab;
        if cancel {
            self.collab.cancel_password_prompt();
        }
        if confirm && let Some(toast) = self.collab.confirm_password_prompt() {
            self.toast_error(toast);
        }
    }
}

/// Opens one top-level tui column inside an auto-sized dialog window. The
/// `MaxContent` available width keeps the former content-driven window width
/// (a definite full width would stretch the dialog to the default area size).
fn dialog_column_auto(ui: &mut egui::Ui, salt: &'static str, add: impl FnOnce(&mut Tui)) {
    let base = ui.id().with(salt);
    let style = tui::column_gap(ui.spacing().item_spacing.y);
    tui::tui(ui, base)
        .with_available_space(Size {
            width: AvailableSpace::MaxContent,
            height: AvailableSpace::MinContent,
        })
        .style(style)
        .show(add);
}

/// A leaf pinned to `width` (auto height), used for text inputs and dialog rows
/// inside an auto-sized dialog: an auto-width leaf measures a text input at its
/// 24px minimum, so input-bearing content carries a definite width instead.
fn fixed_width_style(width: f32) -> Style {
    Style {
        size: Size {
            width: Dimension::length(width),
            height: Dimension::auto(),
        },
        ..Default::default()
    }
}

/// A `flexible` leaf with a minimum main-axis size: in an auto-width row this
/// gives a text input its former desired width (a bare `flexible` leaf measures
/// its input at the 24px minimum inside a shrink-to-fit dialog).
fn flexible_with_min_width(width: f32) -> Style {
    let mut style = tui::flexible();
    style.min_size.width = Dimension::length(width);
    style
}

#[derive(Clone, Copy)]
enum Confirm {
    Stop,
    Disconnect,
}
