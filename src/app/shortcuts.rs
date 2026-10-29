use super::state::UiState;
use crate::commands::{COMMANDS, CommandId};
use crate::keybindings::KeyBindings;
use std::sync::OnceLock;
use typ_edit_i18n::t;
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

/// One row of the shortcuts overview.
///
/// `keys` and `description` are i18n keys (a literal key like `"Ctrl+S"` falls
/// through to itself); `group` is an i18n key or `None` for a dispatch-only
/// binding that fires an action without being listed (e.g. `Ctrl+R`). When
/// `commands` is non-empty the key column renders the live bindings from
/// [`crate::keybindings::KeyBindings`]; otherwise it is a reference-only row
/// for an editor-local shortcut rendered from [`Self::keys`].
struct ShortcutRow {
    keys: &'static str,
    description: &'static str,
    group: Option<&'static str>,
    commands: Vec<CommandId>,
}

/// An editor-local reference row (a shortcut the editor consumes itself) with
/// its fixed display position in the derived overview.
struct EditorShortcutRow {
    row: u16,
    keys: &'static str,
    description: &'static str,
    group: &'static str,
}

/// Editor-local reference rows that have no registry command.
const EDITOR_SHORTCUT_ROWS: &[EditorShortcutRow] = &[
    EditorShortcutRow {
        row: 10,
        keys: "Tab / Shift+Tab",
        description: "shortcut.indent",
        group: "shortcut.group.editor",
    },
    EditorShortcutRow {
        row: 11,
        keys: "Ctrl+Space",
        description: "shortcut.complete",
        group: "shortcut.group.editor",
    },
    EditorShortcutRow {
        row: 12,
        keys: "F3 / Shift+F3",
        description: "shortcut.find_next",
        group: "shortcut.group.editor",
    },
    EditorShortcutRow {
        row: 13,
        keys: "shortcut.key.ctrl_click",
        description: "shortcut.goto_definition",
        group: "shortcut.group.editor",
    },
    EditorShortcutRow {
        row: 14,
        keys: "Enter",
        description: "shortcut.smart_indent",
        group: "shortcut.group.editor",
    },
    EditorShortcutRow {
        row: 15,
        keys: "shortcut.key.auto",
        description: "shortcut.auto_pair",
        group: "shortcut.group.editor",
    },
    EditorShortcutRow {
        row: 16,
        keys: "Enter / Shift+Enter",
        description: "shortcut.next_match",
        group: "shortcut.group.find",
    },
    EditorShortcutRow {
        row: 17,
        keys: "shortcut.key.case_sensitive",
        description: "shortcut.toggle_case",
        group: "shortcut.group.find",
    },
    EditorShortcutRow {
        row: 18,
        keys: "\\n",
        description: "shortcut.newline_in_find",
        group: "shortcut.group.find",
    },
    EditorShortcutRow {
        row: 19,
        keys: "shortcut.key.ctrl_wheel",
        description: "shortcut.zoom",
        group: "shortcut.group.preview",
    },
    EditorShortcutRow {
        row: 21,
        keys: "shortcut.key.drag",
        description: "shortcut.pan",
        group: "shortcut.group.preview",
    },
    EditorShortcutRow {
        row: 22,
        keys: "shortcut.key.click",
        description: "shortcut.jump_source",
        group: "shortcut.group.preview",
    },
];

/// Every shortcut documented by the overview, in display order: the command
/// rows derived from [`COMMANDS`] shortcut metadata (merged by row number, each
/// row's commands ordered by their within-row position) plus the editor-local
/// reference table. The two sources' row numbers are disjoint, so the merged
/// list is sorted by row to restore the legacy display order. Dispatch-only
/// rows (`group: None`) are not rendered; the live command bindings drive
/// `handle_shortcuts`.
type CommandRow = (
    u16,
    Option<&'static str>,
    &'static str,
    Vec<(u8, CommandId)>,
);

fn shortcut_rows() -> &'static [ShortcutRow] {
    static ROWS: OnceLock<Vec<ShortcutRow>> = OnceLock::new();
    ROWS.get_or_init(|| {
        let mut command_rows: Vec<CommandRow> = Vec::new();
        for meta in COMMANDS {
            let Some(display) = &meta.shortcut else {
                continue;
            };
            match command_rows
                .iter_mut()
                .find(|(row, ..)| *row == display.row)
            {
                Some((_, _, _, commands)) => commands.push((display.order, meta.id)),
                None => command_rows.push((
                    display.row,
                    display.group,
                    display.description,
                    vec![(display.order, meta.id)],
                )),
            }
        }
        let mut rows: Vec<(u16, ShortcutRow)> = command_rows
            .into_iter()
            .map(|(row, group, description, mut commands)| {
                commands.sort_by_key(|(order, _)| *order);
                (
                    row,
                    ShortcutRow {
                        keys: "",
                        description,
                        group,
                        commands: commands.into_iter().map(|(_, id)| id).collect(),
                    },
                )
            })
            .collect();
        rows.extend(EDITOR_SHORTCUT_ROWS.iter().map(|local| {
            (
                local.row,
                ShortcutRow {
                    keys: local.keys,
                    description: local.description,
                    group: Some(local.group),
                    commands: Vec::new(),
                },
            )
        }));
        rows.sort_by_key(|(row, _)| *row);
        rows.into_iter().map(|(_, row)| row).collect()
    })
}

/// The key column of one overview row: the live bindings for command rows,
/// the reference label for editor-local rows. Reference labels keep the
/// canonical `Ctrl+` spelling in the table and are localized for display.
fn shortcut_row_keys(row: &ShortcutRow, bindings: &KeyBindings) -> String {
    if row.commands.is_empty() {
        let keys = t(row.keys);
        return crate::commands::localize_shortcut(keys.as_ref());
    }
    row.commands
        .iter()
        .filter_map(|&id| bindings.label(id))
        .collect::<Vec<_>>()
        .join(" / ")
}

impl UiState {
    /// Read-only overview of every implemented shortcut (Help → Keyboard Shortcuts).
    pub(super) fn shortcuts_window(&mut self, ctx: &egui::Context, bindings: &KeyBindings) {
        let mut open = self.shortcuts_open;
        egui::Modal::new(egui::Id::new("shortcut-window")).show(ctx, |ui| {
            ui.set_min_width(460.0);
            ui.horizontal(|ui| {
                ui.heading(t("shortcut.window_title"));
                if ui.button("×").clicked() {
                    open = false;
                }
            });
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(520.0)
                .show(ui, |ui| {
                    // Fill the window so the scroll bar hugs its edge.
                    ui.set_min_width(ui.available_width());
                    let theme = Theme::from_ui(ui);
                    // The listed rows are contiguous per group in table order.
                    let listed: Vec<&ShortcutRow> = shortcut_rows()
                        .iter()
                        .filter(|row| row.group.is_some())
                        .collect();
                    let mut start = 0;
                    let mut group_index = 0usize;
                    while start < listed.len() {
                        let title = listed[start].group.unwrap_or_default();
                        let end = start
                            + listed[start..]
                                .iter()
                                .take_while(|row| row.group == Some(title))
                                .count();
                        // The keys column is as wide as the widest chip of
                        // the group (the legacy striped-table behaviour).
                        let rows: Vec<(&ShortcutRow, String)> = listed[start..end]
                            .iter()
                            .map(|row| (*row, shortcut_row_keys(row, bindings)))
                            .collect();
                        let keys_width = rows
                            .iter()
                            .map(|(_, keys)| {
                                ui.painter()
                                    .layout_no_wrap(
                                        keys.clone(),
                                        egui::FontId::monospace(13.0),
                                        egui::Color32::PLACEHOLDER,
                                    )
                                    .size()
                                    .x
                            })
                            .fold(0.0f32, f32::max)
                            + 12.0;
                        let base = ui.id().with(("typ_edit_shortcuts", group_index));
                        let item_gap = ui.spacing().item_spacing.y;
                        let mut root_style = tui::column_gap(item_gap);
                        tui::fill_width(&mut root_style);
                        let width = ui.available_width();
                        tui::tui(ui, base)
                            .reserve_width(width)
                            .style(root_style)
                            .show(|tree| {
                                tree.id(base.with("title")).label(
                                    egui::RichText::new(t(title))
                                        .strong()
                                        .color(theme.colors.text_strong),
                                );
                                tree.id(base.with("separator")).ui(separator);
                                tree.id(base.with("rows")).style(tui::column_gap(4.0)).add(
                                    |rows_tree| {
                                        for (offset, (row, keys)) in rows.iter().enumerate() {
                                            let row_id = base.with(("row", offset));
                                            rows_tree.id(row_id).style(tui::row_gap(24.0)).add(
                                                |row_tree| {
                                                    row_tree
                                                        .id(row_id.with("keys"))
                                                        .style(tui::fixed_main(keys_width))
                                                        .ui(|ui| {
                                                            egui::Frame::new()
                                                                .fill(theme.colors.band)
                                                                .corner_radius(
                                                                    egui::CornerRadius::same(
                                                                        theme.radii.small,
                                                                    ),
                                                                )
                                                                .inner_margin(
                                                                    egui::Margin::symmetric(6, 1),
                                                                )
                                                                .show(ui, |ui| {
                                                                    ui.label(
                                                                        egui::RichText::new(keys)
                                                                            .monospace()
                                                                            .size(13.0)
                                                                            .color(
                                                                                theme.colors.text,
                                                                            ),
                                                                    );
                                                                });
                                                        });
                                                    row_tree
                                                        .id(row_id.with("description"))
                                                        .style(tui::flexible())
                                                        .ui(|ui| {
                                                            ui.label(
                                                                egui::RichText::new(t(
                                                                    row.description
                                                                ))
                                                                .color(theme.colors.text),
                                                            );
                                                        });
                                                },
                                            );
                                        }
                                    },
                                );
                            });
                        ui.add_space(8.0);
                        group_index += 1;
                        start = end;
                    }
                });
        });
        self.shortcuts_open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Editor-local `keys` values that are intentionally literal display text
    /// (physical key names), not i18n keys. `t()` echoes a missing key, which is
    /// exactly the intended rendering for these; everything else must resolve.
    const LITERAL_KEY_LABELS: [&str; 6] = [
        "Tab / Shift+Tab",
        "Ctrl+Space",
        "F3 / Shift+F3",
        "Enter",
        "Enter / Shift+Enter",
        "\\n",
    ];

    #[test]
    fn shortcut_rows_match_the_legacy_snapshot() {
        let expected: &[(&[CommandId], &str, &str, Option<&str>)] = &[
            (
                &[CommandId::Save],
                "",
                "shortcut.save",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::SaveAs],
                "",
                "shortcut.save_as",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::FormatDocument],
                "",
                "shortcut.format",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::Find],
                "",
                "shortcut.find",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::CloseTab],
                "",
                "shortcut.close_tab",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::NextTab, CommandId::PreviousTab],
                "",
                "shortcut.cycle_tab",
                Some("shortcut.group.general"),
            ),
            (
                &[
                    CommandId::ToggleTree,
                    CommandId::ToggleEditor,
                    CommandId::TogglePreview,
                ],
                "",
                "shortcut.toggle_pane",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::OpenSettings],
                "",
                "shortcut.open_settings",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::ShowCommandPalette],
                "",
                "menu.help.command_palette",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::ResetView],
                "",
                "shortcut.reset_view",
                Some("shortcut.group.general"),
            ),
            (
                &[CommandId::ShowShortcuts],
                "",
                "menu.help.shortcuts",
                Some("shortcut.group.general"),
            ),
            (
                &[],
                "Tab / Shift+Tab",
                "shortcut.indent",
                Some("shortcut.group.editor"),
            ),
            (
                &[],
                "Ctrl+Space",
                "shortcut.complete",
                Some("shortcut.group.editor"),
            ),
            (
                &[],
                "F3 / Shift+F3",
                "shortcut.find_next",
                Some("shortcut.group.editor"),
            ),
            (
                &[],
                "shortcut.key.ctrl_click",
                "shortcut.goto_definition",
                Some("shortcut.group.editor"),
            ),
            (
                &[],
                "Enter",
                "shortcut.smart_indent",
                Some("shortcut.group.editor"),
            ),
            (
                &[],
                "shortcut.key.auto",
                "shortcut.auto_pair",
                Some("shortcut.group.editor"),
            ),
            (
                &[],
                "Enter / Shift+Enter",
                "shortcut.next_match",
                Some("shortcut.group.find"),
            ),
            (
                &[],
                "shortcut.key.case_sensitive",
                "shortcut.toggle_case",
                Some("shortcut.group.find"),
            ),
            (
                &[],
                "\\n",
                "shortcut.newline_in_find",
                Some("shortcut.group.find"),
            ),
            (
                &[],
                "shortcut.key.ctrl_wheel",
                "shortcut.zoom",
                Some("shortcut.group.preview"),
            ),
            (
                &[CommandId::ZoomIn, CommandId::ZoomOut],
                "",
                "shortcut.zoom",
                Some("shortcut.group.preview"),
            ),
            (
                &[],
                "shortcut.key.drag",
                "shortcut.pan",
                Some("shortcut.group.preview"),
            ),
            (
                &[],
                "shortcut.key.click",
                "shortcut.jump_source",
                Some("shortcut.group.preview"),
            ),
            (&[CommandId::Recompile], "", "shortcut.recompile", None),
        ];

        let rows = shortcut_rows();
        assert_eq!(rows.len(), expected.len(), "row count");
        for (index, (row, (commands, keys, description, group))) in
            rows.iter().zip(expected).enumerate()
        {
            assert_eq!(row.commands.as_slice(), *commands, "row {index} commands");
            assert_eq!(row.keys, *keys, "row {index} keys");
            assert_eq!(row.description, *description, "row {index} description");
            assert_eq!(row.group, *group, "row {index} group");
        }
    }

    #[test]
    fn shortcut_rows_key_tables_all_resolve() {
        for row in shortcut_rows() {
            assert_ne!(
                &*t(row.description),
                row.description,
                "unresolved description key: {}",
                row.description
            );
            if let Some(group) = row.group {
                assert_ne!(&*t(group), group, "unresolved group key: {group}");
            }
            if row.commands.is_empty() && !LITERAL_KEY_LABELS.contains(&row.keys) {
                assert_ne!(&*t(row.keys), row.keys, "unresolved keys key: {}", row.keys);
            }
        }
    }
}
