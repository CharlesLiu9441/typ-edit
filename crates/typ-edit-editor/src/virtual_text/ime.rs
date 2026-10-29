//! IME (CJK) preedit/commit handling for the virtualized text path (Stage C).
//!
//! Mirrors egui's `Event::Ime` semantics: a `Preedit` deletes the
//! previously tracked preedit range and inserts the new text as a real buffer
//! edit; a `Commit` deletes the preedit range and inserts the final text;
//! empty `Preedit`/`Commit` clears the tracked range. The same undo-group
//! semantics as typing apply, so a whole composition is one undo step.

use super::keys::InputOutcome;
use crate::EditorState;

/// Publishes the IME geometry egui forwards to the platform IME (screen
/// coordinates) while the widget is focused. winit positions the candidate
/// window from `IMEOutput::rect`, so that rect must be the caret, not the whole
/// editor viewport.
pub(super) fn emit_ime_output(ui: &egui::Ui, cursor: egui::Rect) {
    let to_global = ui
        .ctx()
        .layer_transform_to_global(ui.layer_id())
        .unwrap_or_default();
    let cursor = to_global * cursor;
    ui.output_mut(|output| {
        output.ime = Some(egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Normal,
            rect: cursor,
            cursor_rect: cursor,
            should_interrupt_composition: false,
        });
    });
}

impl EditorState {
    /// Applies one IME event to the buffer, recording whether it changed text.
    pub(super) fn virtual_ime(&mut self, event: egui::ImeEvent, outcome: &mut InputOutcome) {
        match event {
            egui::ImeEvent::Preedit { text, .. } => {
                let had_preedit = self.virtual_text.preedit_range.is_some();
                if text.is_empty() && !had_preedit {
                    return;
                }
                if text == "\n" || text == "\r" {
                    return;
                }
                let start = self.clear_preedit();
                if text.is_empty() {
                    self.virtual_text.preedit_range = None;
                } else {
                    let end = self.replace_selection_raw(start, &text);
                    self.virtual_text.preedit_range = Some(start..end);
                }
                outcome.changed = true;
                outcome.caret_moved = true;
            }
            egui::ImeEvent::Commit(text) => {
                let had_preedit = self.virtual_text.preedit_range.is_some();
                if text.is_empty() && !had_preedit {
                    return;
                }
                if text == "\n" || text == "\r" {
                    return;
                }
                // Remove the primary's composing text (this opens the undo
                // group) and then commit through the multi-selection insert
                // path, so IME commit reaches every caret exactly like typing.
                // The preedit above stays primary-only: one composition string
                // cannot be rendered at N carets.
                self.clear_preedit();
                self.virtual_text.preedit_range = None;
                if !text.is_empty() {
                    self.virtual_insert(&text);
                }
                outcome.changed = true;
                outcome.caret_moved = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_rect_is_the_caret_rect() {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 400.0),
            )),
            ..Default::default()
        };
        let cursor = egui::Rect::from_min_size(egui::pos2(120.0, 80.0), egui::vec2(0.0, 18.0));
        let mut output = ctx.run_ui(raw, |ui| emit_ime_output(ui, cursor));
        output.textures_delta.clear();
        let ime = output.platform_output.ime.expect("IME output");
        assert_eq!(ime.rect, cursor);
        assert_eq!(ime.cursor_rect, cursor);
    }
}
