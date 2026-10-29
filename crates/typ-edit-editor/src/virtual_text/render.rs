//! Rendering, pointer interaction, scrolling and the geometry-dependent input
//! pass for the virtualized text path (Stages A-D).
//!
//! Stage A painted the buffer and owned wheel scrolling; Stage B added the
//! mouse (hit-testing, caret, selection, double/triple click, Shift+click,
//! hover, drag auto-scroll, caret blinking); Stage C routed keyboard/IME/
//! clipboard editing through [`EditorState::virtual_input`]; Stage D adds the
//! editor chrome and overlays: the find bar, gutter-click line selection, the
//! context menu, paste-image detection, match highlights, the current-line
//! band, bracket-pair outlines and diagnostic squiggles.
//!
//! The scroll model lives in [`super::scrollview`], the overlays in
//! [`super::overlay`] and the context menu in [`super::menu`]; this module
//! orchestrates them.

use std::ops::Range;

use egui::{Sense, pos2, vec2};
use typ_edit_model::{BytePos, HighlightKind};

use super::caret;
use super::chrome;
use super::hit;
use super::ime;
use super::keys::detect_paste_image;
use super::layout::LineData;
use super::menu;
use super::overlay::{self, Scene};
use super::parse_delta;
use super::scrollview::{self, ScrollFrame};
use crate::{EditorCommand, EditorInput, EditorOutput, EditorState, SpellAction, WrapMode};

/// Glyph fallback color for spans left as [`egui::Color32::PLACEHOLDER`].
///
/// The retired `TextEdit` path resolved placeholder text with the inactive
/// widget color; using `Visuals::text_color` here made unstyled markup gray.
fn glyph_fallback_color(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().widgets.inactive.text_color()
}

/// Values the render closure reports back to the caller for output assembly.
struct FrameSignals {
    ctrl_click: Option<usize>,
    hover_byte: Option<usize>,
    pointer_byte: Option<usize>,
    menu_command: Option<EditorCommand>,
    show_in_preview: Option<usize>,
    spell: Option<menu::SpellMenuAction>,
    rename_request: Option<usize>,
    references_request: Option<usize>,
}

impl EditorState {
    /// Virtual path entry point: draws the find bar, applies the queued and
    /// editing input, paints the buffer, handles pointer interaction and
    /// scrolling, and reports caret/hover/ctrl-click and the context-menu
    /// actions to the app. Called by [`EditorState::show`] when the virtual
    /// flag is on.
    pub(crate) fn show_virtual_text(
        &mut self,
        ui: &mut egui::Ui,
        input: EditorInput<'_>,
    ) -> EditorOutput {
        let request_focus = input.request_focus;
        let id = egui::Id::new(("typ-edit-virtual-text", self.serial));

        // A buffer swap can leave the caret/anchor past the end.
        self.virtual_text.clamp(self.buffer.len_chars());
        let caret_at_start = self.virtual_text.caret();
        // A buffer edit made outside the input pass (a remote apply) since the
        // last `show` is reported as `changed` once, so the app reparses.
        let external_changed = self.reported_rev != self.buffer.rev();

        let focused_id = ui.memory(|memory| memory.has_focus(id));
        let paste_image = detect_paste_image(ui, focused_id);

        // The find bar sits above the text.
        let pre_caret = Some(self.virtual_text.caret());
        let pre_selection_chars = self.virtual_text.selection_range();
        let pre_selection = pre_selection_chars
            .clone()
            .map(|range| self.buffer.slice(range));
        let pre_selection_range = pre_selection_chars.map(|range| self.selection_bytes(&range));
        let find_actions =
            self.find_bar_ui(ui, pre_caret, pre_selection.as_deref(), pre_selection_range);

        // The find actions edit the buffer and queue a match selection; running
        // them before the input/render pass keeps this frame's matches and
        // selection current.
        let rev_before = self.buffer.rev();
        let find_outcome = self.run_find_actions(find_actions);
        if find_outcome.close {
            ui.memory_mut(|memory| memory.request_focus(id));
            ui.ctx().request_repaint();
        }

        // Input pre-roll: queues, undo/redo, clipboard, find stepping and all
        // buffer edits happen before the layout borrows the buffer.
        let input_outcome = self.virtual_input(ui, id, &input);
        if input_outcome.changed && self.find.open {
            self.schedule_find_refresh_after_edit();
            ui.ctx().request_repaint_after(crate::FIND_DEBOUNCE);
        }

        let font_size = input.font_size;
        let font_family = input.font_family;
        // Plain text never soft-wraps regardless of the configured wrap mode.
        let wrap_mode = if input.plain_text {
            WrapMode::Off
        } else {
            input.wrap_mode
        };
        let line_spacing = input.line_spacing;
        let wrap_line_spacing = input.wrap_line_spacing;
        let parse_generation = input.parse_generation;
        // Spell issues are not parse-derived: they pass straight through even in
        // plain mode (the app decides whether to send any).
        let spell_issues = input.spell_issues;

        // Fresh parse data clears the delta log; while the parse lags an edit,
        // the stale byte ranges are shifted into the current buffer coordinates.
        match self.virtual_text.render_generation {
            Some(prev) if prev != parse_generation => {
                self.virtual_text.render_generation = Some(parse_generation);
                self.virtual_text.parse_deltas.borrow_mut().clear();
            }
            None => self.virtual_text.render_generation = Some(parse_generation),
            _ => {}
        }
        let deltas = self.virtual_text.parse_deltas.borrow().clone();
        let adjusted = (!input.plain_text && !deltas.is_empty())
            .then(|| parse_delta::AdjustedInput::build(&input, &deltas));
        // The input boundary is typed (`ByteRange` / `BytePos`); unwrap it to the
        // char/usize space the layout consumes. Plain mode ignores every
        // parse-derived range, so the vectors stay empty there.
        let untyped_range = |range: &typ_edit_model::ByteRange| range.start.0..range.end.0;
        let highlights_u: Vec<(Range<usize>, HighlightKind)> = if !input.plain_text {
            {
                input
                    .highlights
                    .iter()
                    .map(|(range, kind)| (untyped_range(range), *kind))
                    .collect()
            }
        } else {
            Default::default()
        };
        let blocked_u: Vec<Range<usize>> = if !input.plain_text {
            input.blocked_ranges.iter().map(untyped_range).collect()
        } else {
            Default::default()
        };
        let code_u: Vec<Range<usize>> = if !input.plain_text {
            input.code_ranges.iter().map(untyped_range).collect()
        } else {
            Default::default()
        };
        let math_u: Vec<Range<usize>> = if !input.plain_text {
            input.math_ranges.iter().map(untyped_range).collect()
        } else {
            Default::default()
        };
        let math_atoms_u: Vec<Range<usize>> = if !input.plain_text {
            input.math_atoms.iter().map(untyped_range).collect()
        } else {
            Default::default()
        };
        // Folds ride the same adjustment as every other parse-derived range; in
        // plain mode the parse supplies none.
        let folds: &[typ_edit_model::FoldRange] = if input.plain_text {
            &[]
        } else {
            adjusted
                .as_ref()
                .map_or(input.folds, |a| a.folds.as_slice())
        };
        // In plain mode every parse-derived input is ignored: no highlight
        // kinds (so glyphs use the fallback text color), no atoms/mode ranges
        // and no diagnostic squiggles.
        let (highlights, blocked_ranges, code_ranges, math_ranges, math_atoms, diagnostics) =
            if input.plain_text {
                (&[][..], &[][..], &[][..], &[][..], &[][..], &[][..])
            } else {
                (
                    adjusted
                        .as_ref()
                        .map_or(highlights_u.as_slice(), |a| a.highlights.as_slice()),
                    adjusted
                        .as_ref()
                        .map_or(blocked_u.as_slice(), |a| a.blocked_ranges.as_slice()),
                    adjusted
                        .as_ref()
                        .map_or(code_u.as_slice(), |a| a.code_ranges.as_slice()),
                    adjusted
                        .as_ref()
                        .map_or(math_u.as_slice(), |a| a.math_ranges.as_slice()),
                    adjusted
                        .as_ref()
                        .map_or(math_atoms_u.as_slice(), |a| a.math_atoms.as_slice()),
                    adjusted
                        .as_ref()
                        .map_or(input.diagnostics, |a| a.diagnostics.as_slice()),
                )
            };

        let EditorState {
            buffer,
            serial,
            virtual_text,
            find,
            ..
        } = self;
        let text_rev = buffer.rev();
        let line_count = buffer.line_count().max(1);
        let serial = *serial;

        // Gutter width: the line-number column plus the fold band on its left.
        let digits = line_count.to_string().len();
        let gutter_w =
            (digits as f32 * (font_size * 0.85 * 0.6) + 14.0).max(30.0) + chrome::FOLD_BAND_W;

        let mut menu_command: Option<EditorCommand> = None;
        let mut show_in_preview: Option<usize> = None;
        let mut spell_action: Option<SpellAction> = None;
        let mut completion_anchor: Option<egui::Pos2> = None;
        let mut completion_bounds: Option<egui::Rect> = None;
        let mut caret_screen: Option<egui::Rect> = None;

        let signals = ui
            .horizontal_top(|ui| {
                let (gutter_rect, _) =
                    ui.allocate_exact_size(vec2(gutter_w, ui.available_height()), Sense::hover());
                // Space is reserved with a hover sense, then the same rect is
                // registered with a stable id so focus/blink survive re-layout.
                let (viewport_rect, _) =
                    ui.allocate_exact_size(ui.available_size(), Sense::hover());
                let mut response = ui.interact(viewport_rect, id, Sense::click_and_drag());
                // A focused widget receives a synthetic click from Space/Enter;
                // only real pointer clicks may move the caret.
                response.flags -= egui::response::Flags::FAKE_PRIMARY_CLICKED;
                if request_focus {
                    response.request_focus();
                }
                let focused = ui.memory(|memory| memory.has_focus(id));

                // The gutter splits into a fold band (left 10px, chevrons) and a
                // line-number area; each is its own click target.
                let band_rect = egui::Rect::from_min_max(
                    gutter_rect.min,
                    pos2(
                        gutter_rect.left() + chrome::FOLD_CHEVRON_W,
                        gutter_rect.bottom(),
                    ),
                );
                let band_response = ui.interact(
                    band_rect,
                    egui::Id::new(("fold-band", serial)),
                    Sense::click(),
                );
                let number_rect = egui::Rect::from_min_max(
                    pos2(gutter_rect.left() + chrome::FOLD_BAND_W, gutter_rect.top()),
                    gutter_rect.max,
                );
                let number_response = ui.interact(
                    number_rect,
                    egui::Id::new(("gutter-number", serial)),
                    Sense::click(),
                );
                // The bookmark band is the right half of the fold band.
                let bookmark_rect = egui::Rect::from_min_max(
                    pos2(
                        gutter_rect.left() + chrome::FOLD_CHEVRON_W,
                        gutter_rect.top(),
                    ),
                    pos2(
                        gutter_rect.left() + chrome::FOLD_BAND_W,
                        gutter_rect.bottom(),
                    ),
                );
                let bookmark_response = ui.interact(
                    bookmark_rect,
                    egui::Id::new(("bookmark-band", serial)),
                    Sense::click(),
                );

                let wrap_width = if matches!(wrap_mode, WrapMode::Off) {
                    f32::INFINITY
                } else {
                    (viewport_rect.width() - 2.0).max(80.0)
                };
                let data = LineData {
                    buffer: &*buffer,
                    text_rev,
                    parse_generation,
                    highlights,
                    blocked_ranges,
                    code_ranges,
                    math_ranges,
                    math_atoms,
                    font_family,
                    font_size,
                    wrap_mode,
                    line_spacing,
                    wrap_line_spacing,
                    wrap_width,
                };
                let viewport_h = viewport_rect.height();
                let fallback = virtual_text.cache.row_height(ui, &data);
                // Rebuild the fold candidates from this frame's (adjusted) parse
                // ranges and zero the height of every hidden line before anything
                // measures or scrolls.
                virtual_text.folds.sync(&*buffer, folds);
                let hidden = virtual_text.combined_hidden(&*buffer);
                virtual_text.cache.set_hidden(hidden, fallback);
                if !virtual_text.cache.hidden().is_empty() {
                    virtual_text.relocate_out_of_hidden(&*buffer);
                }
                let now = ui.input(|i| i.time);

                // ---- wheel + scrollbar + normalization + visible lines -----
                let scroll_frame = ScrollFrame {
                    data: &data,
                    line_count,
                    viewport_rect,
                    viewport_h,
                    fallback,
                    hovered: response.hovered(),
                    wrap_mode,
                    serial,
                    now,
                };
                let scrollview::ScrollOutcome {
                    mut visible,
                    scrollbar,
                    hscrollbar,
                } = scrollview::run_scroll(virtual_text, ui, &scroll_frame);
                #[cfg(test)]
                {
                    virtual_text.test_hscrollbar = hscrollbar.as_ref().map(|bar| bar.bar_rect);
                    virtual_text.test_hscrollbar_thumb =
                        hscrollbar.as_ref().map(|bar| bar.thumb_rect);
                    virtual_text.test_scrollbar_hovered =
                        scrollbar.response.hovered() || scrollbar.response.dragged();
                    virtual_text.test_hscrollbar_hovered = hscrollbar
                        .as_ref()
                        .is_some_and(|bar| bar.response.hovered() || bar.response.dragged());
                }

                // ---- gutter click: select the clicked logical line ---------
                if number_response.clicked()
                    && let Some(pointer) = number_response.interact_pointer_pos()
                {
                    response.request_focus();
                    if chrome::handle_gutter_click(
                        virtual_text,
                        ui,
                        &data,
                        line_count,
                        viewport_rect,
                        viewport_h,
                        &visible,
                        pointer.y,
                        now,
                    ) {
                        visible = virtual_text.cache.visible_lines(
                            ui,
                            &data,
                            &virtual_text.anchor,
                            viewport_h,
                            fallback,
                        );
                    }
                }

                // ---- fold band click: toggle the chevron under the pointer --
                if band_response.clicked()
                    && let Some(pointer) = band_response.interact_pointer_pos()
                {
                    response.request_focus();
                    if let Some(line) = chrome::line_at_viewport_y(
                        virtual_text,
                        ui,
                        &data,
                        &visible,
                        pointer.y - viewport_rect.top(),
                    ) && virtual_text.folds.toggle_at_line(&*buffer, line)
                    {
                        virtual_text.relocate_out_of_hidden(&*buffer);
                        let hidden = virtual_text.combined_hidden(&*buffer);
                        virtual_text.cache.set_hidden(hidden, fallback);
                        visible = virtual_text.cache.visible_lines(
                            ui,
                            &data,
                            &virtual_text.anchor,
                            viewport_h,
                            fallback,
                        );
                    }
                }

                // ---- bookmark band click: toggle the bookmark under the pointer
                if bookmark_response.clicked()
                    && let Some(pointer) = bookmark_response.interact_pointer_pos()
                {
                    response.request_focus();
                    if let Some(line) = chrome::line_at_viewport_y(
                        virtual_text,
                        ui,
                        &data,
                        &visible,
                        pointer.y - viewport_rect.top(),
                    ) {
                        virtual_text.bookmarks.toggle_line(&*buffer, line);
                    }
                }

                // ---- queued jump settle: land the target in this frame ----
                if chrome::settle_pending_scroll(
                    virtual_text,
                    ui,
                    &data,
                    line_count,
                    viewport_h,
                    &visible,
                ) {
                    visible = virtual_text.cache.visible_lines(
                        ui,
                        &data,
                        &virtual_text.anchor,
                        viewport_h,
                        fallback,
                    );
                }

                virtual_text.last_visible = match (visible.first(), visible.last()) {
                    (Some((first, _, _)), Some((last, _, _))) => *first..(*last + 1),
                    _ => virtual_text.anchor.top_line..virtual_text.anchor.top_line,
                };
                #[cfg(test)]
                hit::record_test_geometry(virtual_text, ui, &data, &visible, viewport_rect);

                let mut slots = hit::build_slots(virtual_text, ui, &data, &visible, viewport_rect);

                // ---- pointer, vertical navigation, scroll-into-view --------
                let (outcome, anchor_changed) = caret::interact_frame(
                    ui,
                    virtual_text,
                    &caret::FrameInteraction {
                        response: &response,
                        viewport_rect,
                        slots: &slots,
                        data: &data,
                        line_count,
                        viewport_h,
                        focused,
                        input_caret_moved: input_outcome.caret_moved,
                    },
                );
                if anchor_changed {
                    virtual_text
                        .cache
                        .set_horizontal(virtual_text.h_offset, viewport_rect.width());
                    let visible = virtual_text.cache.visible_lines(
                        ui,
                        &data,
                        &virtual_text.anchor,
                        viewport_h,
                        fallback,
                    );
                    slots = hit::build_slots(virtual_text, ui, &data, &visible, viewport_rect);
                }

                // The completion popup is anchored at the caret's bottom-left,
                // falling back to the viewport top-left when the caret is off
                // screen; it is painted after the closure so it floats above.
                completion_anchor = Some(
                    hit::caret_screen_rect(buffer, &slots, virtual_text.caret())
                        .map_or_else(|| viewport_rect.left_top(), |rect| rect.left_bottom()),
                );
                completion_bounds = Some(viewport_rect.intersect(ui.ctx().content_rect()));

                // ---- paint: band, selection, find, glyphs, caret, overlays ---
                let clip = viewport_rect.intersect(ui.clip_rect());
                {
                    let carets = virtual_text.selection.borrow();
                    let primary = carets.primary();
                    let scene = Scene {
                        clip,
                        slots: &slots,
                        buffer,
                        carets: &carets,
                        preedit: virtual_text.preedit_range.as_ref(),
                        find: find.open.then_some(&*find),
                        diagnostics,
                        spell_issues,
                        debug_highlights: input.debug_highlights,
                        debug_highlight_color: input.debug_highlight_color,
                        focused,
                        caret_char: primary.caret,
                        text_color: glyph_fallback_color(ui),
                        blink_origin: virtual_text.blink_origin,
                        blame_inline: input
                            .blame_inline
                            .as_ref()
                            .map(|(line, text)| (*line, text.as_str())),
                        font_family,
                        font_size,
                        remote_carets: input.remote_carets,
                    };
                    overlay::paint_scene(ui, &scene);
                }

                // ---- IME output (screen coords) while focused ----------------
                // The caret rect is reused as the rename overlay's anchor.
                caret_screen = hit::caret_screen_rect(buffer, &slots, virtual_text.caret());
                if focused {
                    let cursor_rect = caret_screen.unwrap_or(viewport_rect);
                    ime::emit_ime_output(ui, cursor_rect);
                }

                let fold_markers = virtual_text.folds.markers(&*buffer);
                let bookmarked = virtual_text.bookmarks.lines(&*buffer);
                chrome::paint_gutter(
                    ui,
                    gutter_rect,
                    viewport_rect,
                    &visible,
                    &mut virtual_text.cache,
                    &data,
                    font_size,
                    &fold_markers,
                    &bookmarked,
                );
                scrollview::paint(ui, clip, &scrollbar);
                if let Some(hbar) = &hscrollbar {
                    scrollview::paint_horizontal(ui, clip, hbar);
                }

                // ---- editor context menu ------------------------------------
                let cursor_byte = buffer.char_to_byte(virtual_text.caret());
                let spell = menu::spell_menu_data(spell_issues, cursor_byte);
                let menu = menu::show(
                    &response,
                    virtual_text.any_selection(),
                    !buffer.is_empty(),
                    cursor_byte,
                    spell.as_ref(),
                );
                menu_command = menu.command;
                show_in_preview = menu.show_in_preview;
                let rename_request = menu::rename_request(&menu, cursor_byte);
                let references_request = menu::references_request(&menu, cursor_byte);
                // The open menu owns the pointer, so the editor hover report is
                // suppressed while it is shown.
                let hover_byte = menu::hover_without_menu(outcome.hover_byte, menu.menu_open);
                let pointer_over_text = ui
                    .ctx()
                    .input(|input| input.pointer.hover_pos())
                    .is_some_and(|pointer| viewport_rect.contains(pointer))
                    && !scrollbar.response.hovered()
                    && !scrollbar.response.dragged()
                    && hscrollbar
                        .as_ref()
                        .is_none_or(|bar| !bar.response.hovered() && !bar.response.dragged())
                    && !menu.menu_open;
                if pointer_over_text {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                }

                FrameSignals {
                    ctrl_click: outcome.ctrl_click,
                    hover_byte,
                    pointer_byte: outcome.pointer_byte,
                    menu_command,
                    show_in_preview,
                    spell: menu.spell,
                    rename_request,
                    references_request,
                }
            })
            .inner;

        // The context menu and the app's queued menu-bar command share
        // [`EditorState::virtual_command`], so both entry points behave the same.
        let mut menu_changed = false;
        if let Some(command) = signals.menu_command {
            let before = self.buffer.rev();
            self.virtual_command(ui, id, command);
            menu_changed = self.buffer.rev() != before;
            if menu_changed && self.find.open {
                self.refresh_find_matches();
            }
        }
        // A suggestion replaces in the editor (the app never sees the text); the
        // add/ignore actions surface to the app, which owns the dictionary.
        match signals.spell {
            Some(menu::SpellMenuAction::Replace { range, text }) => {
                let before = self.buffer.rev();
                self.replace_byte_range(range, &text);
                if self.buffer.rev() != before {
                    menu_changed = true;
                }
                if self.find.open {
                    self.refresh_find_matches();
                }
            }
            Some(menu::SpellMenuAction::AddToDictionary { word }) => {
                spell_action = Some(SpellAction::AddToDictionary(word));
            }
            Some(menu::SpellMenuAction::Ignore { word }) => {
                spell_action = Some(SpellAction::Ignore(word));
            }
            None => {}
        }

        // ---- completion popup (Stage D2) ------------------------------------
        // A caret relocation without an edit (pointer, arrows, a jump) leaves the
        // popup behind; typing refreshes it instead, in `virtual_input`.
        let caret_now = self.virtual_text.caret();
        self.dismiss_if_caret_moved(Some(caret_at_start), caret_now, input_outcome.changed);
        let mut completion_changed = false;
        let completion_anchor = completion_anchor.unwrap_or(egui::Pos2::ZERO);
        let completion_bounds = completion_bounds.unwrap_or(egui::Rect::NOTHING);
        if let Some(index) =
            self.completion_popup_at(ui, completion_anchor, completion_bounds, font_family)
            && let Some((word_start, item)) = self.completion.take().and_then(|popup| {
                popup
                    .items
                    .get(index)
                    .cloned()
                    .map(|item| (popup.word_start, item))
            })
        {
            let caret = self.virtual_text.caret();
            if self.virtual_apply_completion(word_start, caret, &item) {
                completion_changed = true;
                self.after_completion_accept(&item);
                ui.memory_mut(|memory| memory.request_focus(id));
            }
        }

        self.virtual_focus_filter(ui, id);

        // An idle frame closes the undo group so the next keystroke starts a
        // fresh one; a frame that edited the buffer leaves it open so the whole
        // run merges into a single undo step.
        if self.buffer.rev() == rev_before {
            self.group_open.set(false);
        }
        self.reported_rev = self.buffer.rev();
        if input_outcome.changed || find_outcome.changed || menu_changed || completion_changed {
            ui.ctx().request_repaint();
        }
        // A budget-skipped long line repaints so it can shape next frame.
        if self.virtual_text.cache.budget_hit() {
            ui.ctx().request_repaint();
        }

        let cursor_byte = self.buffer.char_to_byte(self.virtual_text.caret());
        let rename_request = signals
            .rename_request
            .or_else(|| input_outcome.rename.then_some(cursor_byte));

        EditorOutput {
            changed: external_changed
                || input_outcome.changed
                || find_outcome.changed
                || menu_changed
                || completion_changed,
            cursor_byte: BytePos(cursor_byte),
            show_in_preview: signals.show_in_preview.map(BytePos),
            ctrl_click: signals.ctrl_click.map(BytePos),
            hover_byte: signals.hover_byte.map(BytePos),
            pointer_byte: signals.pointer_byte.map(BytePos),
            paste_image,
            selections: self
                .selection_byte_ranges()
                .into_iter()
                .map(|range| BytePos(range.start)..BytePos(range.end))
                .collect(),
            spell_action,
            rename_request: rename_request.map(BytePos),
            references_request: signals.references_request.map(BytePos),
            completion_request: input_outcome.completion_request.map(BytePos),
            caret_screen,
            history_request: input_outcome.history_request,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_fallback_matches_the_inactive_widget_text_color() {
        let ctx = egui::Context::default();
        let mut resolved = None;
        let mut inactive = None;
        let mut plain = None;
        ctx.run_ui(Default::default(), |ui| {
            resolved = Some(glyph_fallback_color(ui));
            inactive = Some(ui.visuals().widgets.inactive.text_color());
            plain = Some(ui.visuals().text_color());
        })
        .drop_without_applying_deltas();
        assert_eq!(resolved.unwrap(), inactive.unwrap());
        assert_ne!(
            resolved.unwrap(),
            plain.unwrap(),
            "the fallback must not be the (darker) plain text color"
        );
    }
}
