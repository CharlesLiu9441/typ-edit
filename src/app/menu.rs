use super::dispatch::CommandState;
use super::state::UiState;
use crate::commands::{CommandId, dispatch_order};
use crate::export::ExportFormat;
use crate::keybindings::KeyBindings;
use crate::settings::Settings;
use std::path::PathBuf;
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::menu::{menu_begin_sized, menu_check_item, menu_item, menu_separator};
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{AlignItems, Dimension},
};

/// App-only effects produced by rendering the title-strip menus, applied by the
/// caller after the render returns. At most one field is set per frame (a single
/// click), and they are applied in the order the menu rows were drawn.
#[derive(Default)]
pub(in crate::app) struct MenuOutcome {
    /// A registry command to run.
    pub(in crate::app) command: Option<CommandId>,
    /// A recent project to open.
    pub(in crate::app) open_recent: Option<PathBuf>,
    /// Whether 清除最近项目 was chosen.
    pub(in crate::app) clear_recent: bool,
    /// A direct export to start from the 导出 menu.
    pub(in crate::app) export: Option<ExportFormat>,
    /// Whether the PNG export dialog should open.
    pub(in crate::app) open_png_dialog: bool,
    /// Whether the in-app Typst quick-start window should open.
    pub(in crate::app) typst_help: bool,
}

impl UiState {
    pub(super) fn main_menu(
        &mut self,
        tree: &mut Tui,
        settings: &Settings,
        bindings: &KeyBindings,
        state: &CommandState<'_>,
        has_document: bool,
        exporting: bool,
    ) -> MenuOutcome {
        // One menu container (zero gap: the button padding supplies the visual
        // spacing), one tui leaf per top-level menu. The popup contents stay
        // egui `menu_button` popups, unchanged.
        let mut out = MenuOutcome::default();
        let row_id = tree.current_id().with("menu_buttons");
        let mut style = tui::row();
        style.align_items = Some(AlignItems::Center);
        tree.style(style).id(row_id).add(|tree| {
            menu_buttons(
                tree,
                settings,
                bindings,
                state,
                has_document,
                exporting,
                &mut out,
            );
        });
        out
    }

    /// Consumes the global keyboard shortcuts (Shift-qualified combos first).
    ///
    /// The bindings come from [`crate::keybindings::KeyBindings`]; only the
    /// consume order lives here.
    pub(super) fn handle_shortcuts(
        &mut self,
        ui: &mut egui::Ui,
        bindings: &KeyBindings,
    ) -> Vec<CommandId> {
        let mut commands = Vec::new();
        // The shortcuts settings page is recording a new combo: leave every key
        // event untouched so the capture reads it instead of firing a command.
        if crate::shell::settings_window::is_shortcut_capturing(ui) {
            return commands;
        }
        // While the palette is open it owns every key. This early return runs
        // before the consume below, so exactly one handler sees Ctrl+Shift+P:
        // the palette's own consume in `command_palette`, which closes it.
        if self.palette_open {
            return commands;
        }
        // Closed palette: the sole opener. `run_command` sets `palette_open`,
        // and the event is already consumed, so the palette never double-toggles.
        //
        // `dispatch_order` places `ShowCommandPalette` first, then the legacy
        // dispatch order with Shift-qualified combos before their unshifted
        // counterparts: `consume_key` ignores an extra Shift, so `Ctrl+Shift+S`
        // would otherwise fire plain Save. It covers every recordable command,
        // so a user binding for a previously-unbound command (e.g. OpenFile) is
        // consumed here instead of being silently ignored.
        for id in dispatch_order() {
            if bindings.consume(ui, *id) {
                commands.push(*id);
            }
        }
        commands
    }
}

/// The title strip's menu buttons, one tui leaf each, carrying the menu-bar style.
fn menu_buttons(
    tree: &mut Tui,
    settings: &Settings,
    bindings: &KeyBindings,
    state: &CommandState<'_>,
    has_document: bool,
    exporting: bool,
    out: &mut MenuOutcome,
) {
    let theme = Theme::from_ui(tree.egui_ui());
    let hover = theme.colors.widget_hover_fill.gamma_multiply(0.5);
    let base = tree.current_id().with("menu");
    menu_leaf(tree, base.with("file"), hover, |ui| {
        ui.menu_button(t("menu.file"), |ui| {
            let show_recent = settings.workspace.recent_projects_enabled
                && !settings.workspace.recent_projects.is_empty();
            let mut rows = vec![
                command_row(CommandId::OpenFolder, bindings),
                command_row(CommandId::OpenFile, bindings),
                command_row(CommandId::NewProject, bindings),
            ];
            if show_recent {
                rows.push((t("menu.file.recent").into_owned(), None));
            }
            rows.extend([
                command_row(CommandId::Save, bindings),
                command_row(CommandId::SaveAs, bindings),
                command_row(CommandId::Recompile, bindings),
                command_row(CommandId::Exit, bindings),
            ]);
            menu_begin_sized(ui, &sized_rows(&rows), false);

            command_menu_item(ui, CommandId::OpenFolder, bindings, state, out);
            command_menu_item(ui, CommandId::OpenFile, bindings, state, out);
            command_menu_item(ui, CommandId::NewProject, bindings, state, out);
            if show_recent {
                recent_menu_button(ui, settings, out);
            }
            menu_separator(ui);
            command_menu_item(ui, CommandId::Save, bindings, state, out);
            command_menu_item(ui, CommandId::SaveAs, bindings, state, out);
            menu_separator(ui);
            command_menu_item(ui, CommandId::Recompile, bindings, state, out);
            menu_separator(ui);
            command_menu_item(ui, CommandId::Exit, bindings, state, out);
        });
    });
    menu_leaf(tree, base.with("edit"), hover, |ui| {
        let _edit_menu = ui.menu_button(t("menu.edit"), |ui| {
            let rows = [
                command_row(CommandId::Cut, bindings),
                command_row(CommandId::Copy, bindings),
                command_row(CommandId::Paste, bindings),
                command_row(CommandId::SelectAll, bindings),
                command_row(CommandId::Find, bindings),
                command_row(CommandId::FormatDocument, bindings),
                command_row(CommandId::Undo, bindings),
                command_row(CommandId::Redo, bindings),
            ];
            menu_begin_sized(ui, &sized_rows(&rows), false);

            command_menu_item(ui, CommandId::Cut, bindings, state, out);
            command_menu_item(ui, CommandId::Copy, bindings, state, out);
            command_menu_item(ui, CommandId::Paste, bindings, state, out);
            command_menu_item(ui, CommandId::SelectAll, bindings, state, out);
            menu_separator(ui);
            command_menu_item(ui, CommandId::Find, bindings, state, out);
            menu_separator(ui);
            command_menu_item(ui, CommandId::FormatDocument, bindings, state, out);
            menu_separator(ui);
            command_menu_item(ui, CommandId::Undo, bindings, state, out);
            command_menu_item(ui, CommandId::Redo, bindings, state, out);
        });
        #[cfg(test)]
        crate::app::ui_probe::record("menu.edit.button", _edit_menu.response.rect);
    });
    // The settings entry is a direct action (no popup), unlike its `menu_button` siblings.
    let settings_clicked = menu_leaf(tree, base.with("settings"), hover, |ui| {
        button(
            ui,
            &t(CommandId::OpenSettings.meta().label),
            ButtonVariant::Quiet,
            true,
        )
        .clicked()
    });
    if settings_clicked {
        out.command = Some(CommandId::OpenSettings);
    }
    menu_leaf(tree, base.with("view"), hover, |ui| {
        ui.menu_button(t("menu.view"), |ui| {
            let tree = t("menu.view.tree");
            let editor = t("menu.view.editor");
            let preview = t("menu.view.preview");
            let bookmarks = t("menu.view.bookmarks");
            // Size the popup from every row that can appear, including the
            // shortcut cells. The former label-only list omitted the fold
            // shortcuts, so a long `Ctrl+Shift+OpenBracket` painted past the
            // popup edge. `sized_rows` accounts for the label+shortcut union.
            let rows: Vec<(String, Option<String>)> = vec![
                (tree.to_string(), None),
                (editor.to_string(), None),
                (preview.to_string(), None),
                (bookmarks.to_string(), None),
                command_row(CommandId::FoldAtCaret, bindings),
                command_row(CommandId::UnfoldAtCaret, bindings),
                command_row(CommandId::FoldAll, bindings),
                command_row(CommandId::UnfoldAll, bindings),
            ];
            menu_begin_sized(ui, &sized_rows(&rows), true);
            const VIEW_COMMANDS: [(CommandId, usize); 3] = [
                (CommandId::ToggleTree, 0),
                (CommandId::ToggleEditor, 1),
                (CommandId::TogglePreview, 2),
            ];
            for (id, index) in VIEW_COMMANDS {
                let mut open = !state.layout.collapsed[index];
                let can_toggle = open || state.layout.can_collapse();
                let response = menu_check_item(ui, &mut open, &t(id.meta().label), can_toggle);
                if response.clicked() {
                    out.command = Some(id);
                }
                if !can_toggle {
                    let _ = response.on_disabled_hover_text(t("menu.view.keep_one"));
                }
            }
            menu_separator(ui);
            command_menu_item(ui, CommandId::FoldAtCaret, bindings, state, out);
            command_menu_item(ui, CommandId::UnfoldAtCaret, bindings, state, out);
            menu_separator(ui);
            command_menu_item(ui, CommandId::FoldAll, bindings, state, out);
            command_menu_item(ui, CommandId::UnfoldAll, bindings, state, out);
            menu_separator(ui);
            bookmarks_menu_button(ui, bindings, state, out);
        });
    });
    menu_leaf(tree, base.with("tools"), hover, |ui| {
        ui.menu_button(t("menu.tools"), |ui| {
            let templates = t("menu.tools.templates");
            menu_begin_sized(ui, &[(templates.as_ref(), None)], false);
            command_menu_item(ui, CommandId::OpenTemplateGallery, bindings, state, out);
        });
    });
    menu_leaf(tree, base.with("collab"), hover, |ui| {
        ui.menu_button(t("collab.menu.title"), |ui| {
            let rows = [
                command_row(CommandId::CollabShare, bindings),
                command_row(CommandId::CollabJoin, bindings),
                command_row(CommandId::CollabUsers, bindings),
                command_row(CommandId::CollabStopServer, bindings),
                command_row(CommandId::CollabDisconnect, bindings),
            ];
            menu_begin_sized(ui, &sized_rows(&rows), false);
            command_menu_item(ui, CommandId::CollabShare, bindings, state, out);
            command_menu_item(ui, CommandId::CollabJoin, bindings, state, out);
            menu_separator(ui);
            command_menu_item(ui, CommandId::CollabUsers, bindings, state, out);
            command_menu_item(ui, CommandId::CollabStopServer, bindings, state, out);
            command_menu_item(ui, CommandId::CollabDisconnect, bindings, state, out);
        });
    });
    menu_leaf(tree, base.with("export"), hover, |ui| {
        ui.menu_button(t("menu.export"), |ui| {
            let pdf = t("menu.export.pdf");
            let png = t("menu.export.png");
            let svg = t("menu.export.svg");
            menu_begin_sized(
                ui,
                &[
                    (pdf.as_ref(), None),
                    (png.as_ref(), None),
                    (svg.as_ref(), None),
                ],
                false,
            );
            let enabled = has_document && !exporting;
            if menu_item(ui, &pdf, None, enabled).clicked() {
                ui.close();
                out.export = Some(ExportFormat::Pdf);
            }
            if menu_item(ui, &png, None, enabled).clicked() {
                ui.close();
                out.open_png_dialog = true;
            }
            if menu_item(ui, &svg, None, enabled).clicked() {
                ui.close();
                out.export = Some(ExportFormat::Svg);
            }
            if !has_document {
                ui.label(
                    egui::RichText::new(t("menu.export.no_document"))
                        .color(
                            typ_edit_widgets::theme::Theme::from_ui(ui)
                                .colors
                                .text_muted,
                        )
                        .small(),
                );
            } else if exporting {
                ui.label(
                    egui::RichText::new(t("menu.export.exporting"))
                        .color(
                            typ_edit_widgets::theme::Theme::from_ui(ui)
                                .colors
                                .text_muted,
                        )
                        .small(),
                );
            }
        });
    });
    menu_leaf(tree, base.with("help"), hover, |ui| {
        ui.menu_button(t("menu.help"), |ui| {
            let about = t_with("menu.help.about", &[("version", env!("CARGO_PKG_VERSION"))]);
            let typst = t("menu.help.typst");
            let mut rows: Vec<(String, Option<String>)> = vec![(typst.clone().into_owned(), None)];
            rows.push(command_row(CommandId::ShowCommandPalette, bindings));
            rows.push(command_row(CommandId::ShowShortcuts, bindings));
            rows.push((about.clone(), None));
            menu_begin_sized(ui, &sized_rows(&rows), false);

            if menu_item(ui, &typst, None, true).clicked() {
                ui.close();
                out.typst_help = true;
            }
            menu_separator(ui);
            command_menu_item(ui, CommandId::ShowCommandPalette, bindings, state, out);
            command_menu_item(ui, CommandId::ShowShortcuts, bindings, state, out);
            let _ = menu_item(ui, &about, None, false)
                .on_disabled_hover_text(t("menu.help.about_hint"));
        });
    });
}

/// Adds one top-level menu button as a leaf carrying the menu-bar style.
fn menu_leaf<T>(
    tree: &mut Tui,
    id: egui::Id,
    hover: egui::Color32,
    add: impl FnOnce(&mut egui::Ui) -> T,
) -> T {
    // Short menu labels must never wrap (a wrapped label would double the row
    // height and push the button out of the 28pt title strip), and the natural
    // height keeps the leaf content-sized inside the title row.
    let ui = tree.egui_ui();
    let text_h = ui.text_style_height(&egui::TextStyle::Button);
    // Keep every title-strip leaf tall enough for the token-sized quiet
    // button, whether it hosts a custom button or an egui menu_button.
    let button_h = (text_h + 2.0 * 2.0)
        .max(ui.spacing().interact_size.y)
        .max(Theme::from_ui(ui).spacing.quiet_h);
    // Center the menu button inside the 24pt leaf: egui's menu button is
    // shorter than the leaf by default and would otherwise hug the top edge.
    let pad_y = ((button_h - text_h) * 0.5).max(2.0);
    let mut style = tui::label_leaf();
    style.size.height = Dimension::length(button_h);
    style.align_self = Some(AlignItems::Center);
    tree.style(style)
        .wrap_mode(egui::TextWrapMode::Extend)
        .mut_egui_style(move |style| {
            egui::containers::menu::menu_style(style);
            style.visuals.widgets.hovered.weak_bg_fill = hover;
            style.visuals.widgets.open.weak_bg_fill = hover;
            style.spacing.button_padding = egui::vec2(8.0, pad_y);
        })
        .id(id)
        .ui(add)
}

/// The `(label, shortcut)` pair a command contributes to its menu, used to
/// size a content-sized popup before its rows are drawn.
fn command_row(id: CommandId, bindings: &KeyBindings) -> (String, Option<String>) {
    (
        t(id.meta().label).into_owned(),
        command_shortcut_label(id, bindings),
    )
}

/// Draws one registry command as a menu item and records it on click.
fn command_menu_item(
    ui: &mut egui::Ui,
    id: CommandId,
    bindings: &KeyBindings,
    state: &CommandState<'_>,
    out: &mut MenuOutcome,
) {
    let shortcut = command_shortcut_label(id, bindings);
    let enabled = command_enabled(id, state);
    let response = menu_item(ui, &t(id.meta().label), shortcut.as_deref(), enabled);
    #[cfg(test)]
    if id == CommandId::Find {
        crate::app::ui_probe::record("menu.edit.find", response.rect);
    }
    if response.clicked() {
        ui.close();
        out.command = Some(id);
    }
}

/// The trailing shortcut shown for a command: the live binding when one
/// exists, else the command's display-only static label (localized for the
/// platform, so macOS shows `Cmd+X`).
fn command_shortcut_label(id: CommandId, bindings: &KeyBindings) -> Option<String> {
    bindings.label(id).or_else(|| {
        id.meta()
            .static_keys
            .map(crate::commands::localize_shortcut)
    })
}

/// Whether a command is currently enabled, per its registry predicate.
fn command_enabled(id: CommandId, state: &CommandState<'_>) -> bool {
    id.meta().enabled.is_none_or(|enabled| enabled(state))
}

/// The 最近打开 submenu: the MRU list (capped at the configured limit) plus
/// 清除最近项目. Called only while the feature is enabled.
fn recent_menu_button(ui: &mut egui::Ui, settings: &Settings, out: &mut MenuOutcome) {
    let recents: Vec<PathBuf> = settings.workspace.recent_projects.clone();
    let limit = crate::settings::clamp_recent_limit(settings.workspace.recent_projects_limit);
    let names: Vec<String> = recents
        .iter()
        .take(limit)
        .map(|path| super::files::tree_entry_name(path))
        .collect();
    let clear_label = t("menu.file.recent_clear");
    let mut open: Option<PathBuf> = None;
    let mut clear = false;
    ui.menu_button(t("menu.file.recent"), |ui| {
        let mut rows: Vec<(&str, Option<&str>)> =
            names.iter().map(|name| (name.as_str(), None)).collect();
        rows.push((clear_label.as_ref(), None));
        menu_begin_sized(ui, &rows, false);
        for (path, name) in recents.iter().take(limit).zip(names.iter()) {
            if menu_item(ui, name, None, true)
                .on_hover_text(path.display().to_string())
                .clicked()
            {
                ui.close();
                open = Some(path.clone());
            }
        }
        menu_separator(ui);
        if menu_item(ui, &clear_label, None, true).clicked() {
            ui.close();
            clear = true;
        }
    });
    if let Some(path) = open {
        out.open_recent = Some(path);
    }
    if clear {
        out.clear_recent = true;
    }
}

/// The 书签 submenu: toggle / next / previous / clear all, as registry
/// commands so their shortcut labels match the keybinding table.
fn bookmarks_menu_button(
    ui: &mut egui::Ui,
    bindings: &KeyBindings,
    state: &CommandState<'_>,
    out: &mut MenuOutcome,
) {
    ui.menu_button(t("menu.view.bookmarks"), |ui| {
        let rows = [
            command_row(CommandId::ToggleBookmark, bindings),
            command_row(CommandId::NextBookmark, bindings),
            command_row(CommandId::PreviousBookmark, bindings),
            command_row(CommandId::ClearBookmarks, bindings),
        ];
        menu_begin_sized(ui, &sized_rows(&rows), false);
        command_menu_item(ui, CommandId::ToggleBookmark, bindings, state, out);
        command_menu_item(ui, CommandId::NextBookmark, bindings, state, out);
        command_menu_item(ui, CommandId::PreviousBookmark, bindings, state, out);
        command_menu_item(ui, CommandId::ClearBookmarks, bindings, state, out);
    });
}

/// Borrows owned `(label, shortcut)` pairs as [`menu_begin_sized`] rows.
fn sized_rows(rows: &[(String, Option<String>)]) -> Vec<(&str, Option<&str>)> {
    rows.iter()
        .map(|(label, shortcut)| (label.as_str(), shortcut.as_deref()))
        .collect()
}
