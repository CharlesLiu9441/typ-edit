//! Auto-completion: the popup UI and the refresh glue that drives the pure
//! logic in [`logic`]. The popup state (`CompletionPopup`) stays in `lib.rs`
//! (privacy rule: structs with private fields stay in the parent module).

mod logic;

pub(crate) use logic::*;

use crate::{
    COMPLETION_MAX_HEIGHT, COMPLETION_MAX_ROWS, COMPLETION_POPUP_ID, COMPLETION_POPUP_WIDTH,
    COMPLETION_TOOLTIP_ID, CompletionPopup, EditorInput, EditorState, completion,
};
use typ_edit_model::CompletionItem;
use typ_edit_widgets::theme::Theme;

impl EditorState {
    /// Paints the completion popup anchored at `caret` (its bottom-left, in
    /// screen space) and clamped to `bounds`, applying hover/scroll selection.
    /// Returns the index of a clicked row, if any.
    ///
    /// Called by the virtualized path after it has resolved the caret anchor.
    pub(super) fn completion_popup_at(
        &mut self,
        ui: &mut egui::Ui,
        caret: egui::Pos2,
        bounds: egui::Rect,
        font_family: &str,
    ) -> Option<usize> {
        let family = egui::FontFamily::Name(std::sync::Arc::from(font_family));
        // ---- completion popup ------------------------------------------------
        // Painted last so it floats above squiggles and tooltips. Rows are
        // hover/click targets; clicking one accepts, exactly like Tab/Enter.
        // Candidate names are left-aligned, their kind tags right-aligned.
        let popup = self.completion.as_mut()?;
        let mut completion_hover: Option<usize> = None;
        let mut completion_click: Option<usize> = None;
        // A resting pointer must not fight keyboard navigation, so the hover
        // highlight only follows the mouse when the pointer actually moved or
        // the list scrolled underneath it.
        let pointer_moved = ui.ctx().input(|i| i.pointer.delta() != egui::Vec2::ZERO);
        let mut scroll_offset: Option<f32> = None;
        let popup_width = COMPLETION_POPUP_WIDTH.min(bounds.width().max(160.0));
        let row_height = (ui.text_style_height(&egui::TextStyle::Body) + 6.0).max(18.0);
        // The height follows the candidate count only, never the selection: the
        // selected item's detail lives in a tooltip, so moving the selection
        // cannot resize the list.
        let rows = popup.items.len().min(COMPLETION_MAX_ROWS);
        let popup_h = (rows as f32 * row_height + 8.0).min(COMPLETION_MAX_HEIGHT);
        // Keep the popup inside the editor: shift left at the right edge and
        // flip above the caret when it would overflow the bottom.
        let x = caret
            .x
            .min((bounds.right() - popup_width).max(bounds.left()));
        let below = caret.y + 2.0;
        let y = if below + popup_h > bounds.bottom() {
            (caret.y - popup_h - 4.0).max(bounds.top())
        } else {
            below
        };
        let pos = egui::pos2(x.max(bounds.left()), y);
        egui::Area::new(egui::Id::new(COMPLETION_POPUP_ID))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain_to(ui.ctx().content_rect())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(popup_width);
                    ui.set_max_width(popup_width);
                    ui.set_min_height(popup_h);
                    ui.set_max_height(popup_h);
                    // tui-whitelist: the Area content is exactly one ScrollArea
                    // (the allowed container mechanism) whose rows are painted
                    // one leaf at a time; there is no multi-child layout to
                    // replace. A top-level tui wrapper was evaluated and
                    // reverted: its first-frame `request_discard` makes the
                    // editor's one-input-per-frame frame harness run twice,
                    // for no layout benefit.
                    let scrolled = egui::ScrollArea::vertical()
                        .max_height(COMPLETION_MAX_HEIGHT)
                        .show(ui, |ui| {
                            let width = ui.available_width();
                            let theme = Theme::from_ui(ui);
                            let font_size = ui
                                .style()
                                .text_styles
                                .get(&egui::TextStyle::Body)
                                .map_or(13.0, |font| font.size);

                            for (index, item) in popup.items.iter().enumerate() {
                                let selected = index == popup.selected;
                                // tui-whitelist: 单叶 allocate_exact_size
                                // （候选行整行 + 命中区），名称/类型标签自绘。
                                let (rect, mut row) = ui.allocate_exact_size(
                                    egui::vec2(width, row_height),
                                    egui::Sense::click(),
                                );

                                let row_radius = egui::CornerRadius::same(theme.radii.tiny);
                                if selected {
                                    // Selected is a grayscale row plus the
                                    // accent outline, never a solid accent fill.
                                    ui.painter().rect_stroke(
                                        rect,
                                        row_radius,
                                        egui::Stroke::new(
                                            theme.strokes.accent,
                                            theme.colors.accent,
                                        ),
                                        egui::StrokeKind::Inside,
                                    );
                                } else if row.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        row_radius,
                                        theme.colors.widget_hover_fill.gamma_multiply(0.5),
                                    );
                                }
                                let text_color = if selected {
                                    theme.colors.text_strong
                                } else {
                                    theme.colors.text
                                };
                                let tag_color = if selected {
                                    theme.colors.text_strong
                                } else {
                                    theme.colors.text_weak
                                };
                                // The editor family everywhere, sizes unchanged.
                                let name_font = egui::FontId::new(font_size, family.clone());
                                let tag_font = egui::FontId::new(font_size * 0.85, family.clone());
                                let tag = completion::kind_tag(item.kind);
                                let tag_width = ui
                                    .fonts_mut(|fonts| {
                                        fonts.layout_no_wrap(
                                            tag.to_owned(),
                                            tag_font.clone(),
                                            tag_color,
                                        )
                                    })
                                    .size()
                                    .x;

                                // A long name ellipsizes with one `…` instead of
                                // running under the right-aligned tag.
                                let name_max = (width - 12.0 - tag_width - 6.0).max(8.0);
                                let mut name_job = egui::text::LayoutJob::default();
                                name_job.wrap.max_width = name_max;
                                name_job.wrap.max_rows = 1;
                                name_job.wrap.overflow_character = Some('…');
                                name_job.append(
                                    &item.name,
                                    0.0,
                                    egui::TextFormat {
                                        font_id: name_font,
                                        color: text_color,
                                        ..Default::default()
                                    },
                                );
                                let name_galley = ui.fonts_mut(|fonts| fonts.layout_job(name_job));
                                ui.painter().galley(
                                    egui::pos2(
                                        rect.left() + 6.0,
                                        rect.center().y - name_galley.size().y * 0.5,
                                    ),
                                    name_galley,
                                    text_color,
                                );
                                ui.painter().text(
                                    egui::pos2(rect.right() - 6.0, rect.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    tag,
                                    tag_font,
                                    tag_color,
                                );
                                if selected {
                                    completion_detail_tooltip(ui, rect, item);
                                }

                                if row.hovered() {
                                    completion_hover = Some(index);
                                    // The full name stays reachable when the
                                    // painted one is truncated.
                                    row = row.on_hover_text(item.name.clone());
                                }
                                if row.clicked() {
                                    completion_click = Some(index);
                                }
                                // Keyboard navigation must keep the highlighted
                                // row inside the viewport.
                                if selected && !ui.clip_rect().contains_rect(rect) {
                                    ui.scroll_to_rect(rect, None);
                                }
                            }
                        });
                    scroll_offset = Some(scrolled.state.offset.y);
                });
            });
        let scrolled_this_frame =
            scroll_offset.is_some_and(|offset| (offset - popup.scroll_offset).abs() > 0.5);
        let scroll_ended = popup.was_scrolling && !scrolled_this_frame;
        if let Some(offset) = scroll_offset {
            popup.scroll_offset = offset;
        }
        popup.was_scrolling = scrolled_this_frame;
        if scrolled_this_frame {
            // Guarantee one more frame after the scroll settles, where the
            // hover selection is recomputed at the final resting position.
            ui.ctx().request_repaint();
        }
        if let Some(index) = completion_hover
            && (pointer_moved || scrolled_this_frame || scroll_ended)
            && popup.selected != index
        {
            popup.selected = index;
            ui.ctx().request_repaint();
        }
        completion_click
    }

    /// Recomputes the completion popup from the word ending at the caret.
    ///
    /// Must run after the smart-editing blocks so the prefix matches the final
    /// buffer. Completion only exists where identifiers are evaluated: code
    /// (keywords, values, document definitions) and math (math symbols). Plain
    /// markup gets none; `#` starts logic mode, so a word directly after it is
    /// code even before the parse results catch up. Inside the argument list of
    /// a resolved call the callee's parameter names replace everything else.
    ///
    /// `gate` decides when a closed popup may open: a keystroke opens for
    /// letters, `#`, a comma inside a call slot, or a `.` that extends a dotted
    /// head (`cell.`); a fresh parse result opens an unfiltered parameter list
    /// (the keystroke that got there often raced that parse); a manual
    /// Ctrl/Cmd+Space opens regardless of prefix. An open popup is always
    /// refreshed. A named argument's value region uses the engine's
    /// type-constrained `value_slot` candidates instead of the parameter list.
    pub(super) fn refresh_completion(
        &mut self,
        caret_char: usize,
        input: &EditorInput<'_>,
        gate: completion::CompletionGate,
    ) {
        self.last_parse_generation = input.parse_generation;
        let completion_max = input.completion_max.clamp(1, 1000);
        // World candidates are context-accurate for the member position they
        // were fetched at, so they replace the static + definition list. A
        // keystroke means the text moved on: the app clears the list a frame
        // later, but the refresh must already fall back to static rather than
        // paint members for the old position.
        let world = if matches!(gate, completion::CompletionGate::Keystroke(_)) {
            &[][..]
        } else {
            input.world_completions
        };
        let (word_start, prefix) = if world.is_empty() {
            completion::completion_prefix_buffer(&self.buffer, caret_char)
        } else {
            completion::member_prefix_buffer(&self.buffer, caret_char)
        };
        let prefix = prefix.as_str();
        let code_ranges: Vec<std::ops::Range<usize>> = input
            .code_ranges
            .iter()
            .map(|range| range.start.0..range.end.0)
            .collect();
        let math_ranges: Vec<std::ops::Range<usize>> = input
            .math_ranges
            .iter()
            .map(|range| range.start.0..range.end.0)
            .collect();
        let context = completion::member_completion_context(
            &self.buffer,
            caret_char,
            &code_ranges,
            &math_ranges,
        );
        // The caret sits right after the last typed char, exactly at the end
        // of the half-open `Args` range while the list is still open, so the
        // range end is inclusive here.
        let caret_byte = self.buffer.char_to_byte(caret_char);
        let in_call = input
            .call_params
            .filter(|params| completion::caret_in_call(params, caret_byte));
        let is_params = in_call.is_some();
        // In a named argument's value region the engine supplies the
        // type-constrained candidates; they replace every other source.
        let value_slot = in_call
            .as_ref()
            .and_then(|params| params.value_slot.as_ref());
        // A typed number in a value slot is the entry point for unit completion:
        // digits are not general triggers, so the popup must be allowed to open
        // for them.
        let numeric_slot = value_slot.is_some() && completion::numeric_prefix(prefix);
        let allow_open = match gate {
            completion::CompletionGate::Keystroke(ch) => {
                // A `.` opens only when it extends an existing dotted head
                // (`cell.`); a lone `.` refreshes an open popup but never opens
                // a closed one.
                completion::completion_may_open(ch, is_params)
                    || (ch == Some('.') && prefix.len() > 1)
                    || (numeric_slot && ch.is_some_and(|c| c.is_ascii_digit()))
            }
            completion::CompletionGate::Parse => (is_params && prefix.is_empty()) || numeric_slot,
            completion::CompletionGate::Manual => true,
        };

        // Completion only exists where identifiers are evaluated. A dot in
        // markup must never open the popup, not even when a world member list
        // arrived for it. A resolved call slot implies code context, so it is
        // exempt and keeps its parameter list.
        if context == completion::CompletionContext::Markup && !is_params {
            self.completion = None;
            return;
        }

        let items = if !world.is_empty() {
            if prefix.is_empty() {
                world.iter().take(completion_max).cloned().collect()
            } else {
                completion::rank_completions(prefix, &[], world, false, completion_max)
            }
        } else if let Some(slot) = value_slot {
            if prefix.is_empty() {
                slot.items.iter().take(completion_max).cloned().collect()
            } else {
                completion::rank_completions(prefix, &slot.items, &[], false, completion_max)
            }
        } else if let Some(call) = in_call {
            if prefix.is_empty() {
                call.items.iter().take(completion_max).cloned().collect()
            } else {
                completion::call_slot_items(
                    prefix,
                    call,
                    input.definitions,
                    input.completions,
                    completion_max,
                )
            }
        } else {
            match context {
                completion::CompletionContext::Markup => {
                    self.completion = None;
                    return;
                }
                completion::CompletionContext::Math => {
                    if prefix.is_empty() && matches!(gate, completion::CompletionGate::Manual) {
                        input
                            .completions
                            .iter()
                            .filter(|item| item.kind.is_math())
                            .take(completion_max)
                            .cloned()
                            .collect()
                    } else {
                        completion::dotted_member_items(
                            prefix,
                            &[],
                            input.completions,
                            true,
                            completion_max,
                        )
                    }
                }
                completion::CompletionContext::Code => {
                    if prefix.is_empty() {
                        input
                            .completions
                            .iter()
                            .filter(|item| !item.kind.is_math())
                            .take(completion_max)
                            .cloned()
                            .collect()
                    } else {
                        let mut items = completion::dotted_member_items(
                            prefix,
                            input.definitions,
                            input.completions,
                            false,
                            completion_max,
                        );
                        for item in completion::dotted_method_items(prefix, input.completions) {
                            if !items.iter().any(|existing| existing.name == item.name) {
                                items.push(item);
                            }
                        }
                        items.truncate(completion_max);
                        items
                    }
                }
            }
        };

        if items.is_empty() {
            self.completion = None;
            return;
        }
        if self.completion.is_none() && !allow_open {
            return;
        }
        // Keep the highlighted candidate across refreshes while it survives.
        let previous = self
            .completion
            .as_ref()
            .and_then(|popup| popup.items.get(popup.selected))
            .map(|item| item.name.as_str());
        let selected = previous
            .and_then(|name| items.iter().position(|item| item.name == name))
            .unwrap_or(0);
        let scroll_offset = self
            .completion
            .as_ref()
            .map_or(0.0, |popup| popup.scroll_offset);
        let was_scrolling = self
            .completion
            .as_ref()
            .is_some_and(|popup| popup.was_scrolling);
        let navigated = self
            .completion
            .as_ref()
            .is_some_and(|popup| popup.navigated);
        self.completion = Some(CompletionPopup {
            items,
            selected,
            word_start,
            scroll_offset,
            was_scrolling,
            params: is_params && world.is_empty(),
            navigated,
        });
    }
}

/// Shows the selected candidate's detail to the right of its row.
///
/// The tooltip is its own popup layer anchored to the row's global rect, so it
/// never participates in the list's layout or gets clipped by the popup. A
/// candidate without detail shows nothing.
///
/// tui-whitelist: egui `Tooltip` is the container mechanism; its content is a
/// short stack of stock labels whose wrapping depends on egui's own max-width
/// pass, so it is kept as-is to avoid changing line breaks.
fn completion_detail_tooltip(ui: &egui::Ui, row: egui::Rect, item: &CompletionItem) {
    let preview = completion::completion_preview(item);
    if preview.is_empty() {
        return;
    }
    let mut tooltip = egui::Tooltip::always_open(
        ui.ctx().clone(),
        ui.layer_id(),
        egui::Id::new(COMPLETION_TOOLTIP_ID),
        egui::PopupAnchor::ParentRect(row),
    )
    .gap(6.0);
    tooltip.popup = tooltip.popup.align(egui::RectAlign::RIGHT_START);
    tooltip.show(|ui| {
        ui.set_max_width(420.0);
        for (index, line) in preview.iter().enumerate() {
            let rich = if index == 0 && item.insert != item.name {
                egui::RichText::new(line).monospace().small()
            } else {
                egui::RichText::new(line).weak()
            };
            ui.label(rich);
        }
    });
}
