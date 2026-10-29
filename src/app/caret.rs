//! Caret position helpers: byte offset to 1-based line/column.

/// Byte offsets of every logical line start (`0` plus every byte after a
/// newline). Building this once per file avoids scanning from byte 0 for every
/// diagnostic row.
pub(super) fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(text.bytes().filter(|byte| *byte == b'\n').count() + 1);
    starts.push(0);
    for (at, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(at + ch.len_utf8());
        }
    }
    starts
}

/// Line/column using an existing line-start index.
pub(super) fn line_column_from_starts(text: &str, byte: usize, starts: &[usize]) -> (usize, usize) {
    let mut byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    let line = starts
        .partition_point(|start| *start <= byte)
        .saturating_sub(1);
    let line_start = starts.get(line).copied().unwrap_or(0);
    (line + 1, text[line_start..byte].chars().count() + 1)
}
