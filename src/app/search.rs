//! Left-pane view switching and the project-wide search view.
//!
//! The left pane hosts two views behind a [`tab_switch`] pair: the project tree
//! (项目) and the project-wide search results (搜索). This module owns the
//! search view, its session-only state, the debounced request bookkeeping
//! (mirroring [`super::spell`]) and the flat result navigation that
//! F4 / Shift+F4 drive.

use crate::theme;
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use typ_edit_engine::Engine;
use typ_edit_i18n::{t, t_with};
use typ_edit_model::{ProjectSearchRequest, ProjectSearchResult, SearchMatch, SearchOptions};
use typ_edit_widgets::checkbox::checkbox;
use typ_edit_widgets::text_input::inline_input;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{AlignItems, LengthPercentage, Rect as TaffyRect},
};

/// Idle time after the last query/option/filter change before the debounced
/// search is requested. Longer than the compile debounce: a search walks the
/// whole project, so it should run once typing has settled.
pub(super) const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// Stable id of the query input, so focus and editing state survive across frames.
const SEARCH_INPUT_ID: &str = "typ-edit-search-input";
/// Stable id of the include-filter input.
const SEARCH_INCLUDE_ID: &str = "typ-edit-search-include";
/// Stable id of the exclude-filter input.
const SEARCH_EXCLUDE_ID: &str = "typ-edit-search-exclude";
/// Stable id of the results scroll area, kept distinct from the tree's own
/// (auto-id) area so switching views never carries a scroll offset across.
const SEARCH_RESULTS_ID: &str = "typ-edit-search-results";
/// Width reserved for the right-aligned line-number column, in points.
const LINE_COL_W: f32 = 34.0;
/// Gap between the line-number column and the preview text, in points; mirrors
/// the settings grid's horizontal column spacing.
const LINE_GAP: f32 = 12.0;
/// Minimum match-row height, in points: the settings grid's fixed row height.
const MATCH_ROW_MIN_H: f32 = 28.0;
/// Vertical gap between match rows, in points: the settings grid's row spacing.
const MATCH_ROW_GAP: f32 = 8.0;

/// Which view the left pane shows behind the 项目/搜索/大纲 switcher.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum LeftView {
    /// The project tree (default).
    #[default]
    Project,
    /// The project-wide search view.
    Search,
    /// The active file's document outline.
    Outline,
    /// The active file's commit history.
    History,
}

/// Session-only project-search state: the request inputs, the debounce/request
/// bookkeeping, the last accepted result and the flat selection over it.
#[derive(Default)]
pub(super) struct SearchState {
    pub(super) query: String,
    pub(super) options: SearchOptions,
    /// Comma-separated include globs as typed in the disclosure.
    pub(super) include_text: String,
    /// Comma-separated exclude globs as typed in the disclosure.
    pub(super) exclude_text: String,
    /// Whether the include/exclude disclosure is expanded.
    pub(super) globs_open: bool,
    /// One-shot focus request for the query input (set when switching in).
    pub(super) focus_pending: bool,
    /// Request counter; every sent request carries a fresh generation.
    pub(super) generation: u64,
    /// Newest generation whose result may still replace the stored one.
    pub(super) applied_generation: u64,
    /// Whether the worker holds a request that has not answered yet.
    pub(super) in_flight: bool,
    /// An input change arrived while the state was not clean.
    pub(super) dirty: bool,
    /// Time of the last input change, for the debounce.
    pub(super) last_change: Option<Instant>,
    /// Last accepted result (`None` before the first, and after a root reset).
    pub(super) result: Option<ProjectSearchResult>,
    /// Flat index of the selected match (files in order, matches in order).
    pub(super) selected: usize,
    /// One-shot request to scroll the selected row into view.
    pub(super) scroll_to_selected: bool,
    /// Root the stored result belongs to, to detect a workspace change.
    pub(super) root: Option<PathBuf>,
    /// Header label of the find-references result set, or `None` for a plain
    /// project search. Set when a references answer lands; cleared when the
    /// query is edited.
    pub(super) reference_label: Option<String>,
}

impl SearchState {
    /// Marks the request inputs dirty and stamps the debounce start.
    pub(super) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_change = Some(Instant::now());
    }

    /// Clears the in-flight flag without an applied result (worker death), so
    /// the UI is never stuck waiting on a request that will never answer.
    pub(super) fn clear_in_flight(&mut self) {
        self.in_flight = false;
    }

    /// Requests one-shot focus of the query input (set when switching in).
    pub(super) fn request_focus(&mut self) {
        self.focus_pending = true;
    }

    /// Drops the stored result and resets the flat selection.
    pub(super) fn clear_result(&mut self) {
        self.result = None;
        self.selected = 0;
    }

    /// Drops a pending debounced request, its input now clean.
    pub(super) fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// Resets the stored result when the workspace root changed: adopts `root`,
    /// drops the stale result set, and bumps the applied generation past it so a
    /// late answer is rejected.
    pub(super) fn reset_for_workspace(&mut self, root: Option<PathBuf>) {
        self.root = root;
        self.result = None;
        self.selected = 0;
        self.scroll_to_selected = false;
        self.reference_label = None;
        self.in_flight = false;
        self.applied_generation = self.generation + 1;
    }

    /// Applies an accepted search result, clamping the flat selection into it
    /// and clearing the in-flight flag. The references label always describes
    /// the current result, so a plain result clears it.
    pub(super) fn apply_result(&mut self, search: ProjectSearchResult) {
        self.applied_generation = search.generation;
        self.reference_label = None;
        let total = flat_match_count(&search);
        self.selected = self.selected.min(total.saturating_sub(1));
        self.result = Some(search);
        self.in_flight = false;
    }

    /// Installs a find-references result set: stored, selected at the first
    /// match and labelled with its identifier. The caller switches the view.
    ///
    /// Bumps the applied generation past any in-flight plain search so its stale
    /// answer is rejected by [`typ_edit_engine::accept::accepts_latest`] instead
    /// of overwriting this set; the next search after a query edit uses
    /// `generation + 1` and is accepted. Clearing `in_flight` mirrors
    /// [`Self::reset_for_workspace`]: the plain answer can no longer overwrite
    /// us, so the UI must not stay blocked on it.
    pub(super) fn apply_references(&mut self, result: ProjectSearchResult, label: String) {
        self.result = Some(result);
        self.selected = 0;
        self.reference_label = Some(label);
        self.in_flight = false;
        self.applied_generation = self.generation + 1;
    }

    /// Requests a search for `root` and the current query unless one is already
    /// in flight. A request sent while the worker is busy only queues `dirty`,
    /// so a burst of keystrokes coalesces to the latest query.
    pub(super) fn trigger_search(&mut self, root: Option<PathBuf>, engine: &Engine) {
        let Some(root) = root else {
            return;
        };
        if self.query.is_empty() {
            return;
        }
        if self.in_flight {
            self.dirty = true;
            return;
        }
        self.in_flight = true;
        self.dirty = false;
        self.generation += 1;
        engine.request_search(ProjectSearchRequest {
            generation: self.generation,
            root,
            query: self.query.clone(),
            options: self.options,
            include: split_globs(&self.include_text),
            exclude: split_globs(&self.exclude_text),
        });
    }

    /// Number of matches across every file, in flat display order.
    fn flat_count(&self) -> usize {
        self.result.as_ref().map_or(0, flat_match_count)
    }

    /// The `(path, byte, byte_len)` of flat match `index`.
    fn flat_match(&self, index: usize) -> Option<(PathBuf, usize, usize)> {
        let result = self.result.as_ref()?;
        let mut remaining = index;
        for file in &result.files {
            if remaining < file.matches.len() {
                let matched = &file.matches[remaining];
                return Some((file.path.clone(), matched.byte.0, matched.byte_len));
            }
            remaining -= file.matches.len();
        }
        None
    }

    /// Moves the flat selection by `delta` (clamped, no wrap), scrolls it into
    /// view and returns the match to open. A no-op without any result, so F4
    /// never surprises.
    pub(super) fn step(&mut self, delta: isize) -> Option<(PathBuf, usize, usize)> {
        let total = self.flat_count();
        if total == 0 {
            return None;
        }
        self.selected = step_selection(self.selected, total, delta);
        self.scroll_to_selected = true;
        self.flat_match(self.selected)
    }

    /// The search view body: query input, option toggles, the include/exclude
    /// disclosure, the status line and the result list. Returns the clicked
    /// match's `(path, byte, byte_len)` for the caller to open.
    ///
    /// All header controls are tui children; only the virtualized result rows
    /// inside the scroll leaf stay egui-native.
    pub(super) fn view(
        &mut self,
        tree: &mut Tui,
        has_root: bool,
    ) -> Option<(PathBuf, usize, usize)> {
        let id = egui::Id::new("typ_edit_search_view");

        tree.id(id.with("query")).ui(|ui| {
            let input_w = (ui.available_width() - 4.0).max(40.0);
            let query = inline_input(
                ui,
                &mut self.query,
                &t("search.placeholder"),
                input_w,
                Some(egui::Id::new(SEARCH_INPUT_ID)),
            );
            if self.focus_pending {
                self.focus_pending = false;
                query.request_focus();
            }
            if query.changed() {
                self.reference_label = None;
                self.mark_dirty();
            }
        });

        let mut toggled = false;
        tree.style(tui::row_wrap(4.0))
            .id(id.with("options"))
            .add(|tree| {
                tree.id(id.with("case")).ui(|ui| {
                    if checkbox(
                        ui,
                        &mut self.options.case_sensitive,
                        &t("search.case_sensitive"),
                        true,
                    )
                    .changed()
                    {
                        toggled = true;
                    }
                });
                tree.id(id.with("regex")).ui(|ui| {
                    if checkbox(ui, &mut self.options.regex, &t("search.regex"), true).changed() {
                        toggled = true;
                    }
                });
                tree.id(id.with("whole_word")).ui(|ui| {
                    if checkbox(
                        ui,
                        &mut self.options.whole_word,
                        &t("search.whole_word"),
                        true,
                    )
                    .changed()
                    {
                        toggled = true;
                    }
                });
            });
        if toggled {
            self.mark_dirty();
        }

        tree.id(id.with("globs_toggle")).ui(|ui| {
            checkbox(ui, &mut self.globs_open, &t("search.globs"), true);
        });
        if self.globs_open {
            text_leaf(
                tree,
                id.with("include_label"),
                egui::RichText::new(t("search.include"))
                    .color(crate::theme::text_muted(tree.egui_ui()))
                    .small(),
            );
            tree.id(id.with("include")).ui(|ui| {
                let input_w = (ui.available_width() - 4.0).max(40.0);
                let include = inline_input(
                    ui,
                    &mut self.include_text,
                    &t("search.globs_placeholder"),
                    input_w,
                    Some(egui::Id::new(SEARCH_INCLUDE_ID)),
                );
                if include.changed() {
                    self.mark_dirty();
                }
            });
            text_leaf(
                tree,
                id.with("exclude_label"),
                egui::RichText::new(t("search.exclude"))
                    .color(crate::theme::text_muted(tree.egui_ui()))
                    .small(),
            );
            tree.id(id.with("exclude")).ui(|ui| {
                let input_w = (ui.available_width() - 4.0).max(40.0);
                let exclude = inline_input(
                    ui,
                    &mut self.exclude_text,
                    &t("search.globs_placeholder"),
                    input_w,
                    Some(egui::Id::new(SEARCH_EXCLUDE_ID)),
                );
                if exclude.changed() {
                    self.mark_dirty();
                }
            });
        }

        if let Some(label) = self.reference_label.clone() {
            let header = t_with("references.header", &[("name", label.as_str())]);
            let count = self.result.as_ref().map_or(0, flat_match_count);
            let accent = theme::reveal(tree.egui_ui());
            let mut style = tui::row_gap(4.0);
            style.align_self = Some(AlignItems::Start);
            style.padding = TaffyRect {
                left: LengthPercentage::length(6.0),
                right: LengthPercentage::length(6.0),
                top: LengthPercentage::length(2.0),
                bottom: LengthPercentage::length(2.0),
            };
            tree.style(style)
                .id(id.with("reference_header"))
                .add_with_background_ui(
                    |ui, container| {
                        ui.painter().rect_filled(
                            container.full_container(),
                            egui::CornerRadius::same(2),
                            ui.visuals().faint_bg_color,
                        );
                    },
                    |tree, _| {
                        text_leaf(
                            tree,
                            id.with("reference_name"),
                            egui::RichText::new(header).color(accent).strong(),
                        );
                        if count > 0 {
                            text_leaf(
                                tree,
                                id.with("reference_count"),
                                egui::RichText::new(format!("({count})"))
                                    .color(crate::theme::text_muted(tree.egui_ui()))
                                    .small(),
                            );
                        }
                    },
                );
        }

        if let Some(result) = self.result.as_ref() {
            if let Some(error) = &result.error {
                wrapped_leaf(
                    tree,
                    id.with("error"),
                    egui::RichText::new(error)
                        .color(theme::error(tree.egui_ui()))
                        .small(),
                );
            } else if !self.query.is_empty() {
                let matches = result.total_matches.to_string();
                let files = result.files.len().to_string();
                let mut status =
                    t_with("search.status", &[("matches", &matches), ("files", &files)]);
                if result.truncated {
                    status.push(' ');
                    status.push_str(&t("search.truncated"));
                }
                text_leaf(
                    tree,
                    id.with("status"),
                    egui::RichText::new(status)
                        .color(crate::theme::text_muted(tree.egui_ui()))
                        .small(),
                );
            }
        }

        if !has_root {
            hint_leaf(tree, id.with("no_project"), t("search.no_project"));
            #[cfg(test)]
            crate::app::ui_probe::record("search.no_project", tree.egui_ui().min_rect());
            return None;
        }
        if self.query.is_empty() && self.reference_label.is_none() {
            hint_leaf(tree, id.with("hint"), t("search.hint"));
            return None;
        }
        let result = self.result.as_ref()?;
        if result.error.is_some() {
            return None;
        }
        if result.files.is_empty() {
            hint_leaf(tree, id.with("no_results"), t("search.no_results"));
            return None;
        }
        let selected = &mut self.selected;
        let scroll = &mut self.scroll_to_selected;
        #[cfg(test)]
        crate::app::ui_probe::record("search.results", tree.egui_ui().min_rect());
        tree.style(tui::flexible())
            .id(id.with("results"))
            .ui(|ui| show_results(ui, result, selected, scroll))
    }
}

/// One short label leaf: no shrink, no wrap.
fn text_leaf(tree: &mut Tui, id: egui::Id, text: egui::RichText) -> egui::Response {
    tree.style(tui::label_leaf())
        .id(id)
        .wrap_mode(egui::TextWrapMode::Extend)
        .ui(|ui| ui.label(text))
}

/// One full-width label leaf whose body wraps at the pane width.
fn wrapped_leaf(tree: &mut Tui, id: egui::Id, text: egui::RichText) {
    tree.id(id).ui(|ui| {
        ui.add(egui::Label::new(text).wrap());
    });
}

/// A muted full-width hint / empty-state line.
fn hint_leaf(tree: &mut Tui, id: egui::Id, text: impl Into<String>) {
    wrapped_leaf(
        tree,
        id,
        egui::RichText::new(text.into()).color(crate::theme::text_muted(tree.egui_ui())),
    );
}

/// Splits a comma-separated glob list: trims each entry, drops empty ones and
/// preserves the typed order.
pub(super) fn split_globs(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Moves a flat selection by `delta`, clamped to `0..total` (no wrap). An empty
/// list always selects `0`.
pub(super) fn step_selection(current: usize, total: usize, delta: isize) -> usize {
    if total == 0 {
        return 0;
    }
    let last = total as isize - 1;
    (current as isize + delta).clamp(0, last) as usize
}

/// Whether the debounced search request is due: an edit is pending, no request
/// is in flight, and the last change has been idle for at least
/// [`SEARCH_DEBOUNCE`]. A never-changed input (no idle stamp) is due at once.
pub(super) fn search_due(
    enabled: bool,
    dirty: bool,
    in_flight: bool,
    idle: Option<Duration>,
) -> bool {
    enabled && dirty && !in_flight && idle.is_none_or(|elapsed| elapsed >= SEARCH_DEBOUNCE)
}

/// Number of matches across every file, in flat display order.
pub(super) fn flat_match_count(result: &ProjectSearchResult) -> usize {
    result.files.iter().map(|file| file.matches.len()).sum()
}

/// Splits a preview into `(before, matched, after)` around `range`, a CHAR range
/// into `preview`. An empty or out-of-range span degrades to the whole preview
/// with no highlighted segment, so rendering never panics on a bad range.
fn split_preview(preview: &str, range: Range<usize>) -> (&str, &str, &str) {
    if range.start >= range.end {
        return (preview, "", "");
    }
    let mut start = None;
    let mut end = None;
    for (char_index, (byte_index, _)) in preview.char_indices().enumerate() {
        if char_index == range.start {
            start = Some(byte_index);
        }
        if char_index == range.end {
            end = Some(byte_index);
            break;
        }
    }
    let start = start.unwrap_or(preview.len());
    let end = end.unwrap_or(preview.len());
    if start > end || end > preview.len() {
        return (preview, "", "");
    }
    (&preview[..start], &preview[start..end], &preview[end..])
}

/// Draws the result list and returns the clicked match's `(path, byte, byte_len)`.
///
/// `selected` indexes the flat match list (files in order, matches in order);
/// clicking a row moves it. When `scroll_to_selected` is set, the selected row
/// scrolls itself into view and clears the flag.
fn show_results(
    ui: &mut egui::Ui,
    result: &ProjectSearchResult,
    selected: &mut usize,
    scroll_to_selected: &mut bool,
) -> Option<(PathBuf, usize, usize)> {
    let mut clicked = None;
    let flat = std::cell::Cell::new(0usize);
    // tui-whitelist: ScrollArea 内的虚拟化行,不可嵌套 tui
    egui::ScrollArea::vertical()
        .id_salt(SEARCH_RESULTS_ID)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Match rows share the settings grid's row spacing so the bands read
            // as the same table.
            ui.spacing_mut().item_spacing.y = MATCH_ROW_GAP;
            for (file_index, file) in result.files.iter().enumerate() {
                ui.push_id(("search-file", file_index), |ui| {
                    // File rows stay section headers (weak text + count), not grid rows.
                    ui.label(
                        egui::RichText::new(format!("{} · {}", file.relative, file.matches.len()))
                            .color(crate::theme::text_muted(ui))
                            .small(),
                    );
                    for (match_index, matched) in file.matches.iter().enumerate() {
                        ui.push_id(("search-match", match_index), |ui| {
                            let index = flat.get();
                            // Stripes alternate per file block, the odd-row band of
                            // the settings grid.
                            let stripe = match_index % 2 == 1;
                            if match_row(
                                ui,
                                matched,
                                stripe,
                                index == *selected,
                                scroll_to_selected,
                            ) {
                                *selected = index;
                                clicked =
                                    Some((file.path.clone(), matched.byte.0, matched.byte_len));
                            }
                            flat.set(index + 1);
                        });
                    }
                });
            }
        });
    clicked
}

/// Draws one clickable match row as a two-column grid row (line number |
/// preview) with the settings grid's stripe, and the match segment emphasized.
/// Returns whether the row was clicked.
///
/// `striped_grid`/`grid_row` are mirrored rather than reused: `grid_row`
/// allocates presentation cells and exposes no row response, so the row's own
/// `Sense::click`, hover highlight, selected emphasis and `scroll_to_me` would
/// be lost. The band matches egui's grid stripe (`striped_grid`) exactly —
/// `visuals.faint_bg_color` on odd rows, half the row gap as vertical bleed,
/// 2px horizontal bleed, radius 2.
fn match_row(
    ui: &mut egui::Ui,
    matched: &SearchMatch,
    stripe: bool,
    selected: bool,
    scroll_to_selected: &mut bool,
) -> bool {
    let job = preview_job(ui, matched);
    let galley = ui.painter().layout_job(job);
    let row_h = galley.size().y.max(MATCH_ROW_MIN_H);
    let width = ui.available_width().max(0.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::click());

    let visuals = ui.visuals();
    let painter = ui.painter();
    if stripe {
        let band = egui::Rect::from_min_max(
            egui::pos2(rect.left() - 2.0, rect.top() - MATCH_ROW_GAP * 0.5),
            egui::pos2(rect.right() + 2.0, rect.bottom() + MATCH_ROW_GAP * 0.5),
        );
        painter.rect_filled(band, egui::CornerRadius::same(2), visuals.faint_bg_color);
    }
    if selected {
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(2),
            egui::Stroke::new(2.0, visuals.hyperlink_color),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(2),
            visuals.widgets.hovered.bg_fill.gamma_multiply(0.5),
        );
    }
    painter.text(
        egui::pos2(rect.left() + LINE_COL_W - 4.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("{}", matched.line + 1),
        egui::TextStyle::Small.resolve(ui.style()),
        visuals.weak_text_color(),
    );
    painter.galley(
        egui::pos2(
            rect.left() + LINE_COL_W + LINE_GAP,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        if selected {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        },
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if selected && *scroll_to_selected {
        response.scroll_to_me(Some(egui::Align::Center));
        *scroll_to_selected = false;
    }
    response.clicked()
}

/// Builds the preview's text layout: plain text with the match segment painted
/// in the selection colors, so it stays legible in both themes.
fn preview_job(ui: &egui::Ui, matched: &SearchMatch) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    let sel_bg = ui.visuals().selection.bg_fill;
    let sel_fg = ui.visuals().selection.stroke.color;
    let (before, hit, after) = split_preview(
        &matched.preview,
        matched.preview_match.start.0..matched.preview_match.end.0,
    );
    let plain = egui::TextFormat {
        font_id: font.clone(),
        color: text_color,
        ..Default::default()
    };
    let marked = egui::TextFormat {
        font_id: font,
        color: sel_fg,
        background: sel_bg,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(before, 0.0, plain.clone());
    job.append(hit, 0.0, marked);
    job.append(after, 0.0, plain);
    job.wrap.max_width = (ui.available_width() - LINE_COL_W - LINE_GAP).max(24.0);
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_model::{BytePos, CharPos, FileMatches};

    fn synthetic_result() -> ProjectSearchResult {
        ProjectSearchResult {
            generation: 1,
            files: vec![FileMatches {
                path: PathBuf::from("/proj/chapter.typ"),
                relative: "chapter.typ".to_owned(),
                matches: vec![
                    SearchMatch {
                        line: 2,
                        byte: BytePos(10),
                        byte_len: 4,
                        preview: "the quick brown fox".to_owned(),
                        preview_match: CharPos(4)..CharPos(9),
                    },
                    SearchMatch {
                        line: 7,
                        byte: BytePos(40),
                        byte_len: 4,
                        preview: "another quick one".to_owned(),
                        preview_match: CharPos(8)..CharPos(13),
                    },
                ],
            }],
            total_matches: 2,
            truncated: false,
            error: None,
        }
    }

    #[test]
    fn split_globs_trims_drops_empty_and_keeps_order() {
        assert_eq!(
            split_globs(" chapter*,  draft/* ,, "),
            vec!["chapter*".to_owned(), "draft/*".to_owned()]
        );
        assert!(split_globs("").is_empty());
        assert!(split_globs(" , , ").is_empty());
    }

    #[test]
    fn step_selection_clamps_at_both_ends() {
        assert_eq!(step_selection(0, 5, -1), 0, "cannot move before the first");
        assert_eq!(step_selection(4, 5, 1), 4, "cannot move past the last");
        assert_eq!(step_selection(2, 5, -5), 0);
        assert_eq!(step_selection(1, 5, 99), 4);
        assert_eq!(step_selection(9, 3, 1), 2, "an out-of-range cursor clamps");
    }

    #[test]
    fn step_selection_returns_zero_for_an_empty_list() {
        assert_eq!(step_selection(0, 0, 1), 0);
        assert_eq!(step_selection(7, 0, -1), 0);
    }

    #[test]
    fn search_due_waits_out_the_debounce() {
        assert!(!search_due(
            true,
            true,
            false,
            Some(Duration::from_millis(249))
        ));
        assert!(search_due(
            true,
            true,
            false,
            Some(Duration::from_millis(250))
        ));
        assert!(
            search_due(true, true, false, None),
            "no stamp is due at once"
        );
    }

    #[test]
    fn search_due_is_false_when_disabled_clean_or_in_flight() {
        assert!(!search_due(false, true, false, None));
        assert!(!search_due(true, false, false, None));
        assert!(!search_due(true, true, true, None));
    }

    #[test]
    fn split_preview_maps_char_ranges_across_multibyte_text() {
        // chars: 中(0) a(1) β(2) c(3)
        assert_eq!(split_preview("中aβc", 1..3), ("中", "aβ", "c"));
        assert_eq!(split_preview("中aβc", 0..1), ("", "中", "aβc"));
        assert_eq!(split_preview("abc", 0..3), ("", "abc", ""));
    }

    #[test]
    fn split_preview_degrades_on_empty_or_out_of_range_spans() {
        assert_eq!(split_preview("abc", 1..1), ("abc", "", ""));
        assert_eq!(split_preview("abc", 5..9), ("abc", "", ""));
        assert_eq!(split_preview("abc", 0..9), ("", "abc", ""));
    }

    #[test]
    fn flat_match_count_sums_every_file() {
        assert_eq!(flat_match_count(&synthetic_result()), 2);
        assert_eq!(flat_match_count(&ProjectSearchResult::default()), 0);
    }

    #[test]
    fn apply_result_clears_a_stale_reference_label() {
        let mut state = SearchState::default();
        state.apply_references(synthetic_result(), "foo".to_owned());
        assert!(state.reference_label.is_some(), "references set a label");

        let mut plain = synthetic_result();
        plain.generation = 2;
        state.apply_result(plain);
        assert!(
            state.reference_label.is_none(),
            "a plain result must not keep a references label"
        );
    }

    #[test]
    fn apply_references_sets_the_label_and_bumps_the_applied_generation() {
        let mut state = SearchState {
            generation: 7,
            ..SearchState::default()
        };
        state.apply_references(synthetic_result(), "find".to_owned());
        assert_eq!(state.reference_label.as_deref(), Some("find"));
        assert_eq!(state.applied_generation, 8);
    }

    #[test]
    fn a_late_plain_result_cannot_overwrite_a_references_set() {
        use typ_edit_engine::accept::accepts_latest;
        let mut state = SearchState {
            generation: 7,
            ..SearchState::default()
        };
        state.apply_references(synthetic_result(), "find".to_owned());

        // The plain answer still in flight for generation 7 is now stale.
        assert!(!accepts_latest(state.applied_generation, 7));
        // The next search after a query edit uses generation + 1 and is accepted.
        assert!(accepts_latest(
            state.applied_generation,
            state.generation + 1
        ));
    }

    #[test]
    fn search_results_render_without_panicking() {
        let ctx = egui::Context::default();
        let result = synthetic_result();
        let mut selected = 0usize;
        let mut scroll = false;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            let hit = show_results(ui, &result, &mut selected, &mut scroll);
            assert!(hit.is_none(), "no pointer press means no click");
        })
        .drop_without_applying_deltas();
    }
}
