//! Differential ("对拍") fuzz of [`TextBuffer`] against a naive `String` model
//! with a snapshot-based history (each `new_op = true` edit is one snapshot,
//! matching one undo group).

mod common;

use common::XorShift;
use typ_edit_buffer::TextBuffer;

impl XorShift {
    fn ch(&mut self) -> char {
        match self.next() % 8 {
            0 => '\n',
            1 => 'a',
            2 => 'b',
            3 => 'c',
            4 => '中',
            5 => 'é',
            6 => '😀',
            _ => ' ',
        }
    }

    fn text(&mut self, max: usize) -> String {
        let len = self.below(max + 1);
        (0..len).map(|_| self.ch()).collect()
    }
}

#[derive(Default)]
struct Model {
    text: String,
    undo: Vec<String>,
    redo: Vec<String>,
}

impl Model {
    fn edit(&mut self, next: String) {
        if next != self.text {
            self.undo.push(self.text.clone());
            self.redo.clear();
            self.text = next;
        }
    }

    fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(prev) => {
                self.redo.push(std::mem::replace(&mut self.text, prev));
                true
            }
            None => false,
        }
    }

    fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(&mut self.text, next));
                true
            }
            None => false,
        }
    }

    fn insert_text(&self, at: usize, text: &str) -> String {
        self.text
            .chars()
            .take(at)
            .chain(text.chars())
            .chain(self.text.chars().skip(at))
            .collect()
    }

    fn delete_range(&self, start: usize, end: usize) -> String {
        self.text
            .chars()
            .enumerate()
            .filter(|(i, _)| *i < start || *i >= end)
            .map(|(_, c)| c)
            .collect()
    }
}

fn text_of(buffer: &TextBuffer) -> String {
    buffer.slice(0..buffer.len_chars())
}

fn naive_line_of(text: &str, at: usize) -> usize {
    text.chars().take(at).filter(|&c| c == '\n').count()
}

fn naive_line_start(text: &str, line: usize) -> usize {
    let len = text.chars().count();
    if line == 0 {
        return 0;
    }
    let mut seen = 0usize;
    for (i, ch) in text.chars().enumerate() {
        if ch == '\n' {
            seen += 1;
            if seen == line {
                return i + 1;
            }
        }
    }
    len
}

fn naive_slice(text: &str, a: usize, b: usize) -> String {
    text.chars().skip(a).take(b - a).collect()
}

fn check(buffer: &TextBuffer, model: &Model, rng: &mut XorShift, step: usize) {
    let chars: Vec<char> = model.text.chars().collect();
    let len = chars.len();
    assert_eq!(buffer.len_chars(), len, "len_chars at step {step}");
    assert_eq!(
        buffer.len_bytes(),
        model.text.len(),
        "len_bytes at step {step}"
    );
    assert_eq!(
        buffer.is_empty(),
        model.text.is_empty(),
        "is_empty at step {step}"
    );
    assert_eq!(
        buffer.line_count(),
        model.text.matches('\n').count() + 1,
        "line_count at step {step}"
    );
    assert_eq!(text_of(buffer), model.text, "text at step {step}");

    for _ in 0..4 {
        let at = rng.below(len + 1);
        assert_eq!(buffer.char_at(at), chars.get(at).copied());
        assert_eq!(
            buffer.line_of_char(at),
            naive_line_of(&model.text, at),
            "line_of_char({at}) at step {step}"
        );
        let line = rng.below(buffer.line_count() + 2);
        assert_eq!(
            buffer.line_start(line),
            naive_line_start(&model.text, line),
            "line_start({line}) at step {step}"
        );
    }

    for _ in 0..2 {
        let a = rng.below(len + 1);
        let b = a + rng.below(len - a + 1);
        assert_eq!(
            buffer.slice(a..b),
            naive_slice(&model.text, a, b),
            "slice({a}..{b}) at step {step}"
        );
    }

    let byte = buffer.char_to_byte(len);
    assert_eq!(byte, model.text.len());
    assert_eq!(buffer.byte_to_char(byte), len);
}

#[test]
fn differential_fuzz_against_naive_model() {
    let mut rng = XorShift(0xC0FF_EE00_1234_ABCD);
    let mut buffer = TextBuffer::new();
    let mut model = Model::default();

    for step in 0..10_000 {
        let len = model.text.chars().count();
        let roll = rng.below(100);
        if roll < 40 && len < 300 {
            let at = rng.below(len + 1);
            let text = rng.text(5);
            let next = model.insert_text(at, &text);
            let range = buffer.insert(at, &text, true);
            assert_eq!(range.start, at.min(len));
            model.edit(next);
        } else if roll < 70 && len > 0 {
            let start = rng.below(len);
            let end = start + 1 + rng.below(len - start);
            let removed: String = model.text.chars().skip(start).take(end - start).collect();
            let got = buffer.delete(start..end, true);
            assert_eq!(got, removed);
            let next = model.delete_range(start, end);
            model.edit(next);
        } else if roll < 82 {
            assert_eq!(buffer.undo(), model.undo(), "undo result at step {step}");
        } else if roll < 94 {
            assert_eq!(buffer.redo(), model.redo(), "redo result at step {step}");
        } else {
            let next = rng.text(40);
            buffer.replace_all(&next, true);
            model.edit(next);
        }
        check(&buffer, &model, &mut rng, step);
    }

    assert!(!model.text.is_empty());
    assert!(!model.undo.is_empty());
}
