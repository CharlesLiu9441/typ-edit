//! Keyboard navigation for the project tree.

use std::path::PathBuf;

use super::state::ProjectTree;
use super::{RowRef, TREE_FOCUS_ID, TreeAction, parent_row, step_row};

impl ProjectTree {
    /// Handles keyboard navigation while the tree pane is hovered or focused.
    ///
    /// Returns the path to scroll to, when a key moved the cursor.
    pub(super) fn handle_keys(
        &mut self,
        ui: &mut egui::Ui,
        rows: &[RowRef],
        action: &mut TreeAction,
    ) -> Option<PathBuf> {
        let focus_id = egui::Id::new(TREE_FOCUS_ID);
        let hovered = ui.rect_contains_pointer(ui.max_rect());
        if hovered && ui.input(|i| i.pointer.any_pressed()) {
            ui.ctx().memory_mut(|m| m.request_focus(focus_id));
        }
        let focused = ui.ctx().memory(|m| m.has_focus(focus_id));
        let idle = ui.ctx().memory(|m| m.focused().is_none());
        if !(focused || (hovered && idle)) || rows.is_empty() {
            return None;
        }

        let cursor_index = self
            .cursor
            .as_ref()
            .and_then(|path| rows.iter().position(|row| &row.path == path));
        let mut new_cursor = cursor_index;
        let mut scroll = false;

        let up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
        let down =
            !up && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
        if up || down {
            let delta = if up { -1 } else { 1 };
            let index = match cursor_index {
                Some(index) => step_row(rows, index, delta),
                None if up => rows.len() - 1,
                None => 0,
            };
            new_cursor = Some(index);
            scroll = true;
        }

        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight))
            && let Some(index) = cursor_index
        {
            let row = &rows[index];
            if row.is_dir && !row.expanded {
                self.expanded.insert(row.path.clone());
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft))
            && let Some(index) = cursor_index
        {
            let row = &rows[index];
            if row.is_dir && row.expanded {
                self.expanded.remove(&row.path);
            } else if let Some(parent) = parent_row(rows, index) {
                new_cursor = Some(parent);
                scroll = true;
            }
        }

        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            && let Some(index) = cursor_index.or(new_cursor)
        {
            let row = &rows[index];
            if row.is_dir {
                if !self.expanded.remove(&row.path) {
                    self.expanded.insert(row.path.clone());
                }
            } else {
                *action = TreeAction::OpenFile(row.path.clone());
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F2))
            && let Some(index) = cursor_index.or(new_cursor)
        {
            *action = TreeAction::Rename(rows[index].path.clone());
        }

        let index = new_cursor?;
        let path = rows[index].path.clone();
        if self.cursor.as_deref() != Some(path.as_path()) {
            scroll = true;
        }
        self.cursor = Some(path);
        scroll.then(|| rows[index].path.clone())
    }
}
