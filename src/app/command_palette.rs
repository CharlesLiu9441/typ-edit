//! The Ctrl+Shift+P command palette: a foreground overlay listing every
//! registry command, fuzzy-filtered by the typed query.

mod rank;

use super::dispatch::CommandState;
use super::state::UiState;
use crate::commands::{COMMANDS, CommandId};
use crate::keybindings::KeyBindings;
use rank::{clamp_selection, move_selection, rank_commands};
use typ_edit_i18n::t;
use typ_edit_widgets::text_input::inline_input;
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, TuiBuilderLogic,
    taffy::{Dimension, Size, Style},
};

/// Stable id of the palette area.
const PALETTE_ID: &str = "typ_edit_command_palette";
/// Fixed overlay width.
const PALETTE_WIDTH: f32 = 560.0;
/// Maximum height of the results list.
const PALETTE_MAX_HEIGHT: f32 = 360.0;

/// Deterministic height of the results list for `rows` candidates at
/// `row_height` each, capped at [`PALETTE_MAX_HEIGHT`]. Pure and derived only
/// from the current frame's state, so the popup shrinks *and* grows with the
/// current result count instead of ratcheting to the smallest remembered size.
fn list_height(rows: usize, row_height: f32) -> f32 {
    (rows as f32 * row_height).min(PALETTE_MAX_HEIGHT)
}

/// Results-list leaf: exact height for this frame, full width, never grows.
///
/// The enclosing `Area` feeds its previous content size back as `max_rect`;
/// a fixed-height leaf keeps the popup height derived only from the current
/// candidate count (it shrinks *and* grows instead of ratcheting down).
fn list_style(height: f32) -> Style {
    Style {
        flex_grow: 0.0,
        flex_shrink: 0.0,
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::length(height),
        },
        ..Default::default()
    }
}

impl UiState {
    /// Toggles the palette: opening clears the query, selects the first row and
    /// requests input focus; invoking it again closes the palette.
    pub(super) fn toggle_command_palette(&mut self) {
        if self.palette_open {
            self.palette_open = false;
            return;
        }
        self.palette_open = true;
        self.palette_query.clear();
        self.palette_selected = 0;
        self.palette_focus = true;
    }

    /// Paints the palette overlay and applies its keyboard/mouse interaction.
    /// Returns the command chosen this frame, if any, for the caller to run.
    pub(super) fn command_palette(
        &mut self,
        ui: &mut egui::Ui,
        bindings: &KeyBindings,
        state: &CommandState<'_>,
    ) -> Option<CommandId> {
        if !self.palette_open {
            return None;
        }
        let ctx = ui.ctx().clone();

        let items: Vec<(CommandId, String)> = COMMANDS
            .iter()
            .map(|meta| (meta.id, t(meta.label).into_owned()))
            .collect();
        let results = rank_commands(&self.palette_query, &items);

        let labels: Vec<String> = results
            .iter()
            .map(|id| t(id.meta().label).into_owned())
            .collect();
        let categories: Vec<Option<String>> = results
            .iter()
            .map(|id| id.meta().category.map(|key| t(key).into_owned()))
            .collect();
        let bindings: Vec<Option<String>> = results.iter().map(|id| bindings.label(*id)).collect();
        let enabled: Vec<bool> = results
            .iter()
            .map(|id| id.meta().enabled.is_none_or(|predicate| predicate(state)))
            .collect();

        self.palette_selected = clamp_selection(&enabled, self.palette_selected);

        // Consume the palette's keys before the text field renders, so the
        // field never sees them and the caret never moves.
        let mut delta = 0isize;
        let mut close = false;
        let mut execute = false;
        let toggle_mods = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
        ctx.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                close = true;
            }
            // Wins while open: `handle_shortcuts` early-returns on
            // `palette_open`, so this consume is the only handler for the key.
            if input.consume_key(toggle_mods, egui::Key::P) {
                close = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                delta = 1;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                delta = -1;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                execute = true;
            }
        });
        if delta != 0 {
            self.palette_selected = move_selection(&enabled, self.palette_selected, delta);
        }

        let selected = self.palette_selected;
        let focus_now = std::mem::take(&mut self.palette_focus);
        let mut hover: Option<usize> = None;
        let mut clicked: Option<usize> = None;

        egui::Area::new(egui::Id::new(PALETTE_ID))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 80.0))
            .constrain_to(ctx.content_rect())
            .show(&ctx, |ui| {
                let screen = ui.ctx().content_rect();
                typ_edit_widgets::menu::block_background(
                    &ctx,
                    egui::Id::new("typ_edit_palette_backdrop"),
                    screen,
                    ui.layer_id(),
                );
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(PALETTE_WIDTH);
                    ui.set_max_width(PALETTE_WIDTH);
                    let base = ui.id().with(("typ_edit_command_palette", "content"));
                    let width = ui.available_width();
                    let mut field: Option<egui::Response> = None;
                    let mut root_style = tui::column_gap(0.0);
                    tui::fill_width(&mut root_style);
                    tui::tui(ui, base)
                        .reserve_width(width)
                        .style(root_style)
                        .show(|tree| {
                            field = Some(tree.id(base.with("query")).ui(|ui| {
                                inline_input(
                                    ui,
                                    &mut self.palette_query,
                                    &t("palette.placeholder"),
                                    ui.available_width(),
                                    Some(base.with("query_input")),
                                )
                            }));
                            if focus_now && let Some(field) = &field {
                                field.request_focus();
                            }
                            let field_gap = tree.egui_ui().spacing().item_spacing.y + 4.0;
                            tree.id(base.with("gap"))
                                .style(tui::fixed(field_gap))
                                .add_empty();
                            if results.is_empty() {
                                let muted = typ_edit_widgets::theme::Theme::from_ui(tree.egui_ui())
                                    .colors
                                    .text_muted;
                                tree.id(base.with("no_results")).label(
                                    egui::RichText::new(t("palette.no_results")).color(muted),
                                );
                                return;
                            }
                            let row_height =
                                (tree.egui_ui().text_style_height(&egui::TextStyle::Body) + 8.0)
                                    .max(22.0);
                            let list_height = list_height(results.len(), row_height);
                            // Size the results leaf explicitly for this frame. The
                            // enclosing `Area` feeds its previous content size back as
                            // our `max_rect`, so leaving it to `ScrollArea`'s auto-sizing
                            // ratchets the popup to the smallest height ever shown: it
                            // would shrink but never grow back. An exact-size leaf makes
                            // the height depend only on the current candidate count.
                            tree.id(base.with("list"))
                                .style(list_style(list_height))
                                .ui(|ui| {
                                    egui::ScrollArea::vertical()
                                        .max_height(list_height)
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            let width = ui.available_width();
                                            let theme = Theme::from_ui(ui);
                                            let body = ui
                                                .style()
                                                .text_styles
                                                .get(&egui::TextStyle::Body)
                                                .cloned()
                                                .unwrap_or_else(|| {
                                                    egui::FontId::proportional(13.0)
                                                });

                                            for index in 0..results.len() {
                                                let is_selected = index == selected;
                                                let is_enabled = enabled[index];
                                                let label_color = if !is_enabled {
                                                    theme.colors.text_weak
                                                } else if is_selected {
                                                    theme.colors.text_strong
                                                } else {
                                                    theme.colors.text
                                                };
                                                let category_color = if !is_enabled {
                                                    theme.colors.text_weak
                                                } else if is_selected {
                                                    theme.colors.text
                                                } else {
                                                    theme.colors.text_muted
                                                };
                                                let binding_color = if !is_enabled {
                                                    theme.colors.text_weak
                                                } else if is_selected {
                                                    theme.colors.text
                                                } else {
                                                    theme.colors.text_muted
                                                };

                                                let (rect, row) = ui.allocate_exact_size(
                                                    egui::vec2(width, row_height),
                                                    egui::Sense::click(),
                                                );
                                                let hovered = row.hovered();
                                                let pressed = row.clicked();
                                                let radius =
                                                    egui::CornerRadius::same(theme.radii.tiny);
                                                if is_selected {
                                                    ui.painter().rect_stroke(
                                                        rect,
                                                        radius,
                                                        egui::Stroke::new(
                                                            theme.strokes.accent,
                                                            theme.colors.accent,
                                                        ),
                                                        egui::StrokeKind::Inside,
                                                    );
                                                } else if hovered && is_enabled {
                                                    ui.painter().rect_filled(
                                                        rect,
                                                        radius,
                                                        theme
                                                            .colors
                                                            .widget_hover_fill
                                                            .gamma_multiply(0.5),
                                                    );
                                                }

                                                let binding_width =
                                                    bindings[index].as_ref().map_or(0.0, |text| {
                                                        ui.fonts_mut(|fonts| {
                                                            fonts.layout_no_wrap(
                                                                text.clone(),
                                                                body.clone(),
                                                                binding_color,
                                                            )
                                                        })
                                                        .size()
                                                        .x
                                                    });
                                                let label_max =
                                                    (width - 16.0 - binding_width - 8.0).max(8.0);
                                                let mut job = egui::text::LayoutJob::default();
                                                job.wrap.max_width = label_max;
                                                job.wrap.max_rows = 1;
                                                job.wrap.overflow_character = Some('…');
                                                job.append(
                                                    &labels[index],
                                                    0.0,
                                                    egui::TextFormat {
                                                        font_id: body.clone(),
                                                        color: label_color,
                                                        ..Default::default()
                                                    },
                                                );
                                                if let Some(category) = &categories[index] {
                                                    job.append(
                                                        "   ",
                                                        0.0,
                                                        egui::TextFormat {
                                                            font_id: body.clone(),
                                                            color: category_color,
                                                            ..Default::default()
                                                        },
                                                    );
                                                    job.append(
                                                        category,
                                                        0.0,
                                                        egui::TextFormat {
                                                            font_id: egui::FontId::new(
                                                                body.size * 0.85,
                                                                body.family.clone(),
                                                            ),
                                                            color: category_color,
                                                            ..Default::default()
                                                        },
                                                    );
                                                }
                                                let galley =
                                                    ui.fonts_mut(|fonts| fonts.layout_job(job));
                                                ui.painter().galley(
                                                    egui::pos2(
                                                        rect.left() + 8.0,
                                                        rect.center().y - galley.size().y * 0.5,
                                                    ),
                                                    galley,
                                                    label_color,
                                                );
                                                if let Some(binding) = &bindings[index] {
                                                    ui.painter().text(
                                                        egui::pos2(
                                                            rect.right() - 8.0,
                                                            rect.center().y,
                                                        ),
                                                        egui::Align2::RIGHT_CENTER,
                                                        binding,
                                                        egui::FontId::new(
                                                            body.size * 0.85,
                                                            body.family.clone(),
                                                        ),
                                                        binding_color,
                                                    );
                                                }

                                                if hovered {
                                                    if is_enabled {
                                                        hover = Some(index);
                                                    } else {
                                                        let _ = row.on_hover_text(
                                                            t("palette.disabled").into_owned(),
                                                        );
                                                    }
                                                }
                                                if pressed && is_enabled {
                                                    clicked = Some(index);
                                                }
                                                if is_selected
                                                    && !ui.clip_rect().contains_rect(rect)
                                                {
                                                    ui.scroll_to_rect(rect, None);
                                                }
                                            }
                                        });
                                });
                        });
                });
            });

        if let Some(index) = hover {
            self.palette_selected = index;
        }
        if close {
            self.palette_open = false;
        }
        let chosen = clicked.or(if execute {
            Some(self.palette_selected)
        } else {
            None
        });
        if let Some(index) = chosen
            && enabled.get(index).copied().unwrap_or(false)
            && let Some(&id) = results.get(index)
        {
            self.palette_open = false;
            return Some(id);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: f32 = 22.0;

    #[test]
    fn list_height_grows_and_shrinks_with_the_current_row_count() {
        let few = list_height(2, ROW);
        let many = list_height(8, ROW);
        let back_to_few = list_height(2, ROW);
        assert_eq!(
            back_to_few, few,
            "shrinking to 2 rows must restore the small height"
        );
        assert_eq!(many, 8.0 * ROW);
        assert!(many > few, "more candidates must be taller");
        assert!(back_to_few < many, "fewer candidates must be shorter");
    }

    #[test]
    fn list_height_is_capped_at_the_maximum() {
        assert_eq!(list_height(0, ROW), 0.0);
        assert_eq!(list_height(1, ROW), ROW);
        assert_eq!(list_height(1_000, ROW), PALETTE_MAX_HEIGHT);
    }
}
