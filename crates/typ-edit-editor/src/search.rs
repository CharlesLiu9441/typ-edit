//! KMP (Knuth–Morris–Pratt) pattern matching backing the editor's find bar.

use std::ops::Range;

/// Char-wise KMP automaton (prefix-function based).
pub struct KmpAutomaton {
    pattern: Vec<char>,
    failure: Vec<usize>,
    case_sensitive: bool,
}

impl KmpAutomaton {
    pub fn new(pattern: &str, case_sensitive: bool) -> Self {
        let pattern: Vec<char> = pattern.chars().collect();
        let mut failure = vec![0usize; pattern.len()];
        let mut k = 0usize;
        for i in 1..pattern.len() {
            while k > 0 && !chars_match(pattern[i], pattern[k], case_sensitive) {
                k = failure[k - 1];
            }
            if chars_match(pattern[i], pattern[k], case_sensitive) {
                k += 1;
            }
            failure[i] = k;
        }
        Self {
            pattern,
            failure,
            case_sensitive,
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.pattern.is_empty()
    }

    /// All non-overlapping matches as ascending byte ranges: scanning resumes
    /// after each match, so `"aa"` in `"aaaa"` yields `0..2` and `2..4`.
    pub fn find_all(&self, text: &str) -> Vec<Range<usize>> {
        let mut matches = Vec::new();
        let m = self.pattern.len();
        if m == 0 {
            return matches;
        }
        // Keep only the byte start of each of the last `m` chars. Collecting
        // the whole `(byte, char)` sequence here is a large allocation on a
        // multi-megabyte document and is not needed for KMP bookkeeping.
        let mut starts = std::collections::VecDeque::with_capacity(m);
        let mut q = 0usize;
        for (byte, ch) in text.char_indices() {
            starts.push_back(byte);
            if starts.len() > m {
                starts.pop_front();
            }
            while q > 0 && !chars_match(self.pattern[q], ch, self.case_sensitive) {
                q = self.failure[q - 1];
            }
            if chars_match(self.pattern[q], ch, self.case_sensitive) {
                q += 1;
            }
            if q == m {
                let start = starts.front().copied().unwrap_or(byte);
                matches.push(start..byte + ch.len_utf8());
                q = 0;
                starts.clear();
            }
        }
        matches
    }
}

fn chars_match(a: char, b: char, case_sensitive: bool) -> bool {
    if a == b {
        return true;
    }
    if case_sensitive {
        return false;
    }
    // The common editor text/query path is ASCII; avoid constructing two
    // Unicode lowercase iterators for every character of a multi-megabyte scan.
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(&b);
    }
    a.to_lowercase().eq(b.to_lowercase())
}

/// Upper bound on a regex pattern, in bytes. Guards against pathological
/// compilation cost from a pasted wall of text; well above any realistic query.
pub const MAX_REGEX_LEN: usize = 4 * 1024;

/// A word character for whole-word matching: Unicode alphanumerics (which
/// includes CJK ideographs) plus `_`. This mirrors the regex crate's Unicode
/// `\b` boundaries, so the literal and regex whole-word modes agree.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// The char immediately before byte `at`, or `None` at the string start.
fn char_before(text: &str, at: usize) -> Option<char> {
    text.get(..at)?.chars().next_back()
}

/// The char at byte `at`, or `None` at the string end.
fn char_at(text: &str, at: usize) -> Option<char> {
    text.get(at..)?.chars().next()
}

/// Whether `range` is flanked by non-word characters (or the string edges).
fn whole_word_ok(text: &str, range: &Range<usize>) -> bool {
    let before_ok = char_before(text, range.start).is_none_or(|ch| !is_word_char(ch));
    let after_ok = char_at(text, range.end).is_none_or(|ch| !is_word_char(ch));
    before_ok && after_ok
}

/// Clamps `scope` into `0..=len`, keeping `start <= end`. A missing/collapsed
/// scope (e.g. after the buffer shrank) yields an empty range that matches
/// nothing.
fn clamp_scope(scope: Range<usize>, len: usize) -> Range<usize> {
    let start = scope.start.min(len);
    let end = scope.end.min(len).max(start);
    start..end
}

/// Compiles `pattern` as a regex and returns its byte matches.
///
/// Whole-word is expressed by wrapping the whole pattern in `\b(?:…)\b`: the
/// non-capturing group keeps alternations (`a|b`) scoped so the boundaries
/// apply to the entire pattern rather than just the last alternative.
fn find_regex_matches(
    text: &str,
    pattern: &str,
    case_sensitive: bool,
    whole_word: bool,
) -> Result<Vec<Range<usize>>, String> {
    if pattern.len() > MAX_REGEX_LEN {
        return Err(format!(
            "pattern too long ({} bytes, max {MAX_REGEX_LEN})",
            pattern.len()
        ));
    }
    let body = if whole_word {
        format!(r"\b(?:{pattern})\b")
    } else {
        pattern.to_owned()
    };
    let source = if case_sensitive {
        body
    } else {
        format!("(?i){body}")
    };
    let regex = regex::Regex::new(&source).map_err(|err| err.to_string())?;
    Ok(regex.find_iter(text).map(|m| m.start()..m.end()).collect())
}

/// Mode-aware match computation shared by the find bar.
///
/// `query` is the already-decoded pattern (see [`decode_newlines`]). The literal
/// path keeps the existing KMP semantics; the regex path uses the `regex` crate
/// (linear-time, no catastrophic backtracking). In both paths an optional
/// `scope` restricts matches to those fully inside the given byte range, and a
/// whole-word toggle filters on Unicode word boundaries. An empty query always
/// yields no matches. Invalid regexes are reported as `Err(message)`; the
/// message is meant for a tooltip, not for direct display.
pub fn find_matches(
    text: &str,
    query: &str,
    case_sensitive: bool,
    regex: bool,
    whole_word: bool,
    scope: Option<Range<usize>>,
) -> Result<Vec<Range<usize>>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut matches = if regex {
        find_regex_matches(text, query, case_sensitive, whole_word)?
    } else if case_sensitive {
        // `str::match_indices` is memchr-backed and has the same
        // non-overlapping semantics as the KMP literal path.
        text.match_indices(query)
            .map(|(start, _)| start..start + query.len())
            .collect()
    } else {
        let automaton = KmpAutomaton::new(query, case_sensitive);
        automaton.find_all(text)
    };
    if whole_word && !regex {
        matches.retain(|range| whole_word_ok(text, range));
    }
    if let Some(scope) = scope {
        let scope = clamp_scope(scope, text.len());
        matches.retain(|range| range.start >= scope.start && range.end <= scope.end);
    }
    Ok(matches)
}

/// Escapes a query/replacement so it survives a single-line find field: a
/// newline becomes the two characters `\` `n` and a backslash becomes `\` `\`.
///
/// The single left-to-right pass makes this the exact inverse of
/// [`decode_newlines`] for every input.
pub fn encode_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

/// Decodes the escaping produced by [`encode_newlines`].
///
/// A single left-to-right pass: `\n` becomes a newline and `\\` a backslash;
/// a backslash before anything else is kept verbatim (both characters), so
/// `\\n` is a literal backslash followed by `n`.
pub fn decode_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splices `replacement` over the given ascending byte ranges. Ranges that
/// start before the previous replacement ends are skipped so the result stays
/// well-formed. Returns the new text and the number of replacements applied.
pub fn replace_all(text: &str, matches: &[Range<usize>], replacement: &str) -> (String, usize) {
    if matches.is_empty() {
        return (text.to_owned(), 0);
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    let mut count = 0usize;
    for range in matches {
        if range.start < last || range.end > text.len() || range.start > range.end {
            continue;
        }
        out.push_str(&text[last..range.start]);
        out.push_str(replacement);
        last = range.end;
        count += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::{
        KmpAutomaton, MAX_REGEX_LEN, decode_newlines, encode_newlines, find_matches, replace_all,
    };

    /// Runs the default literal mode (no regex, no whole-word, no scope).
    fn literal(pattern: &str, text: &str) -> Vec<std::ops::Range<usize>> {
        find_matches(text, pattern, true, false, false, None).expect("literal mode never fails")
    }

    fn find(pattern: &str, text: &str) -> Vec<std::ops::Range<usize>> {
        KmpAutomaton::new(pattern, true).find_all(text)
    }

    #[test]
    fn empty_pattern_matches_nothing() {
        let automaton = KmpAutomaton::new("", true);
        assert!(automaton.is_empty());
        assert!(automaton.find_all("abc").is_empty());
    }

    #[test]
    fn non_empty_pattern_is_not_empty() {
        assert!(!KmpAutomaton::new("a", false).is_empty());
    }

    #[test]
    fn finds_single_ascii_occurrence() {
        assert_eq!(find("bcd", "abcde"), vec![1..4]);
    }

    #[test]
    fn finds_multiple_non_overlapping_matches() {
        assert_eq!(find("aa", "aaaa"), vec![0..2, 2..4]);
        assert_eq!(find("aba", "ababa"), vec![0..3]);
    }

    #[test]
    fn backtracks_through_repeated_prefixes() {
        assert_eq!(find("abababc", "abababcabababc"), vec![0..7, 7..14]);
        assert_eq!(find("aab", "aabaaab"), vec![0..3, 4..7]);
    }

    #[test]
    fn no_match_yields_nothing() {
        assert!(find("xyz", "abcabc").is_empty());
    }

    #[test]
    fn pattern_longer_than_text() {
        assert!(find("abcdef", "abc").is_empty());
    }

    #[test]
    fn multibyte_text_yields_byte_ranges() {
        assert_eq!(find("你好", "你好世界，你好"), vec![0..6, 15..21]);
    }

    #[test]
    fn case_insensitive_finds_all_variants() {
        let automaton = KmpAutomaton::new("hello", false);
        assert_eq!(
            automaton.find_all("Hello HELLO hello"),
            vec![0..5, 6..11, 12..17]
        );
    }

    #[test]
    fn case_sensitive_matches_exact_only() {
        assert_eq!(find("hello", "Hello HELLO hello"), vec![12..17]);
    }

    #[test]
    fn case_insensitive_handles_non_ascii() {
        let automaton = KmpAutomaton::new("ärger", false);
        assert_eq!(
            automaton.find_all("Ärger ärger ÄRGER"),
            vec![0..6, 7..13, 14..20]
        );
    }

    #[test]
    fn replace_all_splices_in_order() {
        let text = "aXbXc";
        let matches = find("X", text);
        let (result, count) = replace_all(text, &matches, "YY");
        assert_eq!(result, "aYYbYYc");
        assert_eq!(count, 2);
    }

    #[test]
    fn replace_all_without_matches_returns_the_original() {
        let (result, count) = replace_all("abc", &[], "x");
        assert_eq!(result, "abc");
        assert_eq!(count, 0);
    }

    #[test]
    fn replacement_may_contain_the_pattern() {
        let text = "aaa";
        let matches = find("a", text);
        let (result, count) = replace_all(text, &matches, "aa");
        assert_eq!(result, "aaaaaa");
        assert_eq!(count, 3);
    }

    #[test]
    fn replace_all_handles_multibyte_text() {
        let text = "你好你好";
        let matches = find("你好", text);
        let (result, count) = replace_all(text, &matches, "嗨");
        assert_eq!(result, "嗨嗨");
        assert_eq!(count, 2);
    }

    #[test]
    fn replace_all_skips_overlapping_ranges() {
        let (result, count) = replace_all("abcdef", &[0..3, 1..4], "-");
        assert_eq!(result, "-def");
        assert_eq!(count, 1);
    }

    #[test]
    fn encode_newlines_escapes_newlines_and_backslashes() {
        assert_eq!(encode_newlines("a\nb"), "a\\nb");
        assert_eq!(encode_newlines("a\\b"), "a\\\\b");
        assert_eq!(encode_newlines("a\\nb"), "a\\\\nb");
        assert_eq!(encode_newlines("plain"), "plain");
        assert_eq!(encode_newlines(""), "");
    }

    #[test]
    fn decode_newlines_expands_newlines_and_backslashes() {
        assert_eq!(decode_newlines("a\\nb"), "a\nb");
        assert_eq!(decode_newlines("a\\\\b"), "a\\b");
        assert_eq!(decode_newlines("a\\\\nb"), "a\\nb");
        // A backslash before anything else stays literal.
        assert_eq!(decode_newlines("a\\tb"), "a\\tb");
        assert_eq!(decode_newlines("trailing\\"), "trailing\\");
        assert_eq!(decode_newlines("plain"), "plain");
    }

    #[test]
    fn newline_escaping_round_trips() {
        for text in [
            "",
            "a\nb",
            "a\\b",
            "line1\nline2\n",
            "\n\\\n\\\\",
            "你好\n世界",
        ] {
            assert_eq!(decode_newlines(&encode_newlines(text)), text, "{text:?}");
        }
    }

    #[test]
    fn decoded_newline_pattern_matches_multiline_text() {
        let pattern = decode_newlines("a\\nb");
        assert_eq!(pattern, "a\nb");
        let automaton = KmpAutomaton::new(&pattern, true);
        assert_eq!(automaton.find_all("xa\nby"), vec![1..4]);
    }

    #[test]
    fn default_literal_mode_matches_the_kmp_entry_point() {
        assert_eq!(literal("aba", "ababa"), vec![0..3]);
        assert_eq!(literal("aa", "aaaa"), vec![0..2, 2..4]);
        assert_eq!(literal("你好", "你好世界，你好"), vec![0..6, 15..21]);
        let automaton = KmpAutomaton::new("abababc", true);
        assert_eq!(
            literal("abababc", "abababcabababc"),
            automaton.find_all("abababcabababc")
        );
    }

    #[test]
    fn literal_whole_word_rejects_substrings() {
        assert_eq!(
            find_matches("foo foobar foo_bar", "foo", true, false, true, None),
            Ok(std::iter::once(0..3).collect::<Vec<_>>())
        );
        assert_eq!(
            find_matches("foo foobar foo_bar", "foo", true, false, false, None),
            Ok(vec![0..3, 4..7, 11..14])
        );
    }

    #[test]
    fn literal_whole_word_treats_cjk_as_word_chars() {
        assert_eq!(
            find_matches("你好 你好吗", "你好", true, false, true, None),
            Ok(std::iter::once(0..6).collect::<Vec<_>>())
        );
    }

    #[test]
    fn regex_basic_match() {
        assert_eq!(
            find_matches("foo fxo", "f.o", true, true, false, None),
            Ok(vec![0..3, 4..7])
        );
    }

    #[test]
    fn regex_case_insensitive() {
        assert_eq!(
            find_matches("Foo FOO foo", "foo", false, true, false, None),
            Ok(vec![0..3, 4..7, 8..11])
        );
    }

    #[test]
    fn regex_whole_word_wraps_boundaries() {
        assert_eq!(
            find_matches("cat concatenate", "cat", true, true, true, None),
            Ok(std::iter::once(0..3).collect::<Vec<_>>())
        );
        assert_eq!(
            find_matches("你好吗 你好", "你好", true, true, true, None),
            Ok(std::iter::once(10..16).collect::<Vec<_>>())
        );
    }

    #[test]
    fn invalid_regex_is_err() {
        assert!(find_matches("abc", "(", true, true, false, None).is_err());
    }

    #[test]
    fn overlong_regex_is_err() {
        let long = "a".repeat(MAX_REGEX_LEN + 1);
        assert!(find_matches("abc", &long, true, true, false, None).is_err());
    }

    #[test]
    fn scope_keeps_only_matches_fully_inside() {
        assert_eq!(
            find_matches("foo bar foo", "foo", true, false, false, Some(4..11)),
            Ok(std::iter::once(8..11).collect::<Vec<_>>())
        );
        assert_eq!(
            find_matches("foo bar foo", "foo", true, false, false, Some(0..3)),
            Ok(std::iter::once(0..3).collect::<Vec<_>>())
        );
        assert_eq!(
            find_matches("foo bar foo", "b.r", true, true, false, Some(4..7)),
            Ok(std::iter::once(4..7).collect::<Vec<_>>())
        );
    }

    #[test]
    fn scope_is_clamped_to_the_text_length() {
        assert_eq!(
            find_matches("foo bar", "foo", true, false, false, Some(0..100)),
            Ok(std::iter::once(0..3).collect::<Vec<_>>())
        );
        assert_eq!(
            find_matches("foo bar", "foo", true, false, false, Some(100..200)),
            Ok(Vec::new())
        );
    }

    #[test]
    fn empty_query_matches_nothing_in_every_mode() {
        assert_eq!(
            find_matches("abc", "", true, false, false, None),
            Ok(vec![])
        );
        assert_eq!(
            find_matches("abc", "", true, true, true, Some(0..3)),
            Ok(vec![])
        );
    }
}
