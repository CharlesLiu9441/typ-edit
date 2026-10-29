//! Pure, egui-free completion logic: context/call detection from the buffer and
//! cursor, candidate filtering/ranking, and the insert-text computation. The
//! popup state and the egui-facing handling live in the parent module.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use typ_edit_buffer::TextBuffer;
use typ_edit_i18n::t_static;
use typ_edit_model::{CallParams, CharCount, CompletionItem, CompletionKind};

/// Where the word ending at the caret is completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionContext {
    /// Plain markup: no completion.
    Markup,
    /// Code mode: keywords, values and document definitions.
    Code,
    /// Math mode (`$...$`): math symbols only.
    Math,
}

/// Why the completion popup is being recomputed; decides when a closed popup
/// may open.
#[derive(Clone, Copy)]
pub(crate) enum CompletionGate {
    /// The user typed a character.
    Keystroke(Option<char>),
    /// A fresh parse result arrived.
    Parse,
    /// The user pressed Ctrl/Cmd+Space.
    Manual,
}

/// Decides the completion context of the word starting at `word_start`.
///
/// The parse ranges lag one edit behind: while typing `#a` they still describe
/// the `#`, whose hash leaf is not a code range. A `#` starts logic mode in
/// markup, so a word directly after it counts as code even with empty ranges;
/// the same covers the caret directly after `#` (there `word_start` is the
/// caret and the full logic list is shown).
pub(crate) fn completion_context(
    buffer: &TextBuffer,
    word_start: usize,
    code_ranges: &[Range<usize>],
    math_ranges: &[Range<usize>],
) -> CompletionContext {
    // The context is decided at the start of the word being typed: the caret
    // itself sits *after* the last char, where no half-open range contains it.
    let context_byte = buffer.char_to_byte(word_start);
    let inside = |ranges: &[Range<usize>]| {
        ranges
            .iter()
            .any(|range| range.start <= context_byte && context_byte < range.end)
    };
    if word_after_hash(buffer, word_start) || inside(code_ranges) {
        CompletionContext::Code
    } else if inside(math_ranges) {
        CompletionContext::Math
    } else {
        CompletionContext::Markup
    }
}

/// Whether the character before `word_start` is a `#`.
///
/// In markup a `#` always starts logic mode, so text directly after it is code.
pub(crate) fn word_after_hash(buffer: &TextBuffer, word_start: usize) -> bool {
    word_start > 0 && char_before_caret(buffer, word_start) == Some('#')
}

/// Decides the context of a just-typed member access ending at `caret_char`.
///
/// [`completion_context`] decides from the start of the whole dotted word
/// ([`completion_prefix`] includes the `.`), which is enough for `#foo.`
/// (the `#` starts logic mode) and for a word inside a code/math range. The
/// parse ranges lag one edit behind, though: for `foo().` they still cover
/// `foo()` but not the freshly typed `.`, so the word start falls outside every
/// range. In that case probe the char immediately before the dot, which the
/// stale range still contains. A `.` that is not part of a code or math
/// expression stays [`CompletionContext::Markup`], so it never opens the popup.
pub(crate) fn member_completion_context(
    buffer: &TextBuffer,
    caret_char: usize,
    code_ranges: &[Range<usize>],
    math_ranges: &[Range<usize>],
) -> CompletionContext {
    let (word_start, _) = completion_prefix_buffer(buffer, caret_char);
    let context = completion_context(buffer, word_start, code_ranges, math_ranges);
    if context != CompletionContext::Markup {
        return context;
    }
    // The fallback is only meaningful for the stale case of a just-typed dot:
    // without a dot right before the caret the char before it belongs to an
    // unrelated word and must not promote plain markup to code.
    if char_before_caret(buffer, caret_char) != Some('.') {
        return CompletionContext::Markup;
    }
    let Some(prev_index) = caret_char.checked_sub(2) else {
        return CompletionContext::Markup;
    };
    let probe_byte = buffer.char_to_byte(prev_index);
    let inside = |ranges: &[Range<usize>]| {
        ranges
            .iter()
            .any(|range| range.start <= probe_byte && probe_byte < range.end)
    };
    if inside(code_ranges) {
        CompletionContext::Code
    } else if inside(math_ranges) {
        CompletionContext::Math
    } else {
        CompletionContext::Markup
    }
}

/// Dim localized tag shown next to a candidate name.
pub(crate) fn kind_tag(kind: CompletionKind) -> &'static str {
    match kind {
        CompletionKind::Keyword => t_static("editor.completion.kind.keyword"),
        CompletionKind::Value => t_static("editor.completion.kind.value"),
        CompletionKind::Type => t_static("editor.completion.kind.type"),
        CompletionKind::Function => t_static("editor.completion.kind.function"),
        CompletionKind::Element => t_static("editor.completion.kind.element"),
        CompletionKind::MathFunction => t_static("editor.completion.kind.math_function"),
        CompletionKind::MathSymbol => t_static("editor.completion.kind.math_symbol"),
        CompletionKind::Symbol => t_static("editor.completion.kind.symbol"),
        CompletionKind::Local => t_static("editor.completion.kind.local"),
        CompletionKind::Label => t_static("editor.completion.kind.label"),
        CompletionKind::Param => t_static("editor.completion.kind.param"),
    }
}

/// The char immediately before `caret_char`, if any.
pub(crate) fn char_before_caret(buffer: &TextBuffer, caret_char: usize) -> Option<char> {
    caret_char
        .checked_sub(1)
        .and_then(|index| buffer.char_at(index))
}

/// Dim lines shown under the selected completion: the inserted text when it
/// differs from the name (a parameter renders `name: `), then the doc summary.
pub(crate) fn completion_preview(item: &CompletionItem) -> Vec<String> {
    let mut lines = Vec::new();
    if item.insert != item.name {
        lines.push(item.insert.clone());
    }
    if let Some(docs) = &item.docs {
        lines.push(docs.clone());
    }
    lines
}

/// Whether the just-typed character may open the completion popup.
///
/// ASCII letters, `#` and `.` do; any other keystroke may still refresh an
/// already-open popup but never opens a closed one. A `.` continues a dotted
/// member access (`cell.` -> `cell.x`); the caller's prefix-length gate keeps a
/// lone `.` from opening.
pub(crate) fn completion_trigger(ch: Option<char>) -> bool {
    ch.is_some_and(|c| c.is_ascii_alphabetic() || c == '#' || c == '.')
}

/// Whether typing `typed` right after `prev` starts a field access and should
/// request world-backed member completions (`it.`, `foo().`, `arr[0].`).
///
/// A `.` qualifies only after an identifier-ish char: alphanumeric (which
/// covers CJK, since ideographs are alphabetic) and `_`, plus the closing
/// `)`/`]` of a call or index expression.
pub(crate) fn is_member_completion_trigger(prev: Option<char>, typed: char) -> bool {
    typed == '.' && prev.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ')' | ']'))
}

/// Cheap identity of a world-completion list, used to refresh the popup when an
/// asynchronously fetched member list arrives or is cleared. `None` for an empty
/// list; the visible accept-path fields are hashed.
pub(crate) fn world_completions_signature(items: &[CompletionItem]) -> Option<(usize, u64)> {
    if items.is_empty() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for item in items {
        item.name.hash(&mut hasher);
        item.insert.hash(&mut hasher);
        item.cursor_back.hash(&mut hasher);
    }
    Some((items.len(), hasher.finish()))
}

/// Whether the completion popup may open for the just-typed character.
///
/// Letters and `#` start a word; a comma re-opens the surrounding call's
/// parameter list (the popup often closes while the previous argument is
/// being typed, so the comma must be able to bring it back). A `.` is excluded
/// here: it only opens when it extends an existing dotted head, which needs the
/// prefix length and is therefore gated by `refresh_completion`.
pub(crate) fn completion_may_open(ch: Option<char>, in_call: bool) -> bool {
    (completion_trigger(ch) && ch != Some('.')) || (ch == Some(',') && in_call)
}

/// Whether accepting `item` opens a call and must therefore leave the popup
/// eligible to reopen with the parameter list: a function/element/math function,
/// or any insert that opens a call (`func(`, `head.where(`). Non-callables keep
/// the ordinary dismiss behavior.
pub(crate) fn is_callable_accept(item: &CompletionItem) -> bool {
    item.kind.is_callable() || item.insert.ends_with('(')
}

/// Whether `prefix` is a numeric token (digit-led, optionally with a decimal
/// point or a partial unit) eligible for unit completion.
pub(crate) fn numeric_prefix(prefix: &str) -> bool {
    prefix.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Whether the popup may consume Enter: an unfiltered parameter list is
/// informational only, so Enter must still open a new argument line. A typed
/// prefix or manual navigation makes the popup the Enter owner.
pub(crate) fn popup_owns_enter(params: bool, navigated: bool, has_prefix: bool) -> bool {
    !params || navigated || has_prefix
}

/// Scans the word (`[A-Za-z0-9_.]`) ending at `caret_char`.
///
/// Returns the char index where the word starts plus the word itself. An empty
/// word yields `(caret_char, "")`; whether that triggers completion (e.g. right
/// after `#`, which starts logic mode) is up to the caller. `.` is part of the
/// word so a dotted path like `grid.cel` matches the `grid.cell` candidate.
#[cfg(test)]
pub(crate) fn completion_prefix<'a>(
    buffer: &TextBuffer,
    text: &'a str,
    caret_char: usize,
) -> (usize, &'a str) {
    let caret_byte = buffer.char_to_byte(caret_char);
    let mut word_start = buffer.byte_to_char(caret_byte);
    for (byte, ch) in text[..caret_byte].char_indices().rev() {
        if !ch.is_ascii_alphanumeric() && ch != '_' && ch != '.' {
            break;
        }
        word_start = buffer.byte_to_char(byte);
    }
    (
        word_start,
        &text[buffer.char_to_byte(word_start)..caret_byte],
    )
}

pub(crate) fn completion_prefix_buffer(buffer: &TextBuffer, caret_char: usize) -> (usize, String) {
    let caret = caret_char.min(buffer.len_chars());
    let mut word_start = caret;
    while word_start > 0 {
        let Some(ch) = buffer.char_at(word_start - 1) else {
            break;
        };
        if !ch.is_ascii_alphanumeric() && ch != '_' && ch != '.' {
            break;
        }
        word_start -= 1;
    }
    (word_start, buffer.slice(word_start..caret))
}

/// Buffer-backed version of [`member_prefix`] for callers that must not
/// materialize the whole document.
pub(crate) fn member_prefix_buffer(buffer: &TextBuffer, caret_char: usize) -> (usize, String) {
    let caret = caret_char.min(buffer.len_chars());
    let mut word_start = caret;
    while word_start > 0 {
        let Some(ch) = buffer.char_at(word_start - 1) else {
            break;
        };
        if !ch.is_alphanumeric() && ch != '_' {
            break;
        }
        word_start -= 1;
    }
    (word_start, buffer.slice(word_start..caret))
}

/// Scans the member identifier ending at `caret_char` (the name being typed
/// after a `.`).
///
/// Unlike [`completion_prefix`] the dot is not part of the word, so the
/// returned start is the member word start and the prefix filters only member
/// names: `it.ch` -> `(3, "ch")`, `it.` -> `(3, "")`. Unicode alphanumerics
/// (CJK included) are kept.
#[cfg(test)]
pub(crate) fn member_prefix<'a>(
    buffer: &TextBuffer,
    text: &'a str,
    caret_char: usize,
) -> (usize, &'a str) {
    let caret_byte = buffer.char_to_byte(caret_char);
    let mut word_start = buffer.byte_to_char(caret_byte);
    for (byte, ch) in text[..caret_byte].char_indices().rev() {
        if !ch.is_alphanumeric() && ch != '_' {
            break;
        }
        word_start = buffer.byte_to_char(byte);
    }
    (
        word_start,
        &text[buffer.char_to_byte(word_start)..caret_byte],
    )
}

/// Whether the caret sits in a resolved call's open argument list.
///
/// The parse side already guarantees the caret is in a legal slot, so only the
/// lower bound matters: after `#figure(a, ` the caret is past `range.end` yet
/// still inside the call.
pub(crate) fn caret_in_call(params: &CallParams, caret_byte: usize) -> bool {
    params.range.start.0 <= caret_byte
}

/// Candidates for a caret inside a call's argument list.
///
/// The callee's named parameters matching `prefix` come first, then the normal
/// code candidates (document definitions, then the standard library) filtered
/// and ranked for `prefix`. A dotted prefix matches no parameter and yields the
/// library's dotted expressions (`grid.` -> `grid.cell`). Names keep their
/// first occurrence, so a parameter shadows a library entry of the same name.
/// The merged result holds at most `cap` items.
pub(crate) fn call_slot_items(
    prefix: &str,
    call: &CallParams,
    definitions: &[CompletionItem],
    library: &[CompletionItem],
    cap: usize,
) -> Vec<CompletionItem> {
    let mut items = rank_completions(prefix, &call.items, &[], false, cap);
    items.extend(dotted_member_items(
        prefix,
        definitions,
        library,
        false,
        cap,
    ));
    let mut seen: HashSet<String> = HashSet::new();
    items.retain(|item| seen.insert(item.name.clone()));
    items.truncate(cap);
    items
}

/// Method candidates for the tail of a dotted prefix (`heading.` -> `.where`).
///
/// A field access on a callable library value (element or function) exposes
/// Typst's function methods `where` and `with`. Only the tail after the last
/// dot filters; the insert replaces the whole `head.` prefix, so accepting
/// `where` writes `heading.where(` and leaves the caret inside the parentheses.
pub(crate) fn dotted_method_items(prefix: &str, library: &[CompletionItem]) -> Vec<CompletionItem> {
    let Some(dot) = prefix.rfind('.') else {
        return Vec::new();
    };
    let head = &prefix[..dot];
    let callable = library
        .iter()
        .any(|item| item.name == head && item.kind.is_callable());
    if head.is_empty() || !callable {
        return Vec::new();
    }
    let tail = prefix[dot + 1..].to_lowercase();
    ["where", "with"]
        .iter()
        .filter(|method| method.contains(&tail))
        .map(|method| CompletionItem {
            name: format!("{head}.{method}"),
            kind: CompletionKind::Function,
            insert: format!("{head}.{method}("),
            cursor_back: CharCount(1),
            docs: None,
        })
        .collect()
}

/// Filters and ranks both candidate lists for `prefix`.
///
/// Document-local definitions come first, then the standard library; names are
/// deduplicated case-insensitively (a definition shadows a library entry).
/// Case-insensitive `starts_with` matches rank before `contains` matches, and
/// the input order (both slices are sorted by name) is kept within each rank.
/// The result holds at most `cap` items.
pub(crate) fn rank_completions(
    prefix: &str,
    definitions: &[CompletionItem],
    library: &[CompletionItem],
    math: bool,
    cap: usize,
) -> Vec<CompletionItem> {
    if prefix.is_empty() || cap == 0 {
        return Vec::new();
    }
    let needle = prefix.to_lowercase();
    let mut seen: HashSet<String> = HashSet::new();
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for item in definitions.iter().chain(library) {
        if item.kind.is_math() != math {
            continue;
        }
        let name = item.name.to_lowercase();
        // Code identifiers dedup case-insensitively (a definition shadows a
        // library entry); math symbols are case-sensitive, so `delta` and
        // `Delta` both survive.
        let key = if math {
            item.name.clone()
        } else {
            name.clone()
        };
        if !seen.insert(key) || !name.contains(&needle) {
            continue;
        }
        if name.starts_with(&needle) {
            starts.push(item.clone());
        } else {
            contains.push(item.clone());
        }
    }
    starts.extend(contains);
    starts.truncate(cap);
    starts
}

/// Filters and ranks both candidate lists for `prefix`, member-only for a dot.
///
/// When `prefix` contains a `.` only names that start with the lowercased
/// prefix survive, so `cell.` yields `cell.x`/`cell.colspan` but never
/// `grid.cell.x` (the head must be the dotted head itself). Kept names are in
/// input order (both slices are sorted) and deduplicated exactly like
/// [`rank_completions`] — case-sensitive for math, case-insensitive otherwise.
/// A prefix without a dot behaves exactly like [`rank_completions`].
pub(crate) fn dotted_member_items(
    prefix: &str,
    definitions: &[CompletionItem],
    library: &[CompletionItem],
    math: bool,
    cap: usize,
) -> Vec<CompletionItem> {
    if !prefix.contains('.') {
        return rank_completions(prefix, definitions, library, math, cap);
    }
    if prefix.is_empty() || cap == 0 {
        return Vec::new();
    }
    let needle = prefix.to_lowercase();
    let mut seen: HashSet<String> = HashSet::new();
    let mut items = Vec::new();
    for item in definitions.iter().chain(library) {
        if item.kind.is_math() != math {
            continue;
        }
        let name = item.name.to_lowercase();
        let key = if math {
            item.name.clone()
        } else {
            name.clone()
        };
        if !seen.insert(key) || !name.starts_with(&needle) {
            continue;
        }
        items.push(item.clone());
        if items.len() == cap {
            break;
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_model::BytePos;

    fn candidate(name: &str, kind: CompletionKind) -> CompletionItem {
        CompletionItem::plain(name, kind)
    }

    /// Rope-backed buffer over `text`, so the char↔byte conversions under test
    /// use the same path as production.
    fn buffer(text: &str) -> TextBuffer {
        TextBuffer::from_text(text)
    }

    #[test]
    fn kind_tag_maps_every_completion_kind() {
        assert_eq!(
            kind_tag(CompletionKind::Keyword),
            t_static("editor.completion.kind.keyword")
        );
        assert_eq!(
            kind_tag(CompletionKind::Value),
            t_static("editor.completion.kind.value")
        );
        assert_eq!(
            kind_tag(CompletionKind::Type),
            t_static("editor.completion.kind.type")
        );
        assert_eq!(
            kind_tag(CompletionKind::Function),
            t_static("editor.completion.kind.function")
        );
        assert_eq!(
            kind_tag(CompletionKind::Element),
            t_static("editor.completion.kind.element")
        );
        assert_eq!(
            kind_tag(CompletionKind::MathFunction),
            t_static("editor.completion.kind.math_function")
        );
        assert_eq!(
            kind_tag(CompletionKind::MathSymbol),
            t_static("editor.completion.kind.math_symbol")
        );
        assert_eq!(
            kind_tag(CompletionKind::Symbol),
            t_static("editor.completion.kind.symbol")
        );
        assert_eq!(
            kind_tag(CompletionKind::Local),
            t_static("editor.completion.kind.local")
        );
        assert_eq!(
            kind_tag(CompletionKind::Label),
            t_static("editor.completion.kind.label")
        );
        assert_eq!(
            kind_tag(CompletionKind::Param),
            t_static("editor.completion.kind.param")
        );
    }

    #[test]
    fn word_after_hash_is_false_at_the_buffer_start() {
        assert!(
            !word_after_hash(&buffer("#a"), 0),
            "no char before position 0"
        );
        assert!(
            word_after_hash(&buffer("#a"), 1),
            "the char before is `#` at 0"
        );
        assert!(
            word_after_hash(&buffer("x#a"), 2),
            "`#` sits immediately before"
        );
        assert!(
            !word_after_hash(&buffer("xa"), 2),
            "no hash precedes the word"
        );
    }

    #[test]
    fn code_range_wins_over_an_overlapping_math_range() {
        // Both ranges contain the word start; code is checked first.
        assert_eq!(
            completion_context(
                &buffer("#let x"),
                5,
                std::slice::from_ref(&(5..6)),
                std::slice::from_ref(&(5..6))
            ),
            CompletionContext::Code
        );
    }

    #[test]
    fn char_before_caret_handles_cjk_and_buffer_start() {
        assert_eq!(char_before_caret(&buffer("abc"), 2), Some('b'));
        assert_eq!(char_before_caret(&buffer("你好"), 2), Some('好'));
        assert_eq!(char_before_caret(&buffer("abc"), 0), None);
    }

    #[test]
    fn rank_keeps_input_order_within_a_rank() {
        // Every name starts with the needle, so the definition list order and
        // then the library order must survive.
        let definitions = [candidate("ab", CompletionKind::Local)];
        let library = [
            candidate("aa", CompletionKind::Value),
            candidate("ac", CompletionKind::Value),
        ];
        let ranked = rank_completions("a", &definitions, &library, false, 50);
        let names: Vec<&str> = ranked.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["ab", "aa", "ac"]);
    }

    #[test]
    fn call_slot_items_caps_the_merged_result() {
        let cap = crate::COMPLETION_MAX_DEFAULT;
        let call = CallParams {
            range: BytePos(0)..BytePos(1),
            items: (0..cap + 10)
                .map(|i| candidate(&format!("p{i:03}"), CompletionKind::Param))
                .collect(),
            value_slot: None,
        };
        let items = call_slot_items("p", &call, &[], &[], cap);
        assert_eq!(items.len(), cap);
    }

    #[test]
    fn call_slot_items_honours_the_requested_cap() {
        let call = CallParams {
            range: BytePos(0)..BytePos(1),
            items: (0..10)
                .map(|i| candidate(&format!("p{i:03}"), CompletionKind::Param))
                .collect(),
            value_slot: None,
        };
        let items = call_slot_items("p", &call, &[], &[], 3);
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn completion_context_marks_a_caret_directly_after_hash_as_code() {
        assert_eq!(
            completion_context(&buffer("#"), 1, &[], &[]),
            CompletionContext::Code
        );
    }

    #[test]
    fn rank_keeps_math_symbols_that_differ_only_by_case() {
        let library = [
            candidate("Delta", CompletionKind::MathSymbol),
            candidate("delta", CompletionKind::MathSymbol),
        ];
        let ranked = rank_completions("delta", &[], &library, true, 50);
        let names: Vec<&str> = ranked.iter().map(|item| item.name.as_str()).collect();
        assert!(names.contains(&"delta"), "lowercase symbol: {names:?}");
        assert!(names.contains(&"Delta"), "uppercase symbol: {names:?}");
    }

    #[test]
    fn dotted_method_items_offer_where_for_a_callable_head() {
        let library = [
            candidate("heading", CompletionKind::Element),
            candidate("black", CompletionKind::Value),
        ];
        let items = dotted_method_items("heading.", &library);
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert!(names.contains(&"heading.where"), "{names:?}");
        let where_item = items
            .iter()
            .find(|item| item.name == "heading.where")
            .expect("where candidate");
        assert_eq!(where_item.insert, "heading.where(");
        assert_eq!(where_item.cursor_back, CharCount(1));

        assert!(
            dotted_method_items("heading.w", &library)
                .iter()
                .any(|item| item.name == "heading.where")
        );
        assert!(
            dotted_method_items("black.", &library).is_empty(),
            "a non-callable head offers no methods"
        );
    }

    #[test]
    fn callable_accept_covers_kinds_and_open_parens() {
        assert!(is_callable_accept(&CompletionItem::plain(
            "heading",
            CompletionKind::Element
        )));
        assert!(is_callable_accept(&CompletionItem::plain(
            "sqrt",
            CompletionKind::MathFunction
        )));
        let dotted = CompletionItem {
            name: "heading.where".to_owned(),
            insert: "heading.where(".to_owned(),
            kind: CompletionKind::Function,
            cursor_back: CharCount(1),
            docs: None,
        };
        assert!(is_callable_accept(&dotted));
        assert!(!is_callable_accept(&CompletionItem::plain(
            "red",
            CompletionKind::Value
        )));
        let constructor = CompletionItem {
            name: "rgb()".to_owned(),
            insert: "rgb()".to_owned(),
            kind: CompletionKind::Value,
            cursor_back: CharCount(1),
            docs: None,
        };
        assert!(!is_callable_accept(&constructor));
    }

    #[test]
    fn numeric_prefix_only_accepts_digit_led_tokens() {
        assert!(numeric_prefix("12"));
        assert!(numeric_prefix("12.5"));
        assert!(numeric_prefix("12p"));
        assert!(!numeric_prefix(""));
        assert!(!numeric_prefix("pt"));
        assert!(!numeric_prefix(".5"));
    }

    #[test]
    fn member_trigger_fires_only_for_a_dot_after_an_identifier_like_char() {
        for prev in ['a', 'Z', '9', '_', ')', ']', '你'] {
            assert!(
                is_member_completion_trigger(Some(prev), '.'),
                "`.` after `{prev}` must trigger"
            );
        }
        for prev in [' ', '(', '.', ',', '"', '+', '\n'] {
            assert!(
                !is_member_completion_trigger(Some(prev), '.'),
                "`.` after `{prev}` must not trigger"
            );
        }
        assert!(!is_member_completion_trigger(None, '.'), "no char before");
        assert!(
            !is_member_completion_trigger(Some('a'), 'b'),
            "only a dot triggers"
        );
    }

    #[test]
    fn member_prefix_starts_after_the_dot() {
        // `it.` at caret 3: the empty member right after the dot.
        assert_eq!(member_prefix(&buffer("it."), "it.", 3), (3, ""));
        // `it.ch` at caret 5: only the member identifier is the prefix.
        assert_eq!(member_prefix(&buffer("it.ch"), "it.ch", 5), (3, "ch"));
        assert_eq!(member_prefix(&buffer("#d.foo"), "#d.foo", 6), (3, "foo"));
        // At the buffer start there is no member.
        assert_eq!(member_prefix(&buffer(""), "", 0), (0, ""));
    }

    #[test]
    fn member_context_classifies_markup_code_math_and_stale_ranges() {
        assert_eq!(
            member_completion_context(&buffer("hello."), 6, &[], &[]),
            CompletionContext::Markup,
            "a half-width dot in markup is not a member position"
        );
        assert_eq!(
            member_completion_context(&buffer("#foo."), 5, &[], &[]),
            CompletionContext::Code,
            "the `#` starts logic mode"
        );
        assert_eq!(
            member_completion_context(&buffer("x."), 2, &[], std::slice::from_ref(&(0..2))),
            CompletionContext::Math,
            "a word inside a math range is math"
        );
        assert_eq!(
            member_completion_context(&buffer("foo()."), 6, std::slice::from_ref(&(0..5)), &[]),
            CompletionContext::Code,
            "the stale range still covers the char before the dot"
        );
        assert_eq!(
            member_completion_context(&buffer(""), 0, &[], &[]),
            CompletionContext::Markup,
            "an empty buffer is markup"
        );
    }

    #[test]
    fn member_context_does_not_promote_a_word_after_a_stale_code_token() {
        // The stale range covers `#foo`, not the following markup word: the
        // fallback must not treat the space-separated `bar` as code.
        assert_eq!(
            member_completion_context(&buffer("#foo bar."), 9, std::slice::from_ref(&(0..4)), &[]),
            CompletionContext::Markup
        );
    }

    #[test]
    fn dotted_member_items_keep_only_sub_items_of_the_head() {
        let definitions = [candidate("cell.local", CompletionKind::Local)];
        let library = [
            candidate("cell.x", CompletionKind::Value),
            candidate("cell.colspan", CompletionKind::Value),
            candidate("grid.cell.x", CompletionKind::Value),
            candidate("table.cell", CompletionKind::Value),
        ];
        let items = dotted_member_items("cell.", &definitions, &library, false, 50);
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["cell.local", "cell.x", "cell.colspan"]);
        assert!(
            !names.contains(&"grid.cell.x"),
            "a name merely containing `cell.` is not a sub-item: {names:?}"
        );
        assert!(!names.contains(&"table.cell"), "no unrelated heads");
    }

    #[test]
    fn dotted_member_items_without_a_dot_falls_back_to_ranking() {
        let library = [
            candidate("grid.cell", CompletionKind::Value),
            candidate("figure", CompletionKind::Value),
        ];
        let items = dotted_member_items("cel", &[], &library, false, 50);
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["grid.cell"], "contains ranking still matches");
    }

    #[test]
    fn dotted_member_items_keep_math_cases_distinct() {
        let library = [
            candidate("plus.minus", CompletionKind::MathSymbol),
            candidate("Plus.minus", CompletionKind::MathSymbol),
        ];
        let items = dotted_member_items("plus.", &[], &library, true, 50);
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["plus.minus", "Plus.minus"]);
    }

    #[test]
    fn world_signature_distinguishes_lists_and_empties() {
        let a = [candidate("alpha", CompletionKind::Value)];
        let b = [candidate("beta", CompletionKind::Value)];
        assert_eq!(world_completions_signature(&[]), None);
        let sig_a = world_completions_signature(&a).expect("non-empty");
        let sig_b = world_completions_signature(&b).expect("non-empty");
        assert_ne!(sig_a, sig_b, "different members yield different signatures");
        assert_eq!(
            world_completions_signature(&a),
            world_completions_signature(&a.clone()),
            "the same list is stable"
        );
        assert_eq!(sig_a.0, 1, "the signature carries the length");
    }
}
