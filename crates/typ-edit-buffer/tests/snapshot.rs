//! `TextSnapshot`: O(1) capture, revision tracking, explicit materialization.

use typ_edit_buffer::{TextBuffer, TextSnapshot};

#[test]
fn snapshot_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TextSnapshot>();
}

#[test]
fn snapshot_materializes_exactly_and_tracks_revision() {
    let mut buffer = TextBuffer::from_text("初始文本😀");
    let snap = buffer.snapshot();
    assert_eq!(snap.rev(), buffer.rev());
    assert_eq!(snap.materialize(), buffer.slice(0..buffer.len_chars()));

    buffer.insert(0, "前缀", true);
    let snap2 = buffer.snapshot();
    assert_eq!(snap2.rev(), buffer.rev());
    assert_ne!(snap2.rev(), snap.rev());
    assert_eq!(snap2.materialize(), buffer.slice(0..buffer.len_chars()));
    assert_eq!(
        snap.materialize(),
        "初始文本😀",
        "old snapshot is immutable"
    );
}

#[test]
fn clone_captures_the_same_value() {
    let mut buffer = TextBuffer::from_text("abc");
    let snap = buffer.snapshot();
    buffer.insert(3, "d", true);

    let cloned = snap.clone();
    assert_eq!(cloned.rev(), snap.rev());
    assert_eq!(cloned.materialize(), "abc");
    assert_eq!(snap.materialize(), "abc");
    assert_eq!(buffer.slice(0..buffer.len_chars()), "abcd");
}

#[test]
fn debug_shows_rev_only_never_content() {
    let buffer = TextBuffer::from_text("secret-cjk-秘密");
    let snap = buffer.snapshot();
    let rendered = format!("{snap:?}");
    assert!(rendered.contains("rev"), "debug shows rev: {rendered}");
    assert!(!rendered.contains("secret"), "debug must not leak text");
    assert!(!rendered.contains("秘密"), "debug must not leak text");
}
