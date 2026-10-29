//! Fixture-mirroring tests for the pure syntax lints.
//!
//! Each case echoes a tinymist fixture under
//! `crates/tinymist-query/src/fixtures/lint/`, asserting the same message (with
//! hints/trace folded into it) and byte range.

use typ_edit_model::{BytePos, ByteRange, Severity};
use typst::syntax::Source;

use super::lint_source;

/// Lints `text` as `(range, message)` pairs, checking the shared invariants.
fn lints(text: &str) -> Vec<(ByteRange, String)> {
    lint_source(&Source::detached(text))
        .into_iter()
        .map(|diag| {
            assert_eq!(diag.severity, Severity::Warning);
            assert!(diag.path.as_os_str().is_empty(), "path is filled by caller");
            (diag.range, diag.message)
        })
        .collect()
}

/// The single byte range of `needle` inside `text`.
fn range_of(text: &str, needle: &str) -> ByteRange {
    let start = text.find(needle).expect("needle in text");
    BytePos(start)..BytePos(start + needle.len())
}

#[test]
fn break_and_continue_outside_a_loop_warn() {
    assert_eq!(
        lints("#break"),
        vec![(
            BytePos(1)..BytePos(6),
            "`break` statement in a non-loop context".to_owned()
        )]
    );
    assert_eq!(
        lints("#continue"),
        vec![(
            BytePos(1)..BytePos(9),
            "`continue` statement in a non-loop context".to_owned()
        )]
    );
}

#[test]
fn break_inside_a_loop_is_allowed() {
    assert!(lints("#for value in (1, 2, 3) {\n  break\n}").is_empty());
    assert!(lints("#while true {\n  break\n}").is_empty());
}

#[test]
fn break_in_a_closure_inside_a_loop_warns_and_cites_the_trace() {
    let text = "#for value in (1, 2, 3) {\n  let _ = () => break\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "break"),
            "`break` statement in a non-loop context\n\
             while showing loop element\nwhile calling function"
                .to_owned()
        )]
    );
}

#[test]
fn return_outside_a_function_warns_even_inside_a_loop() {
    assert_eq!(
        lints("#return"),
        vec![(
            BytePos(1)..BytePos(7),
            "`return` statement in a non-function context".to_owned()
        )]
    );
    let text = "#while true {\n  return\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "return"),
            "`return` statement in a non-function context".to_owned()
        )]
    );
}

#[test]
fn return_inside_a_function_or_context_is_allowed() {
    assert!(lints("#let f() = return").is_empty());
    assert!(lints("#let f() = while true {\n  return\n}").is_empty());
    assert!(lints("#context return").is_empty());
}

#[test]
fn set_in_an_ineffective_show_block_warns_with_hint() {
    let text = "#show: {\n  set text(red)\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "set text(red)"),
            "This set statement doesn't take effect.\n\
             Hint: consider changing parent to `show : set text(red)`"
                .to_owned()
        )]
    );
}

#[test]
fn set_in_an_ineffective_if_branch_warns_with_hint() {
    let text = "#if false {\n  set text(red)\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "set text(red)"),
            "This set statement doesn't take effect.\n\
             Hint: consider changing parent to `set text(red) if (false)`"
                .to_owned()
        )]
    );
}

#[test]
fn show_in_an_ineffective_if_branch_warns_with_hint() {
    let text = "#if false {\n  show: text(red)\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "show: text(red)"),
            "This show statement doesn't take effect.\n\
             Hint: consider changing parent to `show : if (false) { .. }`"
                .to_owned()
        )]
    );
}

#[test]
fn show_in_an_ineffective_for_block_warns_with_fold_hint() {
    let text = "#let f() = for i in range(10) {\n  show: it => it\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "show: it => it"),
            "This show statement doesn't take effect.\nHint: consider changing parent to \
             `show: range(10).fold(it => it, (style-it, i) => it => { show: it => it; style-it(it) })`"
                .to_owned()
        )]
    );
}

#[test]
fn blocks_with_real_content_or_only_trivia_are_not_buggy() {
    assert!(lints("#let f() = {\n  show: it => it\n  [Test]\n}").is_empty());
    assert!(lints("#show: {\n}").is_empty());
    assert!(lints("#show: {\n  // comment\n}").is_empty());
    assert!(lints("#while true {\n  [0]\n  break\n}").is_empty());
}

#[test]
fn value_before_a_value_return_is_discarded() {
    let text = "#let f() = {\n  set text(red)\n  return 1;\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "set text(red)"),
            "This `set` expression is implicitly discarded by function return".to_owned()
        )]
    );
}

#[test]
fn set_before_a_bare_return_is_discarded() {
    let text = "#let f() = {\n  set text(red)\n  return;\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "set text(red)"),
            "This `set` expression is implicitly discarded by function return".to_owned()
        )]
    );
}

#[test]
fn array_before_a_return_warns_with_the_underscore_hint() {
    let text = "#let f() = [\n  #(1, 2)\n  #return 1;\n]";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "(1, 2)"),
            "This array is implicitly discarded by function return\nHint: consider ignoring \
             the value explicitly using underscore: `let _ = (1, 2)`"
                .to_owned()
        )]
    );
}

#[test]
fn binary_operands_and_bindings_are_not_discarded() {
    assert!(lints("#let f() = {\n  1 + (1,)\n  return 0\n}").is_empty());
    assert!(lints("#let f() = {\n  let x = (1,)\n  return 1\n}").is_empty());
    assert!(lints("#let f() = {\n  let x\n  x = (1,)\n  return\n}").is_empty());
}

#[test]
fn conditional_branches_warn_independently_but_suppress_the_outer_value() {
    let text =
        "#let f() = {\n  [0]\n  if true {\n    [1]\n  } else {\n    [2]\n  }\n  return [];\n}";
    let one = range_of(text, "1");
    let two = range_of(text, "2");
    let zero = range_of(text, "0");
    let got = lints(text);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].0, one);
    assert_eq!(got[1].0, two);
    assert!(
        got.iter().all(|(range, _)| range.start != zero.start),
        "the value before the conditional is suppressed by the join: {got:?}"
    );
}

#[test]
fn discarded_show_in_a_function_body_warns() {
    let text = "#let f() = {\n  show: it => it\n  return 1;\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "show: it => it"),
            "This `show` expression is implicitly discarded by function return".to_owned()
        )]
    );
}

#[test]
fn discarded_set_in_an_if_branch_warns() {
    let text = "#let f() = if true {\n  set text(red)\n  return;\n} else {\n  return [];\n}";
    assert_eq!(
        lints(text),
        vec![(
            range_of(text, "set text(red)"),
            "This `set` expression is implicitly discarded by function return".to_owned()
        )]
    );
}
