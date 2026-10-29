//! Editor context menu for the virtualized text path.
//!
//! The menu only gathers intent; the buffer operations run through
//! [`crate::EditorState::virtual_command`], the same applier the menu bar uses.

use std::ops::Range;

use crate::EditorCommand;
use typ_edit_i18n::t;
use typ_edit_model::SpellIssue;
use typ_edit_widgets::menu::{menu_begin_sized, menu_item, menu_separator};

/// Suggestion rows a spell menu offers at most; also the reserved sizing slots.
const SPELL_SUGGESTION_MAX: usize = 5;

/// One frame's context-menu result: a queued buffer command, a "show in preview"
/// request at the caret, a rename request, a find-references request and/or a
/// spell action.
#[derive(Default)]
pub(super) struct ContextMenu {
    pub command: Option<EditorCommand>,
    pub show_in_preview: Option<usize>,
    pub spell: Option<SpellMenuAction>,
    pub rename: bool,
    pub references: bool,
    /// Whether the menu is open this frame; the editor suppresses hover while it
    /// owns the pointer.
    pub menu_open: bool,
}

/// The rename request byte for a menu result: the caret byte when the
/// 重命名符号 row was chosen, `None` otherwise. Kept pure so the translation
/// can be exercised without driving an egui menu.
pub(super) fn rename_request(menu: &ContextMenu, cursor_byte: usize) -> Option<usize> {
    menu.rename.then_some(cursor_byte)
}

/// The find-references request byte for a menu result: the caret byte when the
/// 查找引用 row was chosen, `None` otherwise. Pure for the same reason as
/// [`rename_request`].
pub(super) fn references_request(menu: &ContextMenu, cursor_byte: usize) -> Option<usize> {
    menu.references.then_some(cursor_byte)
}

/// The hover report for a frame: suppressed while the context menu is open so
/// the menu's own hover feedback is not shadowed by the editor tooltip.
pub(super) fn hover_without_menu(hover_byte: Option<usize>, menu_open: bool) -> Option<usize> {
    if menu_open { None } else { hover_byte }
}

/// A spell action chosen from the context menu. Mirrors [`crate::SpellAction`]
/// but carries the byte range so the editor can replace in place.
pub(super) enum SpellMenuAction {
    Replace { range: Range<usize>, text: String },
    AddToDictionary { word: String },
    Ignore { word: String },
}

/// The spell issue under the right-clicked caret, borrowed from the input.
pub(super) struct SpellMenuData<'a> {
    pub word: &'a str,
    pub suggestions: &'a [String],
    pub range: Range<usize>,
}

/// The spell issue whose byte range contains `cursor_byte` (`start..=end`), if
/// any. A pure lookup so the menu can be exercised without a UI.
pub(super) fn spell_menu_data<'a>(
    issues: &'a [SpellIssue],
    cursor_byte: usize,
) -> Option<SpellMenuData<'a>> {
    issues
        .iter()
        .find(|issue| issue.range.start.0 <= cursor_byte && cursor_byte <= issue.range.end.0)
        .map(|issue| SpellMenuData {
            word: &issue.word,
            suggestions: &issue.suggestions,
            range: issue.range.start.0..issue.range.end.0,
        })
}

/// The `Replace` action for suggestion `index`, or `None` when out of range.
pub(super) fn suggestion_action(
    spell: &SpellMenuData<'_>,
    index: usize,
) -> Option<SpellMenuAction> {
    spell
        .suggestions
        .get(index)
        .map(|text| SpellMenuAction::Replace {
            range: spell.range.clone(),
            text: text.clone(),
        })
}

/// Shows the editor context menu on `response` and reports the chosen action.
/// When `spell` is present its suggestions and the add/ignore items lead the
/// menu, ahead of the editing items.
pub(super) fn show(
    response: &egui::Response,
    has_selection: bool,
    has_text: bool,
    cursor_byte: usize,
    spell: Option<&SpellMenuData<'_>>,
) -> ContextMenu {
    let mut menu = ContextMenu::default();
    let copy = t("menu.edit.copy");
    let cut = t("menu.edit.cut");
    let paste = t("menu.edit.paste");
    let select_all = t("menu.edit.select_all");
    let show_in_preview = t("editor.menu.show_in_preview");
    let rename = t("editor.menu.rename");
    let find_references = t("editor.menu.find_references");
    let add_to_dictionary = t("editor.menu.spell.add_to_dictionary");
    let ignore = t("editor.menu.spell.ignore");
    let open = response.context_menu(|ui| {
        let suggestions = spell
            .map(|spell| &spell.suggestions[..spell.suggestions.len().min(SPELL_SUGGESTION_MAX)])
            .unwrap_or(&[]);
        let mut rows: Vec<(&str, Option<&str>)> = Vec::new();
        rows.extend(
            suggestions
                .iter()
                .map(|suggestion| (suggestion.as_str(), None)),
        );
        if spell.is_some() {
            rows.push((add_to_dictionary.as_ref(), None));
            rows.push((ignore.as_ref(), None));
        }
        rows.push((copy.as_ref(), None));
        rows.push((cut.as_ref(), None));
        rows.push((paste.as_ref(), None));
        rows.push((select_all.as_ref(), None));
        rows.push((show_in_preview.as_ref(), None));
        rows.push((rename.as_ref(), None));
        rows.push((find_references.as_ref(), None));
        menu_begin_sized(ui, &rows, false);

        if let Some(spell) = spell {
            for (index, suggestion) in suggestions.iter().enumerate() {
                if menu_item(ui, suggestion, None, true).clicked() {
                    menu.spell = suggestion_action(spell, index);
                    ui.close();
                }
            }
            menu_separator(ui);
            if menu_item(ui, &add_to_dictionary, None, true).clicked() {
                menu.spell = Some(SpellMenuAction::AddToDictionary {
                    word: spell.word.to_owned(),
                });
                ui.close();
            }
            if menu_item(ui, &ignore, None, true).clicked() {
                menu.spell = Some(SpellMenuAction::Ignore {
                    word: spell.word.to_owned(),
                });
                ui.close();
            }
            menu_separator(ui);
        }

        if menu_item(ui, &copy, None, has_selection).clicked() {
            menu.command = Some(EditorCommand::Copy);
            ui.close();
        }
        if menu_item(ui, &cut, None, has_selection).clicked() {
            menu.command = Some(EditorCommand::Cut);
            ui.close();
        }
        if menu_item(ui, &paste, None, true).clicked() {
            menu.command = Some(EditorCommand::Paste);
            ui.close();
        }
        if menu_item(ui, &select_all, None, has_text).clicked() {
            menu.command = Some(EditorCommand::SelectAll);
            ui.close();
        }
        menu_separator(ui);
        if menu_item(ui, &show_in_preview, None, true).clicked() {
            menu.show_in_preview = Some(cursor_byte);
            ui.close();
        }
        if menu_item(ui, &rename, None, true).clicked() {
            menu.rename = true;
            ui.close();
        }
        if menu_item(ui, &find_references, None, true).clicked() {
            menu.references = true;
            ui.close();
        }
    });
    menu.menu_open = open.is_some();
    menu
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_model::BytePos;

    fn issue(range: Range<usize>, word: &str, suggestions: &[&str]) -> SpellIssue {
        SpellIssue {
            range: BytePos(range.start)..BytePos(range.end),
            word: word.to_owned(),
            suggestions: suggestions.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn spell_menu_data_finds_the_issue_containing_the_byte() {
        let issues = [
            issue(0..4, "helo", &["hello", "held"]),
            issue(6..10, "wrld", &["world"]),
        ];
        let found = spell_menu_data(&issues, 2).expect("inside the first issue");
        assert_eq!(found.word, "helo");
        assert_eq!(found.range, 0..4);
        assert_eq!(found.suggestions.len(), 2);
        assert_eq!(found.suggestions[1], "held");
    }

    #[test]
    fn spell_menu_data_misses_outside_every_issue() {
        let issues = [issue(0..4, "helo", &[])];
        assert!(spell_menu_data(&issues, 6).is_none());
        assert!(spell_menu_data(&[], 0).is_none());
    }

    #[test]
    fn spell_menu_data_clamps_at_the_boundaries() {
        let issues = [issue(3..7, "helo", &[])];
        assert!(spell_menu_data(&issues, 2).is_none());
        assert!(spell_menu_data(&issues, 3).is_some(), "start inclusive");
        assert!(spell_menu_data(&issues, 7).is_some(), "end inclusive");
        assert!(spell_menu_data(&issues, 8).is_none());
    }

    #[test]
    fn suggestion_action_maps_to_a_replace_over_the_issue_range() {
        let issues = [issue(6..10, "helo", &["hello", "held"])];
        let data = spell_menu_data(&issues, 8).expect("issue under the caret");
        let action = suggestion_action(&data, 1).expect("second suggestion");
        match action {
            SpellMenuAction::Replace { range, text } => {
                assert_eq!(range, 6..10, "the replace covers the issue range");
                assert_eq!(text, "held");
            }
            _ => panic!("a suggestion must map to a Replace action"),
        }
        assert!(suggestion_action(&data, 5).is_none());
    }

    #[test]
    fn rename_request_reports_the_caret_only_when_chosen() {
        let mut menu = ContextMenu::default();
        assert_eq!(rename_request(&menu, 7), None);
        menu.rename = true;
        assert_eq!(rename_request(&menu, 7), Some(7));
    }

    #[test]
    fn references_request_reports_the_caret_only_when_chosen() {
        let mut menu = ContextMenu::default();
        assert_eq!(references_request(&menu, 7), None);
        menu.references = true;
        assert_eq!(references_request(&menu, 7), Some(7));
    }

    #[test]
    fn hover_is_suppressed_while_the_menu_is_open() {
        assert_eq!(hover_without_menu(Some(5), false), Some(5));
        assert_eq!(hover_without_menu(Some(5), true), None);
        assert_eq!(hover_without_menu(None, true), None);
    }
}
