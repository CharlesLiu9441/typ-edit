//! The public rope-backed buffer: queries, grouped history and edit hooks.
//!
//! # Undo/redo protocol
//!
//! Both stacks are `VecDeque<Operation>` whose **back** is the next operation to
//! execute. The undo stack stores *inverse* operations, the redo stack stores
//! *forward* operations. [`TextBuffer::record`] always clears the redo stack,
//! pushes an [`Operation::UndoRedoFlag`] when `new_op` is set, then pushes the
//! inverse; the flag therefore sits *below* its group and is popped last.
//!
//! `undo` applies inverse ops onto the redo stack until it pops a flag, which it
//! re-inserts directly **below the ops moved by this call** (`len - moved`, i.e.
//! `push_front` when the redo stack was empty); `redo` mirrors this exactly. The
//! boundary position matters: a plain `push_front` puts a second undone group's
//! flag behind the *first* undone group's forward ops, so one `redo` would walk
//! past its own group. Placing it below the just-moved ops keeps each group
//! contiguous. Flags are never passed to hooks.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::ops::Range;

use ropey::Rope;

use crate::operation::Operation;
use crate::treap::LineIndex;

/// Observer notified after every applied edit, including undo/redo and each
/// sub-edit of [`TextBuffer::replace_all`]. Flags never reach hooks.
pub trait EditHook {
    /// Called with the applied operation and the already-updated buffer.
    fn on_edit(&mut self, op: &Operation, buffer: &TextBuffer);
}

/// A rope-backed text buffer with an incremental line index, grouped undo/redo
/// and reactive edit hooks. All public positions and ranges are char indices.
pub struct TextBuffer {
    pub(crate) rope: Rope,
    pub(crate) index: LineIndex,
    pub(crate) undo: VecDeque<Operation>,
    pub(crate) redo: VecDeque<Operation>,
    pub(crate) hooks: Vec<Box<dyn EditHook>>,
    pub(crate) rev: u64,
}

impl TextBuffer {
    /// Creates an empty buffer.
    pub fn new() -> Self {
        TextBuffer {
            rope: Rope::new(),
            index: LineIndex::new(),
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            hooks: Vec::new(),
            rev: 0,
        }
    }

    /// Creates a buffer initialized with `text` (history starts empty).
    pub fn from_text(text: &str) -> Self {
        let mut index = LineIndex::new();
        for (i, ch) in text.chars().enumerate() {
            if ch == '\n' {
                index.insert(i);
            }
        }
        TextBuffer {
            rope: Rope::from_str(text),
            index,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            hooks: Vec::new(),
            rev: 0,
        }
    }

    /// Number of characters in the buffer.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Number of bytes in the buffer.
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Returns `true` if the buffer contains no characters.
    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    /// The char at `at`, or `None` if out of bounds.
    pub fn char_at(&self, at: usize) -> Option<char> {
        self.rope.get_char(at)
    }

    /// The text in char range `range` (clamped to the buffer).
    pub fn slice(&self, range: Range<usize>) -> String {
        let len = self.len_chars();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        self.rope.slice(start..end).to_string()
    }

    /// Streams the buffer's UTF-8 bytes to `writer` in rope chunks without
    /// materializing a whole-document `String`.
    pub fn write_to(&self, writer: &mut dyn Write) -> io::Result<()> {
        for chunk in self.rope.chunks() {
            writer.write_all(chunk.as_bytes())?;
        }
        Ok(())
    }

    /// Whether the buffer's UTF-8 bytes equal `other`, compared in rope chunks
    /// without materializing a whole-document `String`.
    pub fn text_equals(&self, other: &str) -> bool {
        if self.len_bytes() != other.len() {
            return false;
        }
        let mut offset = 0;
        for chunk in self.rope.chunks() {
            let end = offset + chunk.len();
            if other.as_bytes().get(offset..end) != Some(chunk.as_bytes()) {
                return false;
            }
            offset = end;
        }
        offset == other.len()
    }

    /// Number of lines: newline count + 1 (an empty buffer has one line).
    pub fn line_count(&self) -> usize {
        self.index.len() + 1
    }

    /// 0-based line index containing char `at` (clamped past the end).
    pub fn line_of_char(&self, at: usize) -> usize {
        self.index.count_lt(at)
    }

    /// Char index where `line` starts; out-of-range lines clamp to the end.
    pub fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        match self.index.kth(line - 1) {
            Some(pos) => (pos + 1).min(self.len_chars()),
            None => self.len_chars(),
        }
    }

    /// Byte offset of char index `at` (clamped to the buffer).
    pub fn char_to_byte(&self, at: usize) -> usize {
        self.rope.char_to_byte(at.min(self.len_chars()))
    }

    /// Char index containing byte offset `byte` (clamped to the buffer).
    pub fn byte_to_char(&self, byte: usize) -> usize {
        self.rope.byte_to_char(byte.min(self.len_bytes()))
    }

    /// Revision counter, bumped on every applied edit (including undo/redo).
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Undoes the top group, returning whether anything was applied.
    pub fn undo(&mut self) -> bool {
        let mut applied = false;
        let mut moved = 0usize;
        loop {
            match self.undo.pop_back() {
                None => break,
                Some(Operation::UndoRedoFlag) => {
                    let at = self.redo.len() - moved;
                    self.redo.insert(at, Operation::UndoRedoFlag);
                    break;
                }
                Some(op) => {
                    let forward = op.inverse();
                    self.apply_op(&op);
                    self.redo.push_back(forward);
                    moved += 1;
                    applied = true;
                }
            }
        }
        applied
    }

    /// Redoes the top group, returning whether anything was applied.
    pub fn redo(&mut self) -> bool {
        let mut applied = false;
        let mut moved = 0usize;
        loop {
            match self.redo.pop_back() {
                None => break,
                Some(Operation::UndoRedoFlag) => {
                    let at = self.undo.len() - moved;
                    self.undo.insert(at, Operation::UndoRedoFlag);
                    break;
                }
                Some(op) => {
                    let inverse = op.inverse();
                    self.apply_op(&op);
                    self.undo.push_back(inverse);
                    moved += 1;
                    applied = true;
                }
            }
        }
        applied
    }

    /// Returns `true` if there is an undoable group.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Returns `true` if there is a redoable group.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Discards all undo/redo history.
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Registers an edit hook. Hooks fire in registration order.
    pub fn add_hook(&mut self, hook: Box<dyn EditHook>) {
        self.hooks.push(hook);
    }

    /// Number of registered hooks.
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    /// Records an inverse into the undo stack, clearing redo. A flag is pushed
    /// first when `new_op`, so it ends up below its group's operations.
    pub(crate) fn record(&mut self, inverse: Operation, new_op: bool) {
        self.redo.clear();
        if new_op {
            self.undo.push_back(Operation::UndoRedoFlag);
        }
        self.undo.push_back(inverse);
    }

    /// Records several inverses (in application order) as one undo group.
    pub(crate) fn record_group(&mut self, inverses: Vec<Operation>, new_op: bool) {
        self.redo.clear();
        if new_op {
            self.undo.push_back(Operation::UndoRedoFlag);
        }
        for inverse in inverses {
            self.undo.push_back(inverse);
        }
    }

    /// Notifies hooks after state mutation. The hooks vector is temporarily
    /// moved out of `self` so a hook can be given `&self` while it is itself
    /// borrowed mutably; it is restored afterwards on **both** the normal and
    /// the panicking path. A panic inside a hook would otherwise drop the taken
    /// vector during unwinding and leave `self.hooks` permanently empty.
    pub(crate) fn notify_hooks(&mut self, op: &Operation) {
        debug_assert!(!op.is_flag(), "flags must never reach hooks");
        let mut hooks = std::mem::take(&mut self.hooks);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for hook in hooks.iter_mut() {
                hook.on_edit(op, &*self);
            }
        }));
        self.hooks = hooks;
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}
