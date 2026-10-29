use crate::completion::{
    CompletionContext, CompletionGate, call_slot_items, caret_in_call, completion_context,
    completion_may_open, completion_prefix, completion_preview, completion_trigger,
    popup_owns_enter, rank_completions, word_after_hash,
};
use crate::find_bar::{first_match_at_or_after, step_match};
use crate::paint::drag_autoscroll_delta;
use crate::smart_edit::{
    auto_dedent, bracket_match, bracket_pair_at_caret, char_slice, dedent_one_level,
    empty_list_item_line, empty_pair_at, indent_empty_list_item, indent_lines, leading_ws_len,
    matching_opener, nearest_unmatched_opener, pair_closer, previous_nonempty_line_start,
    previous_nonempty_line_start_buffer, touched_line_starts, unindent_lines,
    unmatched_double_quote,
};
use crate::{
    COMPLETION_POPUP_ID, COMPLETION_TOOLTIP_ID, CapturedEdit, CompletionPopup,
    DRAG_SCROLL_MAX_SPEED, EditorCommand, EditorInput, EditorOutput, EditorState, FIND_INPUT_ID,
    HistoryRequest, RemoteCaret, WrapMode,
};
use typ_edit_model::{
    BytePos, CallParams, CharCount, CompletionItem, CompletionKind, FoldKind, FoldRange,
    HighlightKind, TextEdit, ValueSlot,
};

/// Wraps the harness's plain byte ranges into the typed boundary the editor's
/// [`EditorInput`] now takes.
fn byte_ranges(ranges: &[std::ops::Range<usize>]) -> Vec<typ_edit_model::ByteRange> {
    ranges
        .iter()
        .map(|range| BytePos(range.start)..BytePos(range.end))
        .collect()
}

/// [`byte_ranges`] for highlight spans (range plus kind).
fn byte_highlights(
    spans: &[(std::ops::Range<usize>, HighlightKind)],
) -> Vec<(typ_edit_model::ByteRange, HighlightKind)> {
    spans
        .iter()
        .map(|(range, kind)| (BytePos(range.start)..BytePos(range.end), *kind))
        .collect()
}

/// Unwraps typed parse ranges for the pure usize-space completion predicates.
fn plain_ranges(ranges: &[typ_edit_model::ByteRange]) -> Vec<std::ops::Range<usize>> {
    ranges
        .iter()
        .map(|range| range.start.0..range.end.0)
        .collect()
}

/// Rope-backed buffer over `text`, so the char↔byte conversions under test take
/// the same path as production.
fn buffer(text: &str) -> typ_edit_buffer::TextBuffer {
    typ_edit_buffer::TextBuffer::from_text(text)
}

/// A minimal [`EditorInput`] with empty slices; tests override the fields they
/// need.
fn empty_input<'a>() -> EditorInput<'a> {
    EditorInput {
        collab_history: false,
        remote_carets: &[],
        highlights: &[],
        diagnostics: &[],
        blocked_ranges: &[],
        code_ranges: &[],
        math_ranges: &[],
        math_atoms: &[],
        spell_issues: &[],
        debug_highlights: &[],
        debug_highlight_color: egui::Color32::TRANSPARENT,
        folds: &[],
        list_lines: &[],
        completions: &[],
        definitions: &[],
        world_completions: &[],
        completion_max: crate::COMPLETION_MAX_DEFAULT,
        parse_generation: 0,
        call_params: None,
        font_family: "sans",
        font_size: 14.0,
        wrap_mode: WrapMode::Off,
        line_spacing: 1.0,
        wrap_line_spacing: 1.0,
        plain_text: false,
        request_focus: false,
        blame_inline: None,
    }
}

/// `auto_indent` for tests that are not list-aware.
fn auto_indent(text: &str, cursor_byte: usize) -> Option<(String, usize)> {
    crate::smart_edit::auto_indent(text, cursor_byte, &[])
}

fn bind_test_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let chain = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts
        .families
        .insert(egui::FontFamily::Name("sans".into()), chain);
    ctx.set_fonts(fonts);
}

#[test]
fn plain_line_has_no_indent() {
    assert_eq!(auto_indent("abc\n", 4), None);
}

#[test]
fn indented_line_copies_its_indent() {
    assert_eq!(auto_indent("  abc\n", 6), Some(("  ".to_owned(), 2)));
    assert_eq!(auto_indent("\t\tfoo\n", 6), Some(("\t\t".to_owned(), 2)));
}

#[test]
fn opening_brackets_add_one_level() {
    for text in ["    abc{\n", "    abc[\n", "    abc(\n"] {
        assert_eq!(
            auto_indent(text, text.len()),
            Some(("      ".to_owned(), 6))
        );
    }
    // Trailing spaces after the bracket still count.
    assert_eq!(auto_indent("  foo{ \n", 8), Some(("    ".to_owned(), 4)));
}

#[test]
fn empty_line_yields_none() {
    assert_eq!(auto_indent("\n", 1), None);
    assert_eq!(auto_indent("abc\n\n", 5), None);
}

#[test]
fn caret_not_after_newline_yields_none() {
    assert_eq!(auto_indent("abc", 3), None);
    assert_eq!(auto_indent("a\nb", 3), None);
    assert_eq!(auto_indent("", 0), None);
}

#[test]
fn cjk_before_the_caret() {
    let text = "  你好\n";
    assert_eq!(auto_indent(text, text.len()), Some(("  ".to_owned(), 2)));

    let text = "  你{\n";
    assert_eq!(auto_indent(text, text.len()), Some(("    ".to_owned(), 4)));
}

#[test]
fn out_of_range_cursor_yields_none() {
    assert_eq!(auto_indent("abc\n", 999), None);
    assert_eq!(auto_indent("abc\n", usize::MAX), None);
}

#[test]
fn closing_delimiter_dedents_one_level() {
    assert_eq!(auto_dedent("[\n    ]", 7), Some(2));
    assert_eq!(auto_dedent("{\n      }", 9), Some(2));
    assert_eq!(auto_dedent("(\n )", 4), Some(1));
    assert_eq!(auto_dedent("$ x\n  $", 7), Some(2));
}

#[test]
fn dedent_requires_the_matching_opener() {
    // The nearest unmatched opener is a `(`, so `]` must not dedent.
    assert_eq!(auto_dedent("(\n  ]", 5), None);
    // No opener at all.
    assert_eq!(auto_dedent("    ]", 5), None);
    // A list marker does not open a delimited context.
    assert_eq!(auto_dedent("-\n  )", 5), None);
}

#[test]
fn nearest_unmatched_opener_finds_the_open_level() {
    assert_eq!(nearest_unmatched_opener("foo((bar", 8), Some('('));
    assert_eq!(nearest_unmatched_opener("(x)", 3), None);
    assert_eq!(nearest_unmatched_opener("[a{b", 4), Some('{'));
    let cjk = "你好(((";
    assert_eq!(nearest_unmatched_opener(cjk, cjk.len()), Some('('));
    assert_eq!(nearest_unmatched_opener("", 0), None);
    assert_eq!(nearest_unmatched_opener("$ x + $", 7), None);
    assert_eq!(nearest_unmatched_opener("$ x + ", 6), Some('$'));
    // An escaped `$` is a literal and must not flip the parity.
    assert_eq!(nearest_unmatched_opener("Cost: \\$", 8), None);
    assert_eq!(nearest_unmatched_opener("Cost: \\$$", 9), Some('$'));
}

#[test]
fn escaped_dollar_does_not_open_a_math_indent() {
    // `\$` is a literal dollar; the next line gains no indent level.
    assert_eq!(auto_indent("Cost: \\$\n", 9), None);
    // An unescaped unclosed `$` still opens one level.
    assert_eq!(auto_indent("Cost: $\n", 8), Some(("  ".to_owned(), 2)));
}

#[test]
fn set_text_refreshes_line_count_and_text() {
    let mut state = EditorState::new();
    state.path = Some(std::path::PathBuf::from("/w/old.typ"));
    state.set_buffer_text("old");

    state.set_text("a\nb\nc".to_owned());

    assert_eq!(state.shared_text().as_str(), "a\nb\nc");
    assert_eq!(state.buffer.len_chars(), 5);
    assert_eq!(state.line_count(), 3, "line_count tracks the new text");
    assert_eq!(
        state.path.as_deref(),
        Some(std::path::Path::new("/w/old.typ")),
        "set_text must leave the path untouched"
    );
}

#[test]
fn line_column_uses_the_buffer_line_index() {
    let mut state = EditorState::new();
    state.set_text("ab\ncde\n".to_owned());
    assert_eq!(state.line_column(0), (1, 1));
    assert_eq!(state.line_column(3), (2, 1));
    assert_eq!(state.line_column(5), (2, 3));
    assert_eq!(state.line_column(7), (3, 1));

    state.set_text("你好\n世界".to_owned());
    assert_eq!(state.line_column(0), (1, 1));
    assert_eq!(state.line_column(3), (1, 2));
    assert_eq!(state.line_column(7), (2, 1));
}

#[test]
fn previous_nonempty_line_skips_blank_lines() {
    assert_eq!(previous_nonempty_line_start("a\n\n  \nb", 6), Some(0));
    assert_eq!(previous_nonempty_line_start("a\nb", 2), Some(0));
    assert_eq!(previous_nonempty_line_start("b", 0), None);
    assert_eq!(previous_nonempty_line_start("\n\nx", 3), None);
}

#[test]
fn previous_nonempty_line_buffer_matches_the_text_helper() {
    let cases = [
        ("a\n\n  \nb", 6, Some(0)),
        ("a\nb", 2, Some(0)),
        ("b", 0, None),
        ("\n\nx", 3, None),
    ];
    for (text, line_start_byte, expected) in cases {
        let buf = buffer(text);
        let line_start_char = buf.byte_to_char(line_start_byte);
        assert_eq!(
            previous_nonempty_line_start_buffer(&buf, line_start_char),
            expected,
            "{text:?} at byte {line_start_byte}"
        );
    }
}

#[test]
fn list_aware_indentation() {
    assert_eq!(
        crate::smart_edit::auto_indent("  - a(\n", 7, &[0]),
        Some(("  - ".to_owned(), 4))
    );
    assert_eq!(
        crate::smart_edit::auto_indent("  - a(\n", 7, &[]),
        Some(("    ".to_owned(), 4))
    );
}

#[test]
fn enter_before_matching_closer_aligns_with_the_opener() {
    assert_eq!(
        crate::smart_edit::auto_indent("foo(abc\n)", 8, &[]),
        Some((String::new(), 0))
    );
    assert_eq!(
        crate::smart_edit::auto_indent("    foo(abc\n)", 12, &[]),
        Some(("  ".to_owned(), 2))
    );
}

#[test]
fn enter_inside_an_empty_pair_expands_to_three_lines() {
    assert_eq!(auto_indent("(\n)", 2), Some(("  \n".to_owned(), 2)));
    assert_eq!(auto_indent("[\n]", 2), Some(("  \n".to_owned(), 2)));
    assert_eq!(auto_indent("{\n}", 2), Some(("  \n".to_owned(), 2)));
    assert_eq!(auto_indent("$\n$", 2), Some(("  \n".to_owned(), 2)));
    assert_eq!(auto_indent("  (\n)", 4), Some(("    \n  ".to_owned(), 4)));
}

#[test]
fn opener_at_line_end_indents_even_without_existing_indent() {
    assert_eq!(auto_indent("$\n", 2), Some(("  ".to_owned(), 2)));
    assert_eq!(auto_indent("(\n", 2), Some(("  ".to_owned(), 2)));
    assert_eq!(auto_indent("abc\n", 4), None);
}

#[test]
fn closed_inline_math_does_not_indent_the_next_line() {
    // A trailing `$` that closes math is not an opener.
    assert_eq!(auto_indent("text $x = 1$\n", 13), None);
    // Balanced inline equations are closed too.
    assert_eq!(auto_indent("$a$ and $b$\n", 12), None);
    // An unclosed `$` keeps the continuation indented one level.
    assert_eq!(auto_indent("text $x = 1\n", 12), Some(("  ".to_owned(), 2)));
}

#[test]
fn math_with_parentheses_indents_at_most_one_level() {
    // Closed math holding parentheses is not an opener.
    assert_eq!(auto_indent("$ f(x)$\n", 8), None);
    // An unclosed math opener adds exactly one level, even with a `(` inside.
    assert_eq!(auto_indent("$ f(\n", 5), Some(("  ".to_owned(), 2)));
    // A plain trailing `(` still adds a level.
    assert_eq!(auto_indent("  foo(\n", 7), Some(("    ".to_owned(), 4)));
}

#[test]
fn list_items_continue_with_their_marker() {
    assert_eq!(
        crate::smart_edit::auto_indent("- item\n", 7, &[0]),
        Some(("- ".to_owned(), 2))
    );
    assert_eq!(
        crate::smart_edit::auto_indent("+ item\n", 7, &[0]),
        Some(("+ ".to_owned(), 2))
    );
    assert_eq!(
        crate::smart_edit::auto_indent("  - item\n", 9, &[0]),
        Some(("  - ".to_owned(), 4))
    );
    assert_eq!(
        crate::smart_edit::auto_indent("  + item\n", 9, &[0]),
        Some(("  + ".to_owned(), 4))
    );
}

#[test]
fn empty_list_item_ends_the_list() {
    assert_eq!(
        crate::smart_edit::auto_indent("- \n", 3, &[0]),
        Some((String::new(), 0))
    );
    assert_eq!(
        crate::smart_edit::auto_indent("  - \n", 5, &[0]),
        Some(("  ".to_owned(), 2))
    );
}

#[test]
fn splitting_a_list_item_does_not_repeat_the_marker() {
    assert_eq!(crate::smart_edit::auto_indent("- it\nem", 5, &[0]), None);
}

#[test]
fn empty_list_item_line_only_matches_blank_markers() {
    assert_eq!(empty_list_item_line("- ", 2, &[0]), Some(0));
    assert_eq!(empty_list_item_line("+ ", 2, &[0]), Some(0));
    assert_eq!(empty_list_item_line("  - ", 4, &[0]), Some(0));
    assert_eq!(empty_list_item_line("-   \t", 5, &[0]), Some(0));
    assert_eq!(empty_list_item_line("- abc", 5, &[0]), None);
    assert_eq!(empty_list_item_line("-", 1, &[0]), None);
    assert_eq!(empty_list_item_line("plain", 5, &[0]), None);
    assert_eq!(empty_list_item_line("- ", 2, &[]), None);
}

#[test]
fn tab_indents_an_empty_list_item_line_and_its_caret() {
    assert_eq!(
        indent_empty_list_item("- ", 2, &[0]),
        Some(("  - ".to_owned(), 4))
    );
    assert_eq!(
        indent_empty_list_item("+ ", 2, &[0]),
        Some(("  + ".to_owned(), 4))
    );
    assert_eq!(
        indent_empty_list_item("-   ", 4, &[0]),
        Some(("  -   ".to_owned(), 6))
    );
    assert_eq!(indent_empty_list_item("- abc", 5, &[0]), None);
    assert_eq!(indent_empty_list_item("plain", 5, &[0]), None);
}

#[test]
fn tab_edits_indent_and_unindent_lines() {
    let (text, range, caret) = indent_lines("a\nb", 0..3, 3);
    assert_eq!(text, "  a\n  b");
    assert_eq!(range, 2..7);
    assert_eq!(caret, 7);

    let (text, range, caret) = unindent_lines("  a\n  b", 0..7, 7);
    assert_eq!(text, "a\nb");
    assert_eq!(range, 0..3);
    assert_eq!(caret, 3);

    assert_eq!(touched_line_starts("a\nb\nc", 4..4), vec![4]);
    assert_eq!(touched_line_starts("a\nb", 0..3), vec![0, 2]);
}

#[test]
fn char_slice_is_boundary_safe() {
    assert_eq!(
        char_slice("你好", 0..1).map(|(range, text)| (range, text.to_owned())),
        Some((0..3, "你".to_owned()))
    );
    assert_eq!(char_slice("ab", 1..1), None);
    assert_eq!(char_slice("ab", 9..12), None);
}

#[test]
fn delimiter_tables() {
    assert_eq!(pair_closer('('), Some(')'));
    assert_eq!(pair_closer('['), Some(']'));
    assert_eq!(pair_closer('$'), Some('$'));
    assert_eq!(pair_closer('a'), None);
    assert_eq!(
        pair_closer('\''),
        None,
        "single quotes are never auto-paired"
    );
    assert_eq!(matching_opener('}'), Some('{'));
    assert_eq!(matching_opener('$'), Some('$'));
    assert_eq!(matching_opener(')'), Some('('));
    assert_eq!(dedent_one_level("    "), "  ");
    assert_eq!(dedent_one_level(" "), "");
    assert_eq!(leading_ws_len("\t x"), 2);
}

#[test]
fn empty_pair_at_only_matches_a_caret_inside_a_pair() {
    assert_eq!(empty_pair_at("()", 1), Some(0..2));
    assert_eq!(empty_pair_at("\"\"", 1), Some(0..2));
    assert_eq!(empty_pair_at("$$", 1), Some(0..2));
    assert_eq!(empty_pair_at("a()b", 2), Some(1..3));
    assert_eq!(empty_pair_at("()", 0), None, "before the pair");
    assert_eq!(empty_pair_at("()", 2), None, "after the pair");
    assert_eq!(empty_pair_at("( )", 1), None, "not an empty pair");
    assert_eq!(empty_pair_at(")(", 1), None, "wrong order");
    assert_eq!(empty_pair_at("''", 1), None, "single quotes stay unpaired");
}

#[test]
fn bracket_at_column_zero_yields_none() {
    assert_eq!(auto_dedent("]", 1), None);
}

#[test]
fn bracket_match_pairs_nested_and_adjacent_brackets() {
    // "a(b[c]d)e": chars 1 '(' , 3 '[', 5 ']', 7 ')'.
    assert_eq!(bracket_match(&buffer("a(b[c]d)e"), 1), Some((1, 7)));
    assert_eq!(bracket_match(&buffer("a(b[c]d)e"), 7), Some((1, 7)));
    assert_eq!(bracket_match(&buffer("a(b[c]d)e"), 3), Some((3, 5)));
    assert_eq!(bracket_match(&buffer("a(b[c]d)e"), 5), Some((3, 5)));
    assert_eq!(bracket_match(&buffer("()"), 0), Some((0, 1)));
    assert_eq!(bracket_match(&buffer("()"), 1), Some((0, 1)));
}

#[test]
fn bracket_match_rejects_mismatches_and_non_brackets() {
    assert_eq!(bracket_match(&buffer("(]"), 0), None);
    assert_eq!(bracket_match(&buffer("[)"), 1), None);
    assert_eq!(bracket_match(&buffer("((("), 0), None);
    assert_eq!(bracket_match(&buffer(")))"), 0), None);
    assert_eq!(bracket_match(&buffer("abc"), 1), None);
    assert_eq!(bracket_match(&buffer(""), 0), None);
    // `$` and `<>` pair for the editor's markup helpers but not here.
    assert_eq!(bracket_match(&buffer("$x$"), 0), None);
    assert_eq!(bracket_match(&buffer("<a>"), 0), None);
}

#[test]
fn bracket_match_counts_nesting() {
    // `([{}])`: the outermost pair and the innermost pair both resolve.
    assert_eq!(bracket_match(&buffer("([{}])"), 0), Some((0, 5)));
    assert_eq!(bracket_match(&buffer("([{}])"), 2), Some((2, 3)));
    assert_eq!(bracket_match(&buffer("([{}])"), 3), Some((2, 3)));
}

#[test]
fn bracket_match_reads_a_bounded_window_of_a_large_buffer() {
    // A large document with a pair near the caret resolves without
    // materializing the document.
    let large = format!("{}()", "x".repeat(2_000_000));
    let large = buffer(&large);
    assert_eq!(
        bracket_match(&large, 2_000_000),
        Some((2_000_000, 2_000_001))
    );
    assert_eq!(
        bracket_match(&large, 2_000_001),
        Some((2_000_000, 2_000_001))
    );

    // A closer beyond the bounded scan window is not found, matching the old
    // full-collection behavior.
    let far = format!("({})", "x".repeat(crate::UNMATCHED_SCAN_LIMIT + 16));
    let far = buffer(&far);
    assert_eq!(bracket_match(&far, 0), None);
}

#[test]
fn bracket_pair_at_caret_prefers_the_preceding_bracket() {
    // Caret between `(` and `)`: the bracket before the caret wins.
    assert_eq!(bracket_pair_at_caret(&buffer("()"), 1), Some((0, 1)));
    // Caret right after `(` (no bracket before): the one after is used.
    assert_eq!(bracket_pair_at_caret(&buffer("()"), 0), Some((0, 1)));
    // Caret not adjacent to a bracket: nothing to highlight.
    assert_eq!(bracket_pair_at_caret(&buffer("abc"), 1), None);
    assert_eq!(bracket_pair_at_caret(&buffer("abc"), 2), None);
}

#[test]
fn completion_preview_lists_insert_when_it_differs_and_docs() {
    let param = CompletionItem {
        name: "weak".to_owned(),
        insert: "weak: ".to_owned(),
        kind: CompletionKind::Param,
        cursor_back: CharCount(0),
        docs: None,
    };
    assert_eq!(completion_preview(&param), vec!["weak: ".to_owned()]);

    let plain = candidate("figure", CompletionKind::Value);
    assert!(completion_preview(&plain).is_empty());

    let mut documented = candidate("figure", CompletionKind::Value);
    documented.docs = Some("A figure.".to_owned());
    assert_eq!(
        completion_preview(&documented),
        vec!["A figure.".to_owned()]
    );

    documented.insert = "figure.caption".to_owned();
    assert_eq!(
        completion_preview(&documented),
        vec!["figure.caption".to_owned(), "A figure.".to_owned()]
    );
}

#[test]
fn non_whitespace_before_bracket_yields_none() {
    assert_eq!(auto_dedent("  foo]", 6), None);
}

#[test]
fn caret_not_after_closing_bracket_yields_none() {
    assert_eq!(auto_dedent("  [", 3), None);
    assert_eq!(auto_dedent("  ]x", 4), None);
    assert_eq!(auto_dedent("", 0), None);
}

#[test]
fn dedent_out_of_range_cursor_yields_none() {
    assert_eq!(auto_dedent("  ]", 999), None);
    assert_eq!(auto_dedent("  ]", usize::MAX), None);
}

#[test]
fn cjk_before_bracket_yields_none() {
    let text = "  你]";
    assert_eq!(auto_dedent(text, text.len()), None);
}

fn candidate(name: &str, kind: CompletionKind) -> CompletionItem {
    CompletionItem::plain(name, kind)
}

#[test]
fn completion_prefix_scans_back_over_word_chars() {
    assert_eq!(
        completion_prefix(&buffer("let foo"), "let foo", 7),
        (4, "foo")
    );
    assert_eq!(completion_prefix(&buffer("let foo"), "let foo", 4), (4, ""));
    assert_eq!(completion_prefix(&buffer("a_b1"), "a_b1", 4), (0, "a_b1"));
    assert_eq!(
        completion_prefix(&buffer("x-lorem"), "x-lorem", 7),
        (2, "lorem")
    );
    assert_eq!(
        completion_prefix(&buffer("#let x_1"), "#let x_1", 8),
        (5, "x_1")
    );
    assert_eq!(
        completion_prefix(&buffer("#"), "#", 1),
        (1, ""),
        "# is a caller-level trigger"
    );
    assert_eq!(completion_prefix(&buffer(""), "", 0), (0, ""));
}

#[test]
fn only_letters_hash_and_dot_open_the_popup() {
    for ch in ['a', 'Z', '#', '.'] {
        assert!(completion_trigger(Some(ch)), "{ch:?}");
    }
    for ch in [' ', '1', '_', '(', '你'] {
        assert!(!completion_trigger(Some(ch)), "{ch:?}");
    }
    assert!(!completion_trigger(None));
}

#[test]
fn comma_opens_only_inside_a_call_slot() {
    assert!(completion_may_open(Some(','), true));
    assert!(!completion_may_open(Some(','), false));
    assert!(completion_may_open(Some('a'), false));
    assert!(!completion_may_open(Some(' '), true));
    assert!(!completion_may_open(None, true));
    // A `.` carries its own prefix-length gate in `refresh_completion`.
    assert!(!completion_may_open(Some('.'), true));
    assert!(!completion_may_open(Some('.'), false));
}

#[test]
fn unfiltered_parameter_list_does_not_own_enter() {
    assert!(!popup_owns_enter(true, false, false));
    assert!(popup_owns_enter(true, true, false));
    assert!(popup_owns_enter(true, false, true));
    assert!(popup_owns_enter(false, false, false));
}

#[test]
fn parse_arrival_opens_the_parameter_list_after_a_keystroke_race() {
    fn input<'a>(generation: u64, call_params: Option<&'a CallParams>) -> EditorInput<'a> {
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            highlights: &[],
            diagnostics: &[],
            blocked_ranges: &[],
            code_ranges: &[],
            math_ranges: &[],
            math_atoms: &[],
            spell_issues: &[],
            debug_highlights: &[],
            debug_highlight_color: egui::Color32::TRANSPARENT,
            folds: &[],
            list_lines: &[],
            completions: &[],
            definitions: &[],
            world_completions: &[],
            completion_max: crate::COMPLETION_MAX_DEFAULT,
            parse_generation: generation,
            call_params,
            font_family: "sans",
            font_size: 14.0,
            wrap_mode: WrapMode::Off,
            line_spacing: 1.0,
            wrap_line_spacing: 1.0,
            plain_text: false,
            request_focus: false,
            blame_inline: None,
        }
    }

    let mut state = EditorState::new();
    state.set_buffer_text("#figure(a, ");
    let call = CallParams {
        range: BytePos(7)..BytePos(11),
        items: vec![CompletionItem {
            name: "placement".to_owned(),
            insert: "placement: ".to_owned(),
            kind: CompletionKind::Param,
            cursor_back: CharCount(0),
            docs: None,
        }],
        value_slot: None,
    };

    // The comma keystroke ran against a parse that predates the `(`: no
    // call resolves yet, so nothing may open.
    state.refresh_completion(11, &input(1, None), CompletionGate::Keystroke(Some(',')));
    assert!(state.completion.is_none());

    // The fresh parse arrives: the unfiltered parameter list opens.
    state.refresh_completion(11, &input(2, Some(&call)), CompletionGate::Parse);
    let popup = state.completion.as_ref().expect("parameter list opens");
    assert_eq!(popup.items.len(), 1);
    assert_eq!(popup.items[0].name, "placement");
    assert!(popup.params);

    // A parse arrival outside a call opens nothing.
    state.completion = None;
    state.refresh_completion(11, &input(3, None), CompletionGate::Parse);
    assert!(state.completion.is_none());

    // A typed prefix keeps a parse arrival from opening either.
    state.set_buffer_text("#figure(a, x");
    state.refresh_completion(13, &input(4, Some(&call)), CompletionGate::Parse);
    assert!(state.completion.is_none());
}

#[test]
fn completion_prefix_scans_dotted_paths() {
    assert_eq!(
        completion_prefix(&buffer("grid.cel"), "grid.cel", 8),
        (0, "grid.cel")
    );
    assert_eq!(
        completion_prefix(&buffer("grid."), "grid.", 5),
        (0, "grid.")
    );
    assert_eq!(
        completion_prefix(&buffer("#grid.cel"), "#grid.cel", 9),
        (1, "grid.cel")
    );
    assert_eq!(
        completion_prefix(&buffer("grid.cell"), "grid.cell", 9),
        (0, "grid.cell")
    );
    assert_eq!(completion_prefix(&buffer(".foo"), ".foo", 4), (0, ".foo"));
}

#[test]
fn completion_prefix_stops_at_cjk() {
    assert_eq!(completion_prefix(&buffer("你好ab"), "你好ab", 4), (2, "ab"));
    assert_eq!(completion_prefix(&buffer("你好"), "你好", 2), (2, ""));
    assert_eq!(completion_prefix(&buffer("abc你好"), "abc你好", 5), (5, ""));
}

#[test]
fn hash_before_the_word_counts_as_code_even_without_ranges() {
    // While typing `#a` the parse result still describes the `#` alone, so
    // `code_ranges` is empty; `#` starts logic mode regardless.
    let (word_start, _) = completion_prefix(&buffer("#a"), "#a", 2);
    assert_eq!(word_start, 1);
    assert!(word_after_hash(&buffer("#a"), word_start));
    assert_eq!(
        completion_context(&buffer("#a"), word_start, &[], &[]),
        CompletionContext::Code
    );
    // The caret directly after `#` is the start of logic mode too.
    assert_eq!(
        completion_context(&buffer("#"), 1, &[], &[]),
        CompletionContext::Code
    );
    // Plain markup stays markup, and math only completes in equations.
    assert_eq!(
        completion_context(&buffer("ab"), 0, &[], &[]),
        CompletionContext::Markup
    );
    assert_eq!(
        completion_context(&buffer("$ab"), 1, &[], std::slice::from_ref(&(1..3))),
        CompletionContext::Math
    );
    // Real ranges keep deciding once the parse catches up.
    assert_eq!(
        completion_context(&buffer("#let x"), 5, std::slice::from_ref(&(5..6)), &[]),
        CompletionContext::Code
    );
}

#[test]
fn completion_context_stays_code_inside_content_blocks() {
    for marked in ["#grid.|", "#box[#grid.|]"] {
        let cursor = marked.find('|').expect("marker");
        let text = marked.replacen('|', "", 1);
        let parsed = typ_edit_engine::parse::run(&text, cursor);
        let (word_start, _) = completion_prefix(&buffer(&text), &text, cursor);
        assert_eq!(
            completion_context(
                &buffer(&text),
                word_start,
                &plain_ranges(&parsed.layout.code_ranges),
                &plain_ranges(&parsed.layout.math_ranges)
            ),
            CompletionContext::Code,
            "{marked:?}"
        );
    }
}

#[test]
fn caret_in_call_covers_the_open_argument_list() {
    let call = CallParams {
        range: BytePos(7)..BytePos(12),
        items: Vec::new(),
        value_slot: None,
    };
    assert!(caret_in_call(&call, 7), "the opening delimiter");
    assert!(caret_in_call(&call, 10), "inside the list");
    assert!(caret_in_call(&call, 12), "at the closing delimiter");
    assert!(
        caret_in_call(&call, 13),
        "a trailing-space caret sits past `range.end` yet is in the call"
    );
    assert!(!caret_in_call(&call, 6), "before the call is outside");
}

#[test]
fn call_slot_items_merges_params_and_expressions() {
    let call = CallParams {
        range: BytePos(7)..BytePos(12),
        items: vec![
            candidate("align", CompletionKind::Param),
            candidate("rowspan", CompletionKind::Param),
        ],
        value_slot: None,
    };
    let definitions = vec![candidate("rowspan", CompletionKind::Local)];
    let library = vec![
        candidate("align", CompletionKind::Value),
        candidate("grid.cell", CompletionKind::Value),
    ];

    let cap = crate::COMPLETION_MAX_DEFAULT;
    let params = call_slot_items("ro", &call, &definitions, &library, cap);
    let names: Vec<&str> = params.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["rowspan"]);
    assert_eq!(params[0].kind, CompletionKind::Param);

    let dotted = call_slot_items("grid.", &call, &definitions, &library, cap);
    let names: Vec<&str> = dotted.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["grid.cell"]);
    assert_eq!(dotted[0].kind, CompletionKind::Value);

    let merged = call_slot_items("ali", &call, &definitions, &library, cap);
    let names: Vec<&str> = merged.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["align"], "a parameter shadows the library entry");
    assert_eq!(merged[0].kind, CompletionKind::Param);
}

/// A `#rect(...)` call whose caret sits in the `fill` value region.
fn fill_value_call() -> CallParams {
    CallParams {
        range: BytePos(5)..BytePos(12),
        items: vec![
            CompletionItem {
                name: "fill".to_owned(),
                insert: "fill: ".to_owned(),
                kind: CompletionKind::Param,
                cursor_back: CharCount(0),
                docs: None,
            },
            candidate("stroke", CompletionKind::Param),
        ],
        value_slot: Some(ValueSlot {
            param: "fill".to_owned(),
            items: vec![
                candidate("rgb()", CompletionKind::Value),
                candidate("red", CompletionKind::Value),
            ],
        }),
    }
}

#[test]
fn empty_value_slot_shows_only_the_type_constrained_candidates() {
    let call = fill_value_call();
    let mut state = EditorState::new();
    state.set_buffer_text("#rect(fill: ");
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        call_params: Some(&call),
        ..empty_input()
    };
    state.refresh_completion(12, &input, CompletionGate::Parse);

    let popup = state.completion.as_ref().expect("the value slot opens");
    let names: Vec<&str> = popup.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(
        names,
        ["rgb()", "red"],
        "slot candidates replace the parameter names"
    );
    assert!(popup.params, "an unfiltered value list stays informational");
}

#[test]
fn typed_prefix_filters_only_the_value_slot_candidates() {
    let call = fill_value_call();
    let definitions = vec![candidate("rect", CompletionKind::Local)];
    let library = vec![
        candidate("rect", CompletionKind::Value),
        candidate("reset", CompletionKind::Value),
    ];
    let mut state = EditorState::new();
    state.set_buffer_text("#rect(fill: r");
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        completions: &library,
        definitions: &definitions,
        call_params: Some(&call),
        ..empty_input()
    };
    state.refresh_completion(13, &input, CompletionGate::Keystroke(Some('r')));

    let popup = state.completion.as_ref().expect("the value slot opens");
    let names: Vec<&str> = popup.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["rgb()", "red"], "only slot candidates matching `r`");
    assert!(!names.contains(&"rect"), "no document definitions");
    assert!(!names.contains(&"reset"), "no library candidates");
}

#[test]
fn absent_value_slot_keeps_the_parameter_list() {
    let call = CallParams {
        range: BytePos(5)..BytePos(12),
        items: vec![candidate("fill", CompletionKind::Param)],
        value_slot: None,
    };
    let mut state = EditorState::new();
    state.set_buffer_text("#rect(fill: ");
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        call_params: Some(&call),
        ..empty_input()
    };
    state.refresh_completion(12, &input, CompletionGate::Parse);

    let popup = state.completion.as_ref().expect("the parameter list opens");
    let names: Vec<&str> = popup.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["fill"]);
}

#[test]
fn dot_after_a_dotted_head_opens_member_completion() {
    let library = vec![
        candidate("cell.x", CompletionKind::Value),
        candidate("cell.colspan", CompletionKind::Value),
        candidate("figure", CompletionKind::Value),
    ];
    let mut state = EditorState::new();
    state.set_buffer_text("#cell.");
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        completions: &library,
        ..empty_input()
    };
    state.refresh_completion(6, &input, CompletionGate::Keystroke(Some('.')));

    let popup = state.completion.as_ref().expect("dotted members open");
    let names: Vec<&str> = popup.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["cell.x", "cell.colspan"]);
    assert!(!popup.params);
}

#[test]
fn a_lone_dot_does_not_open_a_closed_popup() {
    let library = vec![candidate("cell.x", CompletionKind::Value)];
    let mut state = EditorState::new();
    state.set_buffer_text("#.");
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        completions: &library,
        ..empty_input()
    };
    state.refresh_completion(2, &input, CompletionGate::Keystroke(Some('.')));
    assert!(state.completion.is_none(), "a lone `.` never opens");
}

#[test]
fn markup_dot_does_not_offer_dotted_static_candidates() {
    let library = vec![
        candidate("cell.x", CompletionKind::Value),
        candidate("cell.colspan", CompletionKind::Value),
    ];
    let mut state = EditorState::new();
    state.set_buffer_text("hello.");
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        completions: &library,
        ..empty_input()
    };
    state.refresh_completion(6, &input, CompletionGate::Keystroke(Some('.')));
    assert!(
        state.completion.is_none(),
        "a half-width `.` in markup never opens a popup"
    );
}

#[test]
fn dotted_static_candidates_exclude_non_sub_items() {
    let library = vec![
        candidate("cell.x", CompletionKind::Value),
        candidate("cell.colspan", CompletionKind::Value),
        candidate("grid.cell.x", CompletionKind::Value),
        candidate("table.cell", CompletionKind::Value),
    ];
    let mut state = EditorState::new();
    state.set_buffer_text("#cell.");
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        completions: &library,
        ..empty_input()
    };
    state.refresh_completion(6, &input, CompletionGate::Keystroke(Some('.')));

    let popup = state.completion.as_ref().expect("dotted members open");
    let names: Vec<&str> = popup.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["cell.x", "cell.colspan"]);
    assert!(
        !names.contains(&"grid.cell.x"),
        "a name merely containing `cell.` is not a sub-item"
    );
    assert!(!names.contains(&"table.cell"), "no unrelated heads");
}

#[test]
fn math_completion_offers_symbol_cases_and_dotted_variants() {
    let library = typ_edit_engine::completions::library_completions();

    let ranked = rank_completions("delta", &[], &library, true, 50);
    assert!(ranked.iter().any(|item| item.name == "delta"));
    assert!(ranked.iter().any(|item| item.name == "Delta"));

    for (text, expected) in [("$eq.$", "eq.ast"), ("$plus.$", "plus.minus")] {
        let cursor = text.chars().count() - 1;
        let parsed = typ_edit_engine::parse::run(text, cursor);
        let mut state = EditorState::new();
        state.set_buffer_text(text);
        let input = EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &library,
            math_ranges: &parsed.layout.math_ranges,
            ..empty_input()
        };
        state.refresh_completion(cursor, &input, CompletionGate::Keystroke(Some('.')));
        let popup = state
            .completion
            .as_ref()
            .unwrap_or_else(|| panic!("{text:?} opens dotted math symbols"));
        let item = popup
            .items
            .iter()
            .find(|item| item.name == expected)
            .unwrap_or_else(|| panic!("{text:?} offers {expected:?}"));
        assert_eq!(item.insert, expected, "the full dotted name is inserted");
        assert_eq!(item.cursor_back, CharCount(0));
    }
}

#[test]
fn show_target_dot_offers_where_and_where_params_still_resolve() {
    let library = typ_edit_engine::completions::library_completions();
    let text = "#show heading.";
    let cursor = text.chars().count();
    let parsed = typ_edit_engine::parse::run(text, cursor);
    let mut state = EditorState::new();
    state.set_buffer_text(text);
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        completions: &library,
        code_ranges: &parsed.layout.code_ranges,
        ..empty_input()
    };
    state.refresh_completion(cursor, &input, CompletionGate::Keystroke(Some('.')));
    let popup = state
        .completion
        .as_ref()
        .expect("the show target completes");
    assert!(
        popup.items.iter().any(|item| item.name == "heading.where"),
        "{:?}",
        popup
            .items
            .iter()
            .map(|item| &item.name)
            .collect::<Vec<_>>()
    );

    let call = typ_edit_engine::parse::run("#show heading.where(", 20)
        .ide
        .call_params
        .expect("`where(` params resolve");
    assert!(call.items.iter().any(|item| item.name == "level"));
}

/// The screen-space rect of the painted completion popup, if it was shown.
fn completion_popup_rect(ctx: &egui::Context) -> Option<egui::Rect> {
    ctx.memory(|memory| memory.area_rect(egui::Id::new(COMPLETION_POPUP_ID)))
}

/// The screen-space rect of the selected candidate's detail tooltip, if shown.
fn completion_tooltip_rect(ctx: &egui::Context) -> Option<egui::Rect> {
    ctx.memory(|memory| memory.area_rect(egui::Id::new(COMPLETION_TOOLTIP_ID).with(0)))
}

#[test]
fn completion_popup_height_is_stable_across_selection() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);

    let completions = vec![
        CompletionItem::plain("beta", CompletionKind::Value),
        CompletionItem {
            name: "alpha".to_owned(),
            insert: "alpha".to_owned(),
            kind: CompletionKind::Function,
            cursor_back: CharCount(0),
            docs: Some("alpha documentation".to_owned()),
        },
    ];
    let input = || EditorInput {
        collab_history: false,
        remote_carets: &[],
        completions: &completions,
        ..empty_input()
    };
    state.refresh_completion(1, &input(), CompletionGate::Manual);
    assert_eq!(state.completion.as_ref().expect("open").selected, 0);

    vframe_in(&ctx, &mut state, vec![], egui::Modifiers::NONE, input());
    let before = completion_popup_rect(&ctx).expect("popup painted").height();
    vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::ArrowDown)],
        egui::Modifiers::NONE,
        input(),
    );
    vframe_in(&ctx, &mut state, vec![], egui::Modifiers::NONE, input());
    let after = completion_popup_rect(&ctx).expect("popup painted").height();
    assert_eq!(state.completion.as_ref().expect("open").selected, 1);
    assert!(
        (before - after).abs() < 0.01,
        "selection must not resize the popup: {before} -> {after}"
    );
}

#[test]
fn completion_tooltip_only_for_items_with_detail() {
    for (docs, expected) in [(Some("documented candidate"), true), (None, false)] {
        let ctx = egui::Context::default();
        bind_test_fonts(&ctx);
        let mut state = EditorState::new();
        focus_virtual(&ctx, &mut state, "#", 1, false);

        let completions = vec![CompletionItem {
            name: "alpha".to_owned(),
            insert: "alpha".to_owned(),
            kind: CompletionKind::Function,
            cursor_back: CharCount(0),
            docs: docs.map(str::to_owned),
        }];
        let input = || EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        };
        state.refresh_completion(1, &input(), CompletionGate::Manual);
        vframe_in(&ctx, &mut state, vec![], egui::Modifiers::NONE, input());
        assert_eq!(
            completion_tooltip_rect(&ctx).is_some(),
            expected,
            "docs = {docs:?}"
        );
    }
}

#[test]
fn caret_move_without_edit_dismisses_the_popup() {
    let popup = || CompletionPopup {
        items: vec![candidate("pagebreak", CompletionKind::Value)],
        selected: 0,
        word_start: 1,
        scroll_offset: 0.0,
        was_scrolling: false,
        params: false,
        navigated: false,
    };
    let mut state = EditorState::new();
    state.set_buffer_text("#pa");

    // A caret move (same text) closes the popup and guards the new position.
    state.completion = Some(popup());
    state.dismiss_if_caret_moved(Some(1), 3, false);
    assert!(state.completion.is_none());
    assert_eq!(state.dismissed_at, Some((3, 3)));

    // Typing is not a move: the popup stays for the refresh that follows.
    state.dismissed_at = None;
    state.completion = Some(popup());
    state.dismiss_if_caret_moved(Some(1), 3, true);
    assert!(state.completion.is_some());
    assert_eq!(state.dismissed_at, None);

    // List navigation (Up/Down) leaves the caret in place: no dismissal.
    state.completion = Some(popup());
    state.dismiss_if_caret_moved(Some(3), 3, false);
    assert!(state.completion.is_some());

    // A closed popup records no guard for a later parse result.
    state.completion = None;
    state.dismissed_at = None;
    state.dismiss_if_caret_moved(Some(1), 3, false);
    assert!(state.completion.is_none());
    assert_eq!(state.dismissed_at, None);
}

#[test]
fn rank_puts_starts_with_before_contains() {
    let definitions = vec![candidate("set", CompletionKind::Local)];
    let library = vec![
        candidate("reset", CompletionKind::Value),
        candidate("set", CompletionKind::Value),
        candidate("setup", CompletionKind::Value),
    ];
    let ranked = rank_completions("set", &definitions, &library, false, 50);
    let names: Vec<&str> = ranked.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["set", "setup", "reset"]);
    assert_eq!(
        ranked[0].kind,
        CompletionKind::Local,
        "a definition must win the name duplicate"
    );
}

#[test]
fn rank_matches_case_insensitively() {
    let library = vec![candidate("SIT", CompletionKind::Value)];
    let ranked = rank_completions("si", &[], &library, false, 50);
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].name, "SIT");
}

#[test]
fn rank_caps_the_result_and_ignores_empty_prefixes() {
    let library = vec![
        candidate("aa", CompletionKind::Value),
        candidate("ab", CompletionKind::Value),
        candidate("ac", CompletionKind::Value),
    ];
    assert_eq!(rank_completions("a", &[], &library, false, 2).len(), 2);
    assert!(rank_completions("", &[], &library, false, 50).is_empty());
    assert!(rank_completions("a", &[], &library, false, 0).is_empty());

    let definitions = vec![candidate("AB", CompletionKind::Local)];
    let ranked = rank_completions("ab", &definitions, &library, false, 50);
    assert_eq!(ranked.len(), 1, "dedup is case-insensitive");
    assert_eq!(ranked[0].kind, CompletionKind::Local);
}

#[test]
fn rank_filters_by_context() {
    let library = vec![
        candidate("alpha", CompletionKind::Symbol),
        candidate("align", CompletionKind::Value),
    ];
    let code = rank_completions("al", &[], &library, false, 50);
    let names: Vec<&str> = code.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["align"], "math symbols stay out of code completion");
    let math = rank_completions("al", &[], &library, true, 50);
    let names: Vec<&str> = math.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["alpha"], "code names stay out of math completion");
}

#[test]
fn first_match_at_or_after_picks_the_nearest_match() {
    let matches = vec![0..2, 5..7, 10..12];
    assert_eq!(first_match_at_or_after(&matches, 0), Some(0));
    assert_eq!(first_match_at_or_after(&matches, 1), Some(1));
    assert_eq!(first_match_at_or_after(&matches, 5), Some(1));
    assert_eq!(first_match_at_or_after(&matches, 6), Some(2));
    assert_eq!(first_match_at_or_after(&matches, 10), Some(2));
    assert_eq!(first_match_at_or_after(&matches, 11), None);
    assert_eq!(first_match_at_or_after(&matches, 12), None);
    assert_eq!(first_match_at_or_after(&[], 0), None);
}

#[test]
fn step_match_wraps_in_both_directions() {
    assert_eq!(step_match(0, 3, 1), 1);
    assert_eq!(step_match(2, 3, 1), 0);
    assert_eq!(step_match(0, 3, -1), 2);
    assert_eq!(step_match(1, 3, -1), 0);
    assert_eq!(step_match(4, 5, 3), 2);
    assert_eq!(step_match(0, 1, 1), 0);
    assert_eq!(step_match(0, 0, 1), 0);
    assert_eq!(step_match(0, 0, -1), 0);
}

fn viewport() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(200.0, 300.0))
}

#[test]
fn drag_autoscroll_is_idle_inside_the_viewport() {
    let viewport = viewport();
    assert_eq!(drag_autoscroll_delta(250.0, viewport, 1.0 / 60.0), 0.0);
    assert_eq!(drag_autoscroll_delta(100.0, viewport, 1.0 / 60.0), 0.0);
    assert_eq!(drag_autoscroll_delta(400.0, viewport, 1.0 / 60.0), 0.0);
}

#[test]
fn drag_autoscroll_direction_follows_the_overshoot_side() {
    let viewport = viewport();
    assert!(drag_autoscroll_delta(450.0, viewport, 1.0 / 60.0) > 0.0);
    assert!(drag_autoscroll_delta(50.0, viewport, 1.0 / 60.0) < 0.0);
}

#[test]
fn drag_autoscroll_grows_with_overshoot_and_is_capped() {
    let viewport = viewport();
    let near = drag_autoscroll_delta(401.0, viewport, 0.01);
    let far = drag_autoscroll_delta(501.0, viewport, 0.01);
    assert!(far > near, "farther outside must scroll faster");
    let capped = drag_autoscroll_delta(1_000_000.0, viewport, 1.0);
    assert_eq!(capped, DRAG_SCROLL_MAX_SPEED * 0.1, "dt is clamped to 0.1s");
}

fn unwrapped_galley(text: &str) -> std::sync::Arc<egui::Galley> {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut galley = None;
    ctx.run_ui(Default::default(), |ui| {
        let job = egui::text::LayoutJob::simple(
            text.to_owned(),
            egui::FontId::new(14.0, egui::FontFamily::Monospace),
            egui::Color32::WHITE,
            f32::INFINITY,
        );
        galley = Some(ui.fonts_mut(|fonts| fonts.layout_job(job)));
    })
    .drop_without_applying_deltas();
    galley.expect("a galley")
}

fn key_event(k: egui::Key) -> egui::Event {
    key_event_with(egui::Modifiers::NONE, k)
}

fn key_event_with(modifiers: egui::Modifiers, k: egui::Key) -> egui::Event {
    egui::Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// Like [`drive_editor_frame`] but returns the frame's [`EditorOutput`].
fn drive_editor_frame_out(
    ctx: &egui::Context,
    state: &mut EditorState,
    list_lines: &[BytePos],
    events: Vec<egui::Event>,
    request_focus: bool,
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.list_lines = list_lines;
        input.request_focus = request_focus;
        out = Some(state.show(ui, input));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

fn drive_editor_frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    list_lines: &[BytePos],
    events: Vec<egui::Event>,
    request_focus: bool,
) {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events,
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.list_lines = list_lines;
        input.request_focus = request_focus;
        let _ = state.show(ui, input);
    })
    .drop_without_applying_deltas();
}

#[test]
fn tab_indents_an_empty_list_item_in_a_frame() {
    let source = "- ";
    let parse = typ_edit_engine::parse::run(source, source.len());
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_file(std::path::PathBuf::from("probe.typ"), source.to_owned());
    state.jump_to_byte(source.len());
    drive_editor_frame(&ctx, &mut state, &parse.layout.list_lines, vec![], true);
    // egui registers the widget's Tab focus filter only on the first frame it
    // is actually focused, so a steady frame must precede the Tab.
    drive_editor_frame(&ctx, &mut state, &parse.layout.list_lines, vec![], false);
    drive_editor_frame(
        &ctx,
        &mut state,
        &parse.layout.list_lines,
        vec![key_event(egui::Key::Tab)],
        false,
    );
    assert_eq!(state.shared_text().as_str(), "  - ");
}

#[test]
fn enter_indents_only_after_unclosed_inline_math_in_a_frame() {
    for (source, expected) in [
        ("text $x = 1$", "text $x = 1$\n"),
        ("text $x = 1", "text $x = 1\n  "),
    ] {
        let parse = typ_edit_engine::parse::run(source, source.len());
        let ctx = egui::Context::default();
        bind_test_fonts(&ctx);
        let mut state = EditorState::new();
        state.set_file(std::path::PathBuf::from("probe.typ"), source.to_owned());
        state.jump_to_byte(source.len());
        drive_editor_frame(&ctx, &mut state, &parse.layout.list_lines, vec![], true);
        drive_editor_frame(
            &ctx,
            &mut state,
            &parse.layout.list_lines,
            vec![key_event(egui::Key::Enter)],
            false,
        );
        assert_eq!(state.shared_text().as_str(), expected, "source {source:?}");
    }
}

#[test]
fn shared_text_tracks_the_buffer_through_scripted_cjk_edits() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_file(std::path::PathBuf::from("probe.typ"), String::new());

    state.jump_to_byte(0);
    drive_editor_frame(&ctx, &mut state, &[], vec![], true);
    drive_editor_frame(&ctx, &mut state, &[], vec![], false);

    let snapshot = |state: &EditorState| state.buffer.slice(0..state.buffer.len_chars());
    assert_eq!(state.shared_text().as_str(), snapshot(&state).as_str());

    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![egui::Event::Text("你".to_owned())],
        false,
    );
    assert_eq!(
        state.shared_text().as_str(),
        snapshot(&state).as_str(),
        "the accessor must match the rope after a CJK insert"
    );

    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![key_event(egui::Key::Enter)],
        false,
    );
    assert_eq!(state.shared_text().as_str(), "你\n");
    assert_eq!(state.shared_text().as_str(), snapshot(&state).as_str());
    assert_eq!(state.buffer.len_chars(), 2);
    assert_eq!(state.line_count(), 2);
}

#[test]
fn set_text_clears_undo_history_and_keeps_hooks() {
    let mut state = EditorState::new();
    state.set_buffer_text("abc");
    state.edit_bytes(0..0, "x");
    assert!(state.can_undo(), "the edit is undoable");
    assert_eq!(state.shared_text().as_str(), "xabc");

    state.set_text("hello".to_owned());

    assert!(!state.can_undo(), "a fresh load drops undo history");
    assert!(!state.can_redo(), "and redo history too");
    assert_eq!(
        state.buffer.hook_count(),
        4,
        "the four hooks survive reload"
    );
    assert_eq!(state.shared_text().as_str(), "hello");
}

#[test]
fn undo_restores_shared_text() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "ab", 2, false);
    assert_eq!(state.shared_text().as_str(), "ab");

    vframe(&ctx, &mut state, vec![egui::Event::Text("c".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "abc");

    undo_frame(&ctx, &mut state);
    assert_eq!(
        state.shared_text().as_str(),
        "ab",
        "undo must invalidate the rev-keyed text cache"
    );
}

#[test]
fn shared_text_tracks_the_buffer_through_replace_all() {
    let mut state = EditorState::new();
    state.set_buffer_text("你好 world\nfoo world");

    state.find.open = true;
    state.find.query = "world".to_owned();
    state.find.replacement = "世界".to_owned();
    state.refresh_find_matches();
    assert_eq!(state.find.matches.len(), 2, "the fixture must match twice");

    assert!(state.replace_all_matches());
    assert_eq!(state.shared_text().as_str(), "你好 世界\nfoo 世界");
    assert_eq!(
        state.buffer.len_chars(),
        state.shared_text().chars().count()
    );
}

#[test]
fn replace_all_text_keeps_history_and_one_undo_restores_the_previous_text() {
    let mut state = EditorState::new();
    state.set_buffer_text("#let x=1");
    // A real edit opens an undo group; the whole-buffer replace must open its
    // own group rather than merge into it.
    state.edit_bytes(0..0, "# ");
    assert!(state.can_undo());

    state.replace_all_text("# let x = 1\n");

    assert_eq!(state.shared_text().as_str(), "# let x = 1\n");
    assert!(state.can_undo(), "the formatter keeps the undo history");
    assert!(state.buffer.undo());
    assert_eq!(
        state.shared_text().as_str(),
        "# #let x=1",
        "one undo restores the text before the replace, not before the edit"
    );
}

#[test]
fn replace_all_text_with_identical_text_is_a_noop() {
    let mut state = EditorState::new();
    state.set_buffer_text("same");
    let rev_before = state.buffer.rev();

    state.replace_all_text("same");

    assert_eq!(state.buffer.rev(), rev_before, "no revision bump");
    assert!(!state.can_undo(), "no spurious undo entry");
    assert_eq!(state.shared_text().as_str(), "same");
}

#[test]
fn replace_all_text_reanchors_the_caret_through_hooks() {
    let mut state = EditorState::new();
    state.set_buffer_text("hello world");
    state.virtual_text.set_caret(5);

    // An append after the caret leaves it in place.
    state.replace_all_text("hello world!");
    assert_eq!(state.virtual_text.caret(), 5);
    assert_eq!(state.shared_text().as_str(), "hello world!");

    // A change before the caret shifts it but keeps it in bounds.
    state.replace_all_text("HELLO world!");
    let caret = state.virtual_text.caret();
    let len = state.shared_text().chars().count();
    assert!(caret <= len, "caret {caret} clamps to the new length {len}");
}

/// Warms the editor so it owns egui focus (the undo keys are consumed only
/// while focused).
fn warm_focused_editor(ctx: &egui::Context, state: &mut EditorState, source: &str) {
    state.set_file(std::path::PathBuf::from("probe.typ"), source.to_owned());
    state.jump_to_byte(0);
    drive_editor_frame(ctx, state, &[], vec![], true);
    drive_editor_frame(ctx, state, &[], vec![], false);
}

#[test]
fn buffer_undo_reverts_a_typed_run_in_one_group() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    warm_focused_editor(&ctx, &mut state, "");

    for ch in ["a", "b", "c"] {
        drive_editor_frame(
            &ctx,
            &mut state,
            &[],
            vec![egui::Event::Text(ch.to_owned())],
            false,
        );
    }
    assert_eq!(state.shared_text().as_str(), "abc");

    // The whole run is one buffer group: a single undo removes all three.
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Z)],
        false,
    );
    assert_eq!(state.shared_text().as_str(), "");

    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Y)],
        false,
    );
    assert_eq!(state.shared_text().as_str(), "abc");
}

#[test]
fn undo_at_the_start_is_a_noop() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    warm_focused_editor(&ctx, &mut state, "hello");

    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Z)],
        false,
    );
    assert_eq!(state.shared_text().as_str(), "hello");
}

#[test]
fn undo_restores_a_selection_replaced_by_typing() {
    let source = "hello world";
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_file(std::path::PathBuf::from("probe.typ"), source.to_owned());
    state.jump_to_byte(source.len());
    drive_editor_frame(&ctx, &mut state, &[], vec![], true);
    drive_editor_frame(&ctx, &mut state, &[], vec![], false);

    // Select all, then replace the selection with one char in a single frame
    // (egui deletes the selection and inserts the char).
    state.queue_command(EditorCommand::SelectAll);
    drive_editor_frame(&ctx, &mut state, &[], vec![], false);
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![egui::Event::Text("X".to_owned())],
        false,
    );
    assert_eq!(state.shared_text().as_str(), "X");

    // One undo restores the whole selected text.
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Z)],
        false,
    );
    assert_eq!(state.shared_text().as_str(), source);
}

#[test]
fn typing_a_pair_wraps_the_selected_text() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_file(std::path::PathBuf::from("probe.typ"), "abc".to_owned());
    state.jump_to_byte(3);
    drive_editor_frame(&ctx, &mut state, &[], vec![], true);
    drive_editor_frame(&ctx, &mut state, &[], vec![], false);

    state.queue_command(EditorCommand::SelectAll);
    drive_editor_frame(&ctx, &mut state, &[], vec![], false);
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![egui::Event::Text("(".to_owned())],
        false,
    );
    assert_eq!(state.shared_text().as_str(), "(abc)");
}

#[test]
fn caret_stays_in_bounds_after_undo() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    warm_focused_editor(&ctx, &mut state, "");

    for _ in 0..3 {
        drive_editor_frame(
            &ctx,
            &mut state,
            &[],
            vec![egui::Event::Text("你".to_owned())],
            false,
        );
    }
    assert_eq!(state.shared_text().as_str(), "你你你");

    let output = drive_editor_frame_out(
        &ctx,
        &mut state,
        &[],
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Z)],
        false,
    );
    assert!(output.cursor_byte.0 <= state.shared_text().len());
}

#[test]
fn virtual_text_path_paints_and_reports_visible_lines() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_buffer_text("你好世界这是一段文本\nsecond line\nthird line");

    // The first frame measures the viewport; later frames paint from the cache.
    for _ in 0..3 {
        drive_editor_frame(&ctx, &mut state, &[], vec![], false);
    }

    let visible = state.virtual_text_visible_lines();
    assert!(
        !visible.is_empty(),
        "the virtual path must report a non-empty visible range"
    );
    assert!(visible.end <= state.line_count());
}

// ---- Stage B: virtual-mode pointer interaction ---------------------------

fn pointer_button(pos: egui::Pos2, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    }
}

fn pointer_moved(pos: egui::Pos2) -> egui::Event {
    egui::Event::PointerMoved(pos)
}

fn numbered_lines(count: usize) -> String {
    (0..count)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn start_virtual(state: &mut EditorState, text: &str) {
    state.set_buffer_text(text);
}

fn virtual_viewport(state: &EditorState) -> egui::Rect {
    state
        .virtual_text
        .test_viewport
        .expect("the virtual path records its viewport")
}

/// A point 2pt inside the left edge of visible `line`, at the row center.
fn virtual_line_center(state: &EditorState, line: usize) -> egui::Pos2 {
    let viewport = virtual_viewport(state);
    let (_, y, height) = state
        .virtual_text
        .test_line_origins
        .iter()
        .copied()
        .find(|(index, _, _)| *index == line)
        .expect("the line is visible");
    egui::pos2(viewport.left() + 2.0, y + height * 0.5)
}

fn warm_virtual(ctx: &egui::Context, state: &mut EditorState) {
    for _ in 0..3 {
        drive_editor_frame(ctx, state, &[], vec![], false);
    }
}

/// egui 0.36 removed `RawInput::modifiers`; inject the frame's modifier state
/// as a leading `ModifiersChanged` event so `InputState::modifiers` reflects it.
/// egui reads Ctrl/Shift/Command from that state, not from pointer events.
fn with_modifiers(mut events: Vec<egui::Event>, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    events.insert(0, egui::Event::ModifiersChanged(modifiers));
    events
}

/// Runs one virtual-mode frame with explicit frame modifiers.
fn drive_virtual_frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events: with_modifiers(events, modifiers),
        ..Default::default()
    };
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        out = Some(state.show(ui, empty_input()));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

/// Press + release at `pos`, returning the release frame's output.
fn virtual_click(
    ctx: &egui::Context,
    state: &mut EditorState,
    pos: egui::Pos2,
    modifiers: egui::Modifiers,
) -> EditorOutput {
    drive_virtual_frame(
        ctx,
        state,
        vec![pointer_moved(pos), pointer_button(pos, true, modifiers)],
        modifiers,
    );
    drive_virtual_frame(
        ctx,
        state,
        vec![pointer_button(pos, false, modifiers)],
        modifiers,
    )
}

#[test]
fn virtual_text_click_sets_caret_and_focus() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "你好 world\nsecond line");
    warm_virtual(&ctx, &mut state);

    let pos0 = virtual_line_center(&state, 0);
    let out = virtual_click(&ctx, &mut state, pos0, egui::Modifiers::NONE);
    assert_eq!(state.virtual_text.caret(), 0);
    assert_eq!(state.virtual_text.selection_range(), None);
    assert_eq!(out.cursor_byte, BytePos(0));

    // The click focused the widget; a steady frame settles focus memory.
    drive_editor_frame(&ctx, &mut state, &[], vec![], false);
    let id = egui::Id::new(("typ-edit-virtual-text", state.serial));
    assert!(
        ctx.memory(|m| m.has_focus(id)),
        "the click focuses the widget"
    );

    let pos1 = virtual_line_center(&state, 1);
    let line1 = state.buffer.line_start(1);
    let out = virtual_click(&ctx, &mut state, pos1, egui::Modifiers::NONE);
    assert_eq!(state.virtual_text.caret(), line1);
    assert_eq!(out.cursor_byte, BytePos(state.buffer.char_to_byte(line1)));
}

#[test]
fn virtual_text_drag_selects_the_expected_range() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "abc\ndef\nghi");
    warm_virtual(&ctx, &mut state);

    let start = virtual_line_center(&state, 0);
    let end = virtual_line_center(&state, 1);
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![
            pointer_moved(start),
            pointer_button(start, true, egui::Modifiers::NONE),
        ],
        false,
    );
    for _ in 0..2 {
        drive_editor_frame(&ctx, &mut state, &[], vec![pointer_moved(end)], false);
    }
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![pointer_button(end, false, egui::Modifiers::NONE)],
        false,
    );

    assert_eq!(
        state.virtual_text.selection_range(),
        Some(0..state.buffer.line_start(1))
    );
}

#[test]
fn virtual_text_double_click_selects_a_word() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "hello world\nsecond");
    warm_virtual(&ctx, &mut state);

    let pos = virtual_line_center(&state, 0);
    virtual_click(&ctx, &mut state, pos, egui::Modifiers::NONE);
    virtual_click(&ctx, &mut state, pos, egui::Modifiers::NONE);

    assert_eq!(state.virtual_text.selection_range(), Some(0..5));
}

#[test]
fn virtual_text_triple_click_selects_a_line() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "hello world\nsecond");
    warm_virtual(&ctx, &mut state);

    let pos = virtual_line_center(&state, 0);
    for _ in 0..3 {
        virtual_click(&ctx, &mut state, pos, egui::Modifiers::NONE);
    }

    assert_eq!(state.virtual_text.selection_range(), Some(0..11));
}

#[test]
fn virtual_text_shift_click_extends_from_the_previous_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "abcdefghij\nsecond");
    warm_virtual(&ctx, &mut state);

    let left = virtual_line_center(&state, 0);
    virtual_click(&ctx, &mut state, left, egui::Modifiers::NONE);
    assert_eq!(state.virtual_text.caret(), 0);

    let right = egui::pos2(left.x + 60.0, left.y);
    virtual_click(&ctx, &mut state, right, egui::Modifiers::SHIFT);
    let selection = state
        .virtual_text
        .selection_range()
        .expect("a shift click extends the selection");
    assert_eq!(selection.start, 0, "the anchor is the previous caret");
    assert!(selection.end > 0, "the caret moved to the click");
}

#[test]
fn virtual_text_ctrl_click_reports_the_byte_and_moves_the_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "abc\ndef");
    warm_virtual(&ctx, &mut state);

    let pos = virtual_line_center(&state, 0);
    let out = virtual_click(&ctx, &mut state, pos, egui::Modifiers::COMMAND);
    assert_eq!(out.ctrl_click, Some(BytePos(0)));
    assert_eq!(
        state.virtual_text.caret(),
        0,
        "a ctrl click moves the caret to the clicked token"
    );
}

#[test]
fn virtual_text_hover_reports_a_byte() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "hover me\nsecond");
    warm_virtual(&ctx, &mut state);

    let pos = virtual_line_center(&state, 0);
    let out = drive_editor_frame_out(&ctx, &mut state, &[], vec![pointer_moved(pos)], false);
    assert!(out.hover_byte.is_some(), "the pointer is over text");
}

#[test]
fn pointer_over_the_text_viewport_uses_the_text_cursor() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "hover me\nsecond");
    warm_virtual(&ctx, &mut state);

    let pos = virtual_line_center(&state, 0);
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events: vec![pointer_moved(pos)],
        ..Default::default()
    };
    let full = ctx.run_ui(raw, |ui| {
        let _ = state.show(ui, empty_input());
    });
    assert_eq!(full.platform_output.cursor_icon, egui::CursorIcon::Text);
}

#[test]
fn virtual_text_hover_requires_a_real_glyph() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "abc\nsecond");
    warm_virtual(&ctx, &mut state);

    let over_glyph = virtual_line_center(&state, 0);
    let out = drive_editor_frame_out(
        &ctx,
        &mut state,
        &[],
        vec![pointer_moved(over_glyph)],
        false,
    );
    assert!(out.hover_byte.is_some(), "an identifier glyph hovers");

    let viewport = virtual_viewport(&state);
    let blank = egui::pos2(viewport.left() + viewport.width() * 0.5, over_glyph.y);
    let out = drive_editor_frame_out(&ctx, &mut state, &[], vec![pointer_moved(blank)], false);
    assert!(
        out.hover_byte.is_none(),
        "blank space past a short line's glyphs must not hover"
    );
}

#[test]
fn virtual_text_pointer_byte_reports_while_dragging() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "abcdef\nsecond");
    warm_virtual(&ctx, &mut state);

    let pos = virtual_line_center(&state, 0);
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![
            pointer_moved(pos),
            pointer_button(pos, true, egui::Modifiers::NONE),
        ],
        false,
    );
    let out = drive_editor_frame_out(&ctx, &mut state, &[], vec![pointer_moved(pos)], false);
    assert!(
        out.pointer_byte.is_some(),
        "the drop-insertion target is reported while the pointer is over text"
    );
    assert!(
        out.hover_byte.is_none(),
        "the plain-hover report stays suppressed while a button is down"
    );
}

#[test]
fn virtual_text_drag_below_the_viewport_autoscrolls() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, &numbered_lines(200));
    warm_virtual(&ctx, &mut state);

    let start = virtual_line_center(&state, 0);
    let viewport = virtual_viewport(&state);
    let below = egui::pos2(start.x, viewport.bottom() + 300.0);
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![
            pointer_moved(start),
            pointer_button(start, true, egui::Modifiers::NONE),
        ],
        false,
    );
    for _ in 0..4 {
        drive_editor_frame(&ctx, &mut state, &[], vec![pointer_moved(below)], false);
    }
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![pointer_button(below, false, egui::Modifiers::NONE)],
        false,
    );

    assert!(
        state.virtual_text.anchor.top_line > 0,
        "dragging past the bottom must scroll the anchor"
    );
}

#[test]
fn virtual_text_selection_does_not_mutate_cached_galleys() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "abc\ndef\nghi");
    warm_virtual(&ctx, &mut state);

    let before = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("line 0 is cached after warm-up");

    let start = virtual_line_center(&state, 0);
    let end = virtual_line_center(&state, 1);
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![
            pointer_moved(start),
            pointer_button(start, true, egui::Modifiers::NONE),
        ],
        false,
    );
    drive_editor_frame(&ctx, &mut state, &[], vec![pointer_moved(end)], false);
    drive_editor_frame(
        &ctx,
        &mut state,
        &[],
        vec![pointer_button(end, false, egui::Modifiers::NONE)],
        false,
    );

    let after = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("line 0 is still cached");
    assert!(
        std::sync::Arc::ptr_eq(&before, &after),
        "selection painting must not rebuild or mutate the cached galley"
    );
}

// ---- Stage C: virtual-mode editing input ---------------------------------

/// One virtual frame with explicit focus request and soft-wrap setting.
fn drive_virtual_frame_cfg(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    request_focus: bool,
    wrap_mode: WrapMode,
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events: with_modifiers(events, modifiers),
        ..Default::default()
    };
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.request_focus = request_focus;
        input.wrap_mode = wrap_mode;
        out = Some(state.show(ui, input));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

fn vframe(ctx: &egui::Context, state: &mut EditorState, events: Vec<egui::Event>) -> EditorOutput {
    drive_virtual_frame_cfg(
        ctx,
        state,
        events,
        egui::Modifiers::NONE,
        false,
        WrapMode::Off,
    )
}

fn vframe_mod(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) -> EditorOutput {
    drive_virtual_frame_cfg(ctx, state, events, modifiers, false, WrapMode::Off)
}

fn vframe_wrap(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
) -> EditorOutput {
    drive_virtual_frame_cfg(
        ctx,
        state,
        events,
        egui::Modifiers::NONE,
        false,
        WrapMode::Always,
    )
}

/// Focuses the virtual widget (one frame to request focus, one steady) with the
/// caret at `caret_byte`. `wrap` maps to [`WrapMode::Always`] / [`WrapMode::Off`].
fn focus_virtual(
    ctx: &egui::Context,
    state: &mut EditorState,
    text: &str,
    caret_byte: usize,
    wrap: bool,
) {
    state.set_buffer_text(text);
    state.jump_to_byte(caret_byte);
    let wrap_mode = if wrap {
        WrapMode::Always
    } else {
        WrapMode::Off
    };
    for _ in 0..2 {
        drive_virtual_frame_cfg(ctx, state, vec![], egui::Modifiers::NONE, true, wrap_mode);
    }
}

fn copied_text(commands: &[egui::OutputCommand]) -> Option<String> {
    commands.iter().find_map(|command| match command {
        egui::OutputCommand::CopyText(text) => Some(text.clone()),
        _ => None,
    })
}

/// One focused virtual frame that also returns the platform output commands
/// emitted during it (`run_ui` drains them into its `FullOutput`).
fn vframe_commands(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
) -> (EditorOutput, Vec<egui::OutputCommand>) {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = None;
    let mut commands = Vec::new();
    ctx.run_ui(raw, |ui| {
        out = Some(state.show(ui, empty_input()));
        commands = ui.ctx().output(|output| output.commands.clone());
    })
    .drop_without_applying_deltas();
    (out.expect("a frame output"), commands)
}

#[test]
fn virtual_focus_is_claimed_by_the_widget() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);
    let id = egui::Id::new(("typ-edit-virtual-text", state.serial));
    assert!(ctx.memory(|memory| memory.has_focus(id)));
}

#[test]
fn virtual_typing_inserts_and_reports_change() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);

    let out = vframe(&ctx, &mut state, vec![egui::Event::Text("a".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "a");
    assert!(out.changed);
    assert_eq!(out.cursor_byte, BytePos(1));
    assert_eq!(state.virtual_text.caret(), 1);
}

#[test]
fn virtual_typing_keeps_a_distant_cached_line() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let text = numbered_lines(200);
    focus_virtual(&ctx, &mut state, &text, 0, false);

    // Scroll to a line far below the viewport so its galley is shaped, then
    // return to the top; its entry must survive the keystroke below.
    let far_line = 180usize;
    let far_byte: usize = text.lines().take(far_line).map(|line| line.len() + 1).sum();
    state.jump_to_byte(far_byte);
    vframe(&ctx, &mut state, vec![]);
    vframe(&ctx, &mut state, vec![]);
    let before_far = state
        .virtual_text
        .cache
        .cached_galley(far_line)
        .expect("the far line was shaped while scrolled to it");
    let before_first = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the first line was shaped on the opening frames");

    state.jump_to_byte(0);
    vframe(&ctx, &mut state, vec![]);
    vframe(&ctx, &mut state, vec![]);

    let out = vframe(&ctx, &mut state, vec![egui::Event::Text("x".to_owned())]);
    assert!(out.changed);
    assert_eq!(&state.shared_text()[..1], "x");

    let after_far = state
        .virtual_text
        .cache
        .cached_galley(far_line)
        .expect("the far entry is still cached");
    assert!(
        std::sync::Arc::ptr_eq(&before_far, &after_far),
        "typing on line 0 must not invalidate a distant line"
    );
    let after_first = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("line 0 is cached");
    assert!(
        !std::sync::Arc::ptr_eq(&before_first, &after_first),
        "the edited line re-shapes"
    );
}

#[test]
fn virtual_space_types_without_moving_the_caret_to_the_pointer() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef\nuvwxyz", 0, false);

    // Park the pointer over the second line, away from the caret.
    let parked = virtual_line_center(&state, 1);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![pointer_moved(parked)],
        egui::Modifiers::NONE,
    );
    assert_eq!(state.virtual_text.caret(), 0);

    // egui marks a focused widget's Space as a synthetic click; unless that
    // flag is stripped the caret would jump to the parked pointer.
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            pointer_moved(parked),
            key_event(egui::Key::Space),
            egui::Event::Text(" ".to_owned()),
        ],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        state.virtual_text.caret(),
        1,
        "Space advances the caret by one"
    );
    assert_eq!(state.shared_text().as_str(), " abcdef\nuvwxyz");
}

#[test]
fn virtual_stale_parse_deltas_clear_when_the_generation_advances() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef", 6, false);

    // Establish the rendered generation with no edit.
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            parse_generation: 1,
            ..empty_input()
        },
    );
    assert_eq!(state.virtual_text.render_generation, Some(1));
    assert!(state.virtual_text.parse_deltas.borrow().is_empty());

    // An edit while the parse still lags at generation 1 is logged.
    vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text("g".to_owned())],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            parse_generation: 1,
            ..empty_input()
        },
    );
    assert_eq!(state.shared_text().as_str(), "abcdefg");
    assert_eq!(state.virtual_text.parse_deltas.borrow().len(), 1);

    // Another stale frame keeps the log.
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            parse_generation: 1,
            ..empty_input()
        },
    );
    assert_eq!(state.virtual_text.parse_deltas.borrow().len(), 1);

    // The fresh parse arrives: the log clears and its ranges are used.
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            parse_generation: 2,
            ..empty_input()
        },
    );
    assert!(state.virtual_text.parse_deltas.borrow().is_empty());
    assert_eq!(state.virtual_text.render_generation, Some(2));
}

#[test]
fn virtual_backspace_and_delete_edit_the_rope() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 3, false);

    let out = vframe(&ctx, &mut state, vec![key_event(egui::Key::Backspace)]);
    assert_eq!(state.shared_text().as_str(), "ab");
    assert!(out.changed);

    // Delete at the end is a no-op; Home then Delete removes the first char.
    vframe(&ctx, &mut state, vec![key_event(egui::Key::Delete)]);
    assert_eq!(state.shared_text().as_str(), "ab");
    vframe(&ctx, &mut state, vec![key_event(egui::Key::Home)]);
    vframe(&ctx, &mut state, vec![key_event(egui::Key::Delete)]);
    assert_eq!(state.shared_text().as_str(), "b");
}

#[test]
fn virtual_typing_replaces_the_selection() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::A)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.selection_range(), Some(0..3));

    vframe(&ctx, &mut state, vec![egui::Event::Text("X".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "X");
}

#[test]
fn virtual_select_all_reports_the_selection_bytes() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "a汉字b", 0, false);

    let out = vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::A)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(
        out.selections,
        vec![BytePos(0)..BytePos(state.shared_text().len())]
    );

    let collapsed = vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowRight)]);
    assert_eq!(
        collapsed.selections,
        vec![BytePos(state.shared_text().len())..BytePos(state.shared_text().len())],
        "a collapsed caret reports one empty byte range at itself"
    );
}

#[test]
fn virtual_select_all_collapses_to_one_full_selection() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "a汉字b", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::A)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(
        state.virtual_text.selection_range(),
        Some(0..state.shared_text().chars().count())
    );
    assert_eq!(
        state.virtual_text.carets().len(),
        1,
        "Ctrl+A replaces every caret with one full-document selection"
    );
}

#[test]
fn virtual_enter_auto_indents_after_an_opener() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "  foo{", 6, false);

    vframe(&ctx, &mut state, vec![key_event(egui::Key::Enter)]);
    assert_eq!(state.shared_text().as_str(), "  foo{\n    ");
    assert_eq!(state.virtual_text.caret(), 11);
}

#[test]
fn virtual_tab_indents_and_unindents_the_selection() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "a\nb", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::A)],
        egui::Modifiers::COMMAND,
    );
    vframe(&ctx, &mut state, vec![key_event(egui::Key::Tab)]);
    assert_eq!(state.shared_text().as_str(), "  a\n  b");

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::SHIFT, egui::Key::Tab)],
        egui::Modifiers::SHIFT,
    );
    assert_eq!(state.shared_text().as_str(), "a\nb");
}

#[test]
fn virtual_arrows_and_shift_selection() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 0, false);

    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowRight)]);
    assert_eq!(state.virtual_text.caret(), 1);
    assert_eq!(state.virtual_text.selection_range(), None);

    for _ in 0..2 {
        vframe_mod(
            &ctx,
            &mut state,
            vec![key_event_with(
                egui::Modifiers::SHIFT,
                egui::Key::ArrowRight,
            )],
            egui::Modifiers::SHIFT,
        );
    }
    assert_eq!(state.virtual_text.selection_range(), Some(1..3));
}

#[test]
fn virtual_home_and_end_move_within_the_line() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc\ndef", 5, false);

    vframe(&ctx, &mut state, vec![key_event(egui::Key::Home)]);
    assert_eq!(state.virtual_text.caret(), 4);
    vframe(&ctx, &mut state, vec![key_event(egui::Key::End)]);
    assert_eq!(state.virtual_text.caret(), 7);
}

#[test]
fn virtual_word_wise_navigation() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo bar baz", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(
            egui::Modifiers::COMMAND,
            egui::Key::ArrowRight,
        )],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.caret(), 3);
    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(
            egui::Modifiers::COMMAND,
            egui::Key::ArrowLeft,
        )],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.caret(), 0);
}

#[test]
fn virtual_up_down_cross_logical_lines() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc\ndef\nghi", 4, false);

    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowUp)]);
    assert_eq!(state.virtual_text.caret(), 0);
    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowDown)]);
    assert_eq!(state.virtual_text.caret(), 4);
    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowDown)]);
    assert_eq!(state.virtual_text.caret(), 8);
    assert!(state.virtual_text.h_pos().is_some());
}

#[test]
fn virtual_up_down_cross_wrapped_rows() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let text = "lorem ipsum ".repeat(40);
    let end = text.chars().count();
    focus_virtual(&ctx, &mut state, &text, end, true);

    vframe_wrap(&ctx, &mut state, vec![key_event(egui::Key::ArrowUp)]);
    let caret = state.virtual_text.caret();
    assert!(
        caret < end,
        "up must move to an earlier wrapped row: {caret}"
    );
    assert_eq!(state.buffer.line_of_char(caret), 0);
    assert!(state.virtual_text.h_pos().is_some());
}

#[test]
fn virtual_click_clears_the_vertical_x_hint() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    // Three equal-length lines: the vertical column remembered by ArrowUp and
    // the column the click lands on must not be confused.
    focus_virtual(
        &ctx,
        &mut state,
        "abcdefghij\nabcdefghij\nabcdefghij",
        18,
        false,
    );

    // ArrowUp remembers a vertical x near column 8 (caret 18 -> 7/8).
    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowUp)]);
    assert!(state.virtual_text.h_pos().is_some());
    let stale_col = state.virtual_text.caret() - state.buffer.line_start(0);

    // A pointer caret move must drop that remembered x.
    let pos = virtual_line_center(&state, 0);
    virtual_click(&ctx, &mut state, pos, egui::Modifiers::NONE);
    assert!(
        state.virtual_text.h_pos().is_none(),
        "a pointer caret move must clear the vertical x hint"
    );
    let clicked = state.virtual_text.caret();
    let clicked_col = clicked - state.buffer.line_start(0);
    assert_ne!(
        clicked_col, stale_col,
        "the click must land off the remembered column"
    );

    // ArrowDown must continue from the clicked column, not the pre-click x.
    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowDown)]);
    assert_eq!(
        state.virtual_text.caret(),
        state.buffer.line_start(1) + clicked_col,
        "the vertical move must continue from the clicked column"
    );
}

#[test]
fn virtual_page_down_moves_the_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let text = numbered_lines(120);
    focus_virtual(&ctx, &mut state, &text, 0, false);

    vframe(&ctx, &mut state, vec![key_event(egui::Key::PageDown)]);
    let caret = state.virtual_text.caret();
    assert!(
        state.buffer.line_of_char(caret) > 0,
        "page down must advance"
    );
    vframe(&ctx, &mut state, vec![key_event(egui::Key::PageUp)]);
    assert!(state.buffer.line_of_char(state.virtual_text.caret()) < 5);
}

#[test]
fn virtual_undo_groups_a_typed_run_and_redoes_it() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);

    for ch in ["a", "b", "c"] {
        vframe(&ctx, &mut state, vec![egui::Event::Text(ch.to_owned())]);
    }
    assert_eq!(state.shared_text().as_str(), "abc");

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Z)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(
        state.shared_text().as_str(),
        "",
        "the whole typed run is one group"
    );

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Y)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.shared_text().as_str(), "abc");
}

#[test]
fn virtual_undo_at_the_start_is_a_noop() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello", 5, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Z)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.shared_text().as_str(), "hello");
}

#[test]
fn virtual_copy_cut_and_paste_events() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::A)],
        egui::Modifiers::COMMAND,
    );
    let (_, commands) = vframe_commands(&ctx, &mut state, vec![egui::Event::Copy]);
    assert_eq!(copied_text(&commands).as_deref(), Some("abc"));

    vframe(&ctx, &mut state, vec![egui::Event::Paste("xy".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "xy");

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::A)],
        egui::Modifiers::COMMAND,
    );
    let (_, commands) = vframe_commands(&ctx, &mut state, vec![egui::Event::Cut]);
    assert_eq!(state.shared_text().as_str(), "");
    assert_eq!(copied_text(&commands).as_deref(), Some("xy"));
}

#[test]
fn virtual_pending_queues_are_honored() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);

    state.queue_insert_text("xy".to_owned());
    let out = vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.shared_text().as_str(), "xy");
    assert!(out.changed);

    state.jump_to_byte(1);
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.virtual_text.caret(), 1);

    state.queue_command(EditorCommand::SelectAll);
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.virtual_text.selection_range(), Some(0..2));
}

#[test]
fn virtual_ime_preedit_then_commit_is_clean() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);

    let preedit = |text: &str| {
        egui::Event::Ime(egui::ImeEvent::Preedit {
            text: text.to_owned(),
            active_range_chars: None,
        })
    };
    let out = vframe(&ctx, &mut state, vec![preedit("にほ")]);
    assert_eq!(state.shared_text().as_str(), "にほ");
    assert!(out.changed);
    assert_eq!(state.virtual_text.preedit_range, Some(0..2));

    vframe(&ctx, &mut state, vec![preedit("にほん")]);
    assert_eq!(
        state.shared_text().as_str(),
        "にほん",
        "preedit must replace preedit"
    );
    assert_eq!(state.virtual_text.preedit_range, Some(0..3));

    vframe(
        &ctx,
        &mut state,
        vec![egui::Event::Ime(egui::ImeEvent::Commit("日本".to_owned()))],
    );
    assert_eq!(state.shared_text().as_str(), "日本");
    assert_eq!(state.virtual_text.preedit_range, None);
    assert_eq!(state.virtual_text.caret(), 2);
}

#[test]
fn virtual_ime_empty_preedit_without_composition_is_ignored() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 3, false);

    let out = vframe(
        &ctx,
        &mut state,
        vec![egui::Event::Ime(egui::ImeEvent::Preedit {
            text: String::new(),
            active_range_chars: None,
        })],
    );
    assert_eq!(state.shared_text().as_str(), "abc");
    assert!(!out.changed);
}

#[test]
fn virtual_smart_edit_pairs_in_a_large_buffer() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let text = format!("{}(", "x\n".repeat(50_000));
    focus_virtual(&ctx, &mut state, &text, text.len(), false);

    vframe(&ctx, &mut state, vec![egui::Event::Text(")".to_owned())]);

    let out = state.shared_text();
    assert!(out.ends_with("()"), "large-buffer pairing");
    assert_eq!(state.virtual_text.caret(), out.chars().count());
}

#[test]
fn virtual_smart_edit_pairs_skip_and_backspace() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);

    vframe(&ctx, &mut state, vec![egui::Event::Text("(".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "()");
    assert_eq!(state.virtual_text.caret(), 1);

    // A symmetric delimiter skips over the existing one.
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "$$", 1, false);
    vframe(&ctx, &mut state, vec![egui::Event::Text("$".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "$$");
    assert_eq!(state.virtual_text.caret(), 2);

    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "()", 1, false);
    vframe(&ctx, &mut state, vec![key_event(egui::Key::Backspace)]);
    assert_eq!(state.shared_text().as_str(), "");
    assert_eq!(state.virtual_text.caret(), 0);
}

#[test]
fn virtual_quote_pairs_inside_code_ranges_but_not_other_blocked_ranges() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let code = Ranges {
        blocked: std::slice::from_ref(&(0..3)),
        code: std::slice::from_ref(&(0..3)),
        ..Default::default()
    };
    let mut state = EditorState::new();
    state.set_buffer_text("#{}");
    state.jump_to_byte(2);
    for _ in 0..2 {
        drive_virtual_ex(&ctx, &mut state, vec![], WrapMode::Off, code, 0.0);
    }
    drive_virtual_ex(
        &ctx,
        &mut state,
        vec![egui::Event::Text("\"".to_owned())],
        WrapMode::Off,
        code,
        0.1,
    );
    assert_eq!(
        state.shared_text().as_str(),
        "#{\"\"}",
        "a double quote pairs inside a code range"
    );

    // A blocked range that is not code (math/raw/comment) still suppresses it.
    let blocked_range = 0..3;
    let blocked_only = Ranges {
        blocked: std::slice::from_ref(&blocked_range),
        ..Default::default()
    };
    let mut non_code = EditorState::new();
    non_code.set_buffer_text("#{}");
    non_code.jump_to_byte(2);
    for _ in 0..2 {
        drive_virtual_ex(
            &ctx,
            &mut non_code,
            vec![],
            WrapMode::Off,
            blocked_only,
            0.0,
        );
    }
    drive_virtual_ex(
        &ctx,
        &mut non_code,
        vec![egui::Event::Text("\"".to_owned())],
        WrapMode::Off,
        blocked_only,
        0.1,
    );
    assert_eq!(
        non_code.shared_text().as_str(),
        "#{\"}",
        "a non-code blocked range keeps the pair suppressed"
    );
}

#[test]
fn virtual_quote_pairs_inside_math_ranges_but_not_raw() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    // `$"x"$` is a valid Typst string; production marks math as both blocked and
    // a math range, so the quote must pair there.
    let math = Ranges {
        blocked: std::slice::from_ref(&(0..2)),
        math: std::slice::from_ref(&(0..2)),
        ..Default::default()
    };
    let mut state = EditorState::new();
    state.set_buffer_text("$$");
    state.jump_to_byte(1);
    for _ in 0..2 {
        drive_virtual_ex(&ctx, &mut state, vec![], WrapMode::Off, math, 0.0);
    }
    drive_virtual_ex(
        &ctx,
        &mut state,
        vec![egui::Event::Text("\"".to_owned())],
        WrapMode::Off,
        math,
        0.1,
    );
    assert_eq!(
        state.shared_text().as_str(),
        "$\"\"$",
        "a double quote pairs inside a math range"
    );

    // A raw range is blocked but neither code nor math: the quote stays alone.
    let raw_range = 0..2;
    let raw = Ranges {
        blocked: std::slice::from_ref(&raw_range),
        ..Default::default()
    };
    let mut raw_state = EditorState::new();
    raw_state.set_buffer_text("$$");
    raw_state.jump_to_byte(1);
    for _ in 0..2 {
        drive_virtual_ex(&ctx, &mut raw_state, vec![], WrapMode::Off, raw, 0.0);
    }
    drive_virtual_ex(
        &ctx,
        &mut raw_state,
        vec![egui::Event::Text("\"".to_owned())],
        WrapMode::Off,
        raw,
        0.1,
    );
    assert_eq!(
        raw_state.shared_text().as_str(),
        "$\"$",
        "a raw blocked range keeps the pair suppressed"
    );
}

#[test]
fn virtual_no_pairing_after_a_backslash() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);
    vframe(&ctx, &mut state, vec![egui::Event::Text("\\".to_owned())]);
    vframe(&ctx, &mut state, vec![egui::Event::Text("(".to_owned())]);
    assert_eq!(
        state.shared_text().as_str(),
        "\\(",
        "a bracket typed after a backslash is escaped"
    );

    let mut quote = EditorState::new();
    focus_virtual(&ctx, &mut quote, "", 0, false);
    vframe(&ctx, &mut quote, vec![egui::Event::Text("\\".to_owned())]);
    vframe(&ctx, &mut quote, vec![egui::Event::Text("\"".to_owned())]);
    assert_eq!(
        quote.shared_text().as_str(),
        "\\\"",
        "a quote typed after a backslash is escaped"
    );
}

#[test]
fn virtual_unmatched_quote_closes_without_pairing() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "\"abc", 4, false);
    vframe(&ctx, &mut state, vec![egui::Event::Text("\"".to_owned())]);
    assert_eq!(
        state.shared_text().as_str(),
        "\"abc\"",
        "a quote closes an open string instead of pairing"
    );
    assert_eq!(state.virtual_text.caret(), 5);

    // Skip-over wins over the open-string guard: an existing quote is stepped
    // over, not paired beside.
    let mut skip = EditorState::new();
    focus_virtual(&ctx, &mut skip, "\"\"", 1, false);
    vframe(&ctx, &mut skip, vec![egui::Event::Text("\"".to_owned())]);
    assert_eq!(skip.shared_text().as_str(), "\"\"");
    assert_eq!(skip.virtual_text.caret(), 2);

    // The same precedence in front of the closing quote of an open string.
    let mut open_skip = EditorState::new();
    focus_virtual(&ctx, &mut open_skip, "\"abc\"", 4, false);
    vframe(
        &ctx,
        &mut open_skip,
        vec![egui::Event::Text("\"".to_owned())],
    );
    assert_eq!(open_skip.shared_text().as_str(), "\"abc\"");
    assert_eq!(open_skip.virtual_text.caret(), 5);
}

#[test]
fn virtual_single_quote_inserts_verbatim() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);
    vframe(&ctx, &mut state, vec![egui::Event::Text("'".to_owned())]);
    assert_eq!(
        state.shared_text().as_str(),
        "'",
        "a single quote is inserted verbatim"
    );
    assert_eq!(state.virtual_text.caret(), 1);
}

#[test]
fn unmatched_double_quote_counts_parity_and_skips_escapes() {
    assert!(!unmatched_double_quote("abc", 3));
    assert!(unmatched_double_quote("\"abc", 4));
    assert!(!unmatched_double_quote("\"abc\"", 5));
    assert!(
        !unmatched_double_quote("\\\"abc", 5),
        "an escaped quote does not open a string"
    );
    assert!(
        unmatched_double_quote("\"a\\\"b", 5),
        "an escaped quote leaves the real one open"
    );
}

#[test]
fn virtual_wraps_a_selected_range_in_a_pair() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::A)],
        egui::Modifiers::COMMAND,
    );
    vframe(&ctx, &mut state, vec![egui::Event::Text("(".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "(abc)");
    assert_eq!(state.virtual_text.selection_range(), Some(1..4));
}

#[test]
fn virtual_closing_delimiter_auto_dedents() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "(\n  ", 4, false);

    vframe(&ctx, &mut state, vec![egui::Event::Text(")".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "(\n)");
    assert_eq!(state.virtual_text.caret(), 3);
}

// ---- Stage D2: virtual-mode completion -------------------------------------

/// One virtual frame with a caller-supplied [`EditorInput`], so completion
/// tests can feed candidates and parse generations.
fn vframe_in(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    input: EditorInput<'_>,
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events: with_modifiers(events, modifiers),
        ..Default::default()
    };
    let mut input = Some(input);
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        out = Some(state.show(ui, input.take().expect("one input per frame")));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

fn manual_open() -> Vec<egui::Event> {
    vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Space)]
}

#[test]
fn virtual_completion_manual_ctrl_space_opens() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem::plain("let", CompletionKind::Keyword)];

    let out = vframe_in(
        &ctx,
        &mut state,
        manual_open(),
        egui::Modifiers::COMMAND,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert!(!out.changed);
    let popup = state.completion.as_ref().expect("manual trigger opens");
    assert_eq!(popup.items.len(), 1);
    assert_eq!(popup.items[0].name, "let");
}

#[test]
fn virtual_completion_honours_the_configured_cap() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions: Vec<CompletionItem> = (0..10)
        .map(|i| CompletionItem::plain(format!("l{i:02}"), CompletionKind::Keyword))
        .collect();

    vframe_in(
        &ctx,
        &mut state,
        manual_open(),
        egui::Modifiers::COMMAND,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            completion_max: 3,
            ..empty_input()
        },
    );
    let popup = state.completion.as_ref().expect("manual trigger opens");
    assert_eq!(popup.items.len(), 3, "the configured cap bounds the popup");
}

#[test]
fn virtual_typing_hash_opens_completion() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);
    let completions = vec![CompletionItem::plain("let", CompletionKind::Keyword)];

    let out = vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text("#".to_owned())],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert!(out.changed);
    assert_eq!(state.shared_text().as_str(), "#");
    let popup = state
        .completion
        .as_ref()
        .expect("typing `#` opens completion");
    assert_eq!(popup.items[0].name, "let");
}

#[test]
fn virtual_completion_up_down_changes_selection() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![
        CompletionItem::plain("let", CompletionKind::Keyword),
        CompletionItem::plain("lorem", CompletionKind::Keyword),
    ];
    vframe_in(
        &ctx,
        &mut state,
        manual_open(),
        egui::Modifiers::COMMAND,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert_eq!(state.completion.as_ref().expect("open").selected, 0);

    vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::ArrowDown)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert_eq!(state.completion.as_ref().expect("open").selected, 1);
    assert_eq!(
        state.virtual_text.caret(),
        1,
        "Up/Down never move the caret"
    );

    vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::ArrowUp)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert_eq!(state.completion.as_ref().expect("open").selected, 0);
}

#[test]
fn virtual_completion_enter_accepts_and_places_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem {
        name: "let".to_owned(),
        insert: "let ".to_owned(),
        kind: CompletionKind::Keyword,
        cursor_back: CharCount(0),
        docs: None,
    }];
    let typed = EditorInput {
        collab_history: false,
        remote_carets: &[],
        completions: &completions,
        ..empty_input()
    };
    vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text("l".to_owned())],
        egui::Modifiers::NONE,
        typed,
    );
    assert!(
        state.completion.is_some(),
        "typing a letter opens the popup"
    );

    let out = vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::Enter)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert!(out.changed);
    assert_eq!(state.shared_text().as_str(), "#let ");
    assert_eq!(state.virtual_text.caret(), 5);
    assert!(state.completion.is_none(), "accepting dismisses the popup");
}

#[test]
fn virtual_completion_tab_accepts_without_indenting() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem::plain("let", CompletionKind::Keyword)];
    vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text("l".to_owned())],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    let out = vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::Tab)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert!(out.changed);
    assert_eq!(
        state.shared_text().as_str(),
        "#let",
        "Tab accepts, it does not indent"
    );
}

#[test]
fn virtual_dot_after_ident_requests_world_completions() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#let d = 1\n#d", 13, false);

    let out = vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text(".".to_owned())],
        egui::Modifiers::NONE,
        empty_input(),
    );
    assert_eq!(state.shared_text().as_str(), "#let d = 1\n#d.");
    assert_eq!(
        out.completion_request,
        Some(BytePos(state.shared_text().len())),
        "the caret byte is requested after the dot"
    );
}

#[test]
fn virtual_dot_requests_after_a_closing_bracket() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#f()", 4, false);
    // The parse reports `f()` as code; the freshly typed dot lags one edit
    // behind that range, so the context must still resolve as code.
    let code_ranges = [BytePos(1)..BytePos(4)];

    let out = vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text(".".to_owned())],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            code_ranges: &code_ranges,
            ..empty_input()
        },
    );
    assert_eq!(
        out.completion_request,
        Some(BytePos(5)),
        "`).` triggers a member request"
    );
}

#[test]
fn virtual_dot_in_markup_neither_requests_nor_opens_completion() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello", 5, false);

    let out = vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text(".".to_owned())],
        egui::Modifiers::NONE,
        empty_input(),
    );
    assert_eq!(state.shared_text().as_str(), "hello.");
    assert_eq!(
        out.completion_request, None,
        "a markup `.` must not ask the world for members"
    );
    assert!(
        state.completion.is_none(),
        "a markup `.` must not open a popup"
    );
}

#[test]
fn virtual_stale_world_members_do_not_open_completion_in_markup() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello.", 6, false);
    let world = vec![
        CompletionItem::plain("alpha", CompletionKind::Value),
        CompletionItem::plain("beta", CompletionKind::Value),
    ];

    // The app answers an (erroneously) requested member list for a markup dot:
    // the async list arriving a frame later must not open the popup, which is
    // the reported dot-completion bug.
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            world_completions: &world,
            ..empty_input()
        },
    );
    assert!(
        state.completion.is_none(),
        "stale world members must not open completion in markup"
    );
}

#[test]
fn virtual_world_completions_offer_members_and_accept_at_member_start() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#let d = 1\n#d.", 14, false);
    let world = vec![
        CompletionItem::plain("alpha", CompletionKind::Value),
        CompletionItem::plain("beta", CompletionKind::Value),
    ];

    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            world_completions: &world,
            ..empty_input()
        },
    );
    let popup = state
        .completion
        .as_ref()
        .expect("world members open the popup");
    assert_eq!(popup.items.len(), 2);
    assert_eq!(popup.items[0].name, "alpha");
    assert_eq!(
        popup.word_start, 14,
        "the member word starts right after the dot"
    );

    let out = vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::Enter)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            world_completions: &world,
            ..empty_input()
        },
    );
    assert!(out.changed);
    assert_eq!(state.shared_text().as_str(), "#let d = 1\n#d.alpha");
    assert_eq!(state.virtual_text.caret(), 19);
}

#[test]
fn virtual_completion_without_world_members_keeps_the_static_list() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem::plain("let", CompletionKind::Keyword)];

    let out = vframe_in(
        &ctx,
        &mut state,
        manual_open(),
        egui::Modifiers::COMMAND,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    let popup = state.completion.as_ref().expect("manual trigger opens");
    assert_eq!(popup.items[0].name, "let", "static candidates unchanged");
    assert!(
        out.completion_request.is_some(),
        "an explicit request also asks the world"
    );
}

#[test]
fn virtual_manual_ctrl_space_in_markup_neither_requests_nor_opens() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello", 5, false);

    let out = vframe_in(
        &ctx,
        &mut state,
        manual_open(),
        egui::Modifiers::COMMAND,
        empty_input(),
    );
    assert_eq!(
        out.completion_request, None,
        "Ctrl+Space in markup must not ask the world for members"
    );
    assert!(
        state.completion.is_none(),
        "Ctrl+Space in markup must not open a popup"
    );
}

#[test]
fn virtual_stale_world_members_do_not_leak_into_a_keystroke_refresh() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#d.", 3, false);
    let world = vec![CompletionItem::plain("alpha", CompletionKind::Value)];

    // A keystroke and the arrival of a member list fetched for the previous
    // text land together: the keystroke must fall back to the static list.
    let out = vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text("x".to_owned())],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            world_completions: &world,
            ..empty_input()
        },
    );
    assert!(out.changed);
    assert!(
        state.completion.is_none(),
        "the pre-keystroke member list must not paint"
    );
}

#[test]
fn virtual_completion_escape_dismisses() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem::plain("let", CompletionKind::Keyword)];
    vframe_in(
        &ctx,
        &mut state,
        manual_open(),
        egui::Modifiers::COMMAND,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert!(state.completion.is_some());

    let out = vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::Escape)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert!(!out.changed);
    assert!(state.completion.is_none());
    assert!(state.dismissed_at.is_some());
}

#[test]
fn virtual_parse_generation_refreshes_an_open_popup() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let first = vec![
        CompletionItem::plain("let", CompletionKind::Keyword),
        CompletionItem::plain("lorem", CompletionKind::Keyword),
    ];
    vframe_in(
        &ctx,
        &mut state,
        manual_open(),
        egui::Modifiers::COMMAND,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &first,
            ..empty_input()
        },
    );
    assert_eq!(state.completion.as_ref().expect("open").items.len(), 2);

    let second = vec![
        CompletionItem::plain("let", CompletionKind::Keyword),
        CompletionItem::plain("lorem", CompletionKind::Keyword),
        CompletionItem::plain("lsp", CompletionKind::Keyword),
    ];
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &second,
            parse_generation: 1,
            ..empty_input()
        },
    );
    assert_eq!(
        state.completion.as_ref().expect("still open").items.len(),
        3,
        "a fresh parse generation recomputes without a keystroke"
    );
}

/// A minimal call whose only parameter is `level`, non-positional-free.
fn level_call_params() -> CallParams {
    CallParams {
        range: BytePos(1)..BytePos(9),
        items: vec![CompletionItem {
            name: "level".to_owned(),
            insert: "level: ".to_owned(),
            kind: CompletionKind::Param,
            cursor_back: CharCount(0),
            docs: None,
        }],
        value_slot: None,
    }
}

#[test]
fn virtual_completion_callable_accept_reopens_the_parameter_popup() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem {
        name: "heading".to_owned(),
        insert: "heading(".to_owned(),
        kind: CompletionKind::Element,
        cursor_back: CharCount(0),
        docs: None,
    }];
    vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text("h".to_owned())],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::Enter)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert_eq!(state.shared_text().as_str(), "#heading(");
    assert_eq!(
        state.dismissed_at, None,
        "a callable accept leaves no guard"
    );

    let call = level_call_params();
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            call_params: Some(&call),
            parse_generation: 1,
            ..empty_input()
        },
    );
    let popup = state
        .completion
        .as_ref()
        .expect("the parameter list reopens after a callable accept");
    assert!(popup.items.iter().any(|item| item.name == "level"));
    assert!(popup.params);
}

#[test]
fn virtual_completion_click_on_callable_reopens_the_parameter_popup() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem {
        name: "heading".to_owned(),
        insert: "heading(".to_owned(),
        kind: CompletionKind::Element,
        cursor_back: CharCount(0),
        docs: None,
    }];
    vframe_in(
        &ctx,
        &mut state,
        manual_open(),
        egui::Modifiers::COMMAND,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    let rect = completion_popup_rect(&ctx).expect("popup painted");
    let click = egui::pos2(rect.left() + 20.0, rect.center().y);
    vframe_in(
        &ctx,
        &mut state,
        vec![
            pointer_moved(click),
            pointer_button(click, true, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    vframe_in(
        &ctx,
        &mut state,
        vec![pointer_button(click, false, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert_eq!(
        state.shared_text().as_str(),
        "#heading(",
        "the click accepts"
    );
    assert_eq!(
        state.dismissed_at, None,
        "a callable accept leaves no guard"
    );

    let call = level_call_params();
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            call_params: Some(&call),
            parse_generation: 1,
            ..empty_input()
        },
    );
    assert!(
        state.completion.is_some(),
        "the parameter list reopens after a callable click accept"
    );
}

#[test]
fn virtual_completion_non_callable_accept_stays_closed() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem {
        name: "red".to_owned(),
        insert: "red".to_owned(),
        kind: CompletionKind::Value,
        cursor_back: CharCount(0),
        docs: None,
    }];
    vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text("r".to_owned())],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::Enter)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert_eq!(state.shared_text().as_str(), "#red");
    assert!(state.dismissed_at.is_some());

    let call = level_call_params();
    vframe_in(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            call_params: Some(&call),
            parse_generation: 1,
            ..empty_input()
        },
    );
    assert!(
        state.completion.is_none(),
        "a non-callable accept keeps the popup closed"
    );
}

#[test]
fn numeric_value_slot_opens_on_a_digit_prefix() {
    let call = CallParams {
        range: BytePos(5)..BytePos(14),
        items: vec![],
        value_slot: Some(ValueSlot {
            param: "size".to_owned(),
            items: vec![
                candidate("12pt", CompletionKind::Value),
                candidate("12em", CompletionKind::Value),
            ],
        }),
    };
    let mut state = EditorState::new();
    state.set_buffer_text("#text(size: 12");
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        call_params: Some(&call),
        parse_generation: 1,
        ..empty_input()
    };
    state.refresh_completion(14, &input, CompletionGate::Parse);
    let popup = state.completion.as_ref().expect("the unit popup opens");
    let names: Vec<&str> = popup.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["12pt", "12em"]);
}

#[test]
fn virtual_completion_accepts_a_unit_candidate() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#text(size: 12", 14, false);
    let call = CallParams {
        range: BytePos(5)..BytePos(14),
        items: vec![],
        value_slot: Some(ValueSlot {
            param: "size".to_owned(),
            items: vec![
                candidate("12pt", CompletionKind::Value),
                candidate("12em", CompletionKind::Value),
            ],
        }),
    };
    let input = EditorInput {
        collab_history: false,
        remote_carets: &[],
        call_params: Some(&call),
        parse_generation: 1,
        ..empty_input()
    };
    state.refresh_completion(14, &input, CompletionGate::Parse);
    assert!(state.completion.is_some());

    let out = vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::Enter)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            call_params: Some(&call),
            parse_generation: 1,
            ..empty_input()
        },
    );
    assert!(out.changed);
    assert_eq!(state.shared_text().as_str(), "#text(size: 12pt");
    assert_eq!(state.virtual_text.caret(), 16);
}

// ---- Stage D: find bar, chrome and overlays -------------------------------

/// Opens the shared find bar on `query` and refreshes the match list directly,
/// the way a typed query would end up before the frame paints it.
fn open_find_virtual(state: &mut EditorState, query: &str, replacement: &str) {
    state.find.open = true;
    state.find.query = query.to_owned();
    state.find.replacement = replacement.to_owned();
    state.refresh_find_matches();
}

/// One virtual frame carrying diagnostics, for the squiggle overlay.
fn vframe_with_diagnostics(
    ctx: &egui::Context,
    state: &mut EditorState,
    diagnostics: &[typ_edit_model::Diag],
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        ..Default::default()
    };
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.diagnostics = diagnostics;
        out = Some(state.show(ui, input));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

/// One virtual frame carrying spell issues, for the dotted-underline overlay.
fn vframe_with_spell(
    ctx: &egui::Context,
    state: &mut EditorState,
    spell_issues: &[typ_edit_model::SpellIssue],
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        ..Default::default()
    };
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.spell_issues = spell_issues;
        out = Some(state.show(ui, input));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

/// One virtual frame carrying debug hover highlights, for the inspector band.
fn vframe_with_debug_highlights(
    ctx: &egui::Context,
    state: &mut EditorState,
    highlights: &[std::ops::Range<usize>],
    color: egui::Color32,
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        ..Default::default()
    };
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.debug_highlights = highlights;
        input.debug_highlight_color = color;
        out = Some(state.show(ui, input));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

#[test]
fn replace_byte_range_replaces_the_word_and_keeps_text_in_sync() {
    let mut state = EditorState::new();
    state.set_buffer_text("helo wrld");
    state.replace_byte_range(0..4, "hello");
    assert_eq!(state.shared_text().as_str(), "hello wrld");
}

#[test]
fn virtual_frame_with_spell_issues_renders_without_panic() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "helo wrld", 0, false);
    let issues = [typ_edit_model::SpellIssue {
        range: BytePos(0)..BytePos(4),
        word: "helo".to_owned(),
        suggestions: vec!["hello".to_owned(), "held".to_owned()],
    }];
    let out = vframe_with_spell(&ctx, &mut state, &issues);
    assert!(
        !out.changed,
        "painting a spell issue never edits the buffer"
    );
}

#[test]
fn virtual_debug_highlights_paint_without_panic() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello world\nsecond line", 0, false);
    warm_virtual(&ctx, &mut state);

    let out = vframe_with_debug_highlights(
        &ctx,
        &mut state,
        std::slice::from_ref(&(0..5)),
        egui::Color32::from_rgba_unmultiplied(255, 0, 0, 60),
    );
    assert!(
        !out.changed,
        "painting a debug highlight never edits the buffer"
    );
}

#[test]
fn virtual_find_bar_highlights_matches_on_visible_lines() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo bar\nfoo baz", 0, false);
    open_find_virtual(&mut state, "foo", "");
    assert_eq!(state.find.matches.len(), 2, "the fixture must match twice");

    let _ = vframe(&ctx, &mut state, vec![]);
    assert_eq!(
        state.find.matches,
        vec![0..3, 8..11],
        "the highlight ranges stay computable on the visible lines"
    );
}

#[test]
fn find_open_with_selection_prefills_query_without_scoping() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_text("foo bar\nfoo baz".to_owned());
    state.find.open = true;
    state.find.focus_pending = true;
    // A previous scope must not survive the new selection prefill either.
    state.find.in_selection = Some(8..11);

    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 500.0),
        )),
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| {
        let _ = state.find_bar_ui(ui, None, Some("foo"), Some(0..3));
    })
    .drop_without_applying_deltas();

    assert_eq!(state.find.query, "foo", "the selection fills the query");
    assert_eq!(
        state.find.in_selection, None,
        "opening find on a selection must not enable the scope toggle"
    );
    assert_eq!(
        state.find.matches,
        vec![0..3, 8..11],
        "the query still matches the whole buffer"
    );
}

#[test]
fn virtual_find_refresh_after_edit_is_debounced_and_does_not_select() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo foo", 0, false);
    open_find_virtual(&mut state, "foo", "");
    assert_eq!(state.find.matches.len(), 2);

    state.queue_insert_text("x".to_owned());
    let _ = vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.shared_text().as_str(), "xfoo foo");
    assert!(
        state.find.pending_refresh.is_some(),
        "an edit must defer the full scan"
    );
    assert!(
        !state.find.pending_refresh_activates,
        "an edit refresh must not select a match"
    );
    assert_eq!(
        state.find.matches,
        vec![0..3, 4..7],
        "the pre-edit matches stay visible until the debounce elapses"
    );

    // Move the pending deadline into the past instead of sleeping; one frame
    // then performs the deferred scan.
    state.find.pending_refresh = Some(std::time::Instant::now() - crate::FIND_DEBOUNCE);
    let _ = vframe(&ctx, &mut state, vec![]);

    assert!(state.find.pending_refresh.is_none());
    assert_eq!(state.find.matches, vec![1..4, 5..8]);
    assert!(
        state.find.pending_select.is_none(),
        "the deferred edit scan must not move the editor selection"
    );
}

/// Renders only the find bar at `width` and reports the region it occupied in
/// the frame, after enough frames for the striped Grid to settle.
fn find_bar_rect(ctx: &egui::Context, state: &mut EditorState, width: f32) -> egui::Rect {
    let mut rect = egui::Rect::NOTHING;
    for _ in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 400.0),
            )),
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            let _ = state.find_bar_ui(ui, None, None, None);
            rect = ui.min_rect();
        })
        .drop_without_applying_deltas();
    }
    rect
}

/// Opens the find bar with a query and renders it at `width`, returning the bar
/// region and the query input's width.
fn find_bar_at(width: f32) -> (egui::Rect, f32) {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.find.open = true;
    state.find.query = "foo".to_owned();
    let rect = find_bar_rect(&ctx, &mut state, width);
    let input = ctx
        .read_response(egui::Id::new(FIND_INPUT_ID))
        .map_or(0.0, |response| response.rect.width());
    (rect, input)
}

/// Render test: the toggles row wraps onto extra lines when the bar is narrowed
/// below the toggles' natural width (the bar grows taller), and stays a single
/// row at a wide viewport. It checks the bar's rendered height, which only grows
/// when the toggles actually break lines.
#[test]
fn find_bar_toggles_wrap_when_narrow_but_not_when_wide() {
    let (wide, _) = find_bar_at(1200.0);
    let (narrow, _) = find_bar_at(200.0);
    assert!(
        narrow.height() > wide.height(),
        "the toggles must wrap when the bar is narrower than they are: \
         wide={} narrow={}",
        wide.height(),
        narrow.height()
    );
    let (also_wide, _) = find_bar_at(700.0);
    assert_eq!(
        wide.height(),
        also_wide.height(),
        "a wide bar keeps the toggles on one row"
    );
}

/// Render test: the query input's width tracks the available space (its cap is
/// the bar's width minus the trailing controls, not the toggles' width).
#[test]
fn find_bar_query_input_widens_with_available_space() {
    let (_, narrow_input) = find_bar_at(600.0);
    let (_, wide_input) = find_bar_at(1200.0);
    assert!(
        wide_input > narrow_input,
        "the query input widens with the bar: narrow={narrow_input} wide={wide_input}"
    );
    assert!(
        wide_input > 700.0,
        "the query input fills most of a wide bar: {wide_input}"
    );
}

#[test]
fn virtual_f3_steps_through_matches_and_selects_them() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo foo foo", 0, false);
    open_find_virtual(&mut state, "foo", "");
    assert_eq!(state.find.matches.len(), 3);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::F3)],
        egui::Modifiers::NONE,
    );
    assert_eq!(state.find.active, 1);
    assert_eq!(state.virtual_text.selection_range(), Some(4..7));

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::F3)],
        egui::Modifiers::NONE,
    );
    assert_eq!(state.find.active, 2);
    assert_eq!(state.virtual_text.selection_range(), Some(8..11));

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::SHIFT, egui::Key::F3)],
        egui::Modifiers::SHIFT,
    );
    assert_eq!(state.find.active, 1);
    assert_eq!(state.virtual_text.selection_range(), Some(4..7));
}

#[test]
fn virtual_replace_all_mutates_the_buffer_and_reports_changed() {
    let mut state = EditorState::new();
    state.set_buffer_text("foo bar\nfoo baz");
    open_find_virtual(&mut state, "foo", "qux");

    let outcome = state.run_find_actions(crate::find_bar::FindActions {
        replace_current: false,
        replace_all: true,
        close: false,
    });
    assert!(outcome.changed);
    assert_eq!(state.shared_text().as_str(), "qux bar\nqux baz");
}

#[test]
fn virtual_replace_current_edits_the_active_match() {
    let mut state = EditorState::new();
    state.set_buffer_text("foo foo");
    open_find_virtual(&mut state, "foo", "qux");
    state.find.active = 1;

    let outcome = state.run_find_actions(crate::find_bar::FindActions {
        replace_current: true,
        replace_all: false,
        close: false,
    });
    assert!(outcome.changed);
    assert_eq!(state.shared_text().as_str(), "foo qux");
    assert!(
        state.find.pending_select.is_some(),
        "the next match is queued for the virtual selection"
    );
}

// ---- Find selection-scope hint (P1 QA item 11) ----------------------------

#[test]
fn effective_scope_prefers_a_live_selection_over_the_hint() {
    use crate::find_bar::effective_scope;
    assert_eq!(effective_scope(Some(2..5), Some(10..20)), Some(2..5));
}

#[test]
fn effective_scope_falls_back_to_the_hint_without_a_live_selection() {
    use crate::find_bar::effective_scope;
    assert_eq!(effective_scope(None, Some(10..20)), Some(10..20));
}

#[test]
fn effective_scope_accepts_a_live_selection_alone() {
    use crate::find_bar::effective_scope;
    assert_eq!(effective_scope(Some(1..4), None), Some(1..4));
}

#[test]
fn effective_scope_is_none_without_a_selection_or_hint() {
    use crate::find_bar::effective_scope;
    assert_eq!(effective_scope(None, None), None);
}

#[test]
fn find_scope_hint_survives_loss_of_the_live_selection() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello world", 0, false);

    state.virtual_text.set_selection(0, 5);
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(
        state.find.scope_hint,
        Some(0..5),
        "a live selection is remembered"
    );

    state.virtual_text.set_caret(0);
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(
        state.find.scope_hint,
        Some(0..5),
        "collapsing the selection / losing focus must not clear the hint"
    );
}

#[test]
fn virtual_gutter_click_selects_the_whole_logical_line() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "aaa\nbbb\nccc");
    warm_virtual(&ctx, &mut state);

    let viewport = virtual_viewport(&state);
    let (_, y, _) = state
        .virtual_text
        .test_line_origins
        .iter()
        .copied()
        .find(|(line, _, _)| *line == 1)
        .expect("line 1 is visible");
    let pos = egui::pos2(viewport.left() - 15.0, y + 5.0);
    virtual_click(&ctx, &mut state, pos, egui::Modifiers::NONE);

    assert_eq!(
        state.virtual_text.selection_range(),
        Some(4..7),
        "the gutter click selects the second logical line without its newline"
    );
}

#[test]
fn virtual_gutter_click_clears_the_vertical_x_hint() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    // Equal-length lines: the remembered column and the gutter line's end
    // column must not be confused.
    focus_virtual(
        &ctx,
        &mut state,
        "abcdefghij\nabcdefghij\nabcdefghij",
        18,
        false,
    );

    // ArrowUp remembers a vertical x near column 8.
    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowUp)]);
    assert!(state.virtual_text.h_pos().is_some());
    let stale_col = state.virtual_text.caret() - state.buffer.line_start(0);

    // Gutter-click line 0: selects the whole line and must clear the hint.
    let viewport = virtual_viewport(&state);
    let (_, y, _) = state
        .virtual_text
        .test_line_origins
        .iter()
        .copied()
        .find(|(line, _, _)| *line == 0)
        .expect("line 0 is visible");
    let pos = egui::pos2(viewport.left() - 15.0, y + 5.0);
    virtual_click(&ctx, &mut state, pos, egui::Modifiers::NONE);
    assert!(
        state.virtual_text.h_pos().is_none(),
        "a gutter click must clear the vertical x hint"
    );
    let clicked_col = state.virtual_text.caret() - state.buffer.line_start(0);
    assert_ne!(
        clicked_col, stale_col,
        "the gutter lands off the remembered column"
    );

    // ArrowDown must continue from the gutter column, not the pre-click x.
    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowDown)]);
    assert_eq!(
        state.virtual_text.caret(),
        state.buffer.line_start(1) + clicked_col,
        "the vertical move must continue from the gutter column"
    );
}

#[test]
fn virtual_menu_command_paths_copy_cut_and_select_all() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 0, false);

    state.queue_command(EditorCommand::SelectAll);
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.virtual_text.selection_range(), Some(0..3));

    state.queue_command(EditorCommand::Copy);
    let (_, commands) = vframe_commands(&ctx, &mut state, vec![]);
    assert_eq!(copied_text(&commands).as_deref(), Some("abc"));

    state.queue_command(EditorCommand::Cut);
    let (out, commands) = vframe_commands(&ctx, &mut state, vec![]);
    assert_eq!(state.shared_text().as_str(), "");
    assert_eq!(copied_text(&commands).as_deref(), Some("abc"));
    assert!(
        out.changed,
        "a cut edits the buffer and must report changed"
    );
}

#[test]
fn virtual_menu_command_paths_undo_and_redo() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);
    assert!(!state.can_undo());
    assert!(!state.can_redo());

    vframe(&ctx, &mut state, vec![egui::Event::Text("abc".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "abc");
    assert!(state.can_undo());
    assert!(!state.can_redo());

    // The queued Edit-menu command reverts the run and arms redo.
    state.queue_command(EditorCommand::Undo);
    let out = vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.shared_text().as_str(), "");
    assert!(out.changed, "undo edits the buffer and must report changed");
    assert!(!state.can_undo());
    assert!(state.can_redo());

    // The paired command restores the text and re-arms undo.
    state.queue_command(EditorCommand::Redo);
    let out = vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.shared_text().as_str(), "abc");
    assert!(out.changed, "redo edits the buffer and must report changed");
    assert!(state.can_undo());
    assert!(!state.can_redo());
}

#[test]
fn virtual_diagnostic_squiggles_paint_without_panic() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "hello world\nsecond line");
    warm_virtual(&ctx, &mut state);

    let diagnostics = [typ_edit_model::Diag {
        path: std::path::PathBuf::from("probe.typ"),
        range: BytePos(0)..BytePos(5),
        message: "boom".to_owned(),
        severity: typ_edit_model::Severity::Error,
    }];
    let _ = vframe_with_diagnostics(&ctx, &mut state, &diagnostics);
}

#[test]
fn virtual_squiggle_rects_cover_the_expected_line_range() {
    let galley = unwrapped_galley("hello");
    let rects = crate::diagnostics::squiggle_rects(&galley, egui::Pos2::ZERO, 0..5);
    assert_eq!(rects.len(), 1, "one rect for a single-row range");
    assert!(rects[0].width() > 0.0);
    assert!(
        (rects[0].width() - galley.size().x).abs() < 1.0,
        "the squiggle spans the whole word"
    );
}

#[test]
fn virtual_band_and_bracket_pair_paint_without_panic() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "(abc)", 1, false);

    let _ = vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.virtual_text.caret(), 1);
}

#[test]
fn virtual_whitespace_paste_without_a_bitmap_is_a_noop() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 0, false);

    let out = vframe(&ctx, &mut state, vec![egui::Event::Paste(String::new())]);
    assert!(out.paste_image.is_none());
    assert_eq!(state.shared_text().as_str(), "abc");
}

// ---- EditHook caret sync --------------------------------------------------

fn undo_frame(ctx: &egui::Context, state: &mut EditorState) {
    vframe_mod(
        ctx,
        state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Z)],
        egui::Modifiers::COMMAND,
    );
}

#[test]
fn virtual_caret_follows_typing_space_and_undo_redo() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);

    for ch in ["a", " ", "b"] {
        vframe(&ctx, &mut state, vec![egui::Event::Text(ch.to_owned())]);
        let caret = state.virtual_text.caret();
        assert!(
            caret <= state.shared_text().chars().count(),
            "caret {caret} in bounds"
        );
    }
    assert_eq!(state.shared_text().as_str(), "a b");
    assert_eq!(
        state.virtual_text.caret(),
        3,
        "the space did not move the caret away"
    );

    for _ in 0..5 {
        undo_frame(&ctx, &mut state);
        let caret = state.virtual_text.caret();
        assert!(
            caret <= state.shared_text().chars().count(),
            "caret {caret} after undo"
        );
    }
    assert_eq!(state.shared_text().as_str(), "");
    assert_eq!(state.virtual_text.caret(), 0);

    for _ in 0..5 {
        vframe_mod(
            &ctx,
            &mut state,
            vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Y)],
            egui::Modifiers::COMMAND,
        );
    }
    assert_eq!(state.shared_text().as_str(), "a b");
    assert_eq!(state.virtual_text.caret(), 3, "redo lands after the edit");
}

#[test]
fn virtual_undo_after_selection_replacement_restores_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello world", 11, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::A)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.selection_range(), Some(0..11));

    vframe(&ctx, &mut state, vec![egui::Event::Text("X".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "X");
    assert_eq!(state.virtual_text.caret(), 1);

    undo_frame(&ctx, &mut state);
    assert_eq!(state.shared_text().as_str(), "hello world");
    assert_eq!(
        state.virtual_text.caret(),
        11,
        "undo restores the pre-edit caret from the hook"
    );
}

#[test]
fn virtual_caret_follows_an_insert_before_it() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "ab", 2, false);
    open_find_virtual(&mut state, "a", "XY");

    let outcome = state.run_find_actions(crate::find_bar::FindActions {
        replace_current: true,
        replace_all: false,
        close: false,
    });
    assert!(outcome.changed);
    assert_eq!(state.shared_text().as_str(), "XYb");
    assert_eq!(
        state.virtual_text.caret(),
        3,
        "an insert before the caret pushes it right"
    );
}

#[test]
fn virtual_replace_all_then_undo_keeps_the_caret_in_bounds() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo bar\nfoo baz", 11, false);
    open_find_virtual(&mut state, "foo", "qux");

    assert!(state.replace_all_matches());
    assert_eq!(state.shared_text().as_str(), "qux bar\nqux baz");
    let len = state.shared_text().chars().count();
    assert!(state.virtual_text.caret() <= len);

    undo_frame(&ctx, &mut state);
    assert_eq!(state.shared_text().as_str(), "foo bar\nfoo baz");
    assert!(state.virtual_text.caret() <= state.shared_text().chars().count());
}

#[test]
fn virtual_completion_accept_then_undo_restores_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "#", 1, false);
    let completions = vec![CompletionItem {
        name: "let".to_owned(),
        insert: "let ".to_owned(),
        kind: CompletionKind::Keyword,
        cursor_back: CharCount(0),
        docs: None,
    }];
    vframe_in(
        &ctx,
        &mut state,
        vec![egui::Event::Text("l".to_owned())],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert!(state.completion.is_some());
    vframe_in(
        &ctx,
        &mut state,
        vec![key_event(egui::Key::Enter)],
        egui::Modifiers::NONE,
        EditorInput {
            collab_history: false,
            remote_carets: &[],
            completions: &completions,
            ..empty_input()
        },
    );
    assert_eq!(state.shared_text().as_str(), "#let ");
    assert_eq!(state.virtual_text.caret(), 5);

    undo_frame(&ctx, &mut state);
    assert_eq!(state.shared_text().as_str(), "#");
    assert_eq!(state.virtual_text.caret(), 1);
}

#[test]
fn virtual_ime_commit_then_undo_restores_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "", 0, false);

    let preedit = |text: &str| {
        egui::Event::Ime(egui::ImeEvent::Preedit {
            text: text.to_owned(),
            active_range_chars: None,
        })
    };
    vframe(&ctx, &mut state, vec![preedit("にほ")]);
    vframe(
        &ctx,
        &mut state,
        vec![egui::Event::Ime(egui::ImeEvent::Commit("日本".to_owned()))],
    );
    assert_eq!(state.shared_text().as_str(), "日本");
    assert_eq!(state.virtual_text.caret(), 2);

    undo_frame(&ctx, &mut state);
    assert_eq!(state.shared_text().as_str(), "");
    assert_eq!(state.virtual_text.caret(), 0);
}

/// An `ImeEvent::Preedit` carrying `text` with no active range.
fn ime_preedit(text: &str) -> egui::Event {
    egui::Event::Ime(egui::ImeEvent::Preedit {
        text: text.to_owned(),
        active_range_chars: None,
    })
}

/// An `ImeEvent::Commit` carrying `text`.
fn ime_commit(text: &str) -> egui::Event {
    egui::Event::Ime(egui::ImeEvent::Commit(text.to_owned()))
}

#[test]
fn virtual_ime_commit_reaches_every_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo bar foo", 0, false);

    // Two selections over the two `foo` occurrences; Ctrl+D leaves the second
    // occurrence primary, which fixes the preedit target below.
    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 2);

    // The preedit composes at the primary (second `foo`) only; the other
    // selection keeps its text untouched.
    vframe(&ctx, &mut state, vec![ime_preedit("ni")]);
    assert_eq!(
        state.shared_text().as_str(),
        "foo bar ni",
        "preedit replaces only the primary selection"
    );
    assert_eq!(state.virtual_text.preedit_range, Some(8..10));
    assert_eq!(
        state.virtual_text.carets().list()[0].selection(),
        Some(0..3),
        "the extra caret still selects the untouched `foo`"
    );

    // Commit lands at both carets and removes the preedit intermediate text.
    vframe(&ctx, &mut state, vec![ime_commit("你")]);
    assert_eq!(state.shared_text().as_str(), "你 bar 你");
    assert_eq!(state.virtual_text.preedit_range, None);

    // One undo restores the original: preedit and commit share one group.
    assert!(state.buffer.undo(), "the composition is one undo group");
    assert_eq!(
        state.buffer.slice(0..state.buffer.len_chars()).as_str(),
        "foo bar foo"
    );
}

#[test]
fn virtual_ime_commit_without_preedit_inserts_at_every_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "ab\ncd", 1, false);

    // A column caret makes the list two collapsed carets on separate lines.
    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(
            egui::Modifiers::COMMAND | egui::Modifiers::ALT,
            egui::Key::ArrowDown,
        )],
        egui::Modifiers::COMMAND | egui::Modifiers::ALT,
    );
    assert_eq!(state.virtual_text.carets().len(), 2);

    vframe(&ctx, &mut state, vec![ime_commit("你")]);
    assert_eq!(
        state.shared_text().as_str(),
        "a你b\nc你d",
        "a preedit-less commit inserts at every caret"
    );
}

#[test]
fn virtual_pending_jump_lands_exactly_then_typing_advances() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef", 0, false);

    state.jump_to_byte(3);
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.virtual_text.caret(), 3, "the jump lands exactly");

    vframe(&ctx, &mut state, vec![egui::Event::Text("X".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "abcXdef");
    assert_eq!(state.virtual_text.caret(), 4);
}

#[test]
fn virtual_pending_jump_then_insert_lands_at_the_jumped_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef", 0, false);

    state.jump_to_byte(3);
    state.queue_insert_text("x".to_owned());
    vframe(&ctx, &mut state, vec![]);

    assert_eq!(
        state.shared_text().as_str(),
        "abcxdef",
        "the queued insert must land at the jumped caret, not the pre-jump one"
    );
    assert_eq!(state.virtual_text.caret(), 4);
}

#[test]
fn virtual_select_byte_range_selects_the_exact_range() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef", 0, false);

    state.select_byte_range(1, 4);
    vframe(&ctx, &mut state, vec![]);

    assert_eq!(state.virtual_text.selection_range(), Some(1..4));
    assert_eq!(
        state.virtual_text.caret(),
        4,
        "the caret lands at the range end"
    );
}

#[test]
fn virtual_select_byte_range_maps_multibyte_chars() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "你好 world", 0, false);

    // '你' = bytes 0..3 and '好' = bytes 3..6, so the range is chars 0..2.
    state.select_byte_range(0, 6);
    vframe(&ctx, &mut state, vec![]);

    assert_eq!(state.virtual_text.selection_range(), Some(0..2));
}

#[test]
fn virtual_select_byte_range_collapsed_moves_the_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef", 0, false);

    state.select_byte_range(3, 3);
    vframe(&ctx, &mut state, vec![]);

    assert_eq!(state.virtual_text.selection_range(), None);
    assert_eq!(state.virtual_text.caret(), 3);
}

#[test]
fn virtual_outline_jump_settles_a_wrapped_target_down_in_one_frame() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    // Wrapped lines make the fallback-height estimate too short, so a naive
    // jump lands above the target and converges only over several frames.
    let doc = (0..80)
        .map(|_| "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(4))
        .collect::<Vec<_>>()
        .join("\n");
    state.set_buffer_text(&doc);
    state.jump_to_byte(0);
    for _ in 0..2 {
        drive_virtual_frame_cfg(
            &ctx,
            &mut state,
            vec![],
            egui::Modifiers::NONE,
            true,
            WrapMode::Always,
        );
    }

    let target = 60usize;
    let target_byte = state.buffer.char_to_byte(state.buffer.line_start(target));
    state.select_byte_range(target_byte, target_byte + 1);
    drive_virtual_frame_cfg(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        false,
        WrapMode::Always,
    );

    assert!(
        state.virtual_text_visible_lines().contains(&target),
        "the target line must be visible after one frame"
    );
    assert!(
        state
            .virtual_text
            .test_line_origins
            .iter()
            .any(|(line, _, _)| *line == target),
        "the painted slice must contain the target"
    );
}

#[test]
fn virtual_jump_up_settles_a_target_above_in_one_frame() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let doc = (0..80)
        .map(|_| "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(4))
        .collect::<Vec<_>>()
        .join("\n");
    state.set_buffer_text(&doc);
    state.jump_to_byte(state.buffer.char_to_byte(state.buffer.line_start(70)));
    for _ in 0..2 {
        drive_virtual_frame_cfg(
            &ctx,
            &mut state,
            vec![],
            egui::Modifiers::NONE,
            true,
            WrapMode::Always,
        );
    }

    let target = 2usize;
    let target_byte = state.buffer.char_to_byte(state.buffer.line_start(target));
    state.jump_to_byte(target_byte);
    drive_virtual_frame_cfg(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        false,
        WrapMode::Always,
    );

    assert!(
        state.virtual_text_visible_lines().contains(&target),
        "an upward jump must also land in one frame"
    );
}

/// Viewport-relative `(top, bottom)` of painted `line`.
fn painted_viewport_band(state: &EditorState, line: usize) -> (f32, f32) {
    let viewport = virtual_viewport(state);
    let (_, y, height) = state
        .virtual_text
        .test_line_origins
        .iter()
        .copied()
        .find(|(index, _, _)| *index == line)
        .expect("the line is painted");
    let top = y - viewport.top();
    (top, top + height)
}

/// A long wrapped document, matching the preview double-click flow: the caret
/// jumps far below via `jump_to_byte` while the editor is still unfocused.
fn preview_jump_doc() -> String {
    (0..200)
        .map(|i| {
            format!("line {i} ")
                + &"lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(4)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn diag_preview_jump_bands() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    for (label, mid) in [("start", 0usize), ("mid", 100), ("end", 200)] {
        let mut state = EditorState::new();
        state.set_file(std::path::PathBuf::from("probe.typ"), preview_jump_doc());
        let target = 150usize;
        let line_start = state.buffer.line_start(target);
        let next = state.buffer.line_start(target + 1);
        let line_len = next.saturating_sub(line_start).saturating_sub(1);
        let off = mid.min(line_len.saturating_sub(1));
        state.jump_to_byte(state.buffer.char_to_byte(line_start + off));
        eprintln!("=== {label} off={off} line_len={line_len}");
        for f in 0..8 {
            drive_virtual_frame_cfg(
                &ctx,
                &mut state,
                vec![],
                egui::Modifiers::NONE,
                f == 0,
                WrapMode::Always,
            );
            let vp = virtual_viewport(&state);
            if let Some((_, y, h)) = state
                .virtual_text
                .test_line_origins
                .iter()
                .copied()
                .find(|(l, _, _)| *l == target)
            {
                eprintln!(
                    "frame {f}: top={:.1} bottom={:.1} vp_h={:.1} settle={:?} anchor={:?}",
                    y - vp.top(),
                    y - vp.top() + h,
                    vp.height(),
                    state.virtual_text.pending_scroll_settle,
                    state.virtual_text.anchor
                );
            } else {
                eprintln!(
                    "frame {f}: target NOT painted settle={:?} anchor={:?}",
                    state.virtual_text.pending_scroll_settle, state.virtual_text.anchor
                );
            }
        }
    }
}

#[test]
fn virtual_preview_jump_lands_fully_visible_with_margin() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_file(std::path::PathBuf::from("probe.typ"), preview_jump_doc());

    // The preview double-click queues a `jump_to_byte` and requests focus; the
    // frame below runs with the widget still unfocused.
    let target = 150usize;
    let target_byte = state.buffer.char_to_byte(state.buffer.line_start(target));
    state.jump_to_byte(target_byte);
    drive_virtual_frame_cfg(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        true,
        WrapMode::Always,
    );
    for _ in 0..5 {
        drive_virtual_frame_cfg(
            &ctx,
            &mut state,
            vec![],
            egui::Modifiers::NONE,
            false,
            WrapMode::Always,
        );
    }

    let viewport_h = virtual_viewport(&state).height();
    let (top, bottom) = painted_viewport_band(&state, target);
    assert!(
        top >= 1.0,
        "the target top {top} must clear the viewport top"
    );
    assert!(
        bottom <= viewport_h - 1.0,
        "the target bottom {bottom} must clear the viewport bottom (viewport {viewport_h})"
    );
}

#[test]
fn set_text_clears_the_pending_selection() {
    let mut state = EditorState::new();
    state.set_buffer_text("abcdef");
    state.select_byte_range(1, 4);
    assert!(state.pending_select.is_some(), "the range is queued");

    state.set_text("xyz".to_owned());

    assert!(
        state.pending_select.is_none(),
        "a buffer swap drops the queued range"
    );
}

#[test]
fn virtual_pending_jump_then_range_resolves_to_the_range() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef", 0, false);

    state.jump_to_byte(0);
    state.select_byte_range(2, 5);
    vframe(&ctx, &mut state, vec![]);

    assert_eq!(
        state.virtual_text.selection_range(),
        Some(2..5),
        "the range overrides the plain jump"
    );
    assert_eq!(state.virtual_text.caret(), 5);
}

// ---- Long-line window shaping ---------------------------------------------

fn long_line_state(ctx: &egui::Context) -> EditorState {
    let mut state = EditorState::new();
    state.set_file(std::path::PathBuf::from("probe.typ"), "a".repeat(1_000_000));
    state.jump_to_byte(0);
    for request_focus in [true, false] {
        drive_virtual_frame_cfg(
            ctx,
            &mut state,
            vec![],
            egui::Modifiers::NONE,
            request_focus,
            WrapMode::Off,
        );
    }
    state
}

#[test]
fn virtual_long_line_frame_shapes_only_a_bounded_window() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = long_line_state(&ctx);

    vframe(&ctx, &mut state, vec![]);
    assert!(
        state.virtual_text.cache.shaped_chars_this_frame() < 10_000,
        "one frame shaped {} chars",
        state.virtual_text.cache.shaped_chars_this_frame()
    );
    let (_, len, placeholder) = state
        .virtual_text
        .cache
        .cached_window(0)
        .expect("the long line is cached");
    assert!(!placeholder);
    assert!(len < 10_000);
}

#[test]
fn virtual_long_line_caret_outside_window_recenters() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = long_line_state(&ctx);

    let target = 900_000usize;
    state.jump_to_byte(target);
    vframe(&ctx, &mut state, vec![]);

    assert!(state.virtual_text.h_offset > 0.0, "the window recentred");
    assert_eq!(state.virtual_text.caret(), target);
    let (start, len, placeholder) = state
        .virtual_text
        .cache
        .cached_window(0)
        .expect("the long line is cached");
    assert!(!placeholder);
    assert!(
        start <= target && target <= start + len,
        "window {start}..{} must cover {target}",
        start + len
    );
}

#[test]
fn virtual_long_line_wrap_mode_uses_horizontal_windowing() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_file(std::path::PathBuf::from("probe.typ"), "a".repeat(1_000_000));
    state.jump_to_byte(0);
    for request_focus in [true, false] {
        drive_virtual_frame_cfg(
            &ctx,
            &mut state,
            vec![],
            egui::Modifiers::NONE,
            request_focus,
            WrapMode::Always,
        );
    }
    let (_, len, placeholder) = state
        .virtual_text
        .cache
        .cached_window(0)
        .expect("the long line is cached");
    assert!(!placeholder);
    assert!(len < 10_000, "wrap mode still shapes only the window");
}

// ---- WrapMode, horizontal scrollbar, shift-wheel and touch ---------------

/// One virtual frame with a caller-chosen wrap mode and blocked (code/math/raw)
/// byte ranges.
fn drive_virtual_input(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
    wrap_mode: WrapMode,
    blocked_ranges: &[std::ops::Range<usize>],
) {
    let blocked_ranges = byte_ranges(blocked_ranges);
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events,
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.request_focus = true;
        input.wrap_mode = wrap_mode;
        input.blocked_ranges = &blocked_ranges;
        state.show(ui, input);
    })
    .drop_without_applying_deltas();
}

/// Byte-range sets a wrap/momentum test frame feeds the editor.
#[derive(Clone, Copy, Default)]
struct Ranges<'a> {
    blocked: &'a [std::ops::Range<usize>],
    code: &'a [std::ops::Range<usize>],
    math: &'a [std::ops::Range<usize>],
    math_atoms: &'a [std::ops::Range<usize>],
    highlights: &'a [(std::ops::Range<usize>, HighlightKind)],
}

/// One virtual frame with wrap mode, range sets and an explicit input clock;
/// returns the full egui output.
fn drive_virtual_ex(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
    wrap_mode: WrapMode,
    ranges: Ranges<'_>,
    time: f64,
) -> egui::FullOutput {
    let blocked = byte_ranges(ranges.blocked);
    let code = byte_ranges(ranges.code);
    let math = byte_ranges(ranges.math);
    let math_atoms = byte_ranges(ranges.math_atoms);
    let highlights = byte_highlights(ranges.highlights);
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events,
        time: Some(time),
        ..Default::default()
    };
    let mut output = ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.request_focus = true;
        input.wrap_mode = wrap_mode;
        input.blocked_ranges = &blocked;
        input.code_ranges = &code;
        input.math_ranges = &math;
        input.math_atoms = &math_atoms;
        input.highlights = &highlights;
        state.show(ui, input);
    });
    output.textures_delta.clear();
    output
}

/// One plain-text virtual frame with wrap mode and range sets.
fn drive_plain_ex(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
    wrap_mode: WrapMode,
    ranges: Ranges<'_>,
) -> egui::FullOutput {
    let blocked = byte_ranges(ranges.blocked);
    let code = byte_ranges(ranges.code);
    let math = byte_ranges(ranges.math);
    let math_atoms = byte_ranges(ranges.math_atoms);
    let highlights = byte_highlights(ranges.highlights);
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.request_focus = true;
        input.wrap_mode = wrap_mode;
        input.plain_text = true;
        input.blocked_ranges = &blocked;
        input.code_ranges = &code;
        input.math_ranges = &math;
        input.math_atoms = &math_atoms;
        input.highlights = &highlights;
        state.show(ui, input);
    });
    output.textures_delta.clear();
    output
}

/// One virtual frame at a caller-chosen screen width, for the horizontal
/// max/viewport interaction tests.
fn drive_virtual_screen(
    ctx: &egui::Context,
    state: &mut EditorState,
    width: f32,
    events: Vec<egui::Event>,
) {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, 400.0),
        )),
        events,
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| {
        state.show(ui, empty_input());
    })
    .drop_without_applying_deltas();
}

fn mouse_wheel(delta: egui::Vec2, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        phase: egui::TouchPhase::Move,
        modifiers,
    }
}

fn line_wheel(delta: egui::Vec2) -> egui::Event {
    egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta,
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    }
}

fn touch_event(id: u64, phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(id),
        phase,
        pos,
        force: None,
    }
}

fn wrap_over(ctx: &egui::Context, state: &mut EditorState, text: &str, mode: WrapMode) {
    state.set_buffer_text(text);
    for _ in 0..2 {
        drive_virtual_input(ctx, state, vec![], mode, &[]);
    }
}

#[test]
fn markup_typing_preserves_anchor_and_horizontal_scroll() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let long = "b".repeat(400);
    let mut text = String::new();
    for line in 0..80 {
        if line == 40 {
            text.push_str(&long);
            text.push('\n');
        } else {
            text.push_str(&format!("line {line}\n"));
        }
    }
    let long_start = text.find(&long).expect("long line");
    let long_end = long_start + long.len();
    let ranges = long_start..long_end;
    let mut state = EditorState::new();
    state.set_buffer_text(&text);
    state.virtual_text.anchor.top_line = 40;
    state.virtual_text.set_caret(long_start + 200);
    state.virtual_text.h_offset = 500.0;
    for _ in 0..3 {
        drive_virtual_input(
            &ctx,
            &mut state,
            vec![],
            WrapMode::Markup,
            std::slice::from_ref(&ranges),
        );
    }
    let before = state.virtual_text.anchor;
    assert!(state.virtual_text.h_offset > 0.0);
    drive_virtual_input(
        &ctx,
        &mut state,
        vec![egui::Event::Text("x".to_owned())],
        WrapMode::Markup,
        std::slice::from_ref(&ranges),
    );
    assert_eq!(
        state.virtual_text.anchor.top_line, before.top_line,
        "typing must not scroll vertically"
    );
    assert!(
        (state.virtual_text.anchor.offset - before.offset).abs() < 0.5,
        "typing must not change the anchor offset"
    );
    assert!(
        state.virtual_text.h_offset > 0.0,
        "typing must not reset h_offset"
    );
    assert_eq!(
        state
            .virtual_text
            .cache
            .cached_galley(40)
            .map(|g| g.rows.len()),
        Some(1),
        "the stale blocked range must keep the long line single-row"
    );
}

#[test]
fn horizontal_scrollbar_appears_only_for_unwrapped_overflow() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let wide = "a".repeat(400);

    let mut off = EditorState::new();
    wrap_over(&ctx, &mut off, &wide, WrapMode::Off);
    assert!(
        off.virtual_text.test_hscrollbar.is_some(),
        "an unwrapped overflowing line shows the bar"
    );

    let mut always = EditorState::new();
    wrap_over(&ctx, &mut always, &wide, WrapMode::Always);
    assert!(
        always.virtual_text.test_hscrollbar.is_none(),
        "Always wraps everything, so no bar"
    );

    let mut markup = EditorState::new();
    wrap_over(&ctx, &mut markup, &wide, WrapMode::Markup);
    assert!(
        markup.virtual_text.test_hscrollbar.is_none(),
        "an all-markup line wraps, so no bar"
    );

    let mut blocked = EditorState::new();
    blocked.set_buffer_text(&wide);
    let ranges_range = 0..wide.len();
    let ranges = std::slice::from_ref(&ranges_range);
    for _ in 0..2 {
        drive_virtual_input(&ctx, &mut blocked, vec![], WrapMode::Markup, ranges);
    }
    assert!(
        blocked.virtual_text.test_hscrollbar.is_some(),
        "a blocked line stays one row and overflows"
    );
}

#[test]
fn horizontal_scrollbar_drag_changes_h_offset() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &"a".repeat(400), WrapMode::Off);

    let bar = state
        .virtual_text
        .test_hscrollbar
        .expect("the wide line overflows");
    let press = egui::pos2(bar.left() + 4.0, bar.center().y);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            pointer_moved(press),
            pointer_button(press, true, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![pointer_moved(press + egui::vec2(60.0, 0.0))],
        egui::Modifiers::NONE,
    );
    assert!(
        state.virtual_text.h_offset > 0.0,
        "dragging the thumb pans the content, got {}",
        state.virtual_text.h_offset
    );
}

#[test]
fn shift_wheel_scrolls_horizontally_like_the_horizontal_wheel() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let wide = "a".repeat(400);

    let mut plained = EditorState::new();
    wrap_over(&ctx, &mut plained, &wide, WrapMode::Off);
    let at = virtual_line_center(&plained, 0);
    drive_virtual_frame(
        &ctx,
        &mut plained,
        vec![
            pointer_moved(at),
            mouse_wheel(egui::vec2(-5.0, 0.0), egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    let horizontal = plained.virtual_text.h_offset;

    let mut shifted = EditorState::new();
    wrap_over(&ctx, &mut shifted, &wide, WrapMode::Off);
    let at = virtual_line_center(&shifted, 0);
    drive_virtual_frame(
        &ctx,
        &mut shifted,
        vec![
            pointer_moved(at),
            mouse_wheel(egui::vec2(0.0, -5.0), egui::Modifiers::SHIFT),
        ],
        egui::Modifiers::SHIFT,
    );
    let shift = shifted.virtual_text.h_offset;

    assert!(horizontal > 0.0, "the horizontal wheel scrolled");
    assert!(shift > 0.0, "shift+wheel scrolled horizontally");
    assert!(
        (shift - horizontal).abs() < 0.001,
        "shift+wheel shares the horizontal wheel convention: {shift} vs {horizontal}"
    );
}

fn repaint_delay(output: &egui::FullOutput) -> std::time::Duration {
    output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(std::time::Duration::MAX, |viewport| viewport.repaint_delay)
}

#[test]
fn active_scroll_state_repaints_even_on_a_zero_delta_frame() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &numbered_lines(300), WrapMode::Off);
    let at = virtual_line_center(&state, 5);

    let mut first = drive_virtual_ex(
        &ctx,
        &mut state,
        vec![
            pointer_moved(at),
            // A zero-delta Move still puts egui's WheelState into Smoothing.
            mouse_wheel(egui::Vec2::ZERO, egui::Modifiers::NONE),
        ],
        WrapMode::Off,
        Ranges::default(),
        0.02,
    );
    first.textures_delta.clear();
    let mut second = drive_virtual_ex(
        &ctx,
        &mut state,
        vec![pointer_moved(at)],
        WrapMode::Off,
        Ranges::default(),
        0.036,
    );
    second.textures_delta.clear();

    assert_eq!(
        repaint_delay(&second),
        std::time::Duration::ZERO,
        "egui is still scrolling, so the widget must ask for the next frame even with zero delta"
    );
    assert!(
        state.virtual_text.momentum.is_none(),
        "the zero delta should not create a fake inertia tail"
    );
}

#[test]
fn wheel_scroll_requests_an_immediate_repaint_each_frame() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &numbered_lines(300), WrapMode::Off);

    let at = virtual_line_center(&state, 5);
    let first = drive_virtual_ex(
        &ctx,
        &mut state,
        vec![
            pointer_moved(at),
            mouse_wheel(egui::vec2(0.0, -60.0), egui::Modifiers::NONE),
        ],
        WrapMode::Off,
        Ranges::default(),
        1.0,
    );
    assert_eq!(
        repaint_delay(&first),
        std::time::Duration::ZERO,
        "the wheel frame must schedule the next frame immediately"
    );

    for step in 1..=4 {
        let frame = drive_virtual_ex(
            &ctx,
            &mut state,
            vec![],
            WrapMode::Off,
            Ranges::default(),
            1.0 + 0.016 * step as f64,
        );
        assert_eq!(
            repaint_delay(&frame),
            std::time::Duration::ZERO,
            "scrolling frame {step} must schedule the next frame immediately"
        );
    }
}

#[test]
fn idle_scroll_frame_normalizes_without_waiting_for_a_timer() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &numbered_lines(300), WrapMode::Off);
    state.virtual_text.anchor.top_line = 0;
    state.virtual_text.anchor.offset = 50.0;
    state.virtual_text.last_scroll_time = 0.0;
    state.virtual_text.momentum = None;

    drive_virtual_ex(
        &ctx,
        &mut state,
        vec![],
        WrapMode::Off,
        Ranges::default(),
        1.0,
    );

    assert!(
        state.virtual_text.anchor.top_line > 0,
        "the whole-line carry should happen on this idle frame: {:?}",
        state.virtual_text.anchor
    );
    assert!(
        state.virtual_text.anchor.offset < 50.0,
        "the offset should be compacted on this idle frame: {:?}",
        state.virtual_text.anchor
    );
}

#[test]
fn line_trackpad_at_30_hz_scrolls_evenly() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &numbered_lines(300), WrapMode::Off);

    let at = virtual_line_center(&state, 5);
    drive_virtual_ex(
        &ctx,
        &mut state,
        vec![pointer_moved(at)],
        WrapMode::Off,
        Ranges::default(),
        1.0,
    );
    let line_h = state
        .virtual_text
        .test_line_origins
        .first()
        .map_or(16.0, |(_, _, height)| *height);
    let scroll_abs = |state: &EditorState| {
        let (line, offset) = scroll_position(state);
        line as f32 * line_h + offset
    };

    let mut previous = scroll_abs(&state);
    let mut steps = Vec::new();
    for frame in 0..30 {
        let events = if frame % 2 == 0 {
            vec![line_wheel(egui::vec2(0.0, -1.0 / 120.0))]
        } else {
            Vec::new()
        };
        drive_virtual_ex(
            &ctx,
            &mut state,
            events,
            WrapMode::Off,
            Ranges::default(),
            1.0 + 0.016 * (frame + 1) as f64,
        );
        let now = scroll_abs(&state);
        steps.push(now - previous);
        previous = now;
    }
    let steady = &steps[12..];
    assert!(
        steady.iter().all(|step| *step > 0.0),
        "a 30 Hz line stream must scroll on every steady-state frame: {steps:?}"
    );
    assert!(
        steady.windows(2).all(|pair| pair[1] >= pair[0] - 0.01),
        "the trackpad stream must not restart from zero: {steps:?}"
    );
}

#[test]
fn mouse_wheel_advances_with_egui_smoothing_without_a_second_tail() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &numbered_lines(300), WrapMode::Off);

    let at = virtual_line_center(&state, 5);
    drive_virtual_ex(
        &ctx,
        &mut state,
        vec![
            pointer_moved(at),
            mouse_wheel(egui::vec2(0.0, -60.0), egui::Modifiers::NONE),
        ],
        WrapMode::Off,
        Ranges::default(),
        1.0,
    );
    let after_wheel = scroll_position(&state);
    assert!(
        after_wheel > (0, 0.0),
        "the first smoothed wheel frame scrolls"
    );
    assert!(
        state.virtual_text.momentum.is_none(),
        "wheel input must not seed the two-finger coast"
    );

    let mut previous = after_wheel;
    for step in 1..=4 {
        drive_virtual_ex(
            &ctx,
            &mut state,
            vec![],
            WrapMode::Off,
            Ranges::default(),
            1.0 + 0.016 * step as f64,
        );
        let now = scroll_position(&state);
        assert!(
            now > previous,
            "egui's smoothed wheel must advance without a gap: step {step}, {previous:?} -> {now:?}"
        );
        previous = now;
    }
}

#[test]
fn two_finger_drag_pans_both_axes() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let text = numbered_lines(200);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &text, WrapMode::Off);

    let viewport = virtual_viewport(&state);
    let a = egui::pos2(viewport.left() + 60.0, viewport.top() + 40.0);
    let b = egui::pos2(viewport.left() + 260.0, viewport.top() + 40.0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            touch_event(1, egui::TouchPhase::Start, a),
            touch_event(2, egui::TouchPhase::Start, b),
        ],
        egui::Modifiers::NONE,
    );
    assert!(state.virtual_text.multi_touch, "two fingers engage the pan");
    let lift = egui::vec2(0.0, -25.0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            touch_event(1, egui::TouchPhase::Move, a + lift),
            touch_event(2, egui::TouchPhase::Move, b + lift),
        ],
        egui::Modifiers::NONE,
    );
    assert!(
        state.virtual_text.anchor.top_line > 0 || state.virtual_text.anchor.offset > 0.0,
        "a vertical two-finger drag moves the anchor"
    );

    let mut wide = EditorState::new();
    wrap_over(&ctx, &mut wide, &"a".repeat(400), WrapMode::Off);
    let viewport = virtual_viewport(&wide);
    let a = egui::pos2(viewport.left() + 60.0, viewport.top() + 40.0);
    let b = egui::pos2(viewport.left() + 260.0, viewport.top() + 40.0);
    drive_virtual_frame(
        &ctx,
        &mut wide,
        vec![
            touch_event(1, egui::TouchPhase::Start, a),
            touch_event(2, egui::TouchPhase::Start, b),
        ],
        egui::Modifiers::NONE,
    );
    let swipe = egui::vec2(-25.0, 0.0);
    drive_virtual_frame(
        &ctx,
        &mut wide,
        vec![
            touch_event(1, egui::TouchPhase::Move, a + swipe),
            touch_event(2, egui::TouchPhase::Move, b + swipe),
        ],
        egui::Modifiers::NONE,
    );
    assert!(
        wide.virtual_text.h_offset > 0.0,
        "a horizontal two-finger drag changes h_offset"
    );

    drive_virtual_frame(
        &ctx,
        &mut wide,
        vec![
            touch_event(1, egui::TouchPhase::End, a),
            touch_event(2, egui::TouchPhase::Cancel, b),
        ],
        egui::Modifiers::NONE,
    );
    assert!(
        wide.virtual_text.touches.is_empty() && !wide.virtual_text.multi_touch,
        "ending/cancelling touches clears the touch state"
    );
}

#[test]
fn single_touch_still_selects_but_multi_touch_does_not() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);

    let mut tap = EditorState::new();
    wrap_over(&ctx, &mut tap, "abc\ndef", WrapMode::Off);
    let at = virtual_line_center(&tap, 0);
    drive_virtual_frame(
        &ctx,
        &mut tap,
        vec![
            touch_event(1, egui::TouchPhase::Start, at),
            pointer_moved(at),
            pointer_button(at, true, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    drive_virtual_frame(
        &ctx,
        &mut tap,
        vec![
            touch_event(1, egui::TouchPhase::End, at),
            pointer_button(at, false, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        tap.virtual_text.caret(),
        0,
        "a single finger still drives the pointer selection"
    );

    let mut pinch = EditorState::new();
    wrap_over(&ctx, &mut pinch, "abc\ndef\nghi", WrapMode::Off);
    let start = virtual_line_center(&pinch, 0);
    let other = virtual_line_center(&pinch, 1);
    drive_virtual_frame(
        &ctx,
        &mut pinch,
        vec![
            touch_event(1, egui::TouchPhase::Start, start),
            touch_event(2, egui::TouchPhase::Start, other),
            pointer_moved(start),
            pointer_button(start, true, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    let end = virtual_line_center(&pinch, 1) + egui::vec2(40.0, 0.0);
    drive_virtual_frame(
        &ctx,
        &mut pinch,
        vec![
            touch_event(1, egui::TouchPhase::Move, start),
            touch_event(2, egui::TouchPhase::Move, other),
            pointer_moved(end),
        ],
        egui::Modifiers::NONE,
    );
    drive_virtual_frame(
        &ctx,
        &mut pinch,
        vec![pointer_button(end, false, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        pinch.virtual_text.selection_range(),
        None,
        "a multi-touch pan must not start a selection"
    );
}

// ---- Shared horizontal maximum: clamp, extent, scrollbar and caret ---------

#[test]
fn horizontal_overscroll_clamps_to_the_shared_max() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &"a".repeat(400), WrapMode::Off);
    let max = state.virtual_text.max_h_offset;
    assert!(max > 0.0, "the wide line overflows");

    let at = virtual_line_center(&state, 0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            pointer_moved(at),
            mouse_wheel(egui::vec2(-100_000.0, 0.0), egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        state.virtual_text.h_offset, max,
        "the horizontal wheel reaches but cannot pass the shared max"
    );

    state.virtual_text.h_offset = 0.0;
    let at = virtual_line_center(&state, 0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            pointer_moved(at),
            mouse_wheel(egui::vec2(0.0, -100_000.0), egui::Modifiers::SHIFT),
        ],
        egui::Modifiers::SHIFT,
    );
    assert_eq!(
        state.virtual_text.h_offset, max,
        "shift+wheel shares the same maximum"
    );

    state.virtual_text.h_offset = 0.0;
    let viewport = virtual_viewport(&state);
    let a = egui::pos2(viewport.left() + 60.0, viewport.top() + 40.0);
    let b = egui::pos2(viewport.left() + 260.0, viewport.top() + 40.0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            touch_event(1, egui::TouchPhase::Start, a),
            touch_event(2, egui::TouchPhase::Start, b),
        ],
        egui::Modifiers::NONE,
    );
    let swipe = egui::vec2(-100_000.0, 0.0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            touch_event(1, egui::TouchPhase::Move, a + swipe),
            touch_event(2, egui::TouchPhase::Move, b + swipe),
        ],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        state.virtual_text.h_offset, max,
        "a two-finger pan shares the same maximum"
    );
}

#[test]
fn long_line_overscroll_keeps_the_window_non_empty() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = long_line_state(&ctx);
    let max = state.virtual_text.max_h_offset;

    let at = virtual_line_center(&state, 0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![
            pointer_moved(at),
            mouse_wheel(egui::vec2(-1_000_000.0, 0.0), egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    assert!(
        state.virtual_text.h_offset <= max + 0.5,
        "the wheel cannot pass the shared max"
    );

    // A stale/injected offset past the end must be pulled back by the frame clamp.
    state.virtual_text.h_offset = max + 1_000_000.0;
    drive_virtual_frame(&ctx, &mut state, vec![], egui::Modifiers::NONE);
    assert_eq!(
        state.virtual_text.h_offset, max,
        "clamped to the shared max"
    );

    let (start, len, placeholder) = state
        .virtual_text
        .cache
        .cached_window(0)
        .expect("the long line is cached");
    assert!(!placeholder);
    assert!(len > 0, "an overscrolled long line still shows content");
    assert_eq!(
        start + len,
        1_000_000,
        "the window reaches the line end instead of going empty"
    );
}

#[test]
fn cjk_line_extent_equals_its_shaped_width() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &"文字".repeat(100), WrapMode::Off);
    assert!(
        state.virtual_text.max_h_offset > 0.0,
        "the CJK line overflows"
    );
    let galley_width = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the CJK line is shaped")
        .size()
        .x;
    let extent = state.virtual_text.max_h_offset + state.virtual_text.h_viewport_width;
    assert!(
        (extent - galley_width).abs() < 0.5,
        "extent {extent} must be the shaped width {galley_width}, not chars × advance"
    );
}

#[test]
fn horizontal_thumb_sits_at_the_end_when_scrolled_to_max() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &"a".repeat(400), WrapMode::Off);
    let max = state.virtual_text.max_h_offset;
    assert!(max > 0.0);

    state.virtual_text.h_offset = max;
    drive_virtual_frame(&ctx, &mut state, vec![], egui::Modifiers::NONE);

    let bar = state
        .virtual_text
        .test_hscrollbar
        .expect("the horizontal bar is drawn");
    let thumb = state
        .virtual_text
        .test_hscrollbar_thumb
        .expect("the thumb is drawn");
    assert!(
        (thumb.right() - bar.right()).abs() < 0.5,
        "the thumb sits at the far end: thumb.right {} vs bar.right {}",
        thumb.right(),
        bar.right()
    );
}

#[test]
fn ctrl_end_on_a_long_line_scrolls_to_max_with_the_end_visible() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = long_line_state(&ctx);
    let max = state.virtual_text.max_h_offset;
    assert!(max > 0.0);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::End)],
        egui::Modifiers::COMMAND,
    );

    assert_eq!(
        state.virtual_text.caret(),
        1_000_000,
        "Ctrl+End reaches the end"
    );
    assert!(
        (state.virtual_text.h_offset - max).abs() < 0.5,
        "Ctrl+End scrolls to the shared max: {} vs {max}",
        state.virtual_text.h_offset
    );
    let (start, len, placeholder) = state
        .virtual_text
        .cache
        .cached_window(0)
        .expect("the long line is cached");
    assert!(!placeholder);
    assert_eq!(
        start + len,
        1_000_000,
        "the line end stays visible after Ctrl+End"
    );
}

#[test]
fn viewport_widening_shrinks_the_max_and_reclamps() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &"a".repeat(400), WrapMode::Off);
    let max_narrow = state.virtual_text.max_h_offset;
    assert!(max_narrow > 0.0);

    state.virtual_text.h_offset = max_narrow;
    drive_virtual_screen(&ctx, &mut state, 1200.0, vec![]);

    let max_wide = state.virtual_text.max_h_offset;
    assert!(
        max_wide < max_narrow,
        "a wider viewport shrinks the max: {max_wide} !< {max_narrow}"
    );
    assert!(
        (state.virtual_text.h_offset - max_wide).abs() < 0.5,
        "h_offset is re-clamped to the narrower max: {} vs {max_wide}",
        state.virtual_text.h_offset
    );
}

// ---- Scrollbar idle/hover styling -----------------------------------------

#[test]
fn scrollbar_hover_does_not_change_reserved_space_or_reshape() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &"word ".repeat(200), WrapMode::Markup);

    let reserved = state.virtual_text.h_viewport_width;
    let galley = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the wrapped line is shaped");
    let rows = galley.rows.len();
    let size = galley.size();
    let track = state.virtual_text.test_hscrollbar;
    assert!(rows >= 2, "the line wraps");

    let viewport = virtual_viewport(&state);
    let hover = egui::pos2(viewport.right() - 6.0, viewport.top() + 5.0);
    for _ in 0..3 {
        drive_virtual_input(
            &ctx,
            &mut state,
            vec![pointer_moved(hover)],
            WrapMode::Markup,
            &[],
        );
    }
    assert!(
        state.virtual_text.test_scrollbar_hovered,
        "the pointer hovers the edge strip"
    );

    assert_eq!(
        state.virtual_text.h_viewport_width, reserved,
        "the reserved width is constant across hover"
    );
    assert_eq!(
        state.virtual_text.test_hscrollbar, track,
        "the horizontal track is constant across hover"
    );
    let after = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the line is still shaped");
    assert_eq!(after.rows.len(), rows, "hovering must not rewrap the text");
    assert_eq!(after.size(), size, "hovering must not reshape the text");
}

#[test]
fn scrollbar_hover_flips_with_the_pointer_over_the_strip() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    wrap_over(&ctx, &mut state, &"a".repeat(400), WrapMode::Off);
    assert!(
        state.virtual_text.test_hscrollbar.is_some(),
        "the wide line overflows"
    );

    let viewport = virtual_viewport(&state);
    let away = egui::pos2(viewport.left() + 30.0, viewport.top() + 30.0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![pointer_moved(away)],
        egui::Modifiers::NONE,
    );
    assert!(!state.virtual_text.test_scrollbar_hovered);
    assert!(!state.virtual_text.test_hscrollbar_hovered);

    let vertical = egui::pos2(viewport.right() - 6.0, viewport.center().y);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![pointer_moved(vertical)],
        egui::Modifiers::NONE,
    );
    assert!(
        state.virtual_text.test_scrollbar_hovered,
        "the pointer hovers the vertical strip"
    );

    let horizontal = egui::pos2(viewport.left() + 40.0, viewport.bottom() - 6.0);
    drive_virtual_frame(
        &ctx,
        &mut state,
        vec![pointer_moved(horizontal)],
        egui::Modifiers::NONE,
    );
    assert!(
        state.virtual_text.test_hscrollbar_hovered,
        "the pointer hovers the horizontal strip"
    );
    assert!(
        !state.virtual_text.test_scrollbar_hovered,
        "the pointer left the vertical strip"
    );
}

/// Anchor scroll position `(top_line, offset)`, ordered so a larger value means
/// the content scrolled further down.
fn scroll_position(state: &EditorState) -> (usize, f32) {
    (
        state.virtual_text.anchor.top_line,
        state.virtual_text.anchor.offset,
    )
}

#[test]
fn touch_flick_coasts_after_release() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_buffer_text(&numbered_lines(300));
    for _ in 0..2 {
        drive_virtual_ex(
            &ctx,
            &mut state,
            vec![],
            WrapMode::Off,
            Ranges::default(),
            0.0,
        );
    }

    let viewport = virtual_viewport(&state);
    let a = egui::pos2(viewport.left() + 60.0, viewport.top() + 150.0);
    let b = egui::pos2(viewport.left() + 260.0, viewport.top() + 150.0);
    let lifted = |point: egui::Pos2, dy: f32| egui::pos2(point.x, point.y - dy);

    drive_virtual_ex(
        &ctx,
        &mut state,
        vec![
            touch_event(1, egui::TouchPhase::Start, a),
            touch_event(2, egui::TouchPhase::Start, b),
        ],
        WrapMode::Off,
        Ranges::default(),
        0.0,
    );
    let flick = |ctx: &egui::Context, state: &mut EditorState, dy: f32, time: f64| {
        drive_virtual_ex(
            ctx,
            state,
            vec![
                touch_event(1, egui::TouchPhase::Move, lifted(a, dy)),
                touch_event(2, egui::TouchPhase::Move, lifted(b, dy)),
            ],
            WrapMode::Off,
            Ranges::default(),
            time,
        )
    };
    flick(&ctx, &mut state, 30.0, 0.05);
    flick(&ctx, &mut state, 60.0, 0.10);
    let before_release = scroll_position(&state);
    drive_virtual_ex(
        &ctx,
        &mut state,
        vec![
            touch_event(1, egui::TouchPhase::End, lifted(a, 60.0)),
            touch_event(2, egui::TouchPhase::End, lifted(b, 60.0)),
        ],
        WrapMode::Off,
        Ranges::default(),
        0.12,
    );
    assert!(
        state.virtual_text.momentum.is_some(),
        "a fast flick releases momentum"
    );
    assert!(
        scroll_position(&state) > before_release,
        "the release frame already advances; inertia must not pause for one frame"
    );
    let released = scroll_position(&state);

    for step in 1..=5 {
        drive_virtual_ex(
            &ctx,
            &mut state,
            vec![],
            WrapMode::Off,
            Ranges::default(),
            0.12 + 0.01 * step as f64,
        );
    }
    assert!(
        scroll_position(&state) > released,
        "the flick keeps moving the anchor after the fingers lift: {released:?} -> {:?}",
        scroll_position(&state)
    );
}

#[test]
fn markup_frame_wraps_a_mixed_line_with_math_as_ordinary_text() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let words = "aaaa bbbb cccc dddd eeee ffff gggg hhhh ";
    let text = format!("{words}${}$ {words}", "x".repeat(200));
    let mut state = EditorState::new();
    state.set_buffer_text(&text);
    let math = text.find('$').expect("inline math")..text.rfind('$').expect("inline math") + 1;
    let ranges = Ranges {
        blocked: std::slice::from_ref(&math),
        math: std::slice::from_ref(&math),
        ..Ranges::default()
    };
    for _ in 0..2 {
        drive_virtual_ex(&ctx, &mut state, vec![], WrapMode::Markup, ranges, 0.0);
    }

    let galley = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the line is shaped");
    assert!(galley.rows.len() >= 2, "the mixed line wraps");
    let mut boundary = 0usize;
    let split_inside_math = galley.rows.iter().any(|row| {
        boundary += row.glyphs.len();
        math.start < boundary && boundary < math.end
    });
    assert!(
        split_inside_math,
        "math is ordinary text and may be split by the width"
    );
}

#[test]
fn markup_frame_breaks_an_oversized_math_atom_after_a_comma() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let text = format!("x ${}b$ y", "a,".repeat(300));
    let mut state = EditorState::new();
    state.set_buffer_text(&text);
    let math = text.find('$').expect("math")..text.rfind('$').expect("math") + 1;
    let ranges = Ranges {
        blocked: std::slice::from_ref(&math),
        math: std::slice::from_ref(&math),
        math_atoms: std::slice::from_ref(&math),
        ..Ranges::default()
    };
    for _ in 0..2 {
        drive_virtual_ex(&ctx, &mut state, vec![], WrapMode::Markup, ranges, 0.0);
    }

    let galley = state
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the line is shaped");
    assert!(galley.rows.len() >= 2, "the long math line wraps");
    let mut start = 0usize;
    let atom_row = galley.rows.iter().find(|row| {
        let row_start = start;
        start += row.glyphs.len();
        row_start == math.start
    });
    let atom_row: String = atom_row
        .expect("a row starts at the atom")
        .glyphs
        .iter()
        .map(|glyph| glyph.chr)
        .collect();
    assert!(
        atom_row.ends_with(','),
        "the atom row breaks after a comma: {atom_row:?}"
    );
}

#[test]
fn plain_text_forces_no_wrap_despite_wrap_always() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let text = "word ".repeat(120);
    let line = text.trim_end();

    let mut plain = EditorState::new();
    plain.set_buffer_text(line);
    for _ in 0..2 {
        drive_plain_ex(
            &ctx,
            &mut plain,
            vec![],
            WrapMode::Always,
            Ranges::default(),
        );
    }
    let plain_galley = plain
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the plain line is shaped");
    assert_eq!(
        plain_galley.rows.len(),
        1,
        "plain text stays one row even with WrapMode::Always"
    );

    let mut typst = EditorState::new();
    typst.set_buffer_text(line);
    for _ in 0..2 {
        drive_virtual_ex(
            &ctx,
            &mut typst,
            vec![],
            WrapMode::Always,
            Ranges::default(),
            0.0,
        );
    }
    let typst_galley = typst
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the Typst line is shaped");
    assert!(
        typst_galley.rows.len() >= 2,
        "the fixture is long enough to wrap outside plain mode"
    );
}

#[test]
fn plain_text_ignores_highlight_spans() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let text = "let x = 1";
    let highlights = [(0..3, HighlightKind::Keyword)];

    let mut plain = EditorState::new();
    plain.set_buffer_text(text);
    for _ in 0..2 {
        drive_plain_ex(
            &ctx,
            &mut plain,
            vec![],
            WrapMode::Off,
            Ranges {
                highlights: &highlights,
                ..Ranges::default()
            },
        );
    }
    let galley = plain
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the plain line is shaped");
    assert!(
        galley
            .job
            .sections
            .iter()
            .all(|section| section.format.color == egui::Color32::PLACEHOLDER),
        "plain mode must discard highlight kinds so the fallback color applies"
    );

    let mut typst = EditorState::new();
    typst.set_buffer_text(text);
    for _ in 0..2 {
        drive_virtual_ex(
            &ctx,
            &mut typst,
            vec![],
            WrapMode::Off,
            Ranges {
                highlights: &highlights,
                ..Ranges::default()
            },
            0.0,
        );
    }
    let galley = typst
        .virtual_text
        .cache
        .cached_galley(0)
        .expect("the Typst line is shaped");
    assert!(
        galley
            .job
            .sections
            .iter()
            .any(|section| section.format.color != egui::Color32::PLACEHOLDER),
        "the same span is applied outside plain mode"
    );
}

#[test]
fn plain_text_suppresses_completion() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_buffer_text("");
    let completions = vec![CompletionItem::plain("let", CompletionKind::Keyword)];

    for frame in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 400.0),
            )),
            events: if frame == 2 {
                vec![egui::Event::Text("#".to_owned())]
            } else {
                vec![]
            },
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            let mut input = empty_input();
            input.request_focus = true;
            input.plain_text = true;
            input.completions = &completions;
            state.show(ui, input);
        })
        .drop_without_applying_deltas();
    }
    assert_eq!(
        state.shared_text().as_str(),
        "#",
        "the trigger char was typed"
    );
    assert!(
        state.completion.is_none(),
        "plain text must never open the completion popup"
    );
}

// ---- Code folding (P2-2b) --------------------------------------------------

/// A fold whose start line is `start_line` and end line `end_line` (byte range
/// over the buffer, as the parse would report it).
fn fold_over(
    start_line: usize,
    end_line: usize,
    buffer: &typ_edit_buffer::TextBuffer,
) -> FoldRange {
    FoldRange {
        start: BytePos(buffer.char_to_byte(buffer.line_start(start_line))),
        end: BytePos(buffer.char_to_byte(buffer.line_start(end_line))),
        kind: FoldKind::Block,
    }
}

/// One virtual frame carrying `folds` as the parse's fold candidates.
fn drive_fold_frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    folds: &[FoldRange],
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events: with_modifiers(events, modifiers),
        ..Default::default()
    };
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.folds = folds;
        out = Some(state.show(ui, input));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

fn painted_lines(state: &EditorState) -> Vec<usize> {
    state
        .virtual_text
        .test_line_origins
        .iter()
        .map(|(line, _, _)| *line)
        .collect()
}

fn painted_y(state: &EditorState, line: usize) -> f32 {
    state
        .virtual_text
        .test_line_origins
        .iter()
        .find(|(index, _, _)| *index == line)
        .map(|(_, y, _)| *y)
        .unwrap_or_else(|| panic!("line {line} is painted"))
}

#[test]
fn virtual_folds_hide_lines_from_the_layout() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "a\nb\nc\nd\ne");
    let folds = [fold_over(0, 2, &state.buffer)];
    for _ in 0..3 {
        drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);
    }
    let before_line3_y = painted_y(&state, 3);

    state.virtual_text.set_caret(0);
    assert!(state.fold_at_caret());
    for _ in 0..2 {
        drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);
    }

    let painted = painted_lines(&state);
    assert!(
        !painted.contains(&1) && !painted.contains(&2),
        "{painted:?}"
    );
    assert!(painted.contains(&0) && painted.contains(&3) && painted.contains(&4));
    assert_eq!(
        state.virtual_text.fold_hidden_lines(&state.buffer),
        vec![1..3]
    );
    assert_eq!(state.virtual_text.fold_collapsed_count(), 1);
    assert!(
        painted_y(&state, 3) < before_line3_y,
        "the hidden lines' height is removed above line 3"
    );
}

#[test]
fn virtual_vertical_nav_skips_hidden_lines() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "a\nb\nc\nd\ne", 0, false);
    let folds = [fold_over(0, 2, &state.buffer)];
    for _ in 0..2 {
        drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);
    }
    state.virtual_text.set_caret(0);
    assert!(state.toggle_fold_line(0));
    for _ in 0..2 {
        drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);
    }

    drive_fold_frame(
        &ctx,
        &mut state,
        &folds,
        vec![key_event(egui::Key::ArrowDown)],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        state.virtual_text.caret(),
        state.buffer.line_start(3),
        "ArrowDown lands past the hidden region"
    );
}

#[test]
fn virtual_fold_all_and_unfold_all_toggle_every_candidate() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "a\nb\nc\nd\ne");
    let folds = [
        fold_over(0, 2, &state.buffer),
        fold_over(3, 4, &state.buffer),
    ];
    for _ in 0..2 {
        drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);
    }

    assert!(state.fold_all());
    assert_eq!(state.virtual_text.fold_collapsed_count(), 2);
    assert!(!state.fold_all(), "already collapsed");
    assert!(state.unfold_all());
    assert_eq!(state.virtual_text.fold_collapsed_count(), 0);
    assert!(!state.unfold_all());
}

#[test]
fn virtual_caret_relocates_out_of_a_collapsed_region() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "a\nb\nc\nd\ne");
    let folds = [fold_over(0, 2, &state.buffer)];
    for _ in 0..2 {
        drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);
    }

    state.virtual_text.set_caret(state.buffer.line_start(2));
    assert!(state.toggle_fold_line(0));
    assert_eq!(
        state.virtual_text.caret(),
        state.buffer.line_start(1) - 1,
        "the caret moves to the end of the fold's start line"
    );
    assert!(state.virtual_text.h_pos().is_none());
}

#[test]
fn virtual_fold_toggle_survives_an_edit_shift() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "a\nb\nc\nd", 0, false);
    let folds = [fold_over(2, 3, &state.buffer)];
    for _ in 0..2 {
        drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);
    }
    state.virtual_text.set_caret(state.buffer.line_start(2));
    assert!(state.fold_at_caret());

    // Type at the start while the parse still lags: the stale fold range must be
    // shifted by the delta log and stay collapsed.
    state.virtual_text.set_caret(0);
    drive_fold_frame(
        &ctx,
        &mut state,
        &folds,
        vec![egui::Event::Text("X".to_owned())],
        egui::Modifiers::NONE,
    );
    drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);

    assert_eq!(
        state.virtual_text.fold_collapsed_count(),
        1,
        "the collapsed fold shifted with the edit"
    );
    assert_eq!(
        state.virtual_text.fold_hidden_lines(&state.buffer),
        vec![3..4]
    );
}

/// A press + release at `pos` while carrying `folds` as the parse input.
fn fold_click(
    ctx: &egui::Context,
    state: &mut EditorState,
    folds: &[FoldRange],
    pos: egui::Pos2,
) -> EditorOutput {
    drive_fold_frame(
        ctx,
        state,
        folds,
        vec![
            pointer_moved(pos),
            pointer_button(pos, true, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    drive_fold_frame(
        ctx,
        state,
        folds,
        vec![pointer_button(pos, false, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    )
}

#[test]
fn virtual_fold_band_click_toggles_the_chevron() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "a\nb\nc\nd\ne");
    let folds = [fold_over(0, 2, &state.buffer)];
    for _ in 0..3 {
        drive_fold_frame(&ctx, &mut state, &folds, vec![], egui::Modifiers::NONE);
    }

    let viewport = virtual_viewport(&state);
    let (_, y, _) = state
        .virtual_text
        .test_line_origins
        .iter()
        .copied()
        .find(|(line, _, _)| *line == 0)
        .expect("line 0 is visible");
    // The band is the leftmost 10px of the widened gutter; the viewport's left
    // edge sits after the 50px gutter plus the 8px item gap, so the band center
    // is 53px left of it.
    let pos = egui::pos2(viewport.left() - 53.0, y + 5.0);
    fold_click(&ctx, &mut state, &folds, pos);

    assert_eq!(
        state.virtual_text.fold_collapsed_count(),
        1,
        "a chevron band click collapses the fold"
    );
    assert_eq!(
        state.virtual_text.fold_hidden_lines(&state.buffer),
        vec![1..3]
    );
}

#[test]
fn virtual_plain_text_ignores_folds() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "a\nb\nc\nd\ne");
    let folds = [fold_over(0, 2, &state.buffer)];

    for _ in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 400.0),
            )),
            events: vec![],
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            let mut input = empty_input();
            input.folds = &folds;
            input.plain_text = true;
            state.show(ui, input);
        })
        .drop_without_applying_deltas();
    }

    assert_eq!(state.virtual_text.fold_collapsed_count(), 0);
    assert!(state.virtual_text.cache.hidden().is_empty());
    assert_eq!(painted_lines(&state).len(), 5, "no line is hidden");
}

// ---- Gutter bookmarks (P2-2c) ----------------------------------------------

/// A press + release at `pos` (the bookmark band).
fn bookmark_band_click(
    ctx: &egui::Context,
    state: &mut EditorState,
    pos: egui::Pos2,
) -> EditorOutput {
    drive_virtual_frame(
        ctx,
        state,
        vec![
            pointer_moved(pos),
            pointer_button(pos, true, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    drive_virtual_frame(
        ctx,
        state,
        vec![pointer_button(pos, false, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    )
}

#[test]
fn virtual_bookmark_band_click_toggles_the_line() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    start_virtual(&mut state, "a\nb\nc\nd\ne");
    warm_virtual(&ctx, &mut state);

    let viewport = virtual_viewport(&state);
    let (_, y, _) = state
        .virtual_text
        .test_line_origins
        .iter()
        .copied()
        .find(|(line, _, _)| *line == 1)
        .expect("line 1 is visible");
    // The bookmark band is the right half of the 20px fold band; the viewport's
    // left edge sits after the 50px gutter plus the 8px item gap, so the band
    // center is 43px left of it.
    let pos = egui::pos2(viewport.left() - 43.0, y + 5.0);
    bookmark_band_click(&ctx, &mut state, pos);
    assert_eq!(state.virtual_text.bookmark_count(), 1);
    assert_eq!(state.bookmark_lines(), vec![1]);

    // A second click on the same band toggles the bookmark back off.
    bookmark_band_click(&ctx, &mut state, pos);
    assert_eq!(state.virtual_text.bookmark_count(), 0);
    assert!(state.bookmark_lines().is_empty());
}

#[test]
fn virtual_toggle_bookmark_command_uses_the_caret_line() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "aa\nbb\ncc", 3, false);

    assert!(state.toggle_bookmark());
    assert_eq!(state.bookmark_lines(), vec![1], "caret on line 1");

    assert!(state.toggle_bookmark());
    assert!(state.bookmark_lines().is_empty(), "toggling removes it");
}

#[test]
fn virtual_bookmark_navigation_jumps_the_caret_without_wrap() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "a\nb\nc\nd\ne", 0, false);
    assert!(state.toggle_bookmark_line(1));
    assert!(state.toggle_bookmark_line(3));

    assert!(state.next_bookmark());
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.virtual_text.caret(), state.buffer.line_start(1));

    assert!(state.next_bookmark());
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.virtual_text.caret(), state.buffer.line_start(3));
    assert!(
        !state.next_bookmark(),
        "strictly greater has no wrap-around"
    );

    assert!(state.prev_bookmark());
    vframe(&ctx, &mut state, vec![]);
    assert_eq!(state.virtual_text.caret(), state.buffer.line_start(1));
    assert!(!state.prev_bookmark(), "strictly less has no wrap-around");
}

#[test]
fn virtual_clear_bookmarks_removes_them_all() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "a\nb\nc", 0, false);
    assert!(state.toggle_bookmark_line(0));
    assert!(state.toggle_bookmark_line(2));
    assert_eq!(state.virtual_text.bookmark_count(), 2);

    assert!(state.clear_bookmarks());
    assert_eq!(state.virtual_text.bookmark_count(), 0);
    assert!(!state.clear_bookmarks(), "already empty");
}

#[test]
fn virtual_bookmark_survives_an_edit_shift() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "a\nb\nc\nd", 4, false);
    assert!(state.toggle_bookmark());
    assert_eq!(state.bookmark_lines(), vec![2]);

    // Insert a newline at the very start: the bookmark's line index shifts but
    // it stays attached to the same text.
    state.virtual_text.set_caret(0);
    vframe(&ctx, &mut state, vec![key_event(egui::Key::Enter)]);
    assert_eq!(state.shared_text().as_str(), "\na\nb\nc\nd");
    assert_eq!(
        state.bookmark_lines(),
        vec![3],
        "the bookmark stayed on `c`"
    );
}

#[test]
fn virtual_bookmarks_are_independent_per_editor() {
    let mut first = EditorState::new();
    first.set_buffer_text("a\nb\nc");
    first.virtual_text.set_caret(0);
    assert!(first.toggle_bookmark());

    let mut second = EditorState::new();
    second.set_buffer_text("a\nb\nc");
    assert_eq!(first.bookmark_lines(), vec![0]);
    assert!(second.bookmark_lines().is_empty(), "a tab has its own set");
}

// ---- P2-3b: multi-caret creation, motion and painting ----------------------

#[test]
fn virtual_alt_click_adds_and_removes_a_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef\nabcdef", 6, false);
    let pos = virtual_line_center(&state, 0);

    virtual_click(&ctx, &mut state, pos, egui::Modifiers::ALT);
    assert_eq!(
        state.virtual_text.carets().len(),
        2,
        "alt+click adds a caret"
    );

    virtual_click(&ctx, &mut state, pos, egui::Modifiers::ALT);
    assert_eq!(
        state.virtual_text.carets().len(),
        1,
        "alt+click on the same char removes that caret"
    );
    assert_eq!(
        state.virtual_text.caret(),
        6,
        "the original caret survives the removal"
    );
}

#[test]
fn virtual_plain_click_collapses_extra_carets() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef\nabcdef", 6, false);
    let pos = virtual_line_center(&state, 0);

    virtual_click(&ctx, &mut state, pos, egui::Modifiers::ALT);
    assert_eq!(state.virtual_text.carets().len(), 2);

    let other = virtual_line_center(&state, 1);
    virtual_click(&ctx, &mut state, other, egui::Modifiers::NONE);
    assert_eq!(
        state.virtual_text.carets().len(),
        1,
        "a plain click collapses back to a single caret"
    );
    assert_eq!(state.virtual_text.caret(), state.buffer.line_start(1));
}

#[test]
fn virtual_escape_collapses_extra_carets() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef\nabcdef", 6, false);
    let pos = virtual_line_center(&state, 0);

    virtual_click(&ctx, &mut state, pos, egui::Modifiers::ALT);
    assert_eq!(state.virtual_text.carets().len(), 2);

    vframe(&ctx, &mut state, vec![key_event(egui::Key::Escape)]);
    assert_eq!(
        state.virtual_text.carets().len(),
        1,
        "Escape collapses to the primary caret"
    );
}

#[test]
fn virtual_ctrl_alt_down_adds_a_column_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc\ndef\nghi", 1, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(
            egui::Modifiers::COMMAND | egui::Modifiers::ALT,
            egui::Key::ArrowDown,
        )],
        egui::Modifiers::COMMAND | egui::Modifiers::ALT,
    );
    assert_eq!(
        state.virtual_text.carets().len(),
        2,
        "Ctrl+Alt+Down adds a column caret"
    );
    assert_eq!(
        state.virtual_text.carets().list()[1].caret,
        5,
        "the new caret keeps the primary's column on the next line"
    );

    vframe(&ctx, &mut state, vec![egui::Event::Text("X".to_owned())]);
    assert_eq!(state.shared_text().as_str(), "aXbc\ndXef\nghi");
}

#[test]
fn virtual_up_down_moves_every_caret_preserving_columns() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abcdef\nabcdef\nabcdef", 2, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(
            egui::Modifiers::COMMAND | egui::Modifiers::ALT,
            egui::Key::ArrowDown,
        )],
        egui::Modifiers::COMMAND | egui::Modifiers::ALT,
    );
    let list = state.virtual_text.carets().list().to_vec();
    assert_eq!((list[0].caret, list[1].caret), (2, 9));

    vframe(&ctx, &mut state, vec![key_event(egui::Key::ArrowDown)]);
    let list = state.virtual_text.carets().list().to_vec();
    assert_eq!(
        (list[0].caret, list[1].caret),
        (9, 16),
        "both carets move down one line at their own column"
    );

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::SHIFT, egui::Key::ArrowDown)],
        egui::Modifiers::SHIFT,
    );
    let list = state.virtual_text.carets().list().to_vec();
    assert_eq!(list[0].selection(), Some(9..16), "the first caret extends");
    assert_eq!(
        list[1].selection(),
        Some(16..20),
        "the second caret extends"
    );
}

#[test]
fn virtual_multicaret_vertical_nav_off_screen_shapes_nothing() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let text = numbered_lines(600);
    focus_virtual(&ctx, &mut state, &text, 0, false);

    // The primary stays on the first (visible) line; 40 more carets sit far
    // below the viewport, on lines the cache never shaped.
    let base = state.virtual_text.carets().primary();
    let targets: Vec<usize> = (0..40)
        .map(|i| state.buffer.line_start(80 + i * 12))
        .collect();
    {
        let mut carets = state.virtual_text.carets_mut();
        carets.list_mut().clear();
        carets.list_mut().push(base);
        for target in targets {
            let mut selection = base;
            selection.caret = target;
            selection.anchor = None;
            selection.h_pos = None;
            carets.list_mut().push(selection);
        }
        carets.set_primary(0);
    }
    assert_eq!(state.virtual_text.carets().len(), 41);

    // A steady frame settles the visible cache; the far lines stay unshaped.
    vframe(&ctx, &mut state, vec![]);

    // Repeated Shift+Down across the off-screen carets must not shape any line
    // for them (the estimate path is stateless).
    for _ in 0..3 {
        vframe_mod(
            &ctx,
            &mut state,
            vec![key_event_with(egui::Modifiers::SHIFT, egui::Key::ArrowDown)],
            egui::Modifiers::SHIFT,
        );
        assert_eq!(
            state.virtual_text.cache.shaped_chars_this_frame(),
            0,
            "a multi-caret Shift+Down shaped lines for far off-screen carets"
        );
    }
}

#[test]
fn virtual_ctrl_d_adds_successive_occurrences() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo bar foo baz foo", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 2);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 3);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(
        state.virtual_text.carets().len(),
        3,
        "no further occurrence, no new caret"
    );
}

#[test]
fn virtual_ctrl_d_on_a_single_occurrence_is_a_noop() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo bar", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 1);
    assert_eq!(state.virtual_text.selection_range(), None);
}

#[test]
fn virtual_ctrl_shift_l_selects_all_occurrences() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo bar foo baz foo", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::L,
        )],
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
    );
    let carets = state.virtual_text.carets();
    let ranges: Vec<_> = carets.list().iter().map(|sel| sel.selection()).collect();
    assert_eq!(carets.len(), 3);
    assert_eq!(
        ranges,
        vec![Some(0..3), Some(8..11), Some(16..19)],
        "one selection per occurrence, in document order"
    );
}

#[test]
fn virtual_multi_caret_frame_reports_every_selection() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo\nfoo", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 2);

    let out = vframe(&ctx, &mut state, vec![]);
    assert_eq!(
        out.selections,
        vec![BytePos(0)..BytePos(3), BytePos(4)..BytePos(7)],
        "the frame reports one byte range per caret, in caret-list order"
    );
}

#[test]
fn virtual_copy_joins_multiple_selections_with_newlines() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo\nfoo", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 2);

    let (_, commands) = vframe_commands(&ctx, &mut state, vec![egui::Event::Copy]);
    assert_eq!(
        copied_text(&commands).as_deref(),
        Some("foo\nfoo"),
        "every selection is copied, ascending and newline-joined"
    );
}

#[test]
fn virtual_cut_removes_every_selection_in_one_undo_step() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo\nfoo", 0, false);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 2);

    let (_, commands) = vframe_commands(&ctx, &mut state, vec![egui::Event::Cut]);
    assert_eq!(copied_text(&commands).as_deref(), Some("foo\nfoo"));
    assert_eq!(state.shared_text().as_str(), "\n");

    assert!(state.buffer.undo(), "the multi-cut is one undo group");
    assert_eq!(
        state.buffer.slice(0..state.buffer.len_chars()).as_str(),
        "foo\nfoo"
    );
}

#[test]
fn cursor_count_tracks_the_caret_list() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "foo\nfoo", 0, false);
    assert_eq!(state.cursor_count(), 1);

    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 2);
    assert_eq!(state.cursor_count(), 2);
}

// ---- rename: F2 request and grouped edits ---------------------------------

#[test]
fn virtual_f2_requests_rename_at_the_caret() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "alpha beta", 3, false);

    let out = vframe(&ctx, &mut state, vec![key_event(egui::Key::F2)]);
    assert_eq!(out.rename_request, Some(BytePos(3)));
    assert!(
        out.caret_screen.is_some(),
        "the caret rect anchors the overlay"
    );
}

#[test]
fn virtual_f2_with_a_modifier_never_requests_rename() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "alpha beta", 3, false);

    let out = vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::SHIFT, egui::Key::F2)],
        egui::Modifiers::SHIFT,
    );
    assert_eq!(out.rename_request, None);
}

#[test]
fn virtual_context_menu_references_request_defaults_to_none() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "alpha beta", 3, false);

    let out = vframe(&ctx, &mut state, vec![]);
    assert_eq!(
        out.references_request, None,
        "no request until the 查找引用 row is chosen"
    );
}

#[test]
fn apply_byte_edits_applies_two_edits_in_one_undo_step() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    state.set_buffer_text("foo bar foo");
    let edits = [
        TextEdit {
            range: BytePos(0)..BytePos(3),
            new_text: "baz".to_owned(),
        },
        TextEdit {
            range: BytePos(8)..BytePos(11),
            new_text: "baz".to_owned(),
        },
    ];

    assert!(state.apply_byte_edits(&edits));
    assert_eq!(state.shared_text().as_str(), "baz bar baz");

    state.queue_command(EditorCommand::Undo);
    let _ = vframe(&ctx, &mut state, vec![]);
    assert_eq!(
        state.shared_text().as_str(),
        "foo bar foo",
        "one undo step reverts both edits"
    );
}

#[test]
fn apply_byte_edits_skips_overlapping_and_out_of_range_edits() {
    let mut state = EditorState::new();
    state.set_buffer_text("abcdef");
    let edits = [
        TextEdit {
            range: BytePos(0)..BytePos(3),
            new_text: "X".to_owned(),
        },
        TextEdit {
            range: BytePos(2)..BytePos(5),
            new_text: "Y".to_owned(),
        },
        TextEdit {
            range: BytePos(4)..BytePos(99),
            new_text: "Z".to_owned(),
        },
        TextEdit {
            range: BytePos(100)..BytePos(101),
            new_text: "W".to_owned(),
        },
    ];

    assert!(state.apply_byte_edits(&edits));
    assert_eq!(
        state.shared_text().as_str(),
        "XdZ",
        "overlap 2..5 and start-past-end 100..101 are skipped"
    );
}

#[test]
fn apply_byte_edits_is_a_noop_on_empty_or_invalid_input() {
    let mut state = EditorState::new();
    state.set_buffer_text("abc");
    assert!(!state.apply_byte_edits(&[]));
    let noop = [TextEdit {
        range: BytePos(1)..BytePos(1),
        new_text: String::new(),
    }];
    assert!(!state.apply_byte_edits(&noop));
    assert_eq!(state.shared_text().as_str(), "abc");
}

// ---- Collab: capture, remote apply, history routing, remote carets --------

/// One virtual frame with explicit collab inputs. `collab_history` routes
/// undo/redo away from the buffer; `remote` paints remote cursors.
fn vframe_collab(
    ctx: &egui::Context,
    state: &mut EditorState,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    collab_history: bool,
    remote: &[RemoteCaret],
) -> EditorOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events: with_modifiers(events, modifiers),
        ..Default::default()
    };
    let mut out = None;
    ctx.run_ui(raw, |ui| {
        let mut input = empty_input();
        input.collab_history = collab_history;
        input.remote_carets = remote;
        out = Some(state.show(ui, input));
    })
    .drop_without_applying_deltas();
    out.expect("a frame output")
}

#[test]
fn capture_records_byte_ops_for_a_typed_insert() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let capture = state.enable_edit_capture();
    focus_virtual(&ctx, &mut state, "ab", 2, false);
    let _ = capture.take();

    let _ = vframe(&ctx, &mut state, vec![egui::Event::Text("x".to_owned())]);
    let edits = capture.take();
    assert!(
        edits
            .iter()
            .any(|edit| edit.at_byte == 2 && edit.delete_bytes == 0 && edit.insert.contains('x')),
        "expected a pure insert at byte 2, got {edits:?}"
    );
}

#[test]
fn multi_caret_single_edit_captures_every_operation() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let capture = state.enable_edit_capture();
    focus_virtual(&ctx, &mut state, "foo\nfoo", 0, false);
    vframe_mod(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::D)],
        egui::Modifiers::COMMAND,
    );
    assert_eq!(state.virtual_text.carets().len(), 2);
    let _ = capture.take();

    let _ = vframe(&ctx, &mut state, vec![egui::Event::Text("x".to_owned())]);
    let edits = capture.take();
    let deletes = edits
        .iter()
        .filter(|edit| edit.delete_bytes == 3 && edit.insert == "foo")
        .count();
    let inserts = edits
        .iter()
        .filter(|edit| edit.delete_bytes == 0 && edit.insert == "x")
        .count();
    assert_eq!(deletes, 2, "both selections delete their 'foo': {edits:?}");
    assert_eq!(inserts, 2, "both selections insert 'x': {edits:?}");
}

#[test]
fn apply_remote_edits_updates_the_buffer_without_recapturing() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let capture = state.enable_edit_capture();
    focus_virtual(&ctx, &mut state, "hello", 0, false);
    let _ = vframe(&ctx, &mut state, vec![]);
    let _ = capture.take();

    let edits = vec![CapturedEdit {
        at_byte: 5,
        delete_bytes: 0,
        insert: " world".to_owned(),
    }];
    assert!(state.apply_remote_edits(&edits));
    assert_eq!(state.shared_text().as_str(), "hello world");
    assert!(
        capture.take().is_empty(),
        "a remote apply must not be re-captured"
    );

    let out = vframe(&ctx, &mut state, vec![]);
    assert!(out.changed, "the remote apply reports changed next frame");
}

#[test]
fn apply_remote_edits_rejects_a_delete_mismatch_atomically() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello", 0, false);

    let edits = vec![
        CapturedEdit {
            at_byte: 4,
            delete_bytes: 0,
            insert: "!".to_owned(),
        },
        CapturedEdit {
            at_byte: 0,
            delete_bytes: 3,
            insert: "ZZZ".to_owned(),
        },
    ];
    assert!(!state.apply_remote_edits(&edits));
    assert_eq!(
        state.shared_text().as_str(),
        "hello",
        "a rejected batch leaves the buffer unchanged"
    );
}

#[test]
fn replace_all_text_remote_does_not_capture() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    let capture = state.enable_edit_capture();
    focus_virtual(&ctx, &mut state, "old", 0, false);
    let _ = capture.take();

    state.replace_all_text_remote("brand new");
    assert_eq!(state.shared_text().as_str(), "brand new");
    assert!(
        capture.take().is_empty(),
        "a remote whole-text replace must not be captured"
    );
}

#[test]
fn collab_history_ctrl_z_emits_a_request_without_touching_the_buffer() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "abc", 3, false);
    let _ = vframe(&ctx, &mut state, vec![egui::Event::Text("x".to_owned())]);
    let text_before = state.shared_text().as_str().to_owned();

    let out = vframe_collab(
        &ctx,
        &mut state,
        vec![key_event_with(egui::Modifiers::COMMAND, egui::Key::Z)],
        egui::Modifiers::COMMAND,
        true,
        &[],
    );
    assert_eq!(out.history_request, Some(HistoryRequest::Undo));
    assert_eq!(
        state.shared_text().as_str(),
        text_before,
        "collab undo must not edit the local buffer"
    );
}

#[test]
fn remote_caret_painting_smoke() {
    let ctx = egui::Context::default();
    bind_test_fonts(&ctx);
    let mut state = EditorState::new();
    focus_virtual(&ctx, &mut state, "hello world", 0, false);

    let remote = [RemoteCaret {
        user: "ann".to_owned(),
        color_index: 3,
        path: std::path::PathBuf::from("doc.typ"),
        carets: vec![6],
        selections: vec![(0, 5)],
    }];
    let out = vframe_collab(
        &ctx,
        &mut state,
        vec![],
        egui::Modifiers::NONE,
        false,
        &remote,
    );
    assert!(
        !out.changed,
        "painting remote cursors does not edit the buffer"
    );
    assert_eq!(state.shared_text().as_str(), "hello world");
}
