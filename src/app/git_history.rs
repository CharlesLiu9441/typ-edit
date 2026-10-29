//! Left-pane 历史 view and the floating diff window.
//!
//! The history view lists the active file's commits from the worker's newest
//! `Log` answer; clicking a row opens a diff window backed by the newest `Diff`
//! answer. Both are read-only projections of [`GitState`](super::git::GitState).

use super::git::GitState;
use super::git_ui::{DiffWindow, GitUiState, relative_time};
use super::state::SessionState;
use egui::RichText;
use typ_edit_git::CommitInfo;
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

/// Stable id of the history scroll area.
const HISTORY_SCROLL_ID: &str = "typ-edit-git-history";
/// Fixed history row height, in points.
const HISTORY_ROW_H: f32 = 22.0;
/// Right-aligned width reserved for the hash/time column, in points.
const HISTORY_RIGHT_W: f32 = 150.0;

/// Which body the history view shows this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryPhase {
    /// `Settings::git_enabled` is off.
    Disabled,
    /// The folder is not a git repository.
    NoRepo,
    /// No file is open.
    NoFile,
    /// A log request is in flight or none has landed for the active file.
    Loading,
    /// The active file has no commits.
    Empty,
    /// A non-empty commit list (its length).
    List(usize),
}

/// The history view's body for the given environment flags.
pub(super) fn history_phase(
    enabled: bool,
    has_repo: bool,
    has_file: bool,
    loaded: bool,
    count: usize,
) -> HistoryPhase {
    if !enabled {
        HistoryPhase::Disabled
    } else if !has_repo {
        HistoryPhase::NoRepo
    } else if !has_file {
        HistoryPhase::NoFile
    } else if !loaded {
        HistoryPhase::Loading
    } else if count == 0 {
        HistoryPhase::Empty
    } else {
        HistoryPhase::List(count)
    }
}

/// Clamps a history selection to a list of `count` rows.
pub(super) fn clamp_history_selection(selected: usize, count: usize) -> usize {
    selected.min(count.saturating_sub(1))
}

/// How one unified-diff line is colored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffLine {
    Added,
    Removed,
    Hunk,
    Header,
    Context,
}

/// Classifies one line of a unified diff. Header checks precede the `+`/`-`
/// checks so `+++`/`---` file headers render dim rather than as changes.
pub(super) fn classify_diff_line(line: &str) -> DiffLine {
    if line.starts_with("@@") {
        DiffLine::Hunk
    } else if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("similarity")
        || line.starts_with("rename ")
        || line.starts_with("Binary files")
    {
        DiffLine::Header
    } else if line.starts_with('+') {
        DiffLine::Added
    } else if line.starts_with('-') {
        DiffLine::Removed
    } else {
        DiffLine::Context
    }
}

impl GitUiState {
    /// The 历史 body: the commit list for the active file.
    pub(super) fn history_view(
        &mut self,
        ui: &mut egui::Ui,
        git: &mut GitState,
        session: &SessionState,
        enabled: bool,
    ) {
        git.request_history_log(session);
        ui.add_space(2.0);
        match self.history_phase(git, session, enabled) {
            HistoryPhase::Disabled => {
                ui.label(
                    RichText::new(t("git.history.disabled"))
                        .color(Theme::from_ui(ui).colors.text_muted),
                );
                return;
            }
            HistoryPhase::NoRepo => {
                ui.label(
                    RichText::new(t("git.history.no_repo"))
                        .color(Theme::from_ui(ui).colors.text_muted),
                );
                return;
            }
            HistoryPhase::NoFile => {
                ui.label(
                    RichText::new(t("git.history.no_file"))
                        .color(Theme::from_ui(ui).colors.text_muted),
                );
                return;
            }
            HistoryPhase::Loading => {
                ui.label(
                    RichText::new(t("git.history.loading"))
                        .color(Theme::from_ui(ui).colors.text_muted),
                );
                return;
            }
            HistoryPhase::Empty => {
                ui.label(
                    RichText::new(t("git.history.empty"))
                        .color(Theme::from_ui(ui).colors.text_muted),
                );
                return;
            }
            HistoryPhase::List(count) => {
                self.history_selected = clamp_history_selection(self.history_selected, count);
            }
        }

        let branch = git.snapshot.branch.clone();
        if !branch.is_empty() {
            ui.label(
                RichText::new(branch)
                    .small()
                    .color(Theme::from_ui(ui).colors.text_muted),
            );
        }
        let now = super::git_ui::unix_now();
        let clicked = {
            let log = &git.log;
            let selected = &mut self.history_selected;
            show_history_rows(ui, log, selected, now)
        };
        if let Some(index) = clicked {
            self.open_diff(index, git, session);
        }
    }

    /// The history view's phase from the current environment and answers.
    fn history_phase(&self, git: &GitState, session: &SessionState, enabled: bool) -> HistoryPhase {
        let relative = git.active_repo_path(session);
        let loaded = relative.is_some() && relative.as_deref() == git.log_path.as_deref();
        let count = if loaded { git.log.len() } else { 0 };
        history_phase(
            enabled,
            git.repo_root.is_some(),
            session.tabs[session.active_tab].editor.path.is_some(),
            loaded,
            count,
        )
    }

    /// Opens the diff window for history row `index` and requests its patch.
    fn open_diff(&mut self, index: usize, git: &mut GitState, session: &SessionState) {
        let Some((oid, short)) = git
            .log
            .get(index)
            .map(|commit| (commit.oid.clone(), commit.short.clone()))
        else {
            return;
        };
        let name = session.tabs[session.active_tab]
            .editor
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = t_with("git.diff.title", &[("name", &name), ("short", &short)]);
        self.diff = Some(DiffWindow {
            title,
            oid: oid.clone(),
        });
        let path = git.active_repo_path(session);
        git.git_request_diff(oid, path);
    }

    /// Paints the floating diff window and its newest patch.
    pub(super) fn git_diff_window(&mut self, ctx: &egui::Context, git: &GitState) {
        let Some(window) = self.diff.clone() else {
            return;
        };
        let mut open = true;
        egui::Modal::new(egui::Id::new("typ-edit-git-diff")).show(ctx, |ui| {
            ui.set_min_width(680.0);
            ui.set_min_height(320.0);
            ui.horizontal(|ui| {
                ui.heading(window.title.clone());
                if ui.button("×").clicked() {
                    open = false;
                }
            });
            // The diff window is a plain egui host: one top-level tui
            // column fills it, and the patch itself stays a scroll area.
            let base = ui.id().with(("typ_edit_git_diff", &window.oid));
            let mut style = tui::column();
            tui::fill(&mut style);
            tui::tui(ui, base)
                .reserve_available_space()
                .style(style)
                .show(|tree| match git.diff.as_ref() {
                    Some((oid, text)) if *oid == window.oid => {
                        if text.is_empty() {
                            tree.id(base.with("empty")).ui(|ui| {
                                ui.label(
                                    egui::RichText::new(t("git.diff.empty"))
                                        .color(Theme::from_ui(ui).colors.text_muted),
                                )
                            });
                        } else {
                            // tui-whitelist: diff 文本区保持原 ScrollArea 绘制
                            tree.style(tui::flexible())
                                .id(base.with("patch"))
                                .add(|tree| show_diff(tree.egui_ui_mut(), text));
                        }
                    }
                    _ => {
                        tree.id(base.with("loading")).ui(|ui| {
                            ui.label(
                                egui::RichText::new(t("git.diff.loading"))
                                    .color(Theme::from_ui(ui).colors.text_muted),
                            )
                        });
                    }
                });
        });
        if !open {
            self.diff = None;
        }
    }
}

/// Draws the history rows and returns the clicked row index.
fn show_history_rows(
    ui: &mut egui::Ui,
    log: &[CommitInfo],
    selected: &mut usize,
    now: i64,
) -> Option<usize> {
    let mut clicked = None;
    // tui-whitelist: ScrollArea 内的虚拟化行,不可嵌套 tui
    egui::ScrollArea::vertical()
        .id_salt(HISTORY_SCROLL_ID)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, commit) in log.iter().enumerate() {
                let width = ui.available_width().max(0.0);
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(width, HISTORY_ROW_H), egui::Sense::click());
                let visuals = ui.visuals();
                if index == *selected {
                    ui.painter().rect_stroke(
                        rect,
                        egui::CornerRadius::same(2),
                        egui::Stroke::new(2.0, visuals.hyperlink_color),
                        egui::StrokeKind::Inside,
                    );
                } else if response.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        egui::CornerRadius::same(2),
                        visuals.widgets.hovered.bg_fill.gamma_multiply(0.5),
                    );
                }
                let painter = ui.painter().with_clip_rect(rect);
                let small = egui::TextStyle::Small.resolve(ui.style());
                let right = format!("{} · {}", commit.short, relative_time(now, commit.time));
                painter.text(
                    egui::pos2(rect.right() - 6.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    right,
                    small.clone(),
                    visuals.weak_text_color(),
                );
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = (rect.width() - HISTORY_RIGHT_W).max(24.0);
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                job.append(
                    &commit.author,
                    0.0,
                    egui::TextFormat {
                        font_id: small.clone(),
                        color: visuals.strong_text_color(),
                        ..Default::default()
                    },
                );
                job.append(
                    "  ",
                    0.0,
                    egui::TextFormat {
                        font_id: small.clone(),
                        color: visuals.weak_text_color(),
                        ..Default::default()
                    },
                );
                job.append(
                    &commit.summary,
                    0.0,
                    egui::TextFormat {
                        font_id: small,
                        color: visuals.text_color(),
                        ..Default::default()
                    },
                );
                let galley = painter.layout_job(job);
                painter.galley(
                    egui::pos2(rect.left() + 6.0, rect.center().y - galley.size().y * 0.5),
                    galley,
                    visuals.strong_text_color(),
                );
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if response.clicked() {
                    *selected = index;
                    clicked = Some(index);
                }
            }
        });
    clicked
}

/// Draws one unified patch as a scrollable monospace list, colored per line.
fn show_diff(ui: &mut egui::Ui, text: &str) {
    let theme = Theme::from_ui(ui);
    egui::ScrollArea::both()
        .id_salt("typ-edit-git-diff-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for line in text.lines() {
                let color = match classify_diff_line(line) {
                    DiffLine::Added => theme.colors.success,
                    DiffLine::Removed => theme.colors.error,
                    DiffLine::Hunk => theme.colors.accent,
                    DiffLine::Header => theme.colors.text_muted,
                    DiffLine::Context => theme.colors.text,
                };
                ui.label(
                    egui::RichText::new(line)
                        .monospace()
                        .size(12.0)
                        .color(color),
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tree::badges_available;

    #[test]
    fn history_phase_walks_disabled_then_loading_list_and_empty() {
        assert_eq!(
            history_phase(false, false, false, false, 0),
            HistoryPhase::Disabled
        );
        assert_eq!(
            history_phase(true, false, false, false, 0),
            HistoryPhase::NoRepo
        );
        assert_eq!(
            history_phase(true, true, false, false, 0),
            HistoryPhase::NoFile
        );
        assert_eq!(
            history_phase(true, true, true, false, 0),
            HistoryPhase::Loading
        );
        assert_eq!(
            history_phase(true, true, true, true, 0),
            HistoryPhase::Empty
        );
        assert_eq!(
            history_phase(true, true, true, true, 3),
            HistoryPhase::List(3)
        );
    }

    #[test]
    fn history_selection_clamps_to_the_list() {
        assert_eq!(clamp_history_selection(5, 3), 2);
        assert_eq!(clamp_history_selection(1, 3), 1);
        assert_eq!(clamp_history_selection(0, 0), 0);
        assert_eq!(clamp_history_selection(9, 0), 0);
    }

    #[test]
    fn classify_diff_line_separates_added_removed_hunk_header_context() {
        assert_eq!(classify_diff_line("+new"), DiffLine::Added);
        assert_eq!(classify_diff_line("-old"), DiffLine::Removed);
        assert_eq!(classify_diff_line("@@ -1,3 +1,4 @@"), DiffLine::Hunk);
        assert_eq!(classify_diff_line("+++ b/file.typ"), DiffLine::Header);
        assert_eq!(classify_diff_line("--- a/file.typ"), DiffLine::Header);
        assert_eq!(classify_diff_line("diff --git a/x b/x"), DiffLine::Header);
        assert_eq!(classify_diff_line("index 123..456"), DiffLine::Header);
        assert_eq!(classify_diff_line(" context"), DiffLine::Context);
        assert_eq!(classify_diff_line(""), DiffLine::Context);
    }

    #[test]
    fn git_disabled_defaults_to_no_badges_no_blame_and_the_history_hint() {
        assert_eq!(
            history_phase(false, false, false, false, 0),
            HistoryPhase::Disabled
        );
        assert!(!badges_available(false, true));
        assert!(!super::super::git_ui::blame_visible(false, true, true));
    }
}
