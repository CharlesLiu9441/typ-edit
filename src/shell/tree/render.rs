//! Row rendering, painting and context menus for the project tree.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::{Atom, AtomKind, Atoms, Button, CornerRadius, RichText, Stroke, StrokeKind};
use typ_edit_i18n::t;
use typ_edit_widgets::menu::{menu_begin_sized, menu_item, menu_separator};
use typ_edit_widgets::theme::Theme;

use super::git_status::{self, GitTreeStatus};
use super::state::{DirCache, cached_entries, display_name};
use super::{ROW_H, TreeAction, TreeIconStyle};
use typ_edit_git::StatusKind;

/// Shared state threaded through the recursive renderer.
pub(super) struct TreeDraw<'a> {
    pub(super) main: Option<&'a Path>,
    pub(super) open_file: Option<&'a Path>,
    pub(super) cursor: Option<&'a Path>,
    pub(super) scroll_to: Option<&'a Path>,
    pub(super) style: TreeIconStyle,
    pub(super) git: Option<GitTreeStatus<'a>>,
    pub(super) expanded: &'a mut HashSet<PathBuf>,
    pub(super) cache: &'a mut DirCache,
    pub(super) action: &'a mut TreeAction,
}

/// Stable id of a row's reserved icon cell; unique per path, so the response can carry its rect.
fn icon_cell_id(path: &Path) -> egui::Id {
    egui::Id::new(("tree_row_icon", path))
}

/// Width of a directory row's reserved chevron cell, in points.
const CHEVRON_W: f32 = 12.0;

/// Width of a row's trailing status-badge cell, in points.
const BADGE_W: f32 = 16.0;

/// Status of a row: a file's own status, or a directory's most severe
/// descendant. `None` while git is off or the path is outside the repository.
fn row_status(draw: &TreeDraw<'_>, path: &Path, is_dir: bool) -> Option<StatusKind> {
    let git = draw.git?;
    if is_dir {
        git_status::dir_status(git.repo_root, git.statuses, path)
    } else {
        git_status::status_for(git.repo_root, git.statuses, path)
    }
}

/// Paints the status badge in a row's trailing cell and attaches its tooltip.
fn paint_status_badge(ui: &mut egui::Ui, row: egui::Rect, path: &Path, kind: StatusKind) {
    let theme = Theme::from_ui(ui);
    let color = match kind {
        StatusKind::Modified => theme.colors.warn,
        StatusKind::Added => theme.colors.success,
        StatusKind::Deleted => theme.colors.error,
        StatusKind::Untracked => theme.colors.text_muted,
    };
    let rect = egui::Rect::from_min_max(
        egui::pos2(row.right() - BADGE_W, row.top()),
        egui::pos2(row.right(), row.bottom()),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        git_status::badge(kind),
        egui::TextStyle::Small.resolve(ui.style()),
        color,
    );
    ui.interact(
        rect,
        egui::Id::new(("tree_badge", path)),
        egui::Sense::hover(),
    )
    .on_hover_text(t(git_status::tooltip_key(kind)));
}

/// Stable id of a directory row's reserved chevron cell.
fn chevron_cell_id(path: &Path) -> egui::Id {
    egui::Id::new(("tree_row_chevron", path))
}

/// Builds the atoms of a tree row: an optional leading chevron cell (directories only),
/// the reserved icon cell, the label, the grow atom that pins the label to the left, and — for the
/// main file — the trailing star behind the grow atom.
fn row_atoms(
    icon_id: egui::Id,
    chevron_id: Option<egui::Id>,
    label: RichText,
    star: Option<RichText>,
) -> Atoms<'static> {
    let mut atoms = Atoms::new(Atom::custom(
        icon_id,
        egui::Vec2::splat(typ_edit_widgets::icons::ICON_PX),
    ));
    if let Some(chevron_id) = chevron_id {
        atoms.push_left(Atom::custom(
            chevron_id,
            egui::vec2(CHEVRON_W, typ_edit_widgets::icons::ICON_PX),
        ));
    }
    atoms.push_right(label);
    atoms.push_right(Atom::grow());
    if let Some(star) = star {
        atoms.push_right(Atom::from(AtomKind::text(star)));
    }
    atoms
}

/// Draw a full-width, fixed-height tree row.
///
/// Returns the row response, which carries the rects of [`Atom::custom`] cells (the reserved icon
/// cell in particular) so the caller can paint into them.
///
/// `ui` is expected to already be indented to the row's depth (see [`egui::Ui::indent`]), so the
/// row spans from the indentation to the panel's right edge and the whole row is clickable. The
/// label is pinned to the left with [`Atom::grow`], regardless of the enclosing layout.
///
// tui-whitelist: ScrollArea 内的虚拟化行,不可嵌套 tui
fn full_width_row<'a>(
    ui: &mut egui::Ui,
    selected: bool,
    sense: egui::Sense,
    atoms: Atoms<'a>,
) -> egui::AtomLayoutResponse {
    let width = ui.available_width().max(0.0);
    // tui-whitelist: ScrollArea 内的虚拟化行,不可嵌套 tui
    let layout = egui::Layout::centered_and_justified(ui.layout().main_dir());
    ui.allocate_ui_with_layout(egui::vec2(width, ROW_H), layout, |ui| {
        Button::selectable(selected, atoms)
            .sense(sense)
            .wrap_mode(egui::TextWrapMode::Truncate)
            .atom_ui(ui)
    })
    .inner
}

/// Paints the keyboard cursor's subtle selection background behind the next row.
///
/// Called before the row is allocated, so the (frame-less when inactive) row button keeps the text
/// readable on top.
fn paint_cursor_background(ui: &egui::Ui, is_cursor: bool) {
    if !is_cursor {
        return;
    }
    let theme = Theme::from_ui(ui);
    let rect = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(ui.available_width().max(0.0), ROW_H),
    );
    let radius = CornerRadius::same(theme.radii.control);
    ui.painter().rect_filled(rect, radius, theme.colors.band);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(theme.strokes.card, theme.colors.accent),
        egui::StrokeKind::Inside,
    );
}

/// Recursively renders the children of `dir`.
///
/// Directories come first, then files, case-insensitively alphabetical. Directory contents come
/// from the per-directory cache, so no filesystem call is made on a steady frame. Directory rows
/// are never drawn as selected — expansion is shown by the leading chevron and the open/closed
/// folder icon — except when the keyboard cursor sits on them, while hover feedback stays.
/// `cursor` is that keyboard cursor,
/// `open_file` the file currently open in the editor (its row gets an outline).
pub(super) fn show_dir_children(ui: &mut egui::Ui, dir: &Path, draw: &mut TreeDraw<'_>) {
    let Some(entries) = cached_entries(draw.cache, dir) else {
        ui.label(RichText::new(t("tree.unreadable")).color(Theme::from_ui(ui).colors.text_muted));
        return;
    };
    if entries.is_empty() {
        ui.label(RichText::new(t("tree.empty_dir")).color(Theme::from_ui(ui).colors.text_muted));
        return;
    }

    let theme = Theme::from_ui(ui);
    let accent = theme.colors.accent;

    for (path, is_dir) in entries.iter() {
        let is_dir = *is_dir;
        // Path-scoped id: keeps row ids stable when items above expand/collapse, which is what
        // egui's debug "changed id between passes" red-rect diagnostic trips on.
        ui.push_id(path.as_path(), |ui| {
            let name = display_name(path);
            let icon_id = icon_cell_id(path);
            let is_cursor = draw.cursor == Some(path.as_path());
            let row_rect = egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(ui.available_width().max(0.0), ROW_H),
            );
            // Skip off-screen rows, but still scroll to the keyboard target and recurse into an
            // expanded directory, whose children may be visible even when its own row is not.
            if !ui.is_rect_visible(row_rect.expand2(egui::vec2(0.0, ROW_H))) {
                if is_cursor && draw.scroll_to == Some(path.as_path()) {
                    ui.scroll_to_rect(row_rect, Some(egui::Align::Center));
                }
                ui.allocate_space(egui::vec2(ui.available_width().max(0.0), ROW_H));
                if is_dir && draw.expanded.contains(path) {
                    ui.indent(path.as_path(), |ui| {
                        show_dir_children(ui, path, draw);
                    });
                }
                return;
            }
            if is_dir {
                let is_expanded = draw.expanded.contains(path);
                let chevron_id = chevron_cell_id(path);
                paint_cursor_background(ui, is_cursor);
                let response = full_width_row(
                    ui,
                    false,
                    egui::Sense::click(),
                    row_atoms(icon_id, Some(chevron_id), RichText::new(name), None),
                );
                if is_cursor && draw.scroll_to == Some(path.as_path()) {
                    ui.scroll_to_rect(response.response.rect, Some(egui::Align::Center));
                }
                if let Some(rect) = response.rect(chevron_id) {
                    typ_edit_widgets::icons::chevron(
                        ui,
                        rect,
                        is_expanded,
                        theme.colors.text_muted,
                    );
                }
                // The open/closed folder icon mirrors the chevron's expansion state.
                if let Some(rect) = response.rect(icon_id) {
                    let color = draw.style.folder_color.unwrap_or(accent);
                    typ_edit_widgets::icons::folder(
                        ui,
                        rect,
                        is_expanded,
                        draw.style.folder_filled,
                        color,
                    );
                }
                let response = response.response;
                if let Some(kind) = row_status(draw, path, true) {
                    paint_status_badge(ui, response.rect, path, kind);
                }
                if response.clicked() {
                    if is_expanded {
                        draw.expanded.remove(path);
                    } else {
                        draw.expanded.insert(path.clone());
                    }
                }
                let new_file = t("tree.new_file");
                let new_folder = t("tree.new_folder");
                let reveal = t("tree.reveal");
                let terminal = t("tree.terminal");
                let refresh = t("tree.refresh");
                let rename = t("tree.rename");
                let delete = t("tree.delete");
                response.context_menu(|ui| {
                    menu_begin_sized(
                        ui,
                        &[
                            (new_file.as_ref(), None),
                            (new_folder.as_ref(), None),
                            (reveal.as_ref(), None),
                            (terminal.as_ref(), None),
                            (refresh.as_ref(), None),
                            (rename.as_ref(), None),
                            (delete.as_ref(), None),
                        ],
                        false,
                    );
                    if menu_item(ui, &new_file, None, true).clicked() {
                        *draw.action = TreeAction::NewFile(path.clone());
                        ui.close();
                    }
                    if menu_item(ui, &new_folder, None, true).clicked() {
                        *draw.action = TreeAction::NewFolder(path.clone());
                        ui.close();
                    }
                    menu_separator(ui);
                    if menu_item(ui, &reveal, None, true).clicked() {
                        *draw.action = TreeAction::RevealInExplorer(path.clone(), true);
                        ui.close();
                    }
                    if menu_item(ui, &terminal, None, true).clicked() {
                        *draw.action = TreeAction::OpenInTerminal(path.clone());
                        ui.close();
                    }
                    menu_separator(ui);
                    if menu_item(ui, &refresh, None, true).clicked() {
                        *draw.action = TreeAction::RefreshDir(path.clone());
                        ui.close();
                    }
                    if menu_item(ui, &rename, None, true).clicked() {
                        *draw.action = TreeAction::Rename(path.clone());
                        ui.close();
                    }
                    menu_separator(ui);
                    if menu_item(ui, &delete, None, true).clicked() {
                        *draw.action = TreeAction::Delete(path.clone(), true);
                        ui.close();
                    }
                });
                if is_expanded {
                    ui.indent(path.as_path(), |ui| {
                        show_dir_children(ui, path, draw);
                    });
                }
            } else {
                let is_typ = path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("typ"));
                let is_main = draw.main == Some(path.as_path());
                let is_open = draw.open_file == Some(path.as_path());
                let is_image = crate::media::is_supported_image(path);
                let mut text = RichText::new(name);
                let mut star = None;
                if is_main {
                    text = text.strong().color(accent);
                    star = Some(RichText::new("★").color(accent));
                } else if !is_typ {
                    // Non-Typst files are dimmed but still openable.
                    text = text.color(theme.colors.text_muted);
                }

                paint_cursor_background(ui, is_cursor);
                let sense = if is_image {
                    egui::Sense::click_and_drag()
                } else {
                    egui::Sense::click()
                };
                let response =
                    full_width_row(ui, false, sense, row_atoms(icon_id, None, text, star));
                if is_cursor && draw.scroll_to == Some(path.as_path()) {
                    ui.scroll_to_rect(response.response.rect, Some(egui::Align::Center));
                }
                if let Some(rect) = response.rect(icon_id) {
                    // `None` only when shell extraction fails; the empty cell keeps the label
                    // aligned with the folder rows either way.
                    if let Some(texture) = crate::shell::sysicons::file_icon(ui.ctx(), path) {
                        ui.painter().image(
                            texture.id(),
                            rect.shrink(1.0),
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                }
                let response = response.response;
                let response = if is_main {
                    response.on_hover_text(t("tree.preview_target"))
                } else {
                    response
                };
                if let Some(kind) = row_status(draw, path, false) {
                    paint_status_badge(ui, response.rect, path, kind);
                }
                if is_open {
                    // Outline the row of the file currently open in the editor.
                    ui.painter().rect_stroke(
                        response.rect,
                        CornerRadius::same(theme.radii.control),
                        Stroke::new(theme.strokes.outline, theme.colors.accent),
                        StrokeKind::Inside,
                    );
                }
                if is_image {
                    response.dnd_set_drag_payload(path.clone());
                }
                if response.clicked() {
                    *draw.action = TreeAction::OpenFile(path.clone());
                }
                let set_main = t("tree.set_main");
                let clear_main = t("tree.clear_main");
                let new_file = t("tree.new_file");
                let new_folder = t("tree.new_folder");
                let reveal = t("tree.reveal");
                let terminal = t("tree.terminal");
                let rename = t("tree.rename");
                let delete = t("tree.delete");
                let parent = path.parent().map(Path::to_path_buf);
                response.context_menu(|ui| {
                    // Union of every row that can appear (clear_main only for the
                    // main file; the parent-scoped rows only with a parent) so the
                    // width stays stable when the menu is reopened elsewhere.
                    menu_begin_sized(
                        ui,
                        &[
                            (set_main.as_ref(), None),
                            (clear_main.as_ref(), None),
                            (new_file.as_ref(), None),
                            (new_folder.as_ref(), None),
                            (reveal.as_ref(), None),
                            (terminal.as_ref(), None),
                            (rename.as_ref(), None),
                            (delete.as_ref(), None),
                        ],
                        false,
                    );
                    if menu_item(ui, &set_main, None, true).clicked() {
                        *draw.action = TreeAction::SetMainFile(path.clone());
                        ui.close();
                    }
                    if is_main && menu_item(ui, &clear_main, None, true).clicked() {
                        *draw.action = TreeAction::ClearMainFile;
                        ui.close();
                    }
                    menu_separator(ui);
                    if let Some(parent) = &parent {
                        if menu_item(ui, &new_file, None, true).clicked() {
                            *draw.action = TreeAction::NewFile(parent.clone());
                            ui.close();
                        }
                        if menu_item(ui, &new_folder, None, true).clicked() {
                            *draw.action = TreeAction::NewFolder(parent.clone());
                            ui.close();
                        }
                        menu_separator(ui);
                    }
                    if menu_item(ui, &reveal, None, true).clicked() {
                        *draw.action = TreeAction::RevealInExplorer(path.clone(), false);
                        ui.close();
                    }
                    if let Some(parent) = &parent
                        && menu_item(ui, &terminal, None, true).clicked()
                    {
                        *draw.action = TreeAction::OpenInTerminal(parent.clone());
                        ui.close();
                    }
                    menu_separator(ui);
                    if menu_item(ui, &rename, None, true).clicked() {
                        *draw.action = TreeAction::Rename(path.clone());
                        ui.close();
                    }
                    menu_separator(ui);
                    if menu_item(ui, &delete, None, true).clicked() {
                        *draw.action = TreeAction::Delete(path.clone(), false);
                        ui.close();
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_cell_ids_differ_per_path_and_repeat_per_path() {
        let first = icon_cell_id(Path::new("chapters/one.typ"));
        let second = icon_cell_id(Path::new("chapters/two.typ"));
        assert_ne!(first, second);
        assert_eq!(first, icon_cell_id(Path::new("chapters/one.typ")));
    }

    #[test]
    fn file_row_keeps_the_custom_icon_cell_first() {
        let icon_id = egui::Id::new("test_row_icon");
        let atoms = row_atoms(icon_id, None, RichText::new("name"), None);
        let cell = &atoms[0];
        assert_eq!(cell.id, Some(icon_id));
        assert_eq!(
            cell.size,
            Some(egui::Vec2::splat(typ_edit_widgets::icons::ICON_PX))
        );
        assert!(matches!(&cell.kind, AtomKind::Empty));
    }

    #[test]
    fn directory_row_prepends_a_chevron_cell_before_the_icon_cell() {
        let icon_id = egui::Id::new("test_row_icon");
        let chevron_id = egui::Id::new("test_row_chevron");
        let atoms = row_atoms(icon_id, Some(chevron_id), RichText::new("dir"), None);
        let cell = &atoms[0];
        assert_eq!(cell.id, Some(chevron_id), "the chevron cell leads the row");
        assert!(matches!(&cell.kind, AtomKind::Empty));
        assert_eq!(atoms[1].id, Some(icon_id));
        assert_eq!(atoms.len(), 4);
    }

    #[test]
    fn main_file_row_appends_the_star_behind_the_grow_atom() {
        let atoms = row_atoms(
            egui::Id::new("test_row_icon"),
            None,
            RichText::new("main.typ"),
            Some(RichText::new("★")),
        );
        assert_eq!(atoms.len(), 4);
        assert!(matches!(&atoms[1].kind, AtomKind::Text(_)));
        assert!(
            atoms[2].grow,
            "the label must be pinned left by a grow atom"
        );
        assert!(matches!(&atoms[3].kind, AtomKind::Text(_)));
    }

    #[test]
    fn non_main_rows_end_with_the_grow_atom() {
        let atoms = row_atoms(
            egui::Id::new("test_row_icon"),
            None,
            RichText::new("other.typ"),
            None,
        );
        assert_eq!(atoms.len(), 3);
        assert!(atoms[2].grow);
    }
}
