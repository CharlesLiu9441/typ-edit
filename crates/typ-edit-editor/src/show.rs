//! `EditorState::show` entry point.
//!
//! The custom virtualized widget owns all rendering, scrolling and input; the
//! frame body lives in [`crate::virtual_text`].

use crate::{EditorInput, EditorOutput, EditorState};

impl EditorState {
    /// Draws the editor and reports the frame's [`EditorOutput`].
    pub fn show(&mut self, ui: &mut egui::Ui, input: EditorInput<'_>) -> EditorOutput {
        self.show_virtual_text(ui, input)
    }
}
