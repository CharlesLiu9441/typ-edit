//! Highlight tests for identifiers defined by `#let` and friends.

use std::ops::Range;

use super::*;
use typ_edit_model::HighlightKind;
use typst::syntax::{LinkedNode, Source};

fn highlights_of(text: &str) -> Vec<(Range<usize>, HighlightKind)> {
    let source = Source::new(parse_file_id(), text.to_owned());
    let root = LinkedNode::new(source.root());
    let fused = visit(&root, source.text());
    let defined = DefinedNames::from_items(&fused.definitions);
    highlights(&root, &defined)
}

fn is_variable(spans: &[(Range<usize>, HighlightKind)], range: Range<usize>) -> bool {
    spans
        .iter()
        .any(|(r, kind)| *kind == HighlightKind::Interpolated && *r == range)
}

#[test]
fn let_binding_highlights_definition_and_references() {
    let text = "#let foo = 1\n#let bar = foo";
    let spans = highlights_of(text);
    let foo_def = text.find("foo").unwrap();
    let bar = text.find("bar").unwrap();
    let foo_ref = text.rfind("foo").unwrap();

    assert!(is_variable(&spans, foo_def..foo_def + 3), "definition name");
    assert!(is_variable(&spans, bar..bar + 3), "another binding");
    assert!(is_variable(&spans, foo_ref..foo_ref + 3), "reference");
}

#[test]
fn undefined_identifiers_stay_unhighlighted() {
    let text = "#let bar = baz";
    let spans = highlights_of(text);
    let bar = text.find("bar").unwrap();
    let baz = text.find("baz").unwrap();

    assert!(is_variable(&spans, bar..bar + 3));
    assert!(
        spans.iter().all(|(range, _)| range != &(baz..baz + 3)),
        "an undefined identifier must not be highlighted"
    );
}

#[test]
fn closure_params_and_imports_are_highlighted() {
    let text = "#let f(x) = x\n#import \"m.typ\": thing\n#let y = thing";
    let spans = highlights_of(text);
    let param = text.find("(x)").unwrap() + 1;
    let thing = text.find("thing").unwrap();
    let thing_ref = text.rfind("thing").unwrap();

    assert!(is_variable(&spans, param..param + 1), "closure parameter");
    assert!(is_variable(&spans, thing..thing + 5), "imported name");
    assert!(is_variable(&spans, thing_ref..thing_ref + 5), "reference");
}

#[test]
fn defined_name_highlight_yields_to_upstream_function_tag() {
    let text = "#let f = 1\n#f()";
    let spans = highlights_of(text);
    let call = text.rfind('f').unwrap();

    assert!(
        spans
            .iter()
            .any(|(r, kind)| *kind == HighlightKind::Function && *r == (call..call + 1)),
        "a called binding keeps the upstream Function tag"
    );
}
