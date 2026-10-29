//! Find/replace: match helpers, the bar UI, and the deferred replace actions.

use std::ops::Range;
use std::time::Instant;
use typ_edit_i18n::{t, t_with};

use crate::{
    EditorState, FIND_DEBOUNCE, FIND_FEEDBACK_TTL, FIND_INPUT_ID, FindState, REPLACE_INPUT_ID,
    find_bar, search,
};
use typ_edit_widgets::{
    button::{ButtonVariant, button, icon_button},
    checkbox::checkbox,
    separator::separator,
    text_input::inline_input,
    theme::Theme,
};

/// Fixed width reserved for the transient replace feedback, so its appearance
/// and expiry never shift the surrounding controls.
const FEEDBACK_WIDTH: f32 = 96.0;
/// Fixed width reserved for the `n/m` match counter, so stepping the active
/// match never reflows the action column.
const COUNTER_WIDTH: f32 = 48.0;
/// Smallest width the stretched query/replace column may shrink to.
const MIN_QUERY_WIDTH: f32 = 32.0;
/// Side of the find-bar close icon button.
const CLOSE_SIZE: f32 = 24.0;

/// Width of `text` laid out in the body font, without wrapping.
fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, egui::Color32::PLACEHOLDER)
        .size()
        .x
}

/// Width of a quiet text button labelled `text`, matching
/// `typ_edit_widgets::button`'s metrics (content width plus `quiet_pad_x` on
/// both sides).
fn quiet_button_width(ui: &egui::Ui, text: &str) -> f32 {
    text_width(ui, text) + 2.0 * Theme::from_ui(ui).spacing.quiet_pad_x
}

/// Natural width of a `checkbox` row for `label`, matching
/// `typ_edit_widgets::checkbox`'s metrics (`checkbox_box + checkbox_gap +
/// label`).
fn checkbox_width(ui: &egui::Ui, label: &str) -> f32 {
    let spacing = Theme::from_ui(ui).spacing;
    spacing.checkbox_box + spacing.checkbox_gap + text_width(ui, label)
}

/// Adds a checkbox as one item of a wrapping row.
///
/// A wrapping layout only breaks lines between widgets it lays out itself, so
/// it must reserve each item's width up front. A bare `checkbox(ui, ..)` lays
/// itself out in an inner `add_enabled_ui` scope whose `max_rect` is the
/// exhausted remainder of the row, which the wrapping layout never gets to
/// wrap; reserving the width here is what lets the row break lines.
fn wrapped_checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    label: &str,
    enabled: bool,
) -> egui::Response {
    let size = egui::vec2(
        checkbox_width(ui, label),
        Theme::from_ui(ui).spacing.list_row_h,
    );
    // tui-whitelist: 查找栏位于编辑器 widget 叶内（其宿主是 tui 树的叶节点），
    // 在此再开 tui() 会复现 P1 的嵌套 0×0 bug；保留 P6 前的 egui 行排布。
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| checkbox(ui, checked, label, enabled),
    )
    .inner
}

/// Paints the `n/m` match counter (or its no-match wording) in a fixed-width,
/// single-line cell, so it never reflows the surrounding buttons.
fn paint_counter(ui: &mut egui::Ui, find: &FindState, width: f32) {
    // tui-whitelist: 单叶 allocate_exact_size（固定宽计数格），文字自绘。
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, ui.spacing().interact_size.y),
        egui::Sense::hover(),
    );
    if find.query.is_empty() {
        return;
    }
    let text = if find.matches.is_empty() {
        t("editor.find.no_match").into_owned()
    } else {
        format!("{}/{}", find.active + 1, find.matches.len())
    };
    let color = ui.visuals().text_color();
    let galley =
        ui.painter()
            .layout_no_wrap(text, egui::TextStyle::Body.resolve(ui.style()), color);
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
}

/// Paints the transient replace feedback in a fixed-width, single-line cell,
/// truncating with an ellipsis, and schedules its expiry repaint.
fn paint_feedback(ui: &mut egui::Ui, feedback: Option<&(String, Instant)>) {
    // tui-whitelist: 单叶 allocate_exact_size（固定宽反馈格），文字自绘。
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(FEEDBACK_WIDTH, ui.spacing().interact_size.y),
        egui::Sense::hover(),
    );
    let Some((text, at)) = feedback else {
        return;
    };
    let weak = Theme::from_ui(ui).colors.text_weak;
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = rect.width();
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('…');
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: egui::TextStyle::Body.resolve(ui.style()),
            color: weak,
            ..Default::default()
        },
    );
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        weak,
    );
    ui.ctx()
        .request_repaint_after(FIND_FEEDBACK_TTL.saturating_sub(at.elapsed()));
}

/// Index of the first match whose start is at or after `byte`.
///
/// Matches are ascending and non-overlapping, so a linear scan is exact. Used
/// to make the caret's neighbourhood active after a query change.
pub(crate) fn first_match_at_or_after(matches: &[Range<usize>], byte: usize) -> Option<usize> {
    matches.iter().position(|range| range.start >= byte)
}

/// Wrapping step over `len` matches (the 上一个/下一个 buttons).
pub(crate) fn step_match(active: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (active as isize + delta).rem_euclid(len as isize) as usize
}

/// Picks the byte range the 在选区内 toggle adopts: the live selection when one
/// exists, otherwise the remembered [`FindState::scope_hint`]. Keeping the rule
/// pure makes the focus-loss policy directly testable.
pub(super) fn effective_scope(
    live: Option<Range<usize>>,
    hint: Option<Range<usize>>,
) -> Option<Range<usize>> {
    live.or(hint)
}

/// Result of one frame of the find/replace bar: its deferred buffer actions.
pub(super) struct FindActions {
    pub(super) replace_current: bool,
    pub(super) replace_all: bool,
    pub(super) close: bool,
}

/// Widget-agnostic result of running the deferred find/replace actions.
#[derive(Default)]
pub(super) struct FindOutcome {
    /// A find action edited the buffer (the caller must report `changed`).
    pub(super) changed: bool,
    /// The bar closed this frame; the caller should refocus the editor.
    pub(super) close: bool,
}

impl EditorState {
    /// Recomputes the match list for the current query, modes and buffer.
    ///
    /// The selection scope is clamped first: the buffer can shrink while the
    /// bar stays open, which would otherwise leave the range past the text end.
    /// An invalid regex clears the matches and records the error for the UI.
    pub(super) fn refresh_find_matches(&mut self) {
        self.find.pending_refresh = None;
        self.find.pending_refresh_activates = true;
        let text = self.shared_text();
        if let Some(scope) = &mut self.find.in_selection {
            scope.start = scope.start.min(text.len());
            scope.end = scope.end.min(text.len()).max(scope.start);
        }
        let pattern = search::decode_newlines(&self.find.query);
        match search::find_matches(
            &text,
            &pattern,
            self.find.case_sensitive,
            self.find.regex,
            self.find.whole_word,
            self.find.in_selection.clone(),
        ) {
            Ok(matches) => {
                self.find.matches = matches;
                self.find.regex_error = None;
            }
            Err(message) => {
                self.find.matches = Vec::new();
                self.find.regex_error = Some(message);
            }
        }
        self.find.active = self
            .find
            .active
            .min(self.find.matches.len().saturating_sub(1));
    }

    /// Refreshes the matches and re-anchors the active index on the caret.
    ///
    /// Shared by every control that changes the query or a matching mode, so a
    /// toggle behaves exactly like typing the query again.
    fn refresh_find_and_activate(&mut self, pre_caret: Option<usize>) {
        self.find.pending_refresh = None;
        self.find.pending_refresh_activates = true;
        self.refresh_find_matches();
        let caret_byte = self.buffer.char_to_byte(pre_caret.unwrap_or(0));
        self.find.active =
            find_bar::first_match_at_or_after(&self.find.matches, caret_byte).unwrap_or(0);
        self.find.pending_select = self.find.matches.get(self.find.active).cloned();
    }

    /// Queues the full-buffer scan until the query has been idle for
    /// [`FIND_DEBOUNCE`]. Query typing never blocks on a large document.
    pub(super) fn schedule_find_refresh(&mut self) {
        self.find.pending_refresh = Some(Instant::now());
        self.find.pending_refresh_activates = true;
    }

    /// Queues a scan after an edit without re-anchoring or selecting a match.
    /// A query change that is already pending keeps its activating behavior.
    pub(super) fn schedule_find_refresh_after_edit(&mut self) {
        if self.find.pending_refresh.is_none() {
            self.find.pending_refresh_activates = false;
        }
        self.find.pending_refresh = Some(Instant::now());
    }

    /// Runs a queued query scan now, if any. Used by explicit actions
    /// (Enter / prev / next) so they see the latest query immediately.
    fn flush_find_refresh(&mut self, pre_caret: Option<usize>) {
        if self.find.pending_refresh.take().is_some() {
            self.refresh_find_and_activate(pre_caret);
        }
    }

    /// Draws the find/replace bar above the text and reports the deferred
    /// buffer actions it queued.
    pub(super) fn find_bar_ui(
        &mut self,
        ui: &mut egui::Ui,
        pre_caret: Option<usize>,
        pre_selection: Option<&str>,
        pre_selection_range: Option<Range<usize>>,
    ) -> FindActions {
        let pre_selection_text = pre_selection;
        // ---- find / replace bar ---------------------------------------------
        // Rendered above the text so it never scrolls with the buffer. Buffer
        // mutations are deferred to `run_find_actions` (below), so the text
        // never changes under the fields' feet.
        let mut find_replace_current = false;
        let mut find_replace_all = false;
        let mut find_close = false;
        if let Some((_, at)) = &self.find.feedback
            && at.elapsed() >= FIND_FEEDBACK_TTL
        {
            self.find.feedback = None;
        }
        // Remember the last non-empty selection so 在选区内 stays available
        // after focus moves into the bar (a query change reselects the active
        // match, collapsing the live selection). Never cleared on blur/edits.
        if let Some(range) = &pre_selection_range
            && !range.is_empty()
        {
            self.find.scope_hint = Some(range.clone());
        }
        if self.find.open {
            if let Some(since) = self.find.pending_refresh {
                let elapsed = since.elapsed();
                if elapsed >= FIND_DEBOUNCE {
                    if self.find.pending_refresh_activates {
                        self.refresh_find_and_activate(pre_caret);
                    } else {
                        self.refresh_find_matches();
                    }
                } else {
                    ui.ctx()
                        .request_repaint_after(FIND_DEBOUNCE.saturating_sub(elapsed));
                }
            }
            // Prefill rule: only while the bar was just opened (`focus_pending`)
            // and the query is still empty — a non-empty query is the user's own
            // and is never clobbered by a plain re-focus.
            if self.find.focus_pending
                && self.find.query.is_empty()
                && let Some(selected) = &pre_selection_text
            {
                self.find.query = search::encode_newlines(selected);
                // A selection fills the query but never enables the scope
                // toggle implicitly. The scope remains an explicit user action.
                self.find.in_selection = None;
                self.refresh_find_matches();
            }
            let mut find_focused = false;
            let theme = Theme::from_ui(ui);
            let spacing = ui.spacing().item_spacing.x;
            let row_h = theme.spacing.quiet_h.max(theme.spacing.list_row_h);
            // The bar is a two-column striped grid: an auto label column shared
            // by the query and replace rows (so both inputs line up) and a last
            // column the grid expands into. Each row's last cell is a plain
            // horizontal layout; the toggles get a full-width wrapping row of
            // their own (`options_width`, `wrapped_checkbox`). A shared `Grid`
            // column would inflate to the query row's fixed controls and never
            // wrap — see docs/architecture.md §7 for the full rationale.
            let bar_width = ui.available_width();
            let label_col = text_width(ui, t("editor.find.label").as_ref())
                .max(text_width(ui, t("editor.find.replace_label").as_ref()));
            let nav_col = quiet_button_width(ui, t("editor.find.prev").as_ref())
                + spacing
                + quiet_button_width(ui, t("editor.find.next").as_ref())
                + spacing
                + COUNTER_WIDTH;
            let replace_col = quiet_button_width(ui, t("editor.find.replace").as_ref())
                + spacing
                + quiet_button_width(ui, t("editor.find.replace_all").as_ref());
            let actions_col = nav_col.max(replace_col);
            let feedback_col = CLOSE_SIZE.max(FEEDBACK_WIDTH);
            // The input takes the whole last-column cell minus the trailing
            // action and feedback cells and the spacing between the three, so it
            // stretches to fill and never tracks the toggles' width.
            let input_width = |ui: &egui::Ui| {
                (ui.available_width() - actions_col - feedback_col - 2.0 * spacing)
                    .max(MIN_QUERY_WIDTH)
            };
            // The toggles wrap at the bar's own right edge (the last cell minus
            // the label column and its gap), independent of the shared column's
            // width.
            let options_width = (bar_width - label_col - spacing).max(MIN_QUERY_WIDTH);

            // The settings-style stripe: egui paints `faint_bg_color` on odd
            // rows with a half-row-gap vertical bleed, 2px horizontal bleed and
            // radius 2, matching `striped_grid` and the search results rows.
            //
            // tui-whitelist: the whole find bar is laid out inside the editor
            // widget's leaf, so this `Grid` and the row `horizontal` /
            // `allocate_ui_with_layout` calls below cannot become tui trees
            // (single-tree rule, P1). Geometry and widths are unchanged from
            // P5; only this audit note is new.
            egui::Grid::new("typ-edit-find-bar")
                .num_columns(2)
                .spacing(egui::vec2(spacing, spacing))
                .striped(true)
                .show(ui, |ui| {
                    // Row 1: label | query (stretched) + prev/next + counter + close.
                    ui.label(t("editor.find.label").as_ref());
                    // tui-whitelist: 见上方 Grid 注记（编辑器 widget 叶内的行排布）。
                    ui.horizontal(|ui| {
                        let query = inline_input(
                            ui,
                            &mut self.find.query,
                            t("editor.find.query_hint").as_ref(),
                            input_width(ui),
                            Some(egui::Id::new(FIND_INPUT_ID)),
                        );
                        find_focused |= query.has_focus();
                        if query.changed() {
                            self.schedule_find_refresh();
                            ui.ctx().request_repaint_after(FIND_DEBOUNCE);
                        }
                        let has_matches = !self.find.matches.is_empty();
                        // tui-whitelist: 固定动作列（见上方 Grid 注记）。
                        ui.allocate_ui_with_layout(
                            egui::vec2(actions_col, row_h),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                if button(
                                    ui,
                                    t("editor.find.prev").as_ref(),
                                    ButtonVariant::Quiet,
                                    has_matches,
                                )
                                .on_hover_text(t("editor.find.prev_hover").as_ref())
                                .clicked()
                                {
                                    self.flush_find_refresh(pre_caret);
                                    self.find.active = find_bar::step_match(
                                        self.find.active,
                                        self.find.matches.len(),
                                        -1,
                                    );
                                    self.find.pending_select =
                                        self.find.matches.get(self.find.active).cloned();
                                }
                                if button(
                                    ui,
                                    t("editor.find.next").as_ref(),
                                    ButtonVariant::Quiet,
                                    has_matches,
                                )
                                .on_hover_text(t("editor.find.next_hover").as_ref())
                                .clicked()
                                {
                                    self.flush_find_refresh(pre_caret);
                                    self.find.active = find_bar::step_match(
                                        self.find.active,
                                        self.find.matches.len(),
                                        1,
                                    );
                                    self.find.pending_select =
                                        self.find.matches.get(self.find.active).cloned();
                                }
                                paint_counter(ui, &self.find, COUNTER_WIDTH);
                            },
                        );
                        // tui-whitelist: 固定反馈/关闭列（见上方 Grid 注记）。
                        ui.allocate_ui_with_layout(
                            egui::vec2(feedback_col, row_h),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if icon_button(ui, "×", CLOSE_SIZE, true)
                                    .on_hover_text(t("editor.find.close_hover").as_ref())
                                    .clicked()
                                {
                                    find_close = true;
                                }
                            },
                        );
                        // Enter in the query field steps to the next match
                        // (Shift+Enter to the previous one); singleline surrenders
                        // focus on Enter, so the loss of focus is part of the
                        // trigger.
                        if query.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            self.flush_find_refresh(pre_caret);
                            let delta = if ui.input(|i| i.modifiers.shift) {
                                -1
                            } else {
                                1
                            };
                            self.find.active = find_bar::step_match(
                                self.find.active,
                                self.find.matches.len(),
                                delta,
                            );
                            self.find.pending_select =
                                self.find.matches.get(self.find.active).cloned();
                            self.find.focus_pending = true;
                        }
                        if self.find.focus_pending {
                            query.request_focus();
                            self.find.focus_pending = false;
                        }
                    });
                    ui.end_row();

                    // Row 2: the matching toggles own a full-width row and wrap
                    // onto extra lines when the bar is narrower than they are. The
                    // invalid-regex hint rides along at the end of the flow.
                    ui.label("");
                    // tui-whitelist: 可换行选项行（见上方 Grid 注记）。
                    ui.allocate_ui_with_layout(
                        egui::vec2(options_width, row_h),
                        egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                        |ui| {
                            if wrapped_checkbox(
                                ui,
                                &mut self.find.case_sensitive,
                                t("editor.find.case_sensitive").as_ref(),
                                true,
                            )
                            .on_hover_text(t("editor.find.case_sensitive").as_ref())
                            .clicked()
                            {
                                self.refresh_find_and_activate(pre_caret);
                            }
                            if wrapped_checkbox(ui, &mut self.find.regex, ".*", true)
                                .on_hover_text(t("editor.find.regex").as_ref())
                                .clicked()
                            {
                                self.refresh_find_and_activate(pre_caret);
                            }
                            if wrapped_checkbox(ui, &mut self.find.whole_word, "ab|", true)
                                .on_hover_text(t("editor.find.whole_word").as_ref())
                                .clicked()
                            {
                                self.refresh_find_and_activate(pre_caret);
                            }
                            let scope_available = self.find.in_selection.is_some()
                                || self.find.scope_hint.is_some()
                                || pre_selection_range.is_some();
                            let mut scope_on = self.find.in_selection.is_some();
                            let scope_response = wrapped_checkbox(
                                ui,
                                &mut scope_on,
                                t("editor.find.in_selection").as_ref(),
                                scope_available,
                            )
                            .on_hover_text(t("editor.find.in_selection").as_ref());
                            if scope_response.clicked() {
                                self.find.in_selection = if scope_on {
                                    effective_scope(
                                        pre_selection_range.clone(),
                                        self.find.scope_hint.clone(),
                                    )
                                } else {
                                    None
                                };
                                self.refresh_find_and_activate(pre_caret);
                            }
                            if let Some(err) = &self.find.regex_error {
                                ui.colored_label(
                                    theme.colors.error,
                                    t("editor.find.regex_invalid").as_ref(),
                                )
                                .on_hover_text(err.as_str());
                            }
                        },
                    );
                    ui.end_row();

                    // Row 3: label | replacement (stretched) + replace buttons + feedback.
                    ui.label(t("editor.find.replace_label").as_ref());
                    // tui-whitelist: 见上方 Grid 注记（编辑器 widget 叶内的行排布）。
                    ui.horizontal(|ui| {
                        let replace = inline_input(
                            ui,
                            &mut self.find.replacement,
                            t("editor.find.replace_hint").as_ref(),
                            input_width(ui),
                            Some(egui::Id::new(REPLACE_INPUT_ID)),
                        );
                        find_focused |= replace.has_focus();
                        let has_matches = !self.find.matches.is_empty();
                        // tui-whitelist: 固定替换动作列（见上方 Grid 注记）。
                        ui.allocate_ui_with_layout(
                            egui::vec2(actions_col, row_h),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                if button(
                                    ui,
                                    t("editor.find.replace").as_ref(),
                                    ButtonVariant::Quiet,
                                    has_matches,
                                )
                                .on_hover_text(t("editor.find.replace_hover").as_ref())
                                .clicked()
                                {
                                    find_replace_current = true;
                                }
                                if button(
                                    ui,
                                    t("editor.find.replace_all").as_ref(),
                                    ButtonVariant::Quiet,
                                    has_matches,
                                )
                                .on_hover_text(t("editor.find.replace_all_hover").as_ref())
                                .clicked()
                                {
                                    find_replace_all = true;
                                }
                            },
                        );
                        // tui-whitelist: 固定反馈列（见上方 Grid 注记）。
                        ui.allocate_ui_with_layout(
                            egui::vec2(feedback_col, row_h),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| paint_feedback(ui, self.find.feedback.as_ref()),
                        );
                        // Enter in the replacement field replaces the active match.
                        if replace.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            find_replace_current = true;
                        }
                    });
                    ui.end_row();
                });
            // Escape closes the bar and hands focus back to the editor. Only
            // consumed while a bar field owns focus, so it keeps its meaning
            // everywhere else.
            if find_focused
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                find_close = true;
            }
            separator(ui);
        }
        FindActions {
            replace_current: find_replace_current,
            replace_all: find_replace_all,
            close: find_close,
        }
    }

    /// Runs 全部替换: rewrites every current match, records the result feedback
    /// and refreshes the match list. Returns `true` when the buffer changed.
    ///
    /// Extracted from [`Self::run_find_actions`] so the buffer/mirror
    /// consistency tests can drive the exact mutation path without a UI.
    pub(super) fn replace_all_matches(&mut self) -> bool {
        let replacement = search::decode_newlines(&self.find.replacement);
        let text = self.shared_text();
        let (replaced, count) = search::replace_all(&text, &self.find.matches, &replacement);
        self.find.feedback = Some((
            if count > 0 {
                t_with("editor.find.replaced", &[("n", &count.to_string())])
            } else {
                t("editor.find.no_match").into_owned()
            },
            Instant::now(),
        ));
        if count == 0 {
            return false;
        }
        self.replace_all_text(&replaced);
        self.refresh_find_matches();
        self.find.active = 0;
        self.find.pending_select = None;
        self.find.commit = true;
        true
    }

    /// Widget-agnostic core of the deferred find/replace actions.
    ///
    /// Mutates the buffer and the match list but touches no widget state, so
    /// the virtualized path can run it in its input pre-roll. Returns whether
    /// the buffer changed and whether the bar asked to close.
    pub(super) fn run_find_actions(&mut self, actions: FindActions) -> FindOutcome {
        let FindActions {
            replace_current: find_replace_current,
            replace_all: find_replace_all,
            close: find_close,
        } = actions;
        // The refresh below re-derives the match list; the newly active match is
        // queued in `pending_select` for the caller's selection/scroll pass.
        let text = self.shared_text();
        if find_replace_current
            && let Some(range) = self.find.matches.get(self.find.active).cloned()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end)
        {
            let replacement = search::decode_newlines(&self.find.replacement);
            self.edit_bytes(range.clone(), &replacement);
            self.refresh_find_matches();
            let insert_end = range.start + replacement.len();
            self.find.active =
                find_bar::first_match_at_or_after(&self.find.matches, insert_end).unwrap_or(0);
            self.find.pending_select = self.find.matches.get(self.find.active).cloned();
            self.find.feedback = Some((
                t_with("editor.find.replaced", &[("n", "1")]),
                Instant::now(),
            ));
            self.find.commit = true;
        }
        if find_replace_all {
            self.replace_all_matches();
        }
        if find_close {
            self.find.open = false;
            self.find.pending_refresh = None;
        }
        let outcome = FindOutcome {
            changed: self.find.commit,
            close: find_close,
        };
        self.find.commit = false;
        outcome
    }
}
