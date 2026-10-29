use std::ops::Range;

use super::diag::count_words;
use super::*;
use typ_edit_model::{
    BytePos, ByteRange, CompletionKind, FoldKind, HighlightKind, ProseSegment, Severity,
    SymbolKind, SymbolNode,
};
use typst::syntax::{LinkedNode, Source};

fn parse(text: &str) -> (Vec<(Range<usize>, HighlightKind)>, usize, usize) {
    let source = Source::new(parse_file_id(), text.to_owned());
    let root = LinkedNode::new(source.root());
    let fused = visit(&root, source.text());
    let defined = DefinedNames::from_items(&fused.definitions);
    (highlights(&root, &defined), fused.chars, fused.words)
}

fn run_parse(text: &str) -> ParseResult {
    run(text, text.len())
}

/// Builds a byte-space range from the raw offsets used in test literals.
fn br(start: usize, end: usize) -> ByteRange {
    BytePos(start)..BytePos(end)
}

#[test]
fn word_rule_counts_runs_and_cjk_individually() {
    assert_eq!(count_words(""), 0);
    assert_eq!(count_words("   \n\t "), 0);
    assert_eq!(count_words("hello world"), 2);
    assert_eq!(count_words("  hello   world  "), 2);
    assert_eq!(count_words("don't stop"), 2);
    assert_eq!(count_words("你好世界"), 4);
    assert_eq!(count_words("hello世界"), 3);
    assert_eq!(count_words("你好 hello 世界"), 5);
}

/// The canonical `is_cjk` union (P5-d) counts the CJK symbols/fullwidth block,
/// Hangul and the CJK radicals as individual words too; the parse-only copy
/// missed those ranges, so this pins the intended unification.
#[test]
fn word_rule_unifies_fullwidth_hangul_and_radicals() {
    assert_eq!(
        count_words("ｈｅｌｌｏ"),
        5,
        "fullwidth Latin: one word each"
    );
    assert_eq!(count_words("⺀⺁"), 2, "CJK radicals supplement");
    assert_eq!(count_words("가나다"), 3, "Hangul syllables");
    assert_eq!(count_words("你好，世界"), 5, "ideographs + fullwidth comma");
}

#[test]
fn stats_skip_spaces_and_read_math_text() {
    let (_, chars, words) = parse("Hello world\n\n$x + 1$");
    // "Hello world" (11) + the math leaves "x", "+" and "1" — `Space` leaves are
    // not prose, but the lexer emits operators as math text.
    assert_eq!(chars, 14);
    assert_eq!(words, 5);
}

#[test]
fn highlights_inherit_strong_and_heading() {
    let source = "*bold*";
    let (spans, _, _) = parse(source);
    // The `*` markers inherit the strong tag too, so match the covered text.
    let (range, kind) = spans
        .iter()
        .find(|(range, kind)| *kind == HighlightKind::Strong && &source[range.clone()] == "bold")
        .expect("strong highlight");
    assert_eq!(*kind, HighlightKind::Strong);
    assert_eq!(&source[range.clone()], "bold");

    let source = "= Title\n\nplain";
    let (spans, _, _) = parse(source);
    // The heading tag trickles down to the whitespace after the marker, so match
    // the span that actually covers the title text.
    let (range, kind) = spans
        .iter()
        .find(|(range, kind)| *kind == HighlightKind::Heading && &source[range.clone()] == "Title")
        .expect("heading highlight");
    assert_eq!(*kind, HighlightKind::Heading);
    assert_eq!(&source[range.clone()], "Title");
}

#[test]
fn highlights_code_keywords_and_functions() {
    let (spans, _, _) = parse("#let f(x) = x");
    assert!(
        spans
            .iter()
            .any(|(_, kind)| *kind == HighlightKind::Keyword)
    );
    assert!(
        spans
            .iter()
            .any(|(_, kind)| *kind == HighlightKind::Function)
    );
    assert!(spans.iter().all(|(range, _)| range.start < range.end));
}

#[test]
fn highlights_named_argument_names_only() {
    let text = "#text(font: \"x\", size: 1em)";
    let (spans, _, _) = parse(text);
    let names: Vec<Range<usize>> = spans
        .iter()
        .filter(|(_, kind)| *kind == HighlightKind::ParamName)
        .map(|(range, _)| range.clone())
        .collect();
    let font = text.find("font").unwrap();
    let size = text.find("size").unwrap();
    assert_eq!(names, vec![font..font + 4, size..size + 4]);
    let string = text.find("\"x\"").unwrap();
    assert!(
        spans.iter().any(|(range, kind)| {
            *kind == HighlightKind::String && range == &(string..string + 3)
        })
    );

    let pattern = "#let (a: b) = c";
    let (spans, _, _) = parse(pattern);
    assert!(
        spans
            .iter()
            .all(|(_, kind)| *kind != HighlightKind::ParamName)
    );
}

#[test]
fn syntax_errors_become_diagnostics() {
    let source = Source::new(parse_file_id(), "#let = 1".to_owned());
    let diags = diagnostics(&source);
    assert!(diags.iter().any(|d| d.severity == Severity::Error));
    assert!(diags.iter().all(|d| d.path.as_os_str().is_empty()));
    // Clean input produces no diagnostics.
    let source = Source::new(parse_file_id(), "= Title\n\nHello *world*".to_owned());
    assert!(diagnostics(&source).is_empty());
}

#[test]
fn blocked_ranges_cover_raw_math_inline_code_and_code_block() {
    let text = "text `raw` and $ x + 1 $ here\n\n#let y = 2\n\n```\nblock\n```";
    let result = run_parse(text);
    let ranges = &result.layout.blocked_ranges;
    assert!(
        ranges.windows(2).all(|w| w[0].end <= w[1].start),
        "ranges must be sorted and disjoint: {ranges:?}"
    );
    let covered = |offset: usize| {
        ranges
            .iter()
            .any(|r| r.start.0 <= offset && offset < r.end.0)
    };
    assert!(covered(text.find("raw").unwrap()), "inline raw");
    assert!(covered(text.find("x + 1").unwrap()), "math");
    assert!(covered(text.find("y = 2").unwrap()), "inline code");
    assert!(covered(text.rfind("block").unwrap()), "code block");

    assert!(
        run_parse("plain prose only")
            .layout
            .blocked_ranges
            .is_empty()
    );
}

#[test]
fn content_blocks_re_enter_text_mode() {
    let text = "#let x = [a *b* c]";
    let result = run_parse(text);
    let covered = |offset: usize| {
        result
            .layout
            .blocked_ranges
            .iter()
            .any(|r| r.start.0 <= offset && offset < r.end.0)
    };
    let inside = text.find('*').unwrap();
    assert!(
        !covered(inside),
        "markup inside a content block is text mode"
    );
    let code = text.find("let").unwrap();
    assert!(covered(code), "the surrounding code stays blocked");
}

#[test]
fn list_lines_report_item_line_starts_sorted_and_deduped() {
    let text = "- one\n  - two\n/ term: x\n1. three";
    assert_eq!(
        run_parse(text).layout.list_lines,
        vec![BytePos(0), BytePos(6), BytePos(14), BytePos(24)]
    );
    assert!(run_parse("no lists here").layout.list_lines.is_empty());
}

#[test]
fn list_lines_resolve_line_starts_after_multibyte_text() {
    // Each `-`/`+`/`/` marker starts a list item on its own line; the Chinese
    // glyphs are three UTF-8 bytes each, so the byte offsets differ from the
    // character columns. Line starts: 0, 12, 24, 41.
    let text = "- 项目一\n+ 项目二\n/ 术语: 定义\n- 项目三";
    assert_eq!(text.len(), 52);
    assert_eq!(
        run_parse(text).layout.list_lines,
        vec![BytePos(0), BytePos(12), BytePos(24), BytePos(41)]
    );
}

#[test]
fn definitions_collect_bindings_params_imports_and_labels() {
    let text = "#let foo = 1\n#let f(a, b) = a\n#import \"x.typ\": thing\n\n<mylabel>";
    let defs = run_parse(text).ide.definitions;
    let names: Vec<(String, CompletionKind)> = defs
        .iter()
        .map(|item| (item.name.clone(), item.kind))
        .collect();
    assert_eq!(
        names,
        vec![
            ("a".to_owned(), CompletionKind::Local),
            ("b".to_owned(), CompletionKind::Local),
            ("f".to_owned(), CompletionKind::Local),
            ("foo".to_owned(), CompletionKind::Local),
            ("mylabel".to_owned(), CompletionKind::Label),
            ("thing".to_owned(), CompletionKind::Local),
        ]
    );
    assert!(
        defs.iter()
            .all(|item| item.insert == item.name && item.cursor_back == CharCount(0))
    );

    assert!(run_parse("plain prose only").ide.definitions.is_empty());
}

#[test]
fn call_params_offer_the_callee_parameters_inside_its_arguments() {
    for text in ["#pagebreak(", "#pagebreak(w"] {
        let result = run_parse(text);
        let call = result
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{text:?} must resolve call params"));
        assert_eq!(call.range, br(10, text.len()), "args range of {text:?}");
        assert!(call.items.iter().any(|item| item.name == "weak"
            && item.kind == CompletionKind::Param
            && item.insert == "weak: "));
        assert!(call.items.iter().any(|item| item.name == "to"));
    }
}

#[test]
fn call_params_skip_parameters_already_passed() {
    let call = run_parse("#pagebreak(weak: 1, x")
        .ide
        .call_params
        .expect("call params");
    assert!(!call.items.iter().any(|item| item.name == "weak"));
    assert!(call.items.iter().any(|item| item.name == "to"));
}

/// Parses `marked` with the `|` caret marker removed and the cursor there.
fn run_at_marker(marked: &str) -> ParseResult {
    let cursor = marked.find('|').expect("caret marker `|`");
    let text = marked.replacen('|', "", 1);
    run(&text, cursor)
}

#[test]
fn call_params_work_across_lines() {
    for marked in [
        "#text(\n  a,\n  |\n)",
        "#text(\n  |\n)",
        "#text(\n  a,\n  b,\n  |)",
        "#text(fo|",
    ] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve call params"));
        assert!(!call.items.is_empty());
        assert!(
            call.items
                .iter()
                .all(|item| item.kind == CompletionKind::Param),
            "{marked:?} must only offer parameter candidates"
        );
        assert!(
            call.items.iter().any(|item| item.name == "font"),
            "{marked:?} must offer `font`"
        );
    }

    let named = run_at_marker("#text(\n  font: \"x\",\n  s|)")
        .ide
        .call_params
        .expect("multi-line named argument resolves call params");
    assert!(!named.items.iter().any(|item| item.name == "font"));
    assert!(named.items.iter().any(|item| item.name == "size"));
}

#[test]
fn call_params_resolve_after_trailing_whitespace() {
    for marked in [
        "#figure(a, |",
        "#figure(a,\n|",
        "#figure(a, \n|",
        "#figure(a,\n  |",
        "#figure(a,|",
    ] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve call params"));
        assert!(
            call.items.iter().any(|item| item.name == "alt"),
            "{marked:?} must offer figure's parameters"
        );
    }

    for marked in ["#figure(a, |)", "#figure(a,\n  |\n)"] {
        assert!(
            run_at_marker(marked).ide.call_params.is_some(),
            "{marked:?} must stay resolvable"
        );
    }

    assert!(
        run_at_marker("#figure(a) |").ide.call_params.is_none(),
        "a completed call must not offer parameters after `)`"
    );
}

#[test]
fn value_slot_resolves_after_trailing_whitespace_in_unclosed_calls() {
    // The trailing whitespace of an unclosed call is a sibling of the call, so
    // the caret must still find the named value it follows.
    for (text, param) in [
        ("#rect(fill: ", "fill"),
        ("#rect(fill: red ", "fill"),
        ("#set text(size: ", "size"),
        ("#pagebreak(weak: ", "weak"),
        ("#show heading.where(level: ", "level"),
        ("#show heading.where(outlined: ", "outlined"),
    ] {
        let call = run_parse(text)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{text:?} must resolve call params"));
        let slot = call
            .value_slot
            .unwrap_or_else(|| panic!("{text:?} must carry a value slot"));
        assert_eq!(slot.param, param, "{text:?}");
        assert!(!slot.items.is_empty(), "{text:?} must offer candidates");
    }
}

#[test]
fn unclosed_set_rule_open_slot_offers_params_without_value_slot() {
    let call = run_parse("#set text(a, ")
        .ide
        .call_params
        .expect("an unclosed set rule's positional slot still resolves");
    assert!(
        call.value_slot.is_none(),
        "a positional slot is not a value slot"
    );
    assert!(
        call.items.iter().any(|item| item.name == "font"),
        "the remaining named parameters are still offered"
    );
}

#[test]
fn trailing_positional_value_offers_no_fresh_parameter_list() {
    for text in ["#figure(a ", "#figure(a  ", "#figure(a\n"] {
        assert!(
            run_parse(text).ide.call_params.is_none(),
            "{text:?}: a positional trailing value must not open a fresh list"
        );
    }
    // Without trailing whitespace the caret sits inside the positional token:
    // parameters still resolve via the name-position path, but no value slot
    // may leak out of the positional argument.
    let positional = run_parse("#figure(a")
        .ide
        .call_params
        .expect("a positional name slot still resolves parameters");
    assert!(positional.value_slot.is_none());
}

#[test]
fn call_params_ignore_content_block_positions() {
    assert!(
        run_at_marker("#figure[#grid.|]").ide.call_params.is_none(),
        "a content block position is markup, not an argument slot"
    );
    assert!(
        run_at_marker("#text([a, |])").ide.call_params.is_none(),
        "an array inside a content block is not a call argument slot"
    );
}

#[test]
fn call_params_resolve_value_positions_and_skip_markup_positions() {
    for marked in ["#rect(width: 1c|m)", "#rect(width:| 1cm)"] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve its value slot"));
        let slot = call
            .value_slot
            .unwrap_or_else(|| panic!("{marked:?} must carry a value slot"));
        assert_eq!(slot.param, "width", "{marked:?}");
    }
    assert!(run_at_marker("#rect(wea|d: 1cm)").ide.call_params.is_some());
    assert!(run_at_marker("#text[x|]").ide.call_params.is_none());
    assert!(run_at_marker("#pagebreak(a)|").ide.call_params.is_none());
}

#[test]
fn call_params_stay_out_of_markup_value_positions_and_unknown_calls() {
    assert!(run_parse("plain prose here").ide.call_params.is_none());
    assert!(
        run("#a", 2).ide.call_params.is_none(),
        "one-char logic prefix"
    );
    // An unclosed `weak:` whose trailing whitespace the parser lifts out of
    // `Args` still resolves the value slot.
    let weak = run_parse("#pagebreak(weak: ")
        .ide
        .call_params
        .expect("an unclosed named value resolves after trailing whitespace");
    let slot = weak.value_slot.expect("weak value slot");
    assert_eq!(slot.param, "weak");
    assert!(!slot.items.is_empty(), "weak is a boolean: false/true");
    assert!(run("#pagebreak(\"x\")", 13).ide.call_params.is_none());
    assert!(run_parse("#not_a_library_fn(").ide.call_params.is_none());
}

#[test]
fn call_params_resolve_dotted_library_functions() {
    let call = run_at_marker("#grid.cell(|")
        .ide
        .call_params
        .expect("`grid.cell` must resolve its parameters");
    assert!(
        call.items
            .iter()
            .all(|item| item.kind == CompletionKind::Param),
        "only parameter candidates"
    );
    let names: Vec<&str> = call.items.iter().map(|item| item.name.as_str()).collect();
    assert!(names.contains(&"rowspan"), "grid.cell params: {names:?}");
    assert!(names.contains(&"align"), "grid.cell params: {names:?}");

    let partial = run_at_marker("#grid.cell(ro|")
        .ide
        .call_params
        .expect("a partial parameter name still resolves");
    assert!(partial.items.iter().any(|item| item.name == "rowspan"));

    assert!(
        run_at_marker("#grid.cell(rowspan: |1)")
            .ide
            .call_params
            .is_none()
    );
    assert!(
        run_at_marker("#grid.cell(rowspan:| 1)")
            .ide
            .call_params
            .is_none()
    );
}

#[test]
fn call_params_resolve_inside_set_rules() {
    for marked in ["#set text(|", "#set text(si|", "#set text(fo|"] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve call params"));
        assert!(
            call.items.iter().any(|item| item.name == "font"),
            "{marked:?} must offer `text`'s parameters"
        );
    }
    let value = run_at_marker("#set text(size: |\"x\")")
        .ide
        .call_params
        .expect("a set rule's value position resolves a slot");
    assert_eq!(
        value.value_slot.expect("value slot").param,
        "size",
        "the caret before an existing value still resolves its slot"
    );
}

#[test]
fn call_params_resolve_where_and_with_selector_methods() {
    for marked in [
        "#show heading.where(|",
        "#show heading.where(le|",
        "#show heading.with(|",
        "#show heading.with(le|",
    ] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve call params"));
        assert!(
            call.items.iter().any(|item| item.name == "level"),
            "{marked:?} must offer the base function's parameters"
        );
    }
}

#[test]
fn named_argument_names_in_set_rules_are_param_names() {
    let text = "#set text(size: \"x\")";
    let (spans, _, _) = parse(text);
    let names: Vec<Range<usize>> = spans
        .iter()
        .filter(|(_, kind)| *kind == HighlightKind::ParamName)
        .map(|(range, _)| range.clone())
        .collect();
    let size = text.find("size").unwrap();
    assert_eq!(names, vec![size..size + 4]);
}

#[test]
fn call_params_keep_plain_and_unknown_field_accesses_working() {
    assert!(run_at_marker("#figure(|").ide.call_params.is_some());
    assert!(
        run_at_marker("#not_a_library_fn.field(|")
            .ide
            .call_params
            .is_none()
    );
}

#[test]
fn value_slot_offers_typed_candidates_for_named_argument_values() {
    for marked in ["#rect(fill: |)", "#rect(fill:|)"] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve a value slot"));
        let slot = call
            .value_slot
            .unwrap_or_else(|| panic!("{marked:?} must carry a value slot"));
        assert_eq!(slot.param, "fill", "{marked:?}");
        assert!(!slot.items.is_empty(), "{marked:?}");
        assert!(
            slot.items.iter().any(|item| item.name == "rgb()"),
            "{marked:?} must offer a color constructor"
        );
        assert!(
            slot.items.iter().any(|item| item.name == "red"),
            "{marked:?} must offer named colors"
        );
        assert!(
            call.items.iter().any(|item| item.name == "stroke"),
            "{marked:?} must still offer the remaining parameters"
        );
    }
}

#[test]
fn value_slot_resolves_set_rules() {
    let call = run_at_marker("#set text(size: |)")
        .ide
        .call_params
        .expect("a set rule's value slot resolves");
    let slot = call.value_slot.expect("value slot");
    assert_eq!(slot.param, "size");
    assert!(
        slot.items.iter().any(|item| item.name == "0pt"),
        "a length slot must offer unit candidates"
    );
}

#[test]
fn value_slot_resolves_dotted_callees() {
    let call = run_at_marker("#grid.cell(fill: |)")
        .ide
        .call_params
        .expect("a dotted callee's value slot resolves");
    let slot = call.value_slot.expect("value slot");
    assert_eq!(slot.param, "fill");
    assert!(!slot.items.is_empty());
    assert!(
        slot.items.iter().any(|item| item.name == "rgb()"),
        "grid.cell's fill accepts colors"
    );

    // `rowspan` is an integer with no static candidates, so the slot stays
    // unresolved rather than offering a no-op row.
    assert!(
        run_at_marker("#grid.cell(rowspan: |)")
            .ide
            .call_params
            .and_then(|call| call.value_slot)
            .is_none()
    );
}

#[test]
fn value_slot_stays_none_in_name_and_positional_positions() {
    let named = run_at_marker("#rect(fi|)")
        .ide
        .call_params
        .expect("a parameter name position resolves the parameter list");
    assert!(named.value_slot.is_none());
    assert!(named.items.iter().any(|item| item.name == "fill"));

    let positional = run_at_marker("#figure(1, |)")
        .ide
        .call_params
        .expect("a fresh positional slot resolves");
    assert!(positional.value_slot.is_none());
    assert!(
        positional.items.iter().any(|item| item.name == "alt"),
        "positional slots keep offering parameters"
    );
}

#[test]
fn value_slot_does_not_leak_from_an_outer_call() {
    // `rgb` has no named parameters, so the inner call resolves to nothing;
    // crucially the outer `rect` value slot must not leak through.
    let leaked = run_at_marker("#rect(fill: rgb(|))")
        .ide
        .call_params
        .and_then(|call| call.value_slot);
    assert!(
        leaked.is_none(),
        "the caret belongs to the inner `rgb` call"
    );
}

#[test]
fn value_slot_resolves_typed_values_but_stays_out_of_strings_and_comments() {
    assert!(
        run_at_marker("#rect(fill: \"r|\")")
            .ide
            .call_params
            .is_none()
    );
    assert!(
        run_at_marker("#rect(fill: /* | */)")
            .ide
            .call_params
            .is_none()
    );

    for marked in [
        "#rect(fill: r|ed)",
        "#rect(fill: 0p|t)",
        "#rect(fill: red|)",
    ] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve a value slot"));
        let slot = call
            .value_slot
            .unwrap_or_else(|| panic!("{marked:?} must carry a value slot"));
        assert_eq!(slot.param, "fill", "{marked:?}");
        assert!(!slot.items.is_empty(), "{marked:?}");
    }
}

#[test]
fn value_slot_stays_none_inside_code_math_and_raw_values() {
    for marked in [
        "#rect(fill: { let x = | })",
        "#rect(fill: $x|$)",
        "#rect(fill: $mat(|)$)",
        "#rect(fill: ```co|de```)",
    ] {
        assert!(
            run_at_marker(marked).ide.call_params.is_none(),
            "{marked:?} must not resolve a value slot"
        );
    }
}

#[test]
fn value_slot_items_always_insert_text() {
    for marked in [
        "#rect(fill: |)",
        "#rect(fill: r|ed)",
        "#rect(width: 1c|m)",
        "#rect(stroke: |)",
        "#set text(size: |)",
        "#grid.cell(fill: |)",
    ] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve a value slot"));
        let slot = call
            .value_slot
            .unwrap_or_else(|| panic!("{marked:?} must carry a value slot"));
        let empty: Vec<&String> = slot
            .items
            .iter()
            .filter(|item| item.insert.is_empty())
            .map(|item| &item.name)
            .collect();
        assert!(empty.is_empty(), "{marked:?} has empty inserts: {empty:?}");
    }
}

#[test]
fn value_slot_offers_units_for_a_numeric_prefix() {
    let call = run_at_marker("#text(size: 12|)")
        .ide
        .call_params
        .expect("a numeric length slot resolves");
    let slot = call.value_slot.expect("value slot");
    assert_eq!(slot.param, "size");
    let names: Vec<&str> = slot.items.iter().map(|item| item.name.as_str()).collect();
    assert!(names.contains(&"12pt"), "{names:?}");
    assert!(names.contains(&"12em"), "{names:?}");
    let pt = slot
        .items
        .iter()
        .find(|item| item.name == "12pt")
        .expect("12pt candidate");
    assert_eq!(pt.insert, "12pt");
    assert_eq!(pt.cursor_back, CharCount(0));
}

#[test]
fn value_slot_offers_units_for_a_partial_unit_prefix() {
    let slot = run_at_marker("#text(size: 12p|)")
        .ide
        .call_params
        .expect("a partially typed unit still resolves")
        .value_slot
        .expect("value slot");
    assert!(
        slot.items.iter().any(|item| item.name == "12pt"),
        "the digits are recovered from the partial token"
    );
}

#[test]
fn value_slot_offers_angle_units_for_a_numeric_prefix() {
    let slot = run_at_marker("#rotate(angle: 45|)")
        .ide
        .call_params
        .expect("a numeric angle slot resolves")
        .value_slot
        .expect("value slot");
    let names: Vec<&str> = slot.items.iter().map(|item| item.name.as_str()).collect();
    assert!(names.contains(&"45deg"), "{names:?}");
    assert!(names.contains(&"45rad"), "{names:?}");
    assert!(!names.contains(&"45grad"), "grad is not a Typst unit");
    assert!(!names.contains(&"45turn"), "turn is not a Typst unit");
}

#[test]
fn value_slot_offers_percent_for_a_relative_prefix() {
    let slot = run_at_marker("#pad(x: 2|)")
        .ide
        .call_params
        .expect("a numeric relative slot resolves")
        .value_slot
        .expect("value slot");
    assert!(slot.items.iter().any(|item| item.name == "2%"));
}

#[test]
fn value_slot_keeps_non_unit_slots_for_numeric_prefixes() {
    let slot = run_at_marker("#rect(fill: 0|)")
        .ide
        .call_params
        .expect("a color slot still resolves")
        .value_slot
        .expect("value slot");
    assert!(
        slot.items.iter().all(|item| !item.name.ends_with("pt")),
        "a non-unit slot must not grow unit candidates"
    );
}

#[test]
fn value_slot_has_no_units_inside_strings() {
    assert!(
        run_at_marker("#text(size: \"12|\")")
            .ide
            .call_params
            .is_none()
    );
}

#[test]
fn value_slot_falls_back_to_none_without_candidates() {
    // `figure` declares no `fill` parameter in typst 0.15.1, so the fresh value
    // position has no candidates and stays unresolved.
    assert!(run_at_marker("#figure(fill: |)").ide.call_params.is_none());
}

#[test]
fn named_dictionary_fields_are_param_names() {
    let text = "#set page(margin: (x: 2.1cm, y: 2.2cm))";
    let (spans, _, _) = parse(text);
    let names: Vec<Range<usize>> = spans
        .iter()
        .filter(|(_, kind)| *kind == HighlightKind::ParamName)
        .map(|(range, _)| range.clone())
        .collect();
    let span = |needle: &str| {
        let start = text.find(needle).unwrap();
        start..start + needle.len()
    };
    assert_eq!(names, vec![span("margin"), span("x"), span("y")]);

    // A standalone dictionary literal qualifies too; only its field names are
    // tagged, not the values.
    let text = "#let d = (x: 1cm)";
    let (spans, _, _) = parse(text);
    let start = text.find("x").unwrap();
    assert!(spans.iter().any(|(range, kind)| {
        *kind == HighlightKind::ParamName && range == &(start..start + 1)
    }));
}

#[test]
fn named_dictionary_highlight_ignores_patterns_and_closure_params() {
    for text in ["#let (a: b) = c", "#let f(a: 1) = a"] {
        let (spans, _, _) = parse(text);
        assert!(
            spans
                .iter()
                .all(|(_, kind)| *kind != HighlightKind::ParamName),
            "{text:?} must not tag pattern or closure-parameter names"
        );
    }
}

#[test]
fn alignment_constants_highlight_as_keywords() {
    for (text, name) in [
        ("#align(center)[x]", "center"),
        ("#set align(alignment: right)", "right"),
        ("#show heading: set align(left)", "left"),
        ("#align(horizon)", "horizon"),
        ("#align(bottom)", "bottom"),
    ] {
        let (spans, _, _) = parse(text);
        let start = text.find(name).unwrap();
        let range = start..start + name.len();
        assert!(
            spans
                .iter()
                .any(|(r, kind)| *kind == HighlightKind::Keyword && *r == range),
            "{text:?}: {name:?} must be a Keyword"
        );
    }
}

#[test]
fn alignment_constant_highlight_yields_to_function_and_math_tags() {
    let text = "#center(1)";
    let (spans, _, _) = parse(text);
    let start = text.find("center").unwrap();
    let range = start..start + 6;
    assert!(
        spans
            .iter()
            .any(|(r, kind)| *kind == HighlightKind::Function && *r == range),
        "an identifier directly before an argument list stays a Function"
    );
    assert!(
        !spans
            .iter()
            .any(|(r, kind)| *kind == HighlightKind::Keyword && *r == range)
    );

    let text = "$ center $";
    let (spans, _, _) = parse(text);
    let start = text.find("center").unwrap();
    let range = start..start + 6;
    assert!(
        !spans
            .iter()
            .any(|(r, kind)| *kind == HighlightKind::Keyword && *r == range),
        "the constant rule is code-only; math identifiers keep their own tag"
    );
}

#[test]
fn math_symbols_highlight_distinctly_from_variables() {
    for (text, name) in [
        ("$integral$", "integral"),
        ("$delta$", "delta"),
        ("$Delta$", "Delta"),
    ] {
        let (spans, _, _) = parse(text);
        let start = text.find(name).unwrap();
        let range = start..start + name.len();
        assert!(
            spans
                .iter()
                .any(|(r, kind)| *kind == HighlightKind::MathSymbol && *r == range),
            "{text:?}: {name:?} must be a MathSymbol"
        );
    }

    // A single-grapheme math letter is `MathText` and stays untouched; a
    // multi-character user variable remains an interpolated variable.
    let (spans, _, _) = parse("$x$");
    assert!(
        spans
            .iter()
            .all(|(_, kind)| *kind != HighlightKind::MathSymbol),
        "`x` is not a symbol"
    );
    let (spans, _, _) = parse("$foo$");
    assert!(
        spans
            .iter()
            .any(|(r, kind)| *kind == HighlightKind::Interpolated && *r == (1..4)),
        "a user variable stays interpolated"
    );
    assert!(
        spans
            .iter()
            .all(|(_, kind)| *kind != HighlightKind::MathSymbol),
        "`foo` is not a symbol"
    );
}

#[test]
fn dotted_math_symbols_highlight_as_one_span() {
    let text = "$eq.ast + plus.minus$";
    let (spans, _, _) = parse(text);
    for needle in ["eq.ast", "plus.minus"] {
        let start = text.find(needle).unwrap();
        let range = start..start + needle.len();
        assert!(
            spans
                .iter()
                .any(|(r, kind)| *kind == HighlightKind::MathSymbol && *r == range),
            "{needle:?} must be one MathSymbol span"
        );
    }
}

#[test]
fn defined_math_name_shadows_the_symbol_table() {
    let text = "#let delta = 1\n$delta$";
    let (spans, _, _) = parse(text);
    let start = text.rfind("delta").unwrap();
    let range = start..start + 5;
    assert!(
        spans
            .iter()
            .any(|(r, kind)| *kind == HighlightKind::Interpolated && *r == range)
    );
    assert!(
        spans
            .iter()
            .all(|(_, kind)| *kind != HighlightKind::MathSymbol),
        "a document-local `delta` is not the math symbol"
    );
}

#[test]
fn hash_interpolation_in_math_stays_interpolated() {
    let text = "$#x$";
    let (spans, _, _) = parse(text);
    let x = text.find('x').unwrap();
    assert!(
        spans
            .iter()
            .any(|(r, kind)| *kind == HighlightKind::Interpolated && *r == (x..x + 1))
    );
}

#[test]
fn value_slot_offers_direction_dictionary_keys() {
    for (marked, param, expected) in [
        ("#set page(margin: (|))", "margin", "top"),
        ("#set page(margin: (|", "margin", "x"),
        ("#rect(inset: (|))", "inset", "bottom"),
    ] {
        let call = run_at_marker(marked)
            .ide
            .call_params
            .unwrap_or_else(|| panic!("{marked:?} must resolve call params"));
        let slot = call
            .value_slot
            .unwrap_or_else(|| panic!("{marked:?} must carry a value slot"));
        assert_eq!(slot.param, param, "{marked:?}");
        assert!(
            slot.items.iter().any(|item| item.name == expected),
            "{marked:?} must offer {expected:?}: {:?}",
            slot.items.iter().map(|item| &item.name).collect::<Vec<_>>()
        );
        assert!(
            slot.items
                .iter()
                .all(|item| item.kind == CompletionKind::Param
                    && item.insert.ends_with(": ")
                    && item.cursor_back == CharCount(0)),
            "{marked:?} dictionary keys must insert `key: `"
        );
    }
}

#[test]
fn value_slot_skips_direction_keys_already_written() {
    let call = run_at_marker("#set page(margin: (y: 1cm, |))")
        .ide
        .call_params
        .expect("a dictionary key slot resolves");
    let slot = call.value_slot.expect("value slot");
    assert_eq!(slot.param, "margin");
    assert!(slot.items.iter().any(|item| item.name == "x"));
    assert!(!slot.items.iter().any(|item| item.name == "y"));
}

#[test]
fn value_slot_does_not_offer_dictionary_keys_outside_a_key_slot() {
    // The caret is inside the value of the `y` pair, not a fresh key slot.
    assert!(
        run_at_marker("#set page(margin: (y: 1c|m))")
            .ide
            .call_params
            .and_then(|call| call.value_slot)
            .is_none_or(|slot| !slot.items.iter().any(|item| item.name == "top"))
    );

    // A dictionary nested in a key's value must not inherit the outer keys.
    assert!(
        run_at_marker("#set page(margin: (x: (|)))")
            .ide
            .call_params
            .and_then(|call| call.value_slot)
            .is_none_or(|slot| !slot.items.iter().any(|item| item.name == "top"))
    );

    // A parameter that is not direction-style keeps its ordinary candidates.
    assert!(
        run_at_marker("#set page(width: (|))")
            .ide
            .call_params
            .and_then(|call| call.value_slot)
            .is_none_or(|slot| !slot.items.iter().any(|item| item.name == "top"))
    );
}

/// The math atoms of `text`, i.e. the byte ranges the wrap must not split.
fn atom_ranges(text: &str) -> Vec<ByteRange> {
    run_parse(text).layout.math_atoms
}

#[test]
fn math_atoms_are_single_leaves_with_call_openers_glued() {
    // `sqrt(` stays glued; `2` and `)` are separate atoms, so a break may fall
    // between `2` and `)`.
    assert_eq!(atom_ranges("$sqrt(2)$"), vec![br(1, 6), br(6, 7), br(7, 8)]);
    assert_eq!(
        atom_ranges("$x + 3.14$"),
        vec![br(1, 2), br(3, 4), br(5, 9)]
    );
    assert_eq!(
        atom_ranges("$sin(3.14)$"),
        vec![br(1, 5), br(5, 9), br(9, 10)]
    );
    // Bare identifiers are leaves, so `sin` and `x` are atoms now.
    assert_eq!(atom_ranges("$sin x$"), vec![br(1, 4), br(5, 6)]);
    // Commas are leaves: a break may legally fall before or after one.
    assert_eq!(
        atom_ranges("$a,b,c$"),
        vec![br(1, 2), br(2, 3), br(3, 4), br(4, 5), br(5, 6)]
    );
}

#[test]
fn math_atoms_cover_delimiters_and_shorthands() {
    assert_eq!(
        atom_ranges("$|sqrt(2)|$"),
        vec![br(1, 2), br(2, 7), br(7, 8), br(8, 9), br(9, 10)]
    );
    assert_eq!(atom_ranges("$a -> b$"), vec![br(1, 2), br(3, 5), br(6, 7)]);
    assert_eq!(atom_ranges("$...$"), vec![br(1, 4)]);
    assert_eq!(atom_ranges("$&$"), vec![br(1, 2)]);
}

#[test]
fn math_atoms_glue_across_whitespace_but_not_without_a_paren() {
    // The parser folds the whitespace-separated `sqrt (2)` into a delimited
    // group, but the name and its opener still glue; the atom spans the trivia.
    assert_eq!(
        atom_ranges("$sqrt (2)$"),
        vec![br(1, 7), br(7, 8), br(8, 9)]
    );
    // No opener: the identifier is its own atom.
    assert_eq!(atom_ranges("$sqrt$"), vec![br(1, 5)]);
}

#[test]
fn math_atoms_extend_numbers_over_units_without_merging_adjacent_atoms() {
    assert_eq!(
        atom_ranges("$12pt + 2em$"),
        vec![br(1, 5), br(6, 7), br(8, 11)]
    );
    assert_eq!(atom_ranges("$50%$"), vec![br(1, 4)]);
    // `)sqrt(` is a boundary: the `)` and the next `sqrt(` stay separate.
    assert_eq!(
        atom_ranges("$sqrt(2)sqrt(3)$"),
        vec![
            br(1, 6),
            br(6, 7),
            br(7, 8),
            br(8, 13),
            br(13, 14),
            br(14, 15)
        ]
    );
    // A spaced unit is a separate identifier, not glued.
    assert_eq!(atom_ranges("$2 em$"), vec![br(1, 2), br(3, 5)]);
}

#[test]
fn math_atoms_skip_code_embedded_in_math() {
    assert!(atom_ranges("$#(12pt)$").is_empty());
    assert!(atom_ranges("#let x = 12pt").is_empty());
    // `#x` is code, but the surrounding math leaves still atomize.
    assert_eq!(atom_ranges("$#x + 1$"), vec![br(4, 5), br(6, 7)]);
}

#[test]
fn math_atoms_follow_nested_equations_but_not_strings_or_comments() {
    let nested = "#let x = [a $sqrt(2) + 3.14$ b]";
    assert_eq!(
        atom_ranges(nested),
        vec![br(13, 18), br(18, 19), br(19, 20), br(21, 22), br(23, 27)]
    );

    assert!(atom_ranges("#let s = \"$sqrt(2)$\"").is_empty());
    assert!(atom_ranges("/* $sqrt(2)$ */").is_empty());
    // A bare identifier in an inline equation is a leaf atom now.
    assert_eq!(atom_ranges("text $inline$ more"), vec![br(6, 12)]);
}

#[test]
fn math_atoms_are_sorted_disjoint_and_run_timed_reports_them() {
    let ranges = atom_ranges("$x + 3.14$ and $sqrt(2) + 2em$");
    assert_eq!(
        ranges,
        vec![
            br(1, 2),
            br(3, 4),
            br(5, 9),
            br(16, 21),
            br(21, 22),
            br(22, 23),
            br(24, 25),
            br(26, 29)
        ]
    );
    // Adjacent atoms are allowed (a break may fall between them); they must
    // only never overlap.
    assert!(ranges.windows(2).all(|w| w[0].end <= w[1].start));

    let (result, _) = run_timed("$sqrt(2) + 3.14$", 0, false);
    assert_eq!(
        result.layout.math_atoms,
        vec![br(1, 6), br(6, 7), br(7, 8), br(9, 10), br(11, 15)]
    );
}

/// `fold_ranges` for `text` as `(start line, end line, kind)` tuples.
fn fold_lines(text: &str) -> Vec<(usize, usize, FoldKind)> {
    let source = Source::new(parse_file_id(), text.to_owned());
    let lines = source.lines();
    fold_ranges(&source)
        .into_iter()
        .map(|range| {
            (
                lines
                    .byte_to_line(range.start.0)
                    .expect("start within text"),
                lines.byte_to_line(range.end.0).expect("end within text"),
                range.kind,
            )
        })
        .collect()
}

#[test]
fn fold_ranges_default_empty_and_run_fills_them() {
    assert!(ParseResult::default().folds.is_empty());
    assert!(!run_parse("= H\nbody").folds.is_empty());
}

#[test]
fn heading_sections_nest_by_level() {
    let text = "= A\nintro a\n\n== B\nintro b\n\n=== C\nintro c\n\n= D\nintro d\n";
    assert_eq!(
        fold_lines(text),
        vec![
            (0, 7, FoldKind::Heading),
            (3, 7, FoldKind::Heading),
            (6, 7, FoldKind::Heading),
            (9, 10, FoldKind::Heading),
        ]
    );
}

#[test]
fn heading_with_only_a_title_line_does_not_fold() {
    // The next heading starts on the following line, so the section has no body.
    assert!(fold_lines("= A\n= B\n").is_empty());
}

#[test]
fn blocks_fold_inner_lines_but_keep_the_closing_delimiter_line() {
    assert_eq!(
        fold_lines("#{\n  let x = 1\n  let y = 2\n}\n"),
        vec![(0, 2, FoldKind::Block)]
    );
    assert_eq!(
        fold_lines("#table(\n  [a],\n  [b],\n)\n"),
        vec![(0, 2, FoldKind::Block)]
    );
}

#[test]
fn block_comment_folds_its_body_lines() {
    assert_eq!(
        fold_lines("/* first\nsecond */\n"),
        vec![(0, 1, FoldKind::Comment)]
    );
}

#[test]
fn multi_line_list_item_folds() {
    assert_eq!(
        fold_lines("- first line\n  continued\n- second\n"),
        vec![(0, 1, FoldKind::List)]
    );
}

#[test]
fn multi_line_raw_and_equation_fold() {
    assert_eq!(
        fold_lines("```\nraw body\n```\n"),
        vec![(0, 2, FoldKind::Raw)]
    );
    assert_eq!(
        fold_lines("$\nx + 1\n$\n"),
        vec![(0, 2, FoldKind::Equation)]
    );
}

#[test]
fn single_line_constructs_produce_no_candidates() {
    assert!(
        fold_lines("- item\n\n`code`\n\n$x$\n\n#table([a], [b])\n").is_empty(),
        "single-line lists, raw, equations and calls must not fold"
    );
}

/// The prose segments of `text`, extracted through the spell-enabled path.
fn prose(text: &str) -> Vec<ProseSegment> {
    run_timed(text, 0, true).0.prose
}

#[test]
fn prose_heading_is_one_segment_without_the_marker() {
    let text = "= Hello *world*";
    let segments = prose(text);
    assert_eq!(segments.len(), 1);
    let start = text.find("Hello").unwrap();
    assert_eq!(segments[0].range, br(start, text.len()));
    assert_eq!(
        &text[segments[0].range.start.0..segments[0].range.end.0],
        "Hello *world*"
    );
    assert!(segments[0].holes.is_empty());
}

#[test]
fn prose_paragraphs_split_at_a_blank_line() {
    let text = "one\n\ntwo";
    let segments = prose(text);
    assert_eq!(segments.len(), 2);
    let one = text.find("one").unwrap();
    let two = text.find("two").unwrap();
    assert_eq!(segments[0].range, br(one, one + 3));
    assert_eq!(segments[1].range, br(two, two + 3));
    assert!(segments.iter().all(|segment| segment.holes.is_empty()));
}

#[test]
fn prose_list_items_are_separate_runs_without_markers() {
    let text = "- one\n- two";
    let segments = prose(text);
    assert_eq!(segments.len(), 2);
    let one = text.find("one").unwrap();
    let two = text.find("two").unwrap();
    assert_eq!(segments[0].range, br(one, one + 3));
    assert_eq!(segments[1].range, br(two, two + 3));
}

#[test]
fn prose_inline_holes_cover_math_raw_and_refs() {
    let text = "Text with $x^2$ and `code` and @ref.";
    let segments = prose(text);
    assert_eq!(segments.len(), 1);
    let segment = &segments[0];
    assert_eq!(segment.range, br(0, text.len()));
    let math = text.find("$x^2$").unwrap();
    let raw = text.find("`code`").unwrap();
    let reference = text.find("@ref").unwrap();
    assert_eq!(
        segment.holes,
        vec![
            br(math, math + 5),
            br(raw, raw + 6),
            br(reference, reference + 4)
        ]
    );
}

#[test]
fn prose_hash_block_contributes_none_but_following_paragraph_does() {
    let text = "#let x = 1\n\npara";
    let segments = prose(text);
    assert_eq!(segments.len(), 1);
    let para = text.find("para").unwrap();
    assert_eq!(segments[0].range, br(para, para + 4));
}

#[test]
fn prose_hash_content_block_keeps_its_text_inline() {
    let text = "before #link(\"url\")[linked text] after";
    let segments = prose(text);
    assert_eq!(segments.len(), 1);
    let segment = &segments[0];
    assert_eq!(segment.range, br(0, text.len()));
    let call = text.find("#link").unwrap();
    let body = text.find("[linked").unwrap();
    assert_eq!(segment.holes, vec![br(call, body)]);
}

#[test]
fn prose_is_empty_when_not_requested() {
    let text = "prose here";
    assert!(run_timed(text, 0, false).0.prose.is_empty());
    assert!(!run_timed(text, 0, true).0.prose.is_empty());
}

#[test]
fn prose_keeps_a_cjk_only_run() {
    let text = "你好世界";
    let segments = prose(text);
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].range, br(0, text.len()));
}

#[test]
fn folds_are_sorted_and_deduplicated() {
    let text = "= A\nintro\n\n#{\n  x\n}\n\n== B\nbody\n\n- item\n  cont\n";
    let source = Source::new(parse_file_id(), text.to_owned());
    let folds = fold_ranges(&source);
    assert!(!folds.is_empty());
    assert!(
        folds
            .windows(2)
            .all(|w| (w[0].start, w[0].end) <= (w[1].start, w[1].end)),
        "candidates must be sorted by start: {folds:?}"
    );
    let mut deduped = folds.clone();
    deduped.dedup();
    assert_eq!(deduped.len(), folds.len(), "no duplicate candidates");
}

/// The symbol tree of `text` through the full parse path.
fn parse_symbols(text: &str) -> Vec<SymbolNode> {
    run_parse(text).symbols
}

#[test]
fn symbols_outline_is_nested_and_filled_by_parse() {
    let text = "= Title\n\n#let f(a) = a\n\n== Sub\n\n#let x = 1";
    let symbols = parse_symbols(text);
    assert!(!symbols.is_empty(), "the parse must fill the symbol tree");

    let title = symbols
        .iter()
        .find(|symbol| symbol.name == "Title")
        .expect("heading Title");
    assert_eq!(title.kind, SymbolKind::Heading(1));

    // The function and its parameter nest under the heading section.
    let function = title
        .children
        .iter()
        .find(|symbol| symbol.name == "f")
        .expect("function under Title");
    assert_eq!(function.kind, SymbolKind::Function);
    let param = function
        .children
        .iter()
        .find(|symbol| symbol.name == "a")
        .expect("parameter a under f");
    assert_eq!(param.kind, SymbolKind::Parameter);

    // A deeper heading stays a child of the shallower one, and its own section
    // content nests under it.
    let sub = title
        .children
        .iter()
        .find(|symbol| symbol.name == "Sub")
        .expect("subsection under Title");
    assert_eq!(sub.kind, SymbolKind::Heading(2));
    let variable = sub
        .children
        .iter()
        .find(|symbol| symbol.name == "x")
        .expect("variable x under Sub");
    assert_eq!(variable.kind, SymbolKind::Variable);
}

#[test]
fn lint_diagnostics_are_appended_after_syntax_diagnostics() {
    let result = run_parse("#let = 1\n#break");
    let first_warning = result
        .diagnostics
        .iter()
        .position(|diag| diag.severity == Severity::Warning)
        .expect("a lint warning");
    let last_error = result
        .diagnostics
        .iter()
        .rposition(|diag| diag.severity == Severity::Error)
        .expect("a syntax error");
    assert!(
        last_error < first_warning,
        "syntax diagnostics must precede lints: {:?}",
        result.diagnostics
    );
    assert!(
        result.diagnostics[first_warning]
            .message
            .contains("non-loop context"),
        "{:?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diag| diag.path.as_os_str().is_empty()),
        "the caller fills in the path"
    );
}

#[test]
fn a_lint_only_document_reports_a_warning() {
    let result = run_parse("#return");
    assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
    assert_eq!(result.diagnostics[0].severity, Severity::Warning);
    assert_eq!(result.diagnostics[0].range, br(1, 7));
    assert_eq!(
        result.diagnostics[0].message,
        "`return` statement in a non-function context"
    );
}
