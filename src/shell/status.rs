//! Bottom status bar: compile state, word / char / page counts and caret position.

use egui::RichText;

use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{LengthPercentage, Rect as TaffyRect},
};

/// State of the compile pipeline, shown at the left of the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompileState {
    /// No compile has produced a document yet and none is running.
    #[default]
    Idle,
    /// A compile request is in flight.
    Compiling,
    /// One or more compiler-requested assets are still downloading.
    Fetching,
    /// A document is available and has no errors.
    Ready,
    /// The current document has this many errors.
    Errors(usize),
}

impl CompileState {
    /// Resolves the state from the app's compile flags.
    pub fn resolve(in_flight: bool, has_doc: bool, errors: usize) -> Self {
        if in_flight {
            Self::Compiling
        } else if errors > 0 {
            Self::Errors(errors)
        } else if has_doc {
            Self::Ready
        } else {
            Self::Idle
        }
    }
}

/// Persistent save state of the active buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SaveState {
    /// A pristine unnamed scratch buffer: nothing is on disk yet.
    #[default]
    Untitled,
    /// The active file has no unsaved changes.
    Saved,
    /// The active file has unsaved changes.
    Dirty,
    /// The active file is being formatted and/or written.
    Saving,
}

/// Values displayed in the status bar.
#[derive(Clone, Debug, Default)]
pub struct StatusInfo {
    pub words: usize,
    pub chars: usize,
    pub pages: usize,
    /// 1-based caret line/column.
    pub line: usize,
    pub column: usize,
    /// Number of carets; the count is shown only when it exceeds one.
    pub cursors: usize,
    pub compile: CompileState,
    /// Whether the active buffer is on disk, unsaved, or still untitled.
    pub save_state: SaveState,
    /// Caret-line blame text; `None` when git is off or unavailable.
    pub blame: Option<String>,
    /// Warning shown when a background pipeline stopped permanently; `None`
    /// while every worker is healthy.
    pub health_warning: Option<String>,
    /// git could not be queried (this is the last refresh's first failure);
    /// `None` while git is off, the folder is not a repository, or it is healthy.
    pub git_warning: Option<String>,
    /// LAN collaboration status text; `None` while no session is active.
    pub collab: Option<String>,
    /// Number of active background downloads; zero hides the click action.
    pub background_tasks: usize,
}

/// Maps the active buffer's flags onto the persistent save indicator.
pub fn save_state(dirty: bool, has_path: bool, saving: bool) -> SaveState {
    if saving {
        SaveState::Saving
    } else if dirty {
        SaveState::Dirty
    } else if has_path {
        SaveState::Saved
    } else {
        SaveState::Untitled
    }
}

/// The cursor-count label, or `None` while a single cursor is active (the count
/// is only worth showing when it exceeds one).
fn cursor_count_text(cursors: usize) -> Option<String> {
    (cursors > 1).then(|| t_with("status.cursors", &[("count", &cursors.to_string())]))
}

/// Which status-bar controls were activated this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusAction {
    /// The compile-error indicator was clicked.
    pub errors_clicked: bool,
    /// The collaboration status text was clicked.
    pub collab_clicked: bool,
    /// The background-tasks label was clicked.
    pub background_tasks_clicked: bool,
}

/// Draws the status bar contents (one tui row) into the window's tui tree;
/// reports which controls were clicked.
///
/// The row carries the status card's inner padding: the band above it paints the
/// board and the thin card, and the text has to line up inside that card. It is a
/// non-strategy child of the auto-height band, so padding is safe here (taffy
/// 0.9.2 adds padding to explicitly sized nodes, not to stretched ones).
pub fn show(tree: &mut Tui, info: &StatusInfo, compact: bool) -> StatusAction {
    let theme = Theme::from_ui(tree.egui_ui());
    let muted = theme.colors.text_muted;
    // Status separators are informational chrome, not disabled content: keep
    // them opaque so they never look washed out over the card surface.
    let weak = muted;
    let mut action = StatusAction::default();

    let row_id = tree.current_id().with("status_content");
    let (outer_x, outer_top, outer_bottom) = if compact {
        (0.0, 0.0, 0.0)
    } else {
        (10.0, 5.0, 10.0)
    };
    let mut style = tui::row_gap(4.0);
    style.padding = TaffyRect {
        left: LengthPercentage::length(outer_x + theme.spacing.status_pad_x),
        right: LengthPercentage::length(outer_x + theme.spacing.status_pad_x),
        top: LengthPercentage::length(outer_top + theme.spacing.status_pad_y),
        bottom: LengthPercentage::length(outer_bottom + theme.spacing.status_pad_y),
    };
    tree.style(style).id(row_id).add(|tree| {
        if info.compile != CompileState::Idle {
            tree.style(tui::label_leaf())
                .id(row_id.with("compile"))
                .ui(|ui| {
                    action.errors_clicked =
                        compile_indicator(ui, info.compile).is_some_and(|r| r.clicked());
                });
        }

        match info.save_state {
            SaveState::Untitled => {}
            SaveState::Saved => {
                text_leaf(
                    tree,
                    row_id.with("save_state"),
                    RichText::new(t("status.saved")).small().color(muted),
                );
            }
            SaveState::Dirty => {
                let response = text_leaf(
                    tree,
                    row_id.with("save_state"),
                    RichText::new(t("status.unsaved"))
                        .small()
                        .color(theme.colors.warn_text),
                );
                #[cfg(test)]
                crate::app::ui_probe::record("status.save_state", response.rect);
                #[cfg(not(test))]
                let _ = response;
            }
            SaveState::Saving => {
                let response = text_leaf(
                    tree,
                    row_id.with("save_state"),
                    RichText::new(t("status.saving"))
                        .small()
                        .color(theme.colors.text_muted),
                );
                #[cfg(test)]
                crate::app::ui_probe::record("status.save_state", response.rect);
                #[cfg(not(test))]
                let _ = response;
            }
        }

        if let Some(warning) = &info.health_warning {
            tree.style(tui::flexible())
                .id(row_id.with("health"))
                .ui(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(warning.as_str())
                                .color(Theme::from_ui(ui).colors.warn_text),
                        )
                        .truncate(),
                    );
                });
        }

        if let Some(warning) = &info.git_warning {
            // The only shrinkable left-hand item: a long git error truncates
            // instead of pushing the right-hand statistics off the bar.
            tree.style(tui::flexible())
                .id(row_id.with("git_warning"))
                .ui(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(warning.as_str())
                                .small()
                                .color(Theme::from_ui(ui).colors.warn_text),
                        )
                        .truncate(),
                    );
                });
        }

        // Spring: everything added after this is right-aligned.
        let tasks = info.background_tasks;
        if tasks > 0 {
            tree.style(tui::label_leaf())
                .id(row_id.with("background_tasks"))
                .wrap_mode(egui::TextWrapMode::Extend)
                .ui(|ui| {
                    let label = t_with("status.background_tasks", &[("count", &tasks.to_string())]);
                    let response = ui.add(
                        egui::Label::new(RichText::new(label).small().color(muted))
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .sense(egui::Sense::click()),
                    );
                    if response.clicked() {
                        action.background_tasks_clicked = true;
                    }
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    #[cfg(test)]
                    crate::app::ui_probe::record("status.background_tasks", response.rect);
                });
            separator_leaf(tree, row_id.with("tasks_sep"), weak);
        }

        tree.style(tui::flexible())
            .id(row_id.with("spring"))
            .add_empty();

        if let Some(blame) = &info.blame {
            text_leaf(
                tree,
                row_id.with("blame"),
                RichText::new(blame).small().color(muted),
            );
            separator_leaf(tree, row_id.with("blame_sep"), weak);
        }

        text_leaf(
            tree,
            row_id.with("chars"),
            RichText::new(t_with(
                "status.chars",
                &[("count", &info.chars.to_string())],
            ))
            .small()
            .color(muted),
        );
        separator_leaf(tree, row_id.with("chars_sep"), weak);
        text_leaf(
            tree,
            row_id.with("words"),
            RichText::new(t_with(
                "status.words",
                &[("count", &info.words.to_string())],
            ))
            .small()
            .color(muted),
        );
        separator_leaf(tree, row_id.with("words_sep"), weak);
        text_leaf(
            tree,
            row_id.with("pages"),
            RichText::new(t_with(
                "status.pages",
                &[("count", &info.pages.to_string())],
            ))
            .small()
            .color(muted),
        );

        if let Some(cursors) = cursor_count_text(info.cursors) {
            separator_leaf(tree, row_id.with("cursors_sep"), weak);
            text_leaf(
                tree,
                row_id.with("cursors"),
                RichText::new(cursors).small().color(muted),
            );
        }

        if let Some(collab) = &info.collab {
            separator_leaf(tree, row_id.with("collab_sep"), weak);
            tree.style(tui::label_leaf())
                .id(row_id.with("collab"))
                .wrap_mode(egui::TextWrapMode::Extend)
                .ui(|ui| {
                    let response = ui.add(
                        egui::Label::new(RichText::new(collab).small().color(muted))
                            .sense(egui::Sense::click()),
                    );
                    if response.clicked() {
                        action.collab_clicked = true;
                    }
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                });
        }

        separator_leaf(tree, row_id.with("line_sep"), weak);
        text_leaf(
            tree,
            row_id.with("line_column"),
            RichText::new(t_with(
                "status.line_column",
                &[
                    ("line", &info.line.to_string()),
                    ("column", &info.column.to_string()),
                ],
            ))
            .small()
            .color(muted),
        );
    });
    action
}

/// One short right-hand label: no shrink (the row must never squeeze it into a
/// vertical stack) and no wrapping.
fn text_leaf(tree: &mut Tui, id: egui::Id, text: RichText) -> egui::Response {
    tree.style(tui::label_leaf())
        .id(id)
        .wrap_mode(egui::TextWrapMode::Extend)
        .ui(|ui| ui.label(text))
}

/// The `·` separator between two right-hand statistics.
fn separator_leaf(tree: &mut Tui, id: egui::Id, color: egui::Color32) {
    text_leaf(tree, id, RichText::new("·").small().color(color));
}

/// A colored dot plus label for the current [`CompileState`], returning its
/// response. Only the `Errors` state is clickable (it opens the preview list).
fn compile_indicator(ui: &mut egui::Ui, state: CompileState) -> Option<egui::Response> {
    let theme = Theme::from_ui(ui);
    let (dot_color, text_color, text, clickable) = match state {
        CompileState::Idle => return None,
        CompileState::Compiling => (
            theme.colors.warn,
            theme.colors.warn_text,
            t("status.compiling").into_owned(),
            false,
        ),
        CompileState::Fetching => (
            theme.colors.warn,
            theme.colors.warn_text,
            t("status.fetching").into_owned(),
            false,
        ),
        CompileState::Ready => (
            theme.colors.text_muted,
            theme.colors.text_muted,
            t("status.ready").into_owned(),
            false,
        ),
        CompileState::Errors(count) => (
            theme.colors.error,
            theme.colors.error_text,
            t_with("status.errors", &[("count", &count.to_string())]),
            true,
        ),
    };
    let font = egui::FontId::proportional(egui::TextStyle::Small.resolve(ui.style()).size);
    let dot_galley = ui
        .painter()
        .layout_no_wrap("●".to_owned(), font.clone(), dot_color);
    let label_galley = ui.painter().layout_no_wrap(text, font, text_color);
    let dot_w = dot_galley.size().x;
    let dot_h = dot_galley.size().y;
    let gap = ui.spacing().item_spacing.x;
    let size = egui::vec2(
        dot_w + gap + label_galley.size().x,
        dot_h.max(label_galley.size().y),
    );
    let sense = if clickable {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, allocated) = ui.allocate_exact_size(size, sense);
    // Keep the visible indicator compact but give it a >=24 px hit target.
    let hit_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width(), rect.height().max(24.0)),
    );
    let hit = ui.interact(hit_rect, allocated.id.with("hit"), sense);
    if clickable && hit.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(theme.radii.small),
            theme.colors.widget_hover_fill.gamma_multiply(0.35),
        );
    }
    let painter = ui.painter();
    painter.galley(
        egui::pos2(rect.left(), rect.center().y - dot_h * 0.5),
        dot_galley,
        dot_color,
    );
    painter.galley(
        egui::pos2(
            rect.left() + dot_w + gap,
            rect.center().y - label_galley.size().y * 0.5,
        ),
        label_galley,
        text_color,
    );
    Some(if clickable {
        hit.on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(t("status.view_errors"))
    } else {
        hit
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_flight_then_errors_then_ready() {
        assert_eq!(
            CompileState::resolve(true, true, 4),
            CompileState::Compiling,
            "an in-flight compile always wins"
        );
        assert_eq!(
            CompileState::resolve(false, true, 4),
            CompileState::Errors(4)
        );
        assert_eq!(
            CompileState::resolve(false, false, 2),
            CompileState::Errors(2),
            "errors are reported even without a document"
        );
        assert_eq!(CompileState::resolve(false, true, 0), CompileState::Ready);
        assert_eq!(CompileState::resolve(false, false, 0), CompileState::Idle);
    }

    #[test]
    fn save_state_maps_dirty_and_path_flags() {
        assert_eq!(save_state(false, false, false), SaveState::Untitled);
        assert_eq!(save_state(false, true, false), SaveState::Saved);
        assert_eq!(save_state(true, false, false), SaveState::Dirty);
        assert_eq!(save_state(true, true, false), SaveState::Dirty);
        assert_eq!(save_state(false, true, true), SaveState::Saving);
        assert_eq!(
            save_state(true, true, true),
            SaveState::Saving,
            "an in-flight save owns the indicator even while the buffer is dirty"
        );
    }

    #[test]
    fn cursor_count_label_appears_only_with_multiple_cursors() {
        assert_eq!(cursor_count_text(0), None);
        assert_eq!(cursor_count_text(1), None);
        let text = cursor_count_text(3).expect("multiple cursors show a count");
        assert!(text.contains('3'), "got {text:?}");
        assert!(!text.contains("{count}"), "got {text:?}");
    }
}
