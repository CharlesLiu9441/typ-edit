//! Project tree view with file-operation and "set as main file" context menus.
//!
//! The implementation is split by concern across submodules: [`state`] owns [`ProjectTree`]'s
//! state and its cached directory-listing IO, [`keys`] owns keyboard navigation, and [`render`]
//! owns row drawing and context menus.

use std::path::PathBuf;

use egui::RichText;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::menu::{menu_begin_sized, menu_item};
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{self, Tui, TuiBuilderLogic};

mod git_status;
mod keys;
mod render;
mod state;

pub use git_status::{GitTreeStatus, badges_available, repo_relative};
pub use state::ProjectTree;

use render::{TreeDraw, show_dir_children};

/// Height of a single tree row, in points.
const ROW_H: f32 = 22.0;

/// Stable id under which the tree pane holds keyboard focus.
const TREE_FOCUS_ID: &str = "typ-edit-tree-focus";
/// Stable id of the tree's empty background, which owns the root refresh menu.
const TREE_BG_ID: &str = "typ-edit-tree-bg";

/// Icon appearance threaded into the tree from the settings.
#[derive(Clone, Copy, Debug)]
pub struct TreeIconStyle {
    /// Paint directory folder icons filled instead of outlined.
    pub folder_filled: bool,
    /// Explicit folder tint; `None` follows the visuals' hyperlink color.
    pub folder_color: Option<egui::Color32>,
}

/// One visible row of the flattened tree, used for keyboard navigation and rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowRef {
    pub path: PathBuf,
    pub is_dir: bool,
    pub depth: usize,
    /// Only meaningful for directories.
    pub expanded: bool,
}

/// User action performed in the tree.
#[derive(Default)]
pub enum TreeAction {
    #[default]
    None,
    /// Pick a new workspace folder (empty-state button).
    OpenFolder,
    OpenFile(PathBuf),
    SetMainFile(PathBuf),
    ClearMainFile,
    /// Create a file inside this directory.
    NewFile(PathBuf),
    /// Create a folder inside this directory.
    NewFolder(PathBuf),
    /// Rename this path.
    Rename(PathBuf),
    /// Delete this path (`true` = directory).
    Delete(PathBuf, bool),
    /// Re-read this directory's listing.
    RefreshDir(PathBuf),
    /// Show this path in the system file manager (`true` = directory).
    RevealInExplorer(PathBuf, bool),
    /// Open a terminal at this directory.
    OpenInTerminal(PathBuf),
}

impl ProjectTree {
    /// Adds the tree to the left pane's body container: one flexible leaf hosts
    /// the keyboard handling and the virtualized `ScrollArea` rows. Tree rows
    /// stay egui-native (they are virtualized inside the scroll viewport).
    pub fn show(
        &mut self,
        tree: &mut Tui,
        style: TreeIconStyle,
        git: Option<GitTreeStatus<'_>>,
    ) -> TreeAction {
        let mut action = TreeAction::None;
        let Some(root) = self.root.clone() else {
            // Empty state: a centered tui column instead of the former
            // `vertical_centered` block.
            let id = egui::Id::new("typ_edit_tree_empty");
            tree.style(tui::centered_column()).id(id).add(|tree| {
                tree.style(tui::label_leaf())
                    .id(id.with("hint"))
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .ui(|ui| {
                        ui.label(
                            RichText::new(t("tree.empty")).color(crate::theme::text_muted(ui)),
                        );
                    });
                tree.id(id.with("open_folder")).ui(|ui| {
                    if button(ui, &t("tree.open_folder"), ButtonVariant::Cta, true).clicked() {
                        action = TreeAction::OpenFolder;
                    }
                });
            });
            return action;
        };

        let id = egui::Id::new("typ_edit_tree_body");
        tree.style(tui::flexible()).id(id).ui(|ui| {
            let rows = self.visible_rows(&root);
            let scroll_to = self.handle_keys(ui, &rows, &mut action);

            // tui-whitelist: ScrollArea 内的虚拟化行,不可嵌套 tui
            // The tree's own area below the view switcher, not the whole pane:
            // `ui.max_rect()` would cover (and steal) the switcher and its scrollbar.
            let background = ui.interact(
                ui.available_rect_before_wrap(),
                egui::Id::new(TREE_BG_ID),
                egui::Sense::click(),
            );
            let refresh = t("tree.refresh");
            background.context_menu(|ui| {
                menu_begin_sized(ui, &[(refresh.as_ref(), None)], false);
                if menu_item(ui, &refresh, None, true).clicked() {
                    action = TreeAction::RefreshDir(root.clone());
                    ui.close();
                }
            });

            let main = self.main_file.clone();
            let open_file = self.open_file.clone();
            let cursor = self.cursor.clone();
            let mut draw = TreeDraw {
                main: main.as_deref(),
                open_file: open_file.as_deref(),
                cursor: cursor.as_deref(),
                scroll_to: scroll_to.as_deref(),
                style,
                git,
                expanded: &mut self.expanded,
                cache: &mut self.cache,
                action: &mut action,
            };
            ui.scope(|ui| {
                let theme = Theme::from_ui(ui);
                ui.style_mut().visuals.widgets.hovered.weak_bg_fill =
                    theme.colors.widget_hover_fill.gamma_multiply(0.5);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        show_dir_children(ui, &root, &mut draw);
                    });
            });
        });
        action
    }
}

/// Moves a row cursor by `delta`, clamped to the visible rows.
pub fn step_row(rows: &[RowRef], cursor: usize, delta: isize) -> usize {
    if rows.is_empty() {
        return 0;
    }
    (cursor as isize + delta).clamp(0, rows.len() as isize - 1) as usize
}

/// Index of the nearest preceding row at a shallower depth (the row's parent).
pub fn parent_row(rows: &[RowRef], index: usize) -> Option<usize> {
    let depth = rows.get(index)?.depth;
    if depth == 0 {
        return None;
    }
    (0..index).rev().find(|&i| rows[i].depth < depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, is_dir: bool, depth: usize, expanded: bool) -> RowRef {
        RowRef {
            path: PathBuf::from(path),
            is_dir,
            depth,
            expanded,
        }
    }

    #[test]
    fn step_row_clamps_to_the_visible_rows() {
        let rows = vec![
            row("a", true, 0, false),
            row("b", false, 0, false),
            row("c", false, 0, false),
        ];
        assert_eq!(step_row(&rows, 0, 1), 1);
        assert_eq!(step_row(&rows, 2, 1), 2, "cannot move past the last row");
        assert_eq!(step_row(&rows, 0, -1), 0, "cannot move before the first");
        assert_eq!(step_row(&rows, 1, -5), 0);
        assert_eq!(step_row(&[], 3, 1), 0, "an empty list has no rows");
    }

    #[test]
    fn parent_row_finds_the_enclosing_directory_row() {
        let rows = vec![
            row("a", true, 0, true),
            row("a/b", true, 1, true),
            row("a/b/c.typ", false, 2, false),
        ];
        assert_eq!(parent_row(&rows, 2), Some(1));
        assert_eq!(parent_row(&rows, 1), Some(0));
        assert_eq!(parent_row(&rows, 0), None, "a top-level row has no parent");
        assert_eq!(parent_row(&rows, 9), None, "out-of-range is rejected");
    }
}
