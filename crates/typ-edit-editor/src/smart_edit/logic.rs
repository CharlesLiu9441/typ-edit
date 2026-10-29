//! Pure, egui-free smart-editing logic: auto-indent/dedent, bracket/pair
//! detection and the multi-line indent/unindent text transforms. The per-frame
//! handler that applies them to the editor state lives in the parent module.

use std::ops::Range;

use typ_edit_buffer::TextBuffer;

use crate::completion::char_before_caret;
use crate::{INDENT, UNMATCHED_SCAN_LIMIT, diagnostics};

/// Text to insert after pressing Enter with the caret at `cursor_byte`, plus
/// the caret position measured in chars from the start of that text.
///
/// `text` is the buffer *after* the `\n` has been inserted and `cursor_byte`
/// points immediately after it. An empty bracket/math pair `(|)` expands to
/// three lines; a list item at the end of its line continues with its marker
/// (or ends the list when it was empty); otherwise the previous line's
/// indentation is copied, one level deeper after an opening `{ [ ( $`, or
/// dedented in front of a matching closer. Returns `None` when the caret is not
/// right after a newline, the cursor is out of range, or there is nothing to
/// insert.
#[cfg(test)]
pub(crate) fn auto_indent(
    text: &str,
    cursor_byte: usize,
    list_lines: &[usize],
) -> Option<(String, usize)> {
    if cursor_byte == 0 || cursor_byte > text.len() {
        return None;
    }
    let prev_byte = cursor_byte - 1;
    if !text.is_char_boundary(cursor_byte) || !text.is_char_boundary(prev_byte) {
        return None;
    }
    if text.as_bytes()[prev_byte] != b'\n' {
        return None;
    }

    let line_start = text[..prev_byte].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..prev_byte];
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent = &line[..indent_len];
    let previous_is_list = list_lines.contains(&line_start);
    let after = text[cursor_byte..].chars().next();
    let before = line.chars().next_back();

    // 1. The caret sits between an opener and its closer (`(|)`, `$|$`):
    //    expand the pair to three lines with the caret on the middle one.
    if let Some(opener) = after.and_then(matching_opener)
        && before == Some(opener)
    {
        let insert = format!("{indent}{INDENT}\n{indent}");
        let caret_chars = indent.chars().count() + INDENT.chars().count();
        return Some((insert, caret_chars));
    }

    // 2. A list item at the end of its line continues with the same marker; an
    //    empty item ends the list instead.
    let after_caret = &text[cursor_byte..];
    let at_line_end = after_caret.find('\n').unwrap_or(after_caret.len()) == 0;
    if previous_is_list && at_line_end {
        let item = &line[indent_len..];
        if let Some(marker @ ('-' | '+')) = item.chars().next()
            && item.as_bytes().get(1) == Some(&b' ')
        {
            let mut insert = String::with_capacity(indent_len + 2);
            insert.push_str(indent);
            if item.len() > 2 {
                insert.push(marker);
                insert.push(' ');
            }
            let caret_chars = insert.chars().count();
            return Some((insert, caret_chars));
        }
    }

    // 3. Existing indentation rules.
    let closer = after.and_then(matching_opener);
    if let Some(opener) = closer
        && nearest_unmatched_opener(text, cursor_byte) == Some(opener)
    {
        let aligned = if previous_is_list {
            indent.to_owned()
        } else {
            dedent_one_level(indent).to_owned()
        };
        let caret_chars = aligned.chars().count();
        return Some((aligned, caret_chars));
    }

    // A `$` on this line opens a level only while its math block is left open;
    // a closed inline equation (`$x = 1$`) must not indent the next line.
    let open_dollar =
        line.contains('$') && nearest_unmatched_opener(text, cursor_byte) == Some('$');
    let ends_with_opener =
        !previous_is_list && (line.trim_end().ends_with(['{', '[', '(']) || open_dollar);
    if indent_len == 0 && !ends_with_opener {
        return None;
    }
    let mut insert = String::with_capacity(indent_len + INDENT.len());
    insert.push_str(indent);
    if ends_with_opener {
        insert.push_str(INDENT);
    }
    let caret_chars = insert.chars().count();
    Some((insert, caret_chars))
}

/// Buffer-backed [`auto_indent`]. Reads only the previous line, the char after
/// the caret, and a bounded opener window instead of materializing the document.
/// `cursor_byte` points just after the inserted newline and `list_lines` holds
/// line-start byte offsets.
pub(crate) fn auto_indent_buffer(
    buffer: &TextBuffer,
    cursor_byte: usize,
    list_lines: &[usize],
) -> Option<(String, usize)> {
    if cursor_byte == 0 || cursor_byte > buffer.len_bytes() {
        return None;
    }
    let caret = buffer.byte_to_char(cursor_byte);
    if buffer.char_to_byte(caret) != cursor_byte || buffer.char_at(caret - 1)? != '\n' {
        return None;
    }

    let prev_line = buffer.line_of_char(caret - 1);
    let line_start = buffer.line_start(prev_line);
    let line = buffer.slice(line_start..caret - 1);
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent = &line[..indent_len];
    let previous_is_list = list_lines.contains(&buffer.char_to_byte(line_start));
    let after = buffer.char_at(caret);
    let before = line.chars().next_back();

    if let Some(opener) = after.and_then(matching_opener)
        && before == Some(opener)
    {
        let insert = format!("{indent}{INDENT}\n{indent}");
        let caret_chars = indent.chars().count() + INDENT.chars().count();
        return Some((insert, caret_chars));
    }

    let at_line_end = after.is_none_or(|ch| ch == '\n');
    if previous_is_list && at_line_end {
        let item = &line[indent_len..];
        if let Some(marker @ ('-' | '+')) = item.chars().next()
            && item.as_bytes().get(1) == Some(&b' ')
        {
            let mut insert = String::with_capacity(indent_len + 2);
            insert.push_str(indent);
            if item.len() > 2 {
                insert.push(marker);
                insert.push(' ');
            }
            let caret_chars = insert.chars().count();
            return Some((insert, caret_chars));
        }
    }

    let unmatched = nearest_unmatched_opener_buffer(buffer, caret);
    let closer = after.and_then(matching_opener);
    if let Some(opener) = closer
        && unmatched == Some(opener)
    {
        let aligned = if previous_is_list {
            indent.to_owned()
        } else {
            dedent_one_level(indent).to_owned()
        };
        let caret_chars = aligned.chars().count();
        return Some((aligned, caret_chars));
    }

    let open_dollar = line.contains('$') && unmatched == Some('$');
    let ends_with_opener =
        !previous_is_list && (line.trim_end().ends_with(['{', '[', '(']) || open_dollar);
    if indent_len == 0 && !ends_with_opener {
        return None;
    }
    let mut insert = String::with_capacity(indent_len + INDENT.len());
    insert.push_str(indent);
    if ends_with_opener {
        insert.push_str(INDENT);
    }
    let caret_chars = insert.chars().count();
    Some((insert, caret_chars))
}

/// The nearest unmatched opener before `caret_char`, read through a bounded
/// char window from the rope.
pub(crate) fn nearest_unmatched_opener_buffer(
    buffer: &TextBuffer,
    caret_char: usize,
) -> Option<char> {
    let start = caret_char.saturating_sub(UNMATCHED_SCAN_LIMIT);
    let window = buffer.slice(start..caret_char);
    nearest_unmatched_opener(&window, window.len())
}

/// Buffer-backed [`empty_list_item_line`]: returns the line-start char index.
pub(crate) fn empty_list_item_line_buffer(
    buffer: &TextBuffer,
    caret_byte: usize,
    list_lines: &[usize],
) -> Option<usize> {
    let caret = buffer.byte_to_char(caret_byte.min(buffer.len_bytes()));
    let line = buffer.line_of_char(caret);
    let line_start = buffer.line_start(line);
    if !list_lines.contains(&buffer.char_to_byte(line_start)) {
        return None;
    }
    let next_start = buffer.line_start(line + 1);
    let line_end = if next_start > line_start && buffer.char_at(next_start - 1) == Some('\n') {
        next_start - 1
    } else {
        next_start
    };
    let line_text = buffer.slice(line_start..line_end);
    let rest = line_text.trim_start_matches([' ', '\t']);
    let marker = rest.chars().next()?;
    if !matches!(marker, '-' | '+') {
        return None;
    }
    let after = rest[marker.len_utf8()..].strip_prefix(' ')?;
    after
        .bytes()
        .all(|b| b == b' ' || b == b'\t')
        .then_some(line_start)
}

/// How many leading whitespace characters to remove from the current line when
/// a closing delimiter has just been typed at `cursor_byte`.
///
/// `cursor_byte` must point immediately after exactly one `)`, `]`, `}`, or
/// `$` whose opener is the nearest unmatched opener, and everything before that
/// delimiter on the current line must be spaces/tabs (and non-empty). Returns
/// one indentation level (2 chars), or fewer when the prefix is shorter.
pub(crate) fn auto_dedent(text: &str, cursor_byte: usize) -> Option<usize> {
    if cursor_byte == 0 || cursor_byte > text.len() {
        return None;
    }
    let prev_byte = cursor_byte - 1;
    if !text.is_char_boundary(cursor_byte) || !text.is_char_boundary(prev_byte) {
        return None;
    }
    let closer = text[prev_byte..cursor_byte].chars().next()?;
    if matching_opener(closer)? != nearest_unmatched_opener(text, prev_byte)? {
        return None;
    }

    let line_start = text[..prev_byte].rfind('\n').map_or(0, |i| i + 1);
    let prefix = &text[line_start..prev_byte];
    if prefix.is_empty() || !prefix.bytes().all(|b| b == b' ' || b == b'\t') {
        return None;
    }
    Some(prefix.len().min(INDENT.len()))
}

/// The opener that `c` closes, for the auto-dedent rules.
pub(crate) fn matching_opener(c: char) -> Option<char> {
    Some(match c {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        '$' => '$',
        _ => return None,
    })
}

/// The delimiter that Typst markup pairs with `c`, if any.
///
/// The single quote is deliberately absent: in prose it is an apostrophe, and
/// auto-closing it annoys more than it helps.
pub(crate) fn pair_closer(c: char) -> Option<char> {
    Some(match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        '*' | '_' | '`' | '$' | '"' => c,
        _ => return None,
    })
}

/// The char range of an empty delimiter pair the caret sits inside, if any:
/// the chars at `caret_char - 1` and `caret_char` must form a pair. Deleting at
/// such a caret (Backspace or Delete) removes both chars at once.
#[cfg(test)]
pub(crate) fn empty_pair_at(text: &str, caret_char: usize) -> Option<Range<usize>> {
    if caret_char == 0 {
        return None;
    }
    let mut chars = text.chars().skip(caret_char - 1);
    let before = chars.next()?;
    let after = chars.next()?;
    (pair_closer(before) == Some(after)).then_some(caret_char - 1..caret_char + 1)
}

/// Buffer-backed [`empty_pair_at`]: reads only the two chars around the caret
/// instead of materializing the document.
pub(crate) fn empty_pair_at_buffer(buffer: &TextBuffer, caret_char: usize) -> Option<Range<usize>> {
    if caret_char == 0 {
        return None;
    }
    let before = buffer.char_at(caret_char - 1)?;
    let after = buffer.char_at(caret_char)?;
    (pair_closer(before) == Some(after)).then_some(caret_char - 1..caret_char + 1)
}

/// Whether `c` is one of the bracket glyphs the pair highlighter covers.
pub(crate) fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}')
}

/// Char indices of the bracket matching the one at `char_index` (`()[]{}` only).
///
/// Openers scan forward and closers backward, counting nesting depth, and stop
/// after [`UNMATCHED_SCAN_LIMIT`] scanned chars. The scan reads at most one
/// bounded window from the rope; it never materializes the whole document.
/// A non-bracket or an unmatched bracket yields `None`. The opener/closer lookup
/// reuses [`pair_closer`]/[`matching_opener`].
pub(crate) fn bracket_match(buffer: &TextBuffer, char_index: usize) -> Option<(usize, usize)> {
    let bracket = buffer.char_at(char_index)?;
    let (forward, other) = match bracket {
        '(' | '[' | '{' => (true, pair_closer(bracket)?),
        ')' | ']' | '}' => (false, matching_opener(bracket)?),
        _ => return None,
    };

    let mut depth = 0usize;
    if forward {
        let end = char_index
            .saturating_add(UNMATCHED_SCAN_LIMIT)
            .min(buffer.len_chars());
        let window = buffer.slice(char_index..end);
        for (offset, ch) in window.chars().enumerate() {
            if ch == bracket {
                depth += 1;
            } else if ch == other {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((char_index, char_index + offset));
                }
            }
        }
    } else {
        let start = (char_index + 1).saturating_sub(UNMATCHED_SCAN_LIMIT);
        let window = buffer.slice(start..char_index + 1);
        for (offset, ch) in window.chars().rev().enumerate() {
            let index = char_index - offset;
            if ch == bracket {
                depth += 1;
            } else if ch == other {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((index, char_index));
                }
            }
        }
    }
    None
}

/// The bracket pair to outline at `caret_char`: the bracket immediately before
/// the caret wins, otherwise the one immediately after.
pub(crate) fn bracket_pair_at_caret(
    buffer: &TextBuffer,
    caret_char: usize,
) -> Option<(usize, usize)> {
    if caret_char > 0
        && char_before_caret(buffer, caret_char).is_some_and(is_bracket)
        && let Some(pair) = bracket_match(buffer, caret_char - 1)
    {
        return Some(pair);
    }
    if char_before_caret(buffer, caret_char + 1).is_some_and(is_bracket) {
        return bracket_match(buffer, caret_char);
    }
    None
}

/// The nearest opener before `pos_byte` that has no closer between it and
/// `pos_byte` — this is how an indent level "remembers" its trigger symbol.
/// Scans backwards at most [`UNMATCHED_SCAN_LIMIT`] chars.
pub(crate) fn nearest_unmatched_opener(text: &str, pos_byte: usize) -> Option<char> {
    let pos = diagnostics::floor_char_boundary(text, pos_byte.min(text.len()));
    let prefix = &text[..pos];
    // `$` is symmetric, so it needs counting instead of nesting: an odd count
    // of *unescaped* dollars leaves the last one open. A `$` escaped by the
    // immediately preceding backslash is a literal, not a math delimiter.
    let dollar_open = {
        let mut count = 0usize;
        let mut escaped = false;
        for c in prefix.chars() {
            if c == '$' && !escaped {
                count += 1;
            }
            escaped = c == '\\';
        }
        count % 2 == 1
    };
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;

    for (count, ch) in prefix.chars().rev().enumerate() {
        if count >= UNMATCHED_SCAN_LIMIT {
            return None;
        }
        match ch {
            ')' => paren += 1,
            ']' => bracket += 1,
            '}' => brace += 1,
            '(' | '[' | '{' => {
                let depth = match ch {
                    '(' => &mut paren,
                    '[' => &mut bracket,
                    _ => &mut brace,
                };
                if *depth == 0 {
                    return Some(ch);
                }
                *depth -= 1;
            }
            '$' if dollar_open => {
                return Some('$');
            }
            _ => {}
        }
    }
    None
}

/// Whether an unescaped `"` is open before `pos_byte` (odd count; escaped
/// quotes don't count). `pos_byte` must be a char boundary.
pub(crate) fn unmatched_double_quote(text: &str, pos_byte: usize) -> bool {
    let pos = diagnostics::floor_char_boundary(text, pos_byte.min(text.len()));
    // `"` is symmetric, so it needs counting instead of nesting: an odd count of
    // *unescaped* quotes leaves the last one open. A `"` escaped by the
    // immediately preceding backslash is a literal, not a delimiter.
    let mut count = 0usize;
    let mut escaped = false;
    for c in text[..pos].chars() {
        if c == '"' && !escaped {
            count += 1;
        }
        escaped = c == '\\';
    }
    count % 2 == 1
}

/// Line-start byte offset of the previous non-empty line, if any.
#[cfg(test)]
pub(crate) fn previous_nonempty_line_start(text: &str, line_start: usize) -> Option<usize> {
    if line_start == 0 {
        return None;
    }
    let mut end = diagnostics::floor_char_boundary(text, line_start.saturating_sub(1));
    loop {
        let start = text[..end].rfind('\n').map_or(0, |i| i + 1);
        if !text.get(start..end)?.trim().is_empty() {
            return Some(start);
        }
        if start == 0 {
            return None;
        }
        end = diagnostics::floor_char_boundary(text, start - 1);
    }
}

/// Line-start byte offset of the previous non-empty line, read through the
/// rope's line index. Equivalent to [`previous_nonempty_line_start`] for the
/// same text, without materializing the whole buffer.
pub(crate) fn previous_nonempty_line_start_buffer(
    buffer: &TextBuffer,
    line_start_char: usize,
) -> Option<usize> {
    let current = buffer.line_of_char(line_start_char);
    let mut line = current.checked_sub(1)?;
    loop {
        let start = buffer.line_start(line);
        let end = buffer.line_start(line + 1);
        if !buffer.slice(start..end).trim().is_empty() {
            return Some(buffer.char_to_byte(start));
        }
        if line == 0 {
            return None;
        }
        line -= 1;
    }
}

/// Strips one indentation level (up to [`INDENT`] leading whitespace chars).
pub(crate) fn dedent_one_level(indent: &str) -> &str {
    let n = indent
        .bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
        .min(INDENT.len());
    &indent[n..]
}

/// Byte range + substring of a char range, when non-empty and valid.
#[cfg(test)]
pub(crate) fn char_slice(text: &str, chars: Range<usize>) -> Option<(Range<usize>, &str)> {
    if chars.start >= chars.end {
        return None;
    }
    let start = byte_of_char(text, chars.start);
    let end = byte_of_char(text, chars.end);
    if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    Some((start..end, &text[start..end]))
}

/// Byte offset of `char_idx`, clamped to the text length.
///
/// A test-only single-pass scan: production paths convert through the
/// rope-backed [`TextBuffer`] in `crate::completion`.
#[cfg(test)]
fn byte_of_char(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(byte, _)| byte)
}

#[cfg(test)]
pub(crate) fn indent_empty_list_item(
    text: &str,
    caret_byte: usize,
    list_lines: &[usize],
) -> Option<(String, usize)> {
    let line_start = empty_list_item_line(text, caret_byte, list_lines)?;
    let (new_text, _selection, new_caret) = indent_lines(text, line_start..line_start, caret_byte);
    Some((new_text, new_caret))
}

/// Char-indexed [`touched_line_starts`] read through the rope's line index.
pub(crate) fn touched_line_starts_buffer(
    buffer: &TextBuffer,
    selection: Range<usize>,
) -> Vec<usize> {
    let len = buffer.len_chars();
    let start = selection.start.min(len);
    let end = selection.end.min(len);
    let first = buffer.line_of_char(start);
    let last = if end > start {
        buffer.line_of_char(end - 1)
    } else {
        first
    };
    (first..=last).map(|line| buffer.line_start(line)).collect()
}

/// Byte offsets of the line starts touched by the byte range (a single line
/// start for an empty/caret range). A trailing empty line after the selection
/// is not touched.
#[cfg(test)]
pub(crate) fn touched_line_starts(text: &str, selection: Range<usize>) -> Vec<usize> {
    let start = diagnostics::floor_char_boundary(text, selection.start);
    let end = diagnostics::ceil_char_boundary(text, selection.end);
    let mut starts = vec![text[..start].rfind('\n').map_or(0, |i| i + 1)];
    if end > start {
        for (offset, ch) in text[start..end].char_indices() {
            if ch == '\n' {
                let next = start + offset + 1;
                if next < end {
                    starts.push(next);
                }
            }
        }
    }
    starts
}

/// Inserts one indent level at the start of every touched line. Returns the new
/// text plus the shifted byte offsets of the selection and the caret.
#[cfg(test)]
pub(crate) fn indent_lines(
    text: &str,
    selection: Range<usize>,
    caret: usize,
) -> (String, Range<usize>, usize) {
    let starts = touched_line_starts(text, selection.clone());
    let mut new_text = text.to_owned();
    for &start in starts.iter().rev() {
        new_text.insert_str(start, INDENT);
    }
    let shift = |byte: usize| byte + INDENT.len() * starts.iter().filter(|&&s| s <= byte).count();
    (
        new_text,
        shift(selection.start)..shift(selection.end),
        shift(caret),
    )
}

/// Removes up to one indent level of leading whitespace from every touched
/// line. Returns the new text plus the shifted byte offsets.
#[cfg(test)]
pub(crate) fn unindent_lines(
    text: &str,
    selection: Range<usize>,
    caret: usize,
) -> (String, Range<usize>, usize) {
    let starts = touched_line_starts(text, selection.clone());
    let removals: Vec<(usize, usize)> = starts
        .iter()
        .filter_map(|&start| {
            let n = leading_ws_len(&text[start..]).min(INDENT.len());
            (n > 0).then_some((start, n))
        })
        .collect();
    let mut new_text = text.to_owned();
    for &(start, n) in removals.iter().rev() {
        new_text.replace_range(start..start + n, "");
    }
    let map = |byte: usize| {
        let removed: usize = removals
            .iter()
            .map(|&(start, n)| n.min(byte.saturating_sub(start)))
            .sum();
        byte.saturating_sub(removed)
    };
    (
        new_text,
        map(selection.start)..map(selection.end),
        map(caret),
    )
}

/// Number of leading ASCII whitespace bytes of a line.
#[cfg(test)]
pub(crate) fn leading_ws_len(line: &str) -> usize {
    line.bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
}

/// Line-start byte offset of an empty list item under the caret.
///
/// `list_lines` holds the line-start byte offsets of parsed list items. Returns
/// the caret line's start when that line is a list item whose content after the
/// leading whitespace is exactly a `- ` or `+ ` marker followed only by
/// spaces/tabs (or nothing at all). A non-list line, or a marker with content,
/// yields `None`.
#[cfg(test)]
pub(crate) fn empty_list_item_line(
    text: &str,
    caret_byte: usize,
    list_lines: &[usize],
) -> Option<usize> {
    let caret = diagnostics::floor_char_boundary(text, caret_byte.min(text.len()));
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
    if !list_lines.contains(&line_start) {
        return None;
    }
    let line = text[line_start..].split('\n').next().unwrap_or_default();
    let rest = line.trim_start_matches([' ', '\t']);
    let marker = rest.chars().next()?;
    if !matches!(marker, '-' | '+') {
        return None;
    }
    let after = rest[marker.len_utf8()..].strip_prefix(' ')?;
    after
        .bytes()
        .all(|b| b == b' ' || b == b'\t')
        .then_some(line_start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_bracket_accepts_only_bracket_glyphs() {
        for bracket in ['(', ')', '[', ']', '{', '}'] {
            assert!(is_bracket(bracket), "{bracket:?}");
        }
        for other in ['$', '<', '>', 'a', '你'] {
            assert!(!is_bracket(other), "{other:?}");
        }
    }

    #[test]
    fn empty_pair_at_is_char_indexed_across_cjk() {
        // chars: 0=你 1=( 2=) ; the caret between `(` and `)` is 2.
        assert_eq!(empty_pair_at("你()", 2), Some(1..3));
        assert_eq!(empty_pair_at("你()", 1), None);
    }

    #[test]
    fn empty_pair_at_buffer_is_char_indexed_across_cjk() {
        let buffer = TextBuffer::from_text("你()");
        assert_eq!(empty_pair_at_buffer(&buffer, 2), Some(1..3));
        assert_eq!(empty_pair_at_buffer(&buffer, 1), None);
        assert_eq!(empty_pair_at_buffer(&buffer, 0), None);
    }

    #[test]
    fn auto_indent_buffer_matches_the_text_helper() {
        let cases: &[(&str, &[usize])] = &[
            ("foo(\n)", &[]),
            ("  - \n", &[0]),
            ("(\n)", &[]),
            ("  foo\nx", &[]),
            ("$x = \n", &[]),
        ];
        for (text, list) in cases {
            let caret = text.find('\n').expect("newline") + 1;
            let buffer = TextBuffer::from_text(text);
            assert_eq!(
                auto_indent_buffer(&buffer, caret, list),
                auto_indent(text, caret, list),
                "{text:?}"
            );
        }
    }

    #[test]
    fn empty_list_item_line_buffer_matches_the_text_helper() {
        for (text, list) in [("  - \nnext", &[0usize][..]), ("  - x\nnext", &[0][..])] {
            let caret = text.find('\n').expect("newline");
            let buffer = TextBuffer::from_text(text);
            assert_eq!(
                empty_list_item_line_buffer(&buffer, caret, list),
                empty_list_item_line(text, caret, list),
                "{text:?}"
            );
        }
    }

    #[test]
    fn touched_line_starts_buffer_matches_the_text_helper() {
        let cases = [
            ("abc\ndef", 0..0),
            ("abc\ndef", 0..3),
            ("abc\ndef", 0..4),
            ("abc\ndef", 1..7),
            ("abc\ndef", 4..4),
        ];
        for (text, chars) in cases {
            let buffer = TextBuffer::from_text(text);
            let starts = touched_line_starts_buffer(&buffer, chars.clone());
            let byte_starts: Vec<usize> = starts
                .iter()
                .map(|&start| buffer.char_to_byte(start))
                .collect();
            let text_starts = touched_line_starts(
                text,
                buffer.char_to_byte(chars.start)..buffer.char_to_byte(chars.end),
            );
            assert_eq!(byte_starts, text_starts, "{text:?} {chars:?}");
        }
    }

    #[test]
    fn char_slice_rejects_a_reversed_range() {
        // Built at run time so `clippy::reversed_empty_ranges` stays armed for the
        // production code: a literal `3..1` here would trip that correctness lint
        // and fail `cargo clippy --all-targets`.
        let (start, end) = (3, 1);
        assert_eq!(char_slice("abcd", start..end), None);
        assert_eq!(char_slice("abcd", 2..2), None);
    }

    #[test]
    fn dedent_one_level_is_byte_based_for_tabs() {
        // A tab counts as one whitespace byte, so it is removed whole; a
        // three-space indent only loses the two-byte level.
        assert_eq!(dedent_one_level("\ta"), "a");
        assert_eq!(dedent_one_level("   a"), " a");
        assert_eq!(dedent_one_level("    a"), "  a");
    }

    #[test]
    fn indent_lines_ignores_a_trailing_empty_line() {
        // The selection ends before the final newline, so only `a` and `b` are
        // touched; the trailing empty line gains no indent.
        let (text, range, caret) = indent_lines("a\nb\n", 0..3, 3);
        assert_eq!(text, "  a\n  b\n");
        assert_eq!(range, 2..7);
        assert_eq!(caret, 7);
    }

    #[test]
    fn unindent_lines_removes_at_most_one_level_and_keeps_tabs() {
        // Two tabs at the top level are one level (INDENT is two bytes) and the
        // single tab on the second line is removed whole.
        let (text, range, caret) = unindent_lines("\t\ta\n\tb", 0..5, 5);
        assert_eq!(text, "a\nb");
        assert_eq!(range, 0..2);
        assert_eq!(caret, 2);
    }

    #[test]
    fn auto_indent_does_not_treat_carriage_return_as_whitespace() {
        // A CRLF line has no leading indentation and no opener: nothing to add.
        assert_eq!(auto_indent("abc\r\n", 5, &[]), None);
    }
}
