mod common;

use common::XorShift;
use std::cell::RefCell;
use std::rc::Rc;

use typ_edit_buffer::{EditHook, Operation, TextBuffer};

#[derive(Clone)]
struct Recorder(Rc<RefCell<Vec<Operation>>>);

impl EditHook for Recorder {
    fn on_edit(&mut self, op: &Operation, _buffer: &TextBuffer) {
        self.0.borrow_mut().push(op.clone());
    }
}

fn recorder() -> (Recorder, Rc<RefCell<Vec<Operation>>>) {
    let log = Rc::new(RefCell::new(Vec::new()));
    (Recorder(log.clone()), log)
}

struct Counter(Rc<RefCell<usize>>);

impl EditHook for Counter {
    fn on_edit(&mut self, _op: &Operation, _buffer: &TextBuffer) {
        *self.0.borrow_mut() += 1;
    }
}

struct PanicHook;

impl EditHook for PanicHook {
    fn on_edit(&mut self, _op: &Operation, _buffer: &TextBuffer) {
        panic!("hook panic");
    }
}

fn text_of(buffer: &TextBuffer) -> String {
    buffer.slice(0..buffer.len_chars())
}

fn ins(at: usize, text: &str) -> Operation {
    Operation::Insert {
        at,
        text: text.into(),
    }
}

fn del(at: usize, text: &str) -> Operation {
    Operation::Delete {
        at,
        text: text.into(),
    }
}

fn naive_line_start(text: &str, line: usize) -> usize {
    let len = text.chars().count();
    if line == 0 {
        return 0;
    }
    let mut newline_count = 0usize;
    for (i, ch) in text.chars().enumerate() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count == line {
                return i + 1;
            }
        }
    }
    len
}

fn assert_matches_naive(buffer: &TextBuffer, text: &str) {
    let len = buffer.len_chars();
    assert_eq!(len, text.chars().count());
    assert_eq!(buffer.len_bytes(), text.len());
    assert_eq!(buffer.is_empty(), text.is_empty());
    assert_eq!(buffer.line_count(), text.matches('\n').count() + 1);
    for probe in 0..=len {
        let expected = text.chars().take(probe).filter(|&c| c == '\n').count();
        assert_eq!(
            buffer.line_of_char(probe),
            expected,
            "line_of_char({probe})"
        );
        assert_eq!(buffer.line_start(probe), naive_line_start(text, probe));
        assert_eq!(buffer.char_at(probe), text.chars().nth(probe));
    }
    assert_eq!(buffer.line_start(buffer.line_count() + 5), len);
}

#[test]
fn grouped_edit_undoes_and_redoes() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.insert(5, " world", true);
    assert_eq!(text_of(&buffer), "hello world");
    assert!(buffer.undo());
    assert_eq!(text_of(&buffer), "hello");
    assert!(!buffer.undo());
    assert!(buffer.redo());
    assert_eq!(text_of(&buffer), "hello world");
    assert!(!buffer.redo());
}

#[test]
fn two_groups_undo_and_redo_in_order() {
    let mut buffer = TextBuffer::from_text("a");
    buffer.insert(1, "b", true);
    buffer.insert(2, "c", true);
    assert_eq!(text_of(&buffer), "abc");

    assert!(buffer.undo());
    assert_eq!(text_of(&buffer), "ab");
    assert!(buffer.undo());
    assert_eq!(text_of(&buffer), "a");
    assert!(!buffer.undo());

    assert!(buffer.redo());
    assert_eq!(text_of(&buffer), "ab");
    assert!(buffer.redo());
    assert_eq!(text_of(&buffer), "abc");
    assert!(!buffer.redo());
}

#[test]
fn new_op_false_merges_into_previous_group() {
    let mut buffer = TextBuffer::new();
    buffer.insert(0, "a", true);
    buffer.insert(1, "b", false);
    assert_eq!(text_of(&buffer), "ab");
    assert!(buffer.undo());
    assert_eq!(text_of(&buffer), "");
    assert!(!buffer.undo());
    assert!(buffer.redo());
    assert_eq!(text_of(&buffer), "ab");
}

#[test]
fn new_edit_clears_redo_stack() {
    let mut buffer = TextBuffer::from_text("a");
    buffer.insert(1, "b", true);
    assert!(buffer.undo());
    assert!(buffer.can_redo());
    buffer.insert(0, "x", true);
    assert!(!buffer.can_redo());
    assert!(!buffer.redo());
    assert_eq!(text_of(&buffer), "xa");
}

#[test]
fn delete_undo_restores_exact_content() {
    let text = "line1\nline2\nline3";
    let mut buffer = TextBuffer::from_text(text);
    let removed = buffer.delete(2..8, true);
    assert_eq!(removed, "ne1\nli");
    assert_eq!(text_of(&buffer), "line2\nline3");
    assert!(buffer.undo());
    assert_eq!(text_of(&buffer), text);
    assert_matches_naive(&buffer, text);
}

#[test]
fn interleaved_undo_redo_cycles_stay_consistent() {
    let mut buffer = TextBuffer::from_text("0123456789");
    buffer.insert(5, "X", true);
    buffer.delete(0..3, true);
    assert_eq!(text_of(&buffer), "34X56789");

    for _ in 0..3 {
        assert!(buffer.undo());
        assert_eq!(text_of(&buffer), "01234X56789");
        assert_matches_naive(&buffer, "01234X56789");
        assert!(buffer.undo());
        assert_eq!(text_of(&buffer), "0123456789");
        assert_matches_naive(&buffer, "0123456789");
        assert!(!buffer.undo());
        assert!(buffer.redo());
        assert_eq!(text_of(&buffer), "01234X56789");
        assert!(buffer.redo());
        assert_eq!(text_of(&buffer), "34X56789");
        assert_matches_naive(&buffer, "34X56789");
        assert!(!buffer.redo());
    }
}

#[test]
fn hooks_see_every_applied_op_and_no_flags() {
    let (recorder, log) = recorder();
    let mut buffer = TextBuffer::from_text("ab");
    buffer.add_hook(Box::new(recorder));

    buffer.insert(2, "c", true);
    buffer.insert(3, "d", false);
    buffer.delete(1..2, true);
    buffer.undo();
    buffer.undo();
    buffer.redo();
    buffer.redo();

    let expected = vec![
        ins(2, "c"),
        ins(3, "d"),
        del(1, "b"),
        ins(1, "b"),
        del(3, "d"),
        del(2, "c"),
        ins(2, "c"),
        ins(3, "d"),
        del(1, "b"),
    ];
    let seen = log.borrow().clone();
    assert!(seen.iter().all(|op| !op.is_flag()));
    assert_eq!(seen, expected);
    assert_eq!(text_of(&buffer), "acd");
}

#[test]
fn replace_all_diffs_undoes_and_redoes() {
    let (recorder, log) = recorder();
    let mut buffer = TextBuffer::from_text("hello world");
    buffer.add_hook(Box::new(recorder));
    buffer.replace_all("hello there", true);
    assert_eq!(text_of(&buffer), "hello there");
    assert!(!log.borrow().is_empty());
    assert!(log.borrow().iter().all(|op| !op.is_flag()));

    assert!(buffer.undo());
    assert_eq!(text_of(&buffer), "hello world");
    assert_matches_naive(&buffer, "hello world");
    assert!(buffer.redo());
    assert_eq!(text_of(&buffer), "hello there");
    assert_matches_naive(&buffer, "hello there");
}

#[test]
fn replace_all_multiline_updates_index() {
    let mut buffer = TextBuffer::from_text("line1\nline2\nline3");
    buffer.replace_all("line1\nLINE2\nline3\nline4", true);
    assert_eq!(text_of(&buffer), "line1\nLINE2\nline3\nline4");
    assert_eq!(buffer.line_count(), 4);
    assert_eq!(buffer.line_start(1), 6);
    assert_eq!(buffer.line_start(3), 18);
    assert!(buffer.undo());
    assert_eq!(text_of(&buffer), "line1\nline2\nline3");
    assert_eq!(buffer.line_count(), 3);
    assert_matches_naive(&buffer, "line1\nline2\nline3");
}

#[test]
fn replace_all_identical_is_noop() {
    let (recorder, log) = recorder();
    let mut buffer = TextBuffer::from_text("same");
    buffer.add_hook(Box::new(recorder));
    buffer.replace_all("same", true);
    assert!(log.borrow().is_empty());
    assert!(!buffer.can_undo());
    assert_eq!(buffer.rev(), 0);
}

#[test]
fn hooks_capture_positions_and_utf8() {
    let (recorder, log) = recorder();
    let mut buffer = TextBuffer::from_text("aé中");
    buffer.add_hook(Box::new(recorder));

    buffer.insert(0, "X", true);
    buffer.insert(buffer.len_chars(), "Y", true);
    buffer.delete(1..2, true);

    assert_eq!(text_of(&buffer), "Xé中Y");
    let seen = log.borrow().clone();
    assert_eq!(seen[0], ins(0, "X"));
    assert_eq!(seen[1], ins(4, "Y"));
    assert_eq!(seen[2], del(1, "a"));
}

#[test]
fn byte_and_char_conversion_around_multibyte() {
    let mut buffer = TextBuffer::from_text("a中b");
    assert_eq!(buffer.len_chars(), 3);
    assert_eq!(buffer.len_bytes(), 5);
    assert_eq!(buffer.char_to_byte(1), 1);
    assert_eq!(buffer.char_to_byte(2), 4);
    assert_eq!(buffer.char_to_byte(3), 5);
    assert_eq!(buffer.byte_to_char(4), 2);
    assert_eq!(buffer.byte_to_char(3), 1);
    assert_eq!(buffer.char_at(1), Some('中'));

    buffer.insert(1, "😀", true);
    assert_eq!(text_of(&buffer), "a😀中b");
    assert_eq!(buffer.char_to_byte(2), 5);
    assert_eq!(buffer.byte_to_char(5), 2);
}

#[test]
fn cjk_emoji_roundtrips_lines_and_snapshot() {
    let text = "你好世界\nemoji:😀🎉\n第三行abc";
    let mut buffer = TextBuffer::from_text(text);
    assert_eq!(buffer.len_chars(), 20);
    assert_eq!(buffer.len_bytes(), 40);

    for at in 0..=buffer.len_chars() {
        let byte = buffer.char_to_byte(at);
        assert_eq!(buffer.byte_to_char(byte), at, "roundtrip at char {at}");
    }

    assert_eq!(buffer.slice(0..4), "你好世界");
    assert_eq!(buffer.slice(5..10), "emoji");
    assert_eq!(buffer.slice(11..13), "😀🎉");
    assert_eq!(buffer.slice(14..17), "第三行");

    assert_eq!(buffer.line_count(), 3);
    assert_eq!(buffer.line_of_char(0), 0);
    assert_eq!(buffer.line_of_char(4), 0);
    assert_eq!(buffer.line_of_char(5), 1);
    assert_eq!(buffer.line_of_char(14), 2);
    assert_eq!(buffer.line_start(1), 5);
    assert_eq!(buffer.line_start(2), 14);
    assert_eq!(buffer.char_to_byte(4), 12);
    assert_eq!(buffer.byte_to_char(12), 4);

    let before = buffer.snapshot();
    assert_eq!(before.rev(), buffer.rev());
    assert_eq!(before.materialize(), buffer.slice(0..buffer.len_chars()));
    buffer.insert(0, "开头😀", true);
    assert_eq!(buffer.rev(), before.rev() + 1);
    let after = buffer.snapshot();
    assert_eq!(after.rev(), buffer.rev());
    assert_eq!(after.materialize(), buffer.slice(0..buffer.len_chars()));
    assert_eq!(after.materialize(), format!("开头😀{text}"));
    assert_eq!(
        before.materialize(),
        text,
        "an old snapshot stays immutable"
    );
    assert_matches_naive(&buffer, &format!("开头😀{text}"));
}

#[test]
fn revision_bumps_on_edits_and_history() {
    let mut buffer = TextBuffer::from_text("ab");
    assert_eq!(buffer.rev(), 0);
    buffer.insert(2, "c", true);
    assert_eq!(buffer.rev(), 1);
    assert!(buffer.undo());
    assert_eq!(buffer.rev(), 2);
    assert!(buffer.redo());
    assert_eq!(buffer.rev(), 3);
    buffer.replace_all("xyz", true);
    assert!(buffer.rev() > 3);
    buffer.clear_history();
    assert!(!buffer.can_undo());
    assert!(!buffer.can_redo());
}

#[test]
fn hooks_are_restored_after_a_panicking_hook() {
    let count = Rc::new(RefCell::new(0usize));
    let mut buffer = TextBuffer::from_text("a");
    buffer.add_hook(Box::new(Counter(count.clone())));
    buffer.add_hook(Box::new(PanicHook));
    assert_eq!(buffer.hook_count(), 2);

    let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        buffer.insert(1, "b", true);
    }));
    assert!(first.is_err());
    assert_eq!(buffer.hook_count(), 2);
    assert_eq!(*count.borrow(), 1);

    let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        buffer.insert(1, "c", true);
    }));
    assert!(second.is_err());
    assert_eq!(buffer.hook_count(), 2);
    assert_eq!(*count.borrow(), 2);
    assert_eq!(text_of(&buffer), "acb");
}

impl XorShift {
    fn ch(&mut self) -> char {
        match self.next() % 7 {
            0 => '\n',
            1 => 'a',
            2 => 'b',
            3 => '中',
            4 => 'é',
            5 => '😀',
            _ => 'z',
        }
    }

    fn text(&mut self, min: usize, max: usize) -> String {
        let len = min + self.below(max - min + 1);
        (0..len).map(|_| self.ch()).collect()
    }
}

#[test]
fn inverse_roundtrip_property() {
    let mut rng = XorShift(0x1234_5678_9abc_def0);
    for _ in 0..3_000 {
        let base = rng.text(0, 20);
        let mut buffer = TextBuffer::from_text(&base);
        let (recorder, log) = recorder();
        buffer.add_hook(Box::new(recorder));

        let len = buffer.len_chars();
        let op = if len == 0 || rng.below(2) == 0 {
            let at = rng.below(len + 1);
            let text = rng.text(1, 5);
            buffer.insert(at, &text, true);
            log.borrow().last().cloned().unwrap()
        } else {
            let start = rng.below(len);
            let take = 1 + rng.below(len - start);
            buffer.delete(start..start + take, true);
            log.borrow().last().cloned().unwrap()
        };

        match op.inverse() {
            Operation::Insert { at, text, .. } => {
                buffer.insert(at, &text, false);
            }
            Operation::Delete { at, text, .. } => {
                let n = text.chars().count();
                buffer.delete(at..at + n, false);
            }
            Operation::UndoRedoFlag => unreachable!(),
        }
        assert_eq!(text_of(&buffer), base);
        assert_matches_naive(&buffer, &base);
    }
}

#[test]
fn write_to_streams_every_rope_chunk_and_text_equals_is_len_aware() {
    let buffer = TextBuffer::from_text("第一行\nsecond\n");
    let mut bytes = Vec::new();
    buffer.write_to(&mut bytes).expect("streamed write");
    assert_eq!(bytes, "第一行\nsecond\n".as_bytes());
    assert!(buffer.text_equals("第一行\nsecond\n"));
    assert!(!buffer.text_equals("第一行\nsecond"));
    assert!(!buffer.text_equals("第一行\nSECOND\n"));
}
