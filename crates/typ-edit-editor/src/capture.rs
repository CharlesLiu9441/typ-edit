//! Local-edit capture.
//!
//! [`CaptureHook`] is a buffer [`EditHook`] that records every applied
//! [`Operation`] in **byte** terms (converting the buffer's char indices via
//! [`TextBuffer::char_to_byte`]) unless capture is suppressed. The app enables it
//! through [`crate::EditorState::enable_edit_capture`] and drains the edits once
//! per logic frame; remote application suppresses it so an applied change is
//! never re-captured and echoed back.

use std::cell::RefCell;
use std::rc::Rc;

use typ_edit_buffer::{EditHook, Operation, TextBuffer};

/// One captured local edit, in byte offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedEdit {
    /// Byte offset the edit starts at.
    pub at_byte: usize,
    /// Number of bytes removed at `at_byte` (0 for a pure insert).
    pub delete_bytes: usize,
    /// The operation's text: the inserted text for an insert, or the removed
    /// text for a delete (which also lets the apply path validate the span).
    pub insert: String,
}

/// The shared capture buffer and its suppression flag.
#[derive(Default)]
pub(crate) struct CaptureState {
    edits: Vec<CapturedEdit>,
    suppressed: bool,
}

impl CaptureState {
    /// Sets whether edits are currently recorded.
    pub(crate) fn set_suppressed(&mut self, suppressed: bool) {
        self.suppressed = suppressed;
    }
}

/// Handle to an editor's capture state, shared with its [`CaptureHook`].
#[derive(Clone)]
pub struct EditCapture(pub(crate) Rc<RefCell<CaptureState>>);

impl EditCapture {
    /// Drains and returns the edits captured since the last call.
    pub fn take(&self) -> Vec<CapturedEdit> {
        std::mem::take(&mut self.0.borrow_mut().edits)
    }
}

/// A buffer hook that records every operation unless capture is suppressed.
pub(crate) struct CaptureHook(pub(crate) Rc<RefCell<CaptureState>>);

impl EditHook for CaptureHook {
    fn on_edit(&mut self, op: &Operation, buffer: &TextBuffer) {
        let mut state = self.0.borrow_mut();
        if state.suppressed {
            return;
        }
        match op {
            Operation::Insert { at, text } => state.edits.push(CapturedEdit {
                at_byte: buffer.char_to_byte(*at),
                delete_bytes: 0,
                insert: text.clone(),
            }),
            Operation::Delete { at, text } => state.edits.push(CapturedEdit {
                at_byte: buffer.char_to_byte(*at),
                delete_bytes: text.len(),
                insert: text.clone(),
            }),
            Operation::UndoRedoFlag => {}
        }
    }
}
